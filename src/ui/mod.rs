mod accounts;
mod addresses;
mod compose;
mod messages;
mod reader;

use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use enum_dispatch::enum_dispatch;
use exn_anyhow::into_anyhow;
use jmap_client::mailbox::Role;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Clear, List, ListItem, ListState, Paragraph, TableState, Wrap},
};
use ratatui_textarea::{CursorMove, TextArea};
use termwiz::input::Modifiers;
use zeroize::Zeroizing;

use crate::{
    app::{App, InputEvent, KeyCode, KeyEvent, SshTerminal},
    mail::{Addr, Folder, Mail},
    state::{AppState, User},
    task::Task,
    vault::Db,
};

use accounts::{AccountForm, AccountView, Accounts};
use addresses::Addresses;
use compose::{Compose, Draft};
use messages::Messages;
use reader::Reader;

type Load<T> = Task<Result<T>>;

const SIDEBAR_WIDTH: u16 = 28;
const REFRESH_TICKS: u32 = 50;
const PAGE: u16 = 20;

const GOTO: &[(&str, &str)] = &[
    ("g", "first item"),
    ("e", "last item"),
    ("i", "inbox"),
    ("d", "drafts"),
    ("s", "sent"),
    ("a", "archive"),
    ("j", "junk"),
    ("t", "trash"),
    ("n", "next sidebar entry"),
    ("p", "previous sidebar entry"),
];

const SPACE: &[(&str, &str)] = &[
    ("c", "compose"),
    ("a", "addresses"),
    ("v", "vault"),
    ("l", "lock vault"),
    ("?", "keymap"),
];

const GLOBAL: &[(&str, &str)] = &[
    (":", "command"),
    ("/", "search"),
    ("n / N", "next / previous match"),
    ("g", "goto"),
    ("space", "space menu"),
    ("i", "insert mode"),
    ("esc", "normal mode / back"),
    ("pgup/pgdn", "scroll a page"),
    ("alt-↑/↓", "sidebar"),
];

const COMMANDS: &[(&str, &str)] = &[
    (":q  :quit", "quit"),
    (":w  :send", "send mail / save form"),
    (":lock", "lock the vault"),
    (":help", "show this keymap"),
];

#[derive(Clone)]
pub struct Ctx {
    pub state: AppState,
    pub user: &'static User,
    pub mail: Arc<Mail>,
    pub vault: Option<Db>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
}

pub enum Action {
    None,
    Push(Box<Page>),
    Pop,
    Yank(Zeroizing<String>),
    Unlocked(Db),
}

#[derive(Clone)]
pub enum Op {
    Move(Role),
    Destroy,
    Seen(bool),
}

impl Op {
    async fn run(self, mail: &Mail, id: &str) -> Result<()> {
        match self {
            Op::Move(role) => mail.move_to(id, role).await,
            Op::Destroy => mail.destroy(id).await,
            Op::Seen(seen) => mail.set_seen(id, seen).await,
        }
        .map_err(into_anyhow)
    }
}

const MOVE: &[(&str, &str)] = &[
    ("i", "inbox"),
    ("a", "archive"),
    ("j", "junk"),
    ("t", "trash"),
];

fn move_target(key: &KeyEvent) -> Option<Op> {
    let role = match key.key {
        KeyCode::Char('i') => Role::Inbox,
        KeyCode::Char('a') => Role::Archive,
        KeyCode::Char('j') => Role::Junk,
        KeyCode::Char('t') => Role::Trash,
        _ => return None,
    };
    Some(Op::Move(role))
}

#[enum_dispatch]
pub trait View {
    fn draw(&mut self, frame: &mut Frame, area: Rect);
    fn input(&mut self, key: KeyEvent, ctx: &Ctx) -> Action;
    fn help(&self) -> &'static str;
    fn mode(&self) -> Mode {
        Mode::Normal
    }
    fn update(&mut self, _ctx: &Ctx) -> Action {
        Action::None
    }
    fn resume(&mut self, _ctx: &Ctx) -> Action {
        Action::None
    }
    fn search(&mut self, _query: &str, _forward: bool) {}
    fn command(&mut self, _name: &str, _ctx: &Ctx) -> Option<Action> {
        None
    }
}

#[enum_dispatch(View)]
pub enum Page {
    Messages,
    Reader,
    Compose,
    Addresses,
    Accounts,
    AccountView,
    AccountForm,
}

#[derive(Clone, PartialEq, Eq)]
enum Nav {
    Folder { id: String, name: String },
    Compose,
    Addresses,
    Accounts,
}

impl Nav {
    fn open(&self, ctx: &Ctx) -> Page {
        match self {
            Nav::Folder { id, name } => Messages::new(ctx, name.clone(), id.clone()).into(),
            Nav::Compose => Compose::new(ctx, Draft::default()).into(),
            Nav::Addresses => Addresses::new(ctx).into(),
            Nav::Accounts => Accounts::new(ctx).into(),
        }
    }
}

#[derive(Clone, Copy)]
enum Prefix {
    Goto,
    Space,
}

#[derive(Clone, Copy)]
enum PromptKind {
    Command,
    Search,
}

struct Prompt {
    kind: PromptKind,
    input: Box<TextArea<'static>>,
}

enum Notice {
    Info(String),
    Error(String),
}

struct Session {
    ctx: Ctx,
    folders: Vec<Folder>,
    refresh: Load<Vec<Folder>>,
    nav: Nav,
    stack: Vec<Page>,
    ticks: u32,
    pending: Option<Prefix>,
    prompt: Option<Prompt>,
    search: Option<String>,
    notice: Option<Notice>,
    help: bool,
    clipboard: Option<Zeroizing<String>>,
    quit: bool,
}

impl Session {
    fn new(ctx: Ctx) -> Self {
        let nav = inbox(&ctx);
        let mut session = Self {
            stack: vec![nav.open(&ctx)],
            ctx,
            folders: Vec::new(),
            refresh: Task::new(),
            nav,
            ticks: 0,
            pending: None,
            prompt: None,
            search: None,
            notice: None,
            help: false,
            clipboard: None,
            quit: false,
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
            .chain([Nav::Compose, Nav::Addresses, Nav::Accounts])
            .collect()
    }

    fn go(&mut self, nav: Nav) {
        self.stack = vec![nav.open(&self.ctx)];
        self.nav = nav;
        self.reload_folders();
    }

    fn goto_role(&mut self, role: Role) {
        let folder = self.folders.iter().find(|f| f.role == role);
        match folder {
            Some(f) => self.go(Nav::Folder {
                id: f.id.clone(),
                name: f.name.clone(),
            }),
            None => self.notice = Some(Notice::Error(format!("no {role:?} folder"))),
        }
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

    fn lock(&mut self) {
        self.ctx.vault = None;
        if self.nav == Nav::Accounts {
            self.go(Nav::Accounts);
        }
        self.notice = Some(Notice::Info("vault locked".into()));
    }

    fn mode(&self) -> Mode {
        self.stack.last().map_or(Mode::Normal, View::mode)
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::None => {}
            Action::Push(page) => self.stack.push(*page),
            Action::Pop if self.stack.len() > 1 => {
                self.stack.pop();
                if let Some(page) = self.stack.last_mut() {
                    let action = page.resume(&self.ctx);
                    self.apply(action);
                }
            }
            Action::Pop => {
                let inbox = inbox(&self.ctx);
                if self.nav != inbox {
                    self.go(inbox);
                }
            }
            Action::Yank(text) => {
                self.clipboard = Some(text);
                self.notice = Some(Notice::Info("yanked to clipboard".into()));
            }
            Action::Unlocked(db) => self.ctx.vault = Some(db),
        }
    }

    fn send(&mut self, key: KeyEvent) {
        if let Some(page) = self.stack.last_mut() {
            let action = page.input(key, &self.ctx);
            self.apply(action);
        }
    }

    fn update(&mut self) {
        if let Some(page) = self.stack.last_mut() {
            let action = page.update(&self.ctx);
            self.apply(action);
        }
    }

    fn input(&mut self, key: KeyEvent) {
        if self.help {
            self.help = false;
            return;
        }
        if self.prompt.is_some() {
            self.prompt_input(key);
            return;
        }
        self.notice = None;
        if key.modifiers.contains(Modifiers::ALT)
            && matches!(key.key, KeyCode::UpArrow | KeyCode::DownArrow)
        {
            self.step(key.key == KeyCode::DownArrow);
            return;
        }
        if self.mode() == Mode::Insert {
            self.send(key);
            return;
        }
        if let Some(prefix) = self.pending.take() {
            self.prefixed(prefix, key);
            return;
        }
        match key.key {
            KeyCode::Char(':') => self.open_prompt(PromptKind::Command),
            KeyCode::Char('/') => self.open_prompt(PromptKind::Search),
            KeyCode::Char('g') => self.pending = Some(Prefix::Goto),
            KeyCode::Char(' ') => self.pending = Some(Prefix::Space),
            KeyCode::Char(c @ ('n' | 'N')) if self.search.is_some() => {
                if let (Some(query), Some(page)) = (&self.search, self.stack.last_mut()) {
                    page.search(query, c == 'n');
                }
            }
            _ => self.send(key),
        }
    }

    fn prefixed(&mut self, prefix: Prefix, key: KeyEvent) {
        let KeyCode::Char(c) = key.key else {
            return;
        };
        let press = |key| KeyEvent {
            key,
            modifiers: Modifiers::NONE,
        };
        match (prefix, c) {
            (Prefix::Goto, 'g') => self.send(press(KeyCode::Home)),
            (Prefix::Goto, 'e') => self.send(press(KeyCode::End)),
            (Prefix::Goto, 'i') => self.goto_role(Role::Inbox),
            (Prefix::Goto, 'd') => self.goto_role(Role::Drafts),
            (Prefix::Goto, 's') => self.goto_role(Role::Sent),
            (Prefix::Goto, 'a') => self.goto_role(Role::Archive),
            (Prefix::Goto, 'j') => self.goto_role(Role::Junk),
            (Prefix::Goto, 't') => self.goto_role(Role::Trash),
            (Prefix::Goto, 'n') => self.step(true),
            (Prefix::Goto, 'p') => self.step(false),
            (Prefix::Space, 'c') => self.go(Nav::Compose),
            (Prefix::Space, 'a') => self.go(Nav::Addresses),
            (Prefix::Space, 'v') => self.go(Nav::Accounts),
            (Prefix::Space, 'l') => self.lock(),
            (Prefix::Space, '?') => self.help = true,
            _ => {}
        }
    }

    fn open_prompt(&mut self, kind: PromptKind) {
        self.prompt = Some(Prompt {
            kind,
            input: line_input(String::new()),
        });
    }

    fn prompt_input(&mut self, key: KeyEvent) {
        let Some(prompt) = &mut self.prompt else {
            return;
        };
        match key.key {
            KeyCode::Escape => self.prompt = None,
            KeyCode::Backspace if prompt.input.is_empty() => self.prompt = None,
            KeyCode::Enter => {
                let text = prompt.input.lines().join("");
                let kind = prompt.kind;
                self.prompt = None;
                match kind {
                    PromptKind::Command => self.command(text.trim()),
                    PromptKind::Search if !text.is_empty() => {
                        if let Some(page) = self.stack.last_mut() {
                            page.search(&text, true);
                        }
                        self.search = Some(text);
                    }
                    PromptKind::Search => {}
                }
            }
            _ => {
                prompt.input.input(key);
            }
        }
    }

    fn command(&mut self, name: &str) {
        match name {
            "" => {}
            "q" | "quit" | "q!" | "quit!" => self.quit = true,
            "lock" => self.lock(),
            "help" | "h" => self.help = true,
            _ => {
                let action = self
                    .stack
                    .last_mut()
                    .and_then(|page| page.command(name, &self.ctx));
                match action {
                    Some(action) => self.apply(action),
                    None => {
                        self.notice = Some(Notice::Error(format!("unknown command `{name}`")));
                    }
                }
            }
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
        let accounts = if self.ctx.vault.is_some() {
            Line::from("Accounts")
        } else {
            Line::from(vec![Span::raw("Accounts"), Span::raw(" locked").dim()])
        };
        let items: Vec<ListItem> = folders
            .chain([
                divider,
                ListItem::new("Compose"),
                ListItem::new("Addresses"),
                ListItem::new(accounts),
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

    fn draw_status(&mut self, frame: &mut Frame, area: Rect) {
        if let Some(prompt) = &mut self.prompt {
            let [sigil, input] =
                Layout::horizontal([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
            let sigil_text = match prompt.kind {
                PromptKind::Command => ":",
                PromptKind::Search => "/",
            };
            frame.render_widget(Line::from(sigil_text), sigil);
            prompt.input.set_cursor_style(Style::new().reversed());
            frame.render_widget(&*prompt.input, input);
            return;
        }

        let mode = match self.mode() {
            Mode::Normal => Span::raw(" NOR ").black().on_blue().bold(),
            Mode::Insert => Span::raw(" INS ").black().on_green().bold(),
        };
        let right = match (self.pending, &self.notice) {
            (Some(Prefix::Goto), _) => Span::raw("g").bold(),
            (Some(Prefix::Space), _) => Span::raw("space").bold(),
            (None, Some(Notice::Info(text))) => Span::raw(text.clone()).green(),
            (None, Some(Notice::Error(text))) => Span::raw(text.clone()).red(),
            (None, None) => Span::raw(""),
        };
        let [left, notice] = Layout::horizontal([
            Constraint::Fill(1),
            Constraint::Length(right.width() as u16 + 1),
        ])
        .areas(area);
        let help = self.stack.last().map_or("", View::help);
        frame.render_widget(
            Line::from(vec![mode, Span::raw(" "), Span::raw(help).dim()]),
            left,
        );
        frame.render_widget(Line::from(right), notice);
    }

    fn draw_help(&self, frame: &mut Frame, area: Rect) {
        let section = |title: &'static str, keys: &[(&str, &'static str)]| {
            let mut lines = vec![Line::from(title).bold()];
            lines.extend(keys.iter().map(|(key, desc)| key_line(key, desc)));
            lines.push(Line::from(""));
            lines
        };
        let mut lines = section("global", GLOBAL);
        lines.extend(section("goto (g)", GOTO));
        lines.extend(section("space", SPACE));
        lines.extend(section("commands", COMMANDS));
        lines.push(Line::from("this page").bold());
        if let Some(page) = self.stack.last() {
            lines.extend(
                page.help()
                    .split(" · ")
                    .map(|h| Line::from(format!("  {h}"))),
            );
        }

        let width = 48.min(area.width);
        let height = (lines.len() as u16 + 2).min(area.height);
        let rect = Rect {
            x: area.x + (area.width - width) / 2,
            y: area.y + (area.height - height) / 2,
            width,
            height,
        };
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Paragraph::new(lines).block(Block::bordered().title(" keymap · any key closes ")),
            rect,
        );
    }

    fn draw(&mut self, frame: &mut Frame) {
        let [body, footer] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());
        let [main, sidebar] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(SIDEBAR_WIDTH)])
                .areas(body);
        self.draw_sidebar(frame, sidebar);
        if let Some(page) = self.stack.last_mut() {
            page.draw(frame, main);
        }
        self.draw_status(frame, footer);
        match self.pending {
            Some(Prefix::Goto) => keys_popup(frame, body, " goto ", GOTO),
            Some(Prefix::Space) => keys_popup(frame, body, " space ", SPACE),
            None => {}
        }
        if self.help {
            self.draw_help(frame, body);
        }
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
                    vault: None,
                })
            }),
            session: None,
            exit: false,
        }
    }
}

impl App for Pages {
    fn render(&mut self, terminal: &mut SshTerminal) -> Result<Option<String>> {
        if self.session.is_none()
            && let Task::Done(Ok(ctx)) = self.connecting.poll()
        {
            self.session = Some(Session::new(ctx.clone()));
        }

        match &mut self.session {
            Some(session) => {
                session.update();
                terminal.draw(|frame| session.draw(frame))?;
                if let Some(text) = session.clipboard.take() {
                    terminal.backend_mut().clipboard(&text);
                }
                self.exit |= session.quit;
            }
            None => {
                terminal.draw(|frame| {
                    let [body, footer] =
                        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)])
                            .areas(frame.area());
                    status(frame, body, self.connecting.poll());
                    frame.render_widget(Line::from(" q quit").dim(), footer);
                })?;
            }
        }
        Ok(self.exit.then(|| format!("bye {}", self.user.name)))
    }

    fn input(&mut self, event: InputEvent) {
        let InputEvent::Key(key) = event else {
            return;
        };
        match &mut self.session {
            Some(session) => session.input(key),
            None => self.exit = matches!(key.key, KeyCode::Char('q') | KeyCode::Escape),
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

fn inbox(ctx: &Ctx) -> Nav {
    Nav::Folder {
        id: ctx.mail.inbox.clone(),
        name: "Inbox".into(),
    }
}

fn key_line(key: &str, desc: &'static str) -> Line<'static> {
    Line::from(vec![
        Span::raw(format!("  {key:<12}")).yellow(),
        Span::raw(desc),
    ])
}

fn keys_popup(frame: &mut Frame, area: Rect, title: &str, keys: &[(&str, &'static str)]) {
    let lines: Vec<Line> = keys.iter().map(|(key, desc)| key_line(key, desc)).collect();
    let width = (lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 4)
        .max(title.len() as u16 + 4)
        .min(area.width);
    let height = (lines.len() as u16 + 2).min(area.height);
    let rect = Rect {
        x: area.right() - width,
        y: area.bottom() - height,
        width,
        height,
    };
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(title.to_owned())),
        rect,
    );
}

fn status<T>(frame: &mut Frame, area: Rect, task: &Load<T>) {
    let line = match task {
        Task::Done(Ok(_)) | Task::Unexecuted => return,
        Task::Running(_) => Line::from("loading…").dim(),
        Task::Panicked(e) => Line::from(format!("error: {e}")).red(),
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

fn table_nav(table: &mut TableState, key: &KeyEvent) -> bool {
    match key.key {
        KeyCode::Char('j') | KeyCode::DownArrow => table.select_next(),
        KeyCode::Char('k') | KeyCode::UpArrow => table.select_previous(),
        KeyCode::Home => table.select_first(),
        KeyCode::End => table.select_last(),
        KeyCode::PageDown => table.scroll_down_by(PAGE),
        KeyCode::PageUp => table.scroll_up_by(PAGE),
        _ => return false,
    }
    true
}

fn list_nav(list: &mut ListState, key: &KeyEvent) -> bool {
    match key.key {
        KeyCode::Char('j') | KeyCode::DownArrow => list.select_next(),
        KeyCode::Char('k') | KeyCode::UpArrow => list.select_previous(),
        KeyCode::Home => list.select_first(),
        KeyCode::End => list.select_last(),
        KeyCode::PageDown => list.scroll_down_by(PAGE),
        KeyCode::PageUp => list.scroll_up_by(PAGE),
        _ => return false,
    }
    true
}

fn find_match(
    len: usize,
    current: Option<usize>,
    forward: bool,
    matches: impl Fn(usize) -> bool,
) -> Option<usize> {
    let current = current.unwrap_or(0).min(len.checked_sub(1)?);
    (1..=len)
        .map(|step| {
            if forward {
                (current + step) % len
            } else {
                (current + len - step) % len
            }
        })
        .find(|&i| matches(i))
}

fn contains(haystack: &str, query: &str) -> bool {
    haystack.to_lowercase().contains(&query.to_lowercase())
}

fn pick<'a>(list: &'a [String], current: Option<&String>) -> Option<&'a String> {
    current
        .and_then(|current| list.iter().find(|a| *a == current))
        .or(list.first())
}

fn cycle(list: &[String], current: Option<&String>, forward: bool) -> Option<String> {
    let n = list.len();
    let i = pick(list, current)
        .and_then(|current| list.iter().position(|a| a == current))
        .unwrap_or(0);
    list.get(if forward {
        (i + 1) % n.max(1)
    } else {
        (i + n.max(1) - 1) % n.max(1)
    })
    .cloned()
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

fn yank(text: &str) -> Action {
    Action::Yank(Zeroizing::new(text.to_owned()))
}

fn alt(key: &KeyEvent) -> bool {
    key.modifiers.contains(Modifiers::ALT)
}

fn ctrl(key: &KeyEvent) -> bool {
    key.modifiers.contains(Modifiers::CTRL)
}
