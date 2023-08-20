use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use duct::cmd;
use itertools::Itertools;
use ratatui::{
  layout::{Alignment, Constraint, Direction, Layout, Rect},
  style::{Color, Modifier, Style},
  text::{Line, Span},
  widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use tokio::sync::mpsc::{self, UnboundedSender};
use tracing::{info, trace};
use tui_input::{backend::crossterm::EventHandler, Input};

use super::{logger::Logger, Component, Frame};
use crate::{action::Action, systemd::UnitStatus};

#[derive(Default, Copy, Clone, PartialEq, Eq)]
pub enum Mode {
  #[default]
  Normal,
  Search,
  Processing,
}

#[derive(Default)]
pub struct Home {
  pub logger: Logger,
  pub show_logger: bool,
  pub counter: usize,
  pub ticker: usize,
  pub all_units: Vec<UnitStatus>,
  pub filtered_units: StatefulList<UnitStatus>,
  pub logs: Vec<String>,
  pub mode: Mode,
  pub input: Input,
  pub action_tx: Option<mpsc::UnboundedSender<Action>>,
}

pub struct StatefulList<T> {
  state: ListState,
  items: Vec<T>,
}

impl Default for StatefulList<UnitStatus> {
  fn default() -> Self {
    Self::with_items(vec![])
  }
}

impl<T> StatefulList<T> {
  pub fn with_items(items: Vec<T>) -> StatefulList<T> {
    StatefulList { state: ListState::default(), items }
  }

  fn selected(&self) -> Option<&T> {
    match self.state.selected() {
      Some(i) => Some(&self.items[i]),
      None => None,
    }
  }

  fn next(&mut self) {
    let i = match self.state.selected() {
      Some(i) => {
        if i >= self.items.len() - 1 {
          0
        } else {
          i + 1
        }
      },
      None => 0,
    };
    self.state.select(Some(i));
  }

  fn previous(&mut self) {
    let i = match self.state.selected() {
      Some(i) => {
        if i == 0 {
          self.items.len() - 1
        } else {
          i - 1
        }
      },
      None => 0,
    };
    self.state.select(Some(i));
  }

  fn unselect(&mut self) {
    self.state.select(None);
  }
}

impl Home {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn set_units(&mut self, units: Vec<UnitStatus>) {
    self.all_units = units.clone();
    self.filtered_units = StatefulList::with_items(units);
  }

  // TODO: do we need tick at all?
  pub fn tick(&mut self) {
    trace!("Tick");
    self.ticker = self.ticker.saturating_add(1);
  }


  pub fn next(&mut self) {
    self.logs = vec![];
    self.filtered_units.next();
    self.get_logs();
  }

  pub fn previous(&mut self) {
    self.logs = vec![];
    self.filtered_units.previous();
    self.get_logs();
  }

  pub fn unselect(&mut self) {
    self.logs = vec![];
    self.filtered_units.unselect();
  }

  pub fn get_logs(&mut self) {
    if let Some(selected) = self.filtered_units.selected() {
      let tx = self.action_tx.clone().unwrap();
      let unit_name = selected.name.to_string();

      tokio::spawn(async move {
        info!("Getting logs for {}", unit_name);
        // TODO: is this the best place to load logs?
        // TODO: figure out how to stream logs
        // TODO: debounce?, journald is kinda slow
        if let Ok(stdout) = cmd!("journalctl", "-u", unit_name.clone()).read() {
          tx.send(Action::SetLogs{ unit_name, logs: stdout }).unwrap();
        }

        tx.send(Action::RenderTick).unwrap();
      });
    } else {
      self.logs = vec![];
    }
  }
}

impl Component for Home {
  fn init(&mut self, tx: UnboundedSender<Action>) -> anyhow::Result<()> {
    self.action_tx = Some(tx);
    Ok(())
  }

  fn handle_key_events(&mut self, key: KeyEvent) -> Action {
    match self.mode {
      Mode::Normal | Mode::Processing => {
        match key.code {
          KeyCode::Char('q') => Action::Quit,
          KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => Action::Quit,
          KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Action::Quit,
          KeyCode::Char('z') if key.modifiers.contains(KeyModifiers::CONTROL) => Action::Suspend,
          KeyCode::Char('l') => Action::ToggleShowLogger,
          KeyCode::Up => {
            // if we're filtering the list, and we're at the top, and there's text in the search box, go to search mode
            if self.filtered_units.state.selected() == Some(0) && self.input.value() != "" {
              return Action::EnterSearch;
            }

            self.previous();
            Action::Update // is this right?
          },
          KeyCode::Down => {
            self.next();
            Action::Update // is this right?
          },
          KeyCode::Char('/') => Action::EnterSearch,
          _ => Action::Tick,
        }
      },
      Mode::Search => match key.code {
        KeyCode::Esc | KeyCode::Enter => Action::EnterNormal,
        KeyCode::Down | KeyCode::Tab => {
          self.next();
          Action::EnterNormal
        },
        _ => {
          self.input.handle_event(&crossterm::event::Event::Key(key));
          self.unselect();

          let matching = self.all_units.iter().filter(|u| u.name.contains(&self.input.value())).cloned().collect_vec();
          self.filtered_units.items = matching;
          Action::Update
        },
      },
    }
  }

  fn dispatch(&mut self, action: Action) -> Option<Action> {
    match action {
      Action::Tick => self.tick(),
      Action::ToggleShowLogger => self.show_logger = !self.show_logger,
      Action::EnterNormal => {
        self.mode = Mode::Normal;
      },
      Action::EnterSearch => {
        self.mode = Mode::Search;
      },
      Action::EnterProcessing => {
        self.mode = Mode::Processing;
      },
      Action::ExitProcessing => {
        // TODO: Make this go to previous mode instead
        self.mode = Mode::Normal;
      },
      Action::SetLogs{ unit_name: service_name, logs } => {
        if let Some(selected) = self.filtered_units.selected() {
          if selected.name == service_name {
            // split by lines
            let mut logs = logs.split("\n")
            .map(String::from)
            .collect_vec();
            logs.reverse();
            self.logs = logs;
          }
        }
      },
      _ => (),
    }
    None
  }

  fn render(&mut self, f: &mut Frame<'_>, rect: Rect) {
    let rect = if self.show_logger {
      let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rect);
      self.logger.render(f, chunks[1]);
      chunks[0]
    } else {
      rect
    };

    let rects = Layout::default().constraints([Constraint::Min(3), Constraint::Percentage(100)].as_ref()).split(rect);
    let search_panel = rects[0];
    let main_panel = rects[1];

    let items: Vec<ListItem> = self.filtered_units.items.iter().map(|i| ListItem::new(&*i.name)).collect();

    // Create a List from all list items and highlight the currently selected one
    let items = List::new(items)
      .block(Block::default().borders(Borders::ALL).title("Services"))
      .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD));

    let chunks = Layout::default()
      .direction(Direction::Horizontal)
      .constraints([Constraint::Percentage(30), Constraint::Percentage(70)].as_ref())
      .split(main_panel);

    f.render_stateful_widget(items, chunks[0], &mut self.filtered_units.state);

    let selected_item = match self.filtered_units.state.selected() {
      Some(i) => Some(&self.filtered_units.items[i]),
      None => None,
    };

    // this is expensive to rebuild every time, should we cache it?
    let text = if let Some(i) = selected_item {
      let mut lines = vec![
        Line::from(format!("Name: {}", i.name)),
        Line::from(format!("Description: {}", i.description)),
        Line::from(format!("Load State: {}", i.load_state)),
        Line::from(format!("Active State: {}", i.active_state)),
        Line::from(format!("Sub State: {}", i.sub_state)),
        Line::from(format!("Followed: {}", i.followed)),
        Line::from(format!("Path: {}", i.path)),
        Line::from(format!("Job ID: {}", i.job_id)),
        Line::from(format!("Job Type: {}", i.job_type)),
        Line::from(format!("Job Path: {}", i.job_path)),
        Line::from(""),
      ];

      lines.extend(self.logs.iter().map(|l| Line::from(l.clone())));

      lines
    } else {
      vec![]
    };

    let paragraph = Paragraph::new(text)
        .block(Block::default().title("Service").borders(Borders::ALL))
        .style(Style::default().fg(Color::White).bg(Color::Black))
        // .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });
    f.render_widget(paragraph, chunks[1]);

    // f.render_widget(
    //   Paragraph::new(format!(
    //     "Press j or k to increment or decrement.\n\nCounter: {}\n\nTicker: {}",
    //     self.counter, self.ticker
    //   ))
    //   .block(
    //     Block::default()
    //       .title("Template")
    //       .title_alignment(Alignment::Center)
    //       .borders(Borders::ALL)
    //       .border_style(match self.mode {
    //         Mode::Processing => Style::default().fg(Color::Yellow),
    //         _ => Style::default(),
    //       })
    //       .border_type(BorderType::Rounded),
    //   )
    //   .style(Style::default().fg(Color::Cyan))
    //   .alignment(Alignment::Center),
    //   main_panel,
    // );
    let width = search_panel.width.max(3) - 3; // keep 2 for borders and 1 for cursor
    let scroll = self.input.visual_scroll(width as usize);
    let input = Paragraph::new(self.input.value())
      .style(match self.mode {
        Mode::Search => Style::default().fg(Color::LightGreen),
        _ => Style::default(),
      })
      .scroll((0, scroll as u16))
      .block(Block::default().borders(Borders::ALL).title(Line::from(vec![
        Span::raw("Search "),
        Span::styled("(", Style::default().fg(Color::DarkGray)),
        Span::styled("/", Style::default().add_modifier(Modifier::BOLD).fg(Color::Gray)),
        Span::styled(" to focus, ", Style::default().fg(Color::DarkGray)),
        Span::styled("ESC", Style::default().add_modifier(Modifier::BOLD).fg(Color::Gray)),
        Span::styled(" to unfocus)", Style::default().fg(Color::DarkGray)),
      ])));
    f.render_widget(input, search_panel);
    if self.mode == Mode::Search {
      f.set_cursor(
        (search_panel.x + 1 + self.input.cursor() as u16).min(search_panel.x + search_panel.width - 2),
        search_panel.y + 1,
      )
    }
  }
}
