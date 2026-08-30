//! The interactive session (`valyria` with no arguments) — §4.28's "TUI
//! session". Like every other command it is a pure `valyria_protocol::
//! Client` consumer: it creates tasks, watches the event stream, and
//! sends pause/cancel/permission decisions, all over the trait. It works
//! identically against an embedded runtime or `--connect <socket>`.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use tokio::sync::mpsc;
use valyria_protocol::{
    Client, Empty, PermissionResolveRequest, Request, Response, TaskCreateRequest, TaskIdRequest,
    TaskSummary, WireEvent,
};

#[derive(PartialEq)]
enum Mode {
    Browsing,
    Composing,
}

struct App {
    client: Arc<dyn Client>,
    tasks: Vec<TaskSummary>,
    selected: ListState,
    log: Vec<String>,
    input: String,
    mode: Mode,
    status: String,
    should_quit: bool,
}

impl App {
    fn new(client: Arc<dyn Client>) -> Self {
        let mut selected = ListState::default();
        selected.select(Some(0));
        Self {
            client,
            tasks: Vec::new(),
            selected,
            log: vec!["welcome to valyria — press n for a new task, q to quit".into()],
            input: String::new(),
            mode: Mode::Browsing,
            status: String::new(),
            should_quit: false,
        }
    }

    fn selected_task_id(&self) -> Option<String> {
        self.selected
            .selected()
            .and_then(|i| self.tasks.get(i))
            .map(|t| t.task_id.clone())
    }

    async fn refresh_tasks(&mut self) {
        if let Response::TaskList(list) = self.client.call(Request::TaskList(Empty {})).await {
            self.tasks = list.tasks;
            if self.selected.selected().unwrap_or(0) >= self.tasks.len() {
                self.selected.select(if self.tasks.is_empty() {
                    None
                } else {
                    Some(self.tasks.len() - 1)
                });
            }
        }
    }

    fn log_event(&mut self, ev: &WireEvent) {
        let who = ev.task_id.as_deref().unwrap_or("-");
        let summary = if ev.kind == "state_changed" {
            let to = ev.payload.get("to").and_then(|v| v.as_str()).unwrap_or("?");
            format!("{who}  →  {to}")
        } else {
            format!("{who}  {}", ev.kind)
        };
        self.log.push(summary);
        let overflow = self.log.len().saturating_sub(500);
        if overflow > 0 {
            self.log.drain(0..overflow);
        }
    }

    async fn submit_objective(&mut self) {
        let objective = std::mem::take(&mut self.input);
        self.mode = Mode::Browsing;
        if objective.trim().is_empty() {
            return;
        }
        match self
            .client
            .call(Request::TaskCreate(TaskCreateRequest {
                objective,
                permission_mode: None,
            }))
            .await
        {
            Response::TaskCreate(r) => {
                self.status = format!("created {}", r.task_id);
                self.refresh_tasks().await;
            }
            Response::Error(e) => self.status = format!("error: {}", e.message),
            _ => {}
        }
    }

    async fn signal_selected(&mut self, make: fn(String) -> Request, label: &str) {
        let Some(id) = self.selected_task_id() else {
            return;
        };
        match self.client.call(make(id.clone())).await {
            Response::Ack => self.status = format!("{label} {id}"),
            Response::Error(e) => self.status = format!("error: {}", e.message),
            _ => {}
        }
    }

    async fn resolve_selected(&mut self, approve: bool) {
        let Some(id) = self.selected_task_id() else {
            return;
        };
        match self
            .client
            .call(Request::PermissionResolve(PermissionResolveRequest {
                task_id: id.clone(),
                approve,
            }))
            .await
        {
            Response::Ack => {
                self.status = format!("{} {id}", if approve { "allowed" } else { "denied" })
            }
            Response::Error(e) => self.status = format!("error: {}", e.message),
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.tasks.is_empty() {
            return;
        }
        let cur = self.selected.selected().unwrap_or(0) as isize;
        let next = (cur + delta).rem_euclid(self.tasks.len() as isize);
        self.selected.select(Some(next as usize));
    }

    async fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        if self.mode == Mode::Composing {
            match key.code {
                KeyCode::Enter => self.submit_objective().await,
                KeyCode::Esc => {
                    self.input.clear();
                    self.mode = Mode::Browsing;
                }
                KeyCode::Backspace => {
                    self.input.pop();
                }
                KeyCode::Char(c) => self.input.push(c),
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true
            }
            KeyCode::Char('n') => {
                self.mode = Mode::Composing;
                self.status.clear();
            }
            KeyCode::Char('r') => self.refresh_tasks().await,
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Char('p') => {
                self.signal_selected(
                    |id| Request::TaskPause(TaskIdRequest { task_id: id }),
                    "paused",
                )
                .await
            }
            KeyCode::Char('x') => {
                self.signal_selected(
                    |id| Request::TaskCancel(TaskIdRequest { task_id: id }),
                    "cancelled",
                )
                .await
            }
            KeyCode::Char('s') => {
                self.signal_selected(
                    |id| Request::TaskResume(TaskIdRequest { task_id: id }),
                    "resumed",
                )
                .await
            }
            KeyCode::Char('a') => self.resolve_selected(true).await,
            KeyCode::Char('d') => self.resolve_selected(false).await,
            _ => {}
        }
    }

    fn draw(&mut self, f: &mut Frame) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(3),
                Constraint::Length(3),
            ])
            .split(f.area());

        f.render_widget(
            Paragraph::new(Line::from(
                " valyria — n: new  ↑↓: select  p: pause  s: resume  x: cancel  a/d: allow/deny  r: refresh  q: quit",
            )),
            rows[0],
        );

        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(rows[1]);

        let items: Vec<ListItem> = self
            .tasks
            .iter()
            .map(|t| ListItem::new(vec![Line::from(format!("{:<10} {}", t.state, t.objective))]))
            .collect();
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title("tasks"))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("▶ ");
        f.render_stateful_widget(list, cols[0], &mut self.selected);

        let log_lines: Vec<Line> = self
            .log
            .iter()
            .rev()
            .take(cols[1].height.saturating_sub(2) as usize)
            .rev()
            .map(|l| Line::from(l.clone()))
            .collect();
        f.render_widget(
            Paragraph::new(log_lines)
                .block(Block::default().borders(Borders::ALL).title("events"))
                .wrap(Wrap { trim: true }),
            cols[1],
        );

        let bottom = match self.mode {
            Mode::Composing => format!("objective› {}", self.input),
            Mode::Browsing => {
                if self.status.is_empty() {
                    "ready".to_string()
                } else {
                    self.status.clone()
                }
            }
        };
        f.render_widget(
            Paragraph::new(bottom).block(Block::default().borders(Borders::ALL)),
            rows[2],
        );
    }
}

pub async fn run(client: Arc<dyn Client>) -> io::Result<()> {
    // Bail out cleanly if there is no terminal (piped/CI) rather than
    // corrupting the stream with escape codes.
    if !crossterm::tty::IsTty::is_tty(&io::stdout()) {
        eprintln!(
            "valyria: no interactive terminal; run a subcommand instead (see `valyria --help`)"
        );
        return Ok(());
    }

    let mut terminal = ratatui::init();
    let mut app = App::new(client.clone());
    app.refresh_tasks().await;

    let (key_tx, mut key_rx) = mpsc::unbounded_channel::<KeyEvent>();
    let input_task = std::thread::spawn(move || loop {
        if crossterm::event::poll(Duration::from_millis(200)).unwrap_or(false) {
            if let Ok(Event::Key(k)) = crossterm::event::read() {
                if key_tx.send(k).is_err() {
                    break;
                }
            }
        } else if key_tx.is_closed() {
            break;
        }
    });

    let mut events = client.subscribe_events(0).await;
    let mut tick = tokio::time::interval(Duration::from_millis(750));

    while !app.should_quit {
        terminal.draw(|f| app.draw(f))?;
        tokio::select! {
            Some(key) = key_rx.recv() => app.on_key(key).await,
            Some(ev) = events.next() => {
                app.log_event(&ev);
                if ev.kind == "state_changed" {
                    app.refresh_tasks().await;
                }
            }
            _ = tick.tick() => app.refresh_tasks().await,
        }
    }

    ratatui::restore();
    drop(key_rx); // lets the input thread notice and exit
    let _ = input_task.join();
    Ok(())
}
