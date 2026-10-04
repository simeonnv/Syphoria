mod addresses;
mod compose;
mod messages;
mod reader;

use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use enum_dispatch::enum_dispatch;
use exn_anyhow::into_anyhow;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::{Block, List, ListItem, ListState, Paragraph, Wrap},
};
use ratatui_textarea::{CursorMove, TextArea};
use termwiz::input::Modifiers;

use crate::{
    app::{App, InputEvent, KeyCode, KeyEvent, SshTerminal},
    mail::{Addr, Folder, Mail},
    state::{AppState, User},
    task::Task,
};

use addresses::Addresses;
use compose::{Compose, Draft};
use messages::Messages;
use reader::Reader;

type Load<T> = Task<Result<T>>;

const SIDEBAR_WIDTH: u16 = 28;
const REFRESH_TICKS: u32 = 50;

#[derive(Clone)]
pub struct Ctx {
    pub state: AppState,
    pub user: &'static User,
    pub mail: Arc<Mail>,
}

pub enum Action {
    None,
    Push(Page),
    Pop,
}

#[enum_dispatch]
pub trait View {
    fn draw(&mut self, frame: &mut Frame, area: Rect);
    fn input(&mut self, key: KeyEvent, ctx: &Ctx) -> Action;
    fn help(&self) -> &'static str;
    fn typing(&self) -> bool {
        false
    }
}

#[enum_dispatch(View)]
pub enum Page {
    Messages,
    Reader,
    Compose,
    Addresses,
}

#[derive(Clone, PartialEq, Eq)]
enum Nav {
    Folder { id: String, name: String },
    Compose,
    Addresses,
}

impl Nav {
    fn open(&self, ctx: &Ctx) -> Page {
        match self {
            Nav::Folder { id, name } => Messages::new(ctx, name.clone(), id.clone()).into(),
            Nav::Compose => Compose::new(ctx, Draft::default()).into(),
            Nav::Addresses => Addresses::new(ctx).into(),
        }
    }
}

struct Session {
    ctx: Ctx,
    folders: Vec<Folder>,
    refresh: Load<Vec<Folder>>,
    nav: Nav,
    stack: Vec<Page>,
    ticks: u32,
}

impl Session {
    fn new(ctx: Ctx) -> Self {
        let nav = Nav::Folder {
            id: ctx.mail.inbox.clone(),
            name: "Inbox".into(),
        };
        let mut session = Self {
            stack: vec![nav.open(&ctx)],
            ctx,
            folders: Vec::new(),
            refresh: Task::new(),
            nav,
            ticks: 0,
        };
        session.reload_folders();
        session
    }

    fn reload_folders(&mut self) {
        if matches!(self.refresh, Task::Running(_)) {
            return;
        }
        let mail = self.ctx.mail.clone();
        self.refresh
            .spawn(async move { mail.folders().await.map_err(into_anyhow) });
    }

    fn entries(&self) -> Vec<Nav> {
        self.folders
            .iter()
            .map(|f| Nav::Folder {
                id: f.id.clone(),
                name: f.name.clone(),
            })
            .chain([Nav::Compose, Nav::Addresses])
            .collect()
    }

    fn go(&mut self, nav: Nav) {
        self.stack = vec![nav.open(&self.ctx)];
        self.nav = nav;
        self.reload_folders();
    }

    fn step(&mut self, forward: bool) {
        let entries = self.entries();
        let last = entries.len().saturating_sub(1);
        let i = entries.iter().position(|e| *e == self.nav).unwrap_or(0);
        let i = if forward {
            (i + 1).min(last)
        } else {
            i.saturating_sub(1)
        };
        if let Some(nav) = entries.get(i).cloned() {
            self.go(nav);
        }
    }

    fn draw_sidebar(&mut self, frame: &mut Frame, area: Rect) {
        let fresh = match self.refresh.poll() {
            Task::Done(Ok(list)) => Some(list.clone()),
            _ => None,
        };
        if let Some(list) = fresh {
            self.folders = list;
            self.refresh = Task::new();
        }

        let block = Block::bordered().title(format!(" {} ", self.ctx.user.name));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        if self.folders.is_empty() {
            status(frame, inner, &self.refresh);
            return;
        }

        let folders = self.folders.iter().map(|f| {
            let mut spans = vec![Span::raw(f.name.clone())];
            if f.unread > 0 {
                spans.push(Span::raw(format!(" {}", f.unread)).bold());
            }
            ListItem::new(Line::from(spans))
        });
        let divider = ListItem::new("─".repeat(inner.width as usize)).dim();
        let items: Vec<ListItem> = folders
            .chain([
                divider,
                ListItem::new("Compose"),
                ListItem::new("Addresses"),
            ])
            .collect();

        let position = self.entries().iter().position(|e| *e == self.nav);
        let selected = position.map(|i| if i >= self.folders.len() { i + 1 } else { i });
        let mut state = ListState::default().with_selected(selected);
        frame.render_stateful_widget(
            List::new(items).highlight_style(Style::new().reversed()),
            inner,
            &mut state,
        );
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> &'static str {
        let [main, sidebar] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(SIDEBAR_WIDTH)])
                .areas(area);
        self.draw_sidebar(frame, sidebar);
        match self.stack.last_mut() {
            Some(page) => {
                page.draw(frame, main);
                page.help()
            }
            None => "",
        }
    }

    fn typing(&self) -> bool {
        self.stack.last().is_some_and(View::typing)
    }

    fn input(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(Modifiers::ALT)
            && matches!(key.key, KeyCode::UpArrow | KeyCode::DownArrow)
        {
            self.step(key.key == KeyCode::DownArrow);
            return false;
        }
        if !self.typing() && key.key == KeyCode::Char('q') {
            return true;
        }
        let Some(page) = self.stack.last_mut() else {
            return false;
        };
        match page.input(key, &self.ctx) {
            Action::None => {}
            Action::Push(page) => self.stack.push(page),
            Action::Pop if self.stack.len() > 1 => {
                self.stack.pop();
            }
            Action::Pop => {
                let inbox = Nav::Folder {
                    id: self.ctx.mail.inbox.clone(),
                    name: "Inbox".into(),
                };
                if self.nav != inbox {
                    self.go(inbox);
                }
            }
        }
        false
    }
}

pub struct Pages {
    user: &'static User,
    connecting: Load<Ctx>,
    session: Option<Session>,
    exit: bool,
}

impl Pages {
    pub fn new(state: AppState, user: &'static User) -> Self {
        Self {
            user,
            connecting: Task::new_spawn(async move {
                let mail = state.stalwart.mail(user).await.map_err(into_anyhow)?;
                Ok(Ctx {
                    state,
                    user,
                    mail: Arc::new(mail),
                })
            }),
            session: None,
            exit: false,
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let [body, footer] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());

        let (help, typing) = match &mut self.session {
            Some(session) => (session.draw(frame, body), session.typing()),
            None => {
                status(frame, body, self.connecting.poll());
                ("", false)
            }
        };
        let global = if typing {
            " · alt+↑/↓ sidebar"
        } else {
            " · alt+↑/↓ sidebar · q quit"
        };
        frame.render_widget(Line::from(format!(" {help}{global}")).dim(), footer);
    }
}

impl App for Pages {
    fn render(&mut self, terminal: &mut SshTerminal) -> Result<Option<String>> {
        if self.session.is_none()
            && let Task::Done(Ok(ctx)) = self.connecting.poll()
        {
            self.session = Some(Session::new(ctx.clone()));
        }
        terminal.draw(|frame| self.draw(frame))?;
        Ok(self.exit.then(|| format!("bye {}", self.user.name)))
    }

    fn input(&mut self, event: InputEvent) {
        let InputEvent::Key(key) = event else {
            return;
        };
        match &mut self.session {
            Some(session) => self.exit = session.input(key),
            None => {
                self.exit = matches!(key.key, KeyCode::Char('q') | KeyCode::Escape);
            }
        }
    }

    fn on_tick(&mut self) {
        if let Some(session) = &mut self.session {
            session.ticks = session.ticks.wrapping_add(1);
            if session.ticks.is_multiple_of(REFRESH_TICKS) {
                session.reload_folders();
            }
        }
    }
}

fn status<T>(frame: &mut Frame, area: Rect, task: &Load<T>) {
    let line = match task {
        Task::Done(Ok(_)) | Task::Unexecuted => return,
        Task::Running(_) => Line::from("loading…").dim(),
        Task::Panicked => Line::from("task panicked").red(),
        Task::Done(Err(e)) => Line::from(format!("error: {e:#}")).red(),
    };
    frame.render_widget(Paragraph::new(line).wrap(Wrap { trim: false }), area);
}

fn line_input(text: String) -> Box<TextArea<'static>> {
    let mut area = TextArea::from([text]);
    area.set_cursor_line_style(Style::new());
    area.move_cursor(CursorMove::End);
    Box::new(area)
}

fn date(date: Option<DateTime<Utc>>) -> String {
    date.map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

fn names(list: &[Addr]) -> String {
    list.iter()
        .map(|a| a.name.as_deref().unwrap_or(&a.email))
        .collect::<Vec<_>>()
        .join(", ")
}

fn emails(list: &[Addr]) -> String {
    list.iter()
        .map(|a| a.email.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}
