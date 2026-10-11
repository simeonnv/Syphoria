mod accounts;
mod addresses;
mod compose;
mod keys;
mod messages;
mod reader;
mod theme;

use std::{error::Error, sync::Arc};

use chrono::{DateTime, Utc};
use enum_dispatch::enum_dispatch;
use exn::{Exn, ResultExt};
use jmap_client::mailbox::Role;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::{Clear, List, ListItem, ListState, Paragraph, TableState, Wrap},
};
use ratatui_textarea::{CursorMove, TextArea};
use termwiz::input::Modifiers;
use zeroize::Zeroizing;

use crate::{
    app::{App, InputEvent, KeyCode, KeyEvent, SshTerminal},
    error::{UiError, chain, is_temporary},
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

type Load<T> = Task<exn::Result<T, UiError>>;

const SIDEBAR_WIDTH: u16 = 28;
const REFRESH_TICKS: u32 = 50;
const PAGE: u16 = 20;

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
    Move {
        from: Role,
        to: Role,
    },
    Undo {
        mailbox: String,
        from: Role,
        to: Role,
    },
    Destroy,
    Seen(bool),
}

impl Op {
    async fn run(self, mail: &Mail, id: &str) -> exn::Result<(), UiError> {
        match self {
            Op::Move { from, to } => mail
                .move_to(id, &from, &to)
                .await
                .or_raise(|| UiError::MoveMessage),
            Op::Undo { mailbox, from, to } => mail
                .move_into(id, &mailbox, &from, &to)
                .await
                .or_raise(|| UiError::MoveMessage),
            Op::Destroy => mail.destroy(id).await.or_raise(|| UiError::DeleteMessage),
            Op::Seen(seen) => mail
                .set_seen(id, seen)
                .await
                .or_raise(|| UiError::UpdateSeen),
        }
    }
}

fn move_target(key: &KeyEvent) -> Option<Role> {
    keys::MoveTo::get(key).map(|target| match target {
        keys::MoveTo::Inbox => Role::Inbox,
        keys::MoveTo::Archive => Role::Archive,
        keys::MoveTo::Junk => Role::Junk,
        keys::MoveTo::Trash => Role::Trash,
    })
}

fn junk_target(role: &Role) -> Role {
    match role {
        Role::Junk | Role::Trash | Role::Archive => Role::Inbox,
        _ => Role::Junk,
    }
}

#[enum_dispatch]
pub trait View {
    fn draw(&mut self, frame: &mut Frame, area: Rect);
    fn input(&mut self, key: KeyEvent, ctx: &Ctx) -> Action;
    fn help(&self) -> keys::Help;
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
    Folder {
        id: String,
        name: String,
        role: Role,
    },
    Compose,
    Addresses,
    Accounts,
}

impl Nav {
    fn open(&self, ctx: &Ctx) -> Page {
        match self {
            Nav::Folder { id, name, role } => {
                Messages::new(ctx, name.clone(), id.clone(), role.clone()).into()
            }
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
            .spawn(async move { mail.folders().await.or_raise(|| UiError::LoadFolders) });
    }

    fn entries(&self) -> Vec<Nav> {
        self.folders
            .iter()
            .map(|f| Nav::Folder {
                id: f.id.clone(),
                name: f.name.clone(),
                role: f.role.clone(),
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
                role: f.role.clone(),
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
        if let Some(sidebar) = keys::Always::get(&key) {
            self.step(sidebar == keys::Always::SidebarNext);
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
        match keys::Global::get(&key) {
            Some(keys::Global::Command) => self.open_prompt(PromptKind::Command),
            Some(keys::Global::Search) => self.open_prompt(PromptKind::Search),
            Some(keys::Global::Goto) => self.pending = Some(Prefix::Goto),
            Some(keys::Global::Space) => self.pending = Some(Prefix::Space),
            Some(keys::Global::NextMatch) => self.find(true),
            Some(keys::Global::PrevMatch) => self.find(false),
            None => self.send(key),
        }
    }

    fn find(&mut self, forward: bool) {
        if let (Some(query), Some(page)) = (&self.search, self.stack.last_mut()) {
            page.search(query, forward);
        }
    }

    fn prefixed(&mut self, prefix: Prefix, key: KeyEvent) {
        let press = |key| KeyEvent {
            key,
            modifiers: Modifiers::NONE,
        };
        match prefix {
            Prefix::Goto => match keys::Goto::get(&key) {
                Some(keys::Goto::Top) => self.send(press(KeyCode::Home)),
                Some(keys::Goto::Bottom) => self.send(press(KeyCode::End)),
                Some(keys::Goto::Inbox) => self.goto_role(Role::Inbox),
                Some(keys::Goto::Drafts) => self.goto_role(Role::Drafts),
                Some(keys::Goto::Sent) => self.goto_role(Role::Sent),
                Some(keys::Goto::Archive) => self.goto_role(Role::Archive),
                Some(keys::Goto::Junk) => self.goto_role(Role::Junk),
                Some(keys::Goto::Trash) => self.goto_role(Role::Trash),
                Some(keys::Goto::Next) => self.step(true),
                Some(keys::Goto::Prev) => self.step(false),
                None => {}
            },
            Prefix::Space => match keys::Space::get(&key) {
                Some(keys::Space::Compose) => self.go(Nav::Compose),
                Some(keys::Space::Addresses) => self.go(Nav::Addresses),
                Some(keys::Space::Vault) => self.go(Nav::Accounts),
                Some(keys::Space::Lock) => self.lock(),
                Some(keys::Space::Help) => self.help = true,
                None => {}
            },
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
        match keys::Prompt::get(&key) {
            Some(keys::Prompt::Cancel) => self.prompt = None,
            Some(keys::Prompt::Erase) if prompt.input.is_empty() => self.prompt = None,
            Some(keys::Prompt::Submit) => {
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
            Some(keys::Prompt::Erase) | None => {
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

        let block = theme::block().title(format!(" {} ", self.ctx.user.name));
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
        let divider = ListItem::new("─".repeat(inner.width as usize)).fg(theme::SUBTLE);
        let accounts = if self.ctx.vault.is_some() {
            Line::from("Accounts")
        } else {
            Line::from(vec![
                Span::raw("Accounts"),
                Span::raw(" locked").fg(theme::SUBTLE),
            ])
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
            List::new(items).highlight_style(theme::SELECTED),
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
        let help = self.stack.last().map_or(&[][..], View::help);
        frame.render_widget(
            Line::from(vec![
                mode,
                Span::raw(" "),
                Span::raw(keys::line(help)).fg(theme::SUBTLE),
            ]),
            left,
        );
        frame.render_widget(Line::from(right), notice);
    }

    fn draw_help(&self, frame: &mut Frame, area: Rect) {
        let section = |title: &'static str, mut body: Vec<Line<'static>>| {
            body.insert(0, Line::from(title).bold());
            body.push(Line::from(""));
            body
        };
        let mut lines = section(
            "global",
            bind_lines(&[keys::Global::KEYS, keys::Always::KEYS]),
        );
        lines.extend(section("goto (g)", bind_lines(&[keys::Goto::KEYS])));
        lines.extend(section("space", bind_lines(&[keys::Space::KEYS])));
        lines.extend(section(
            "commands",
            COMMANDS
                .iter()
                .map(|(name, desc)| key_line(name, desc))
                .collect(),
        ));
        lines.push(Line::from("this page").bold());
        if let Some(page) = self.stack.last() {
            lines.extend(bind_lines(page.help()));
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
            Paragraph::new(lines).block(theme::block().title(" keymap · any key closes ")),
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
            Some(Prefix::Goto) => keys_popup(frame, body, " goto ", &[keys::Goto::KEYS]),
            Some(Prefix::Space) => keys_popup(frame, body, " space ", &[keys::Space::KEYS]),
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
                let mail = state
                    .stalwart
                    .mail(user)
                    .await
                    .or_raise(|| UiError::Connect)?;
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
    fn render(&mut self, terminal: &mut SshTerminal) -> exn::Result<Option<String>, UiError> {
        if self.session.is_none()
            && let Task::Done(Ok(ctx)) = self.connecting.poll()
        {
            self.session = Some(Session::new(ctx.clone()));
        }

        match &mut self.session {
            Some(session) => {
                session.update();
                terminal
                    .draw(|frame| session.draw(frame))
                    .or_raise(|| UiError::Draw)?;
                if let Some(text) = session.clipboard.take() {
                    terminal.backend_mut().clipboard(&text);
                }
                self.exit |= session.quit;
            }
            None => {
                terminal
                    .draw(|frame| {
                        let [body, footer] =
                            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)])
                                .areas(frame.area());
                        status(frame, body, self.connecting.poll());
                        frame.render_widget(
                            Line::from(format!(" {}", keys::line(&[keys::Connecting::KEYS])))
                                .fg(theme::SUBTLE),
                            footer,
                        );
                    })
                    .or_raise(|| UiError::Draw)?;
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
            None => self.exit = keys::Connecting::get(&key) == Some(keys::Connecting::Quit),
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
        role: Role::Inbox,
    }
}

fn key_line(key: &str, desc: &'static str) -> Line<'static> {
    Line::from(vec![
        Span::raw(format!("  {key:<12}")).yellow(),
        Span::raw(desc),
    ])
}

fn bind_lines(help: keys::Help) -> Vec<Line<'static>> {
    help.iter()
        .flat_map(|binds| binds.iter())
        .map(|bind| key_line(&bind.label(), bind.help))
        .collect()
}

fn confirm_popup(frame: &mut Frame, area: Rect, title: &str) {
    let mut lines = bind_lines(&[keys::Confirm::KEYS]);
    lines.push(key_line("any", "cancel"));
    lines_popup(frame, area, title, lines);
}

fn keys_popup(frame: &mut Frame, area: Rect, title: &str, help: keys::Help) {
    lines_popup(frame, area, title, bind_lines(help));
}

fn lines_popup(frame: &mut Frame, area: Rect, title: &str, lines: Vec<Line<'static>>) {
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
        Paragraph::new(lines).block(theme::block().title(title.to_owned())),
        rect,
    );
}

fn status<T>(frame: &mut Frame, area: Rect, task: &Load<T>) {
    let line = match task {
        Task::Done(Ok(_)) | Task::Unexecuted => return,
        Task::Running(_) => Line::from("loading…").fg(theme::SUBTLE),
        Task::Panicked(e) => Line::from(format!("error: {e}")).red(),
        Task::Done(Err(e)) => Line::from(failure(e)).red(),
    };
    frame.render_widget(Paragraph::new(line).wrap(Wrap { trim: false }), area);
}

fn failure<E: Error + Send + Sync + 'static>(error: &Exn<E>) -> String {
    let hint = if is_temporary(error.frame()) {
        " · try again"
    } else {
        ""
    };
    format!("error: {}{hint}", chain(error.frame()))
}

fn line_input(text: String) -> Box<TextArea<'static>> {
    let mut area = TextArea::from([text]);
    area.set_cursor_line_style(Style::new());
    area.move_cursor(CursorMove::End);
    Box::new(area)
}

fn table_nav(table: &mut TableState, key: &KeyEvent) -> bool {
    let Some(nav) = keys::Nav::get(key) else {
        return false;
    };
    match nav {
        keys::Nav::Down => table.select_next(),
        keys::Nav::Up => table.select_previous(),
        keys::Nav::Top => table.select_first(),
        keys::Nav::Bottom => table.select_last(),
        keys::Nav::PageDown => table.scroll_down_by(PAGE),
        keys::Nav::PageUp => table.scroll_up_by(PAGE),
    }
    true
}

fn list_nav(list: &mut ListState, key: &KeyEvent) -> bool {
    let Some(nav) = keys::Nav::get(key) else {
        return false;
    };
    match nav {
        keys::Nav::Down => list.select_next(),
        keys::Nav::Up => list.select_previous(),
        keys::Nav::Top => list.select_first(),
        keys::Nav::Bottom => list.select_last(),
        keys::Nav::PageDown => list.scroll_down_by(PAGE),
        keys::Nav::PageUp => list.scroll_up_by(PAGE),
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
