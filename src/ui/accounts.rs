use std::mem;

use exn::{ErrorExt, Exn, ResultExt};
use rand::RngExt;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Row, Table, TableState, Wrap},
};
use ratatui_textarea::TextArea;
use zeroize::Zeroizing;

use super::{
    Action, Ctx, Load, Mode, View, confirm_popup, contains, cycle, failure, find_match,
    keys::{self, Key},
    line_input, pick, status, table_nav, theme, yank,
};
use crate::{
    app::KeyEvent,
    error::UiError,
    task::Task,
    vault::{Account, Db},
};

const MIN_PASSWORD: usize = 8;
const GENERATED: usize = 24;
const CHARSET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789!@#$%^&*-_=+?";

struct Secret(Zeroizing<String>);

impl Secret {
    fn new() -> Self {
        Self(Zeroizing::new(String::with_capacity(256)))
    }

    fn from(text: &str) -> Self {
        let mut secret = Self::new();
        secret.0.push_str(text);
        secret
    }

    fn generate() -> Self {
        let mut rng = rand::rng();
        let mut secret = Self::new();
        for _ in 0..GENERATED {
            secret
                .0
                .push(char::from(CHARSET[rng.random_range(0..CHARSET.len())]));
        }
        secret
    }

    fn input(&mut self, key: &KeyEvent) {
        match Key::new(key) {
            Some(Key::Ctrl('u')) => self.0.clear(),
            Some(Key::Char(c)) => self.0.push(c),
            Some(Key::Backspace) => {
                self.0.pop();
            }
            _ => {}
        }
    }

    fn take(&mut self) -> Zeroizing<String> {
        mem::replace(self, Self::new()).0
    }
}

enum State {
    Locked {
        setup: bool,
        password: Secret,
        first: Option<Zeroizing<String>>,
        attempt: Load<Db>,
        error: Option<Exn<UiError>>,
    },
    Unlocked {
        db: Db,
        list: Load<Vec<Account>>,
        table: TableState,
        confirm: Option<Account>,
    },
}

pub struct Accounts {
    state: State,
}

impl Accounts {
    pub fn new(ctx: &Ctx) -> Self {
        let state = match &ctx.vault {
            Some(db) => unlocked(db.clone()),
            None => State::Locked {
                setup: !ctx.state.vault.exists(ctx.user),
                password: Secret::new(),
                first: None,
                attempt: Task::new(),
                error: None,
            },
        };
        Self { state }
    }
}

fn unlocked(db: Db) -> State {
    State::Unlocked {
        list: load(&db),
        db,
        table: TableState::default().with_selected(0),
        confirm: None,
    }
}

fn load(db: &Db) -> Load<Vec<Account>> {
    let db = db.clone();
    Task::new_spawn(async move { db.accounts().await.or_raise(|| UiError::LoadAccounts) })
}

fn selected<'a>(list: &'a Load<Vec<Account>>, table: &TableState) -> Option<&'a Account> {
    let Task::Done(Ok(list)) = list else {
        return None;
    };
    table.selected().and_then(|i| list.get(i))
}

impl View for Accounts {
    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        match &mut self.state {
            State::Locked {
                setup,
                password,
                first,
                attempt,
                error,
            } => {
                frame.render_widget(theme::block().title(" Accounts "), area);
                let label = match (*setup, first.is_some()) {
                    (false, _) => "vault password",
                    (true, false) => "choose a vault password",
                    (true, true) => "confirm vault password",
                };
                let mut lines = vec![
                    Line::from(label).bold(),
                    Line::from(vec![
                        Span::raw("•".repeat(password.0.chars().count())),
                        Span::raw(" ").reversed(),
                    ]),
                    Line::from(""),
                ];
                match (attempt.poll(), error) {
                    (Task::Running(_), _) => lines.push(Line::from("checking…").fg(theme::SUBTLE)),
                    (_, Some(error)) => lines.push(Line::from(failure(error)).red()),
                    _ => {}
                }
                if *setup {
                    lines.push(
                        Line::from("a forgotten password cannot be recovered").fg(theme::SUBTLE),
                    );
                }

                let width = 56.min(area.width);
                let height = 8.min(area.height);
                let rect = Rect {
                    x: area.x + (area.width - width) / 2,
                    y: area.y + (area.height - height) / 2,
                    width,
                    height,
                };
                let title = if *setup {
                    " create vault "
                } else {
                    " vault locked "
                };
                frame.render_widget(Clear, rect);
                frame.render_widget(
                    Paragraph::new(lines)
                        .wrap(Wrap { trim: false })
                        .block(Block::bordered().title(title).green()),
                    rect,
                );
            }
            State::Unlocked {
                list,
                table,
                confirm,
                ..
            } => {
                let block = theme::block().title(" Accounts ");
                let inner = block.inner(area);
                frame.render_widget(block, area);
                match list.poll() {
                    Task::Done(Ok(list)) if list.is_empty() => {
                        frame.render_widget(
                            Line::from("no accounts yet, press a").fg(theme::SUBTLE),
                            inner,
                        );
                    }
                    Task::Done(Ok(list)) => {
                        let rows = list.iter().map(|a| {
                            Row::new([
                                a.service.as_str(),
                                a.username.as_str(),
                                a.address.as_str(),
                                a.url.as_str(),
                            ])
                        });
                        let widget = Table::new(
                            rows,
                            [
                                Constraint::Fill(1),
                                Constraint::Fill(1),
                                Constraint::Fill(1),
                                Constraint::Fill(1),
                            ],
                        )
                        .header(
                            Row::new(["service", "username", "address", "url"])
                                .style(theme::HEADER),
                        )
                        .row_highlight_style(theme::SELECTED);
                        frame.render_stateful_widget(widget, inner, table);
                    }
                    other => status(frame, inner, other),
                }
                if confirm.is_some() {
                    confirm_popup(frame, area, " delete account? ");
                }
            }
        }
    }

    fn input(&mut self, key: KeyEvent, ctx: &Ctx) -> Action {
        match &mut self.state {
            State::Locked {
                setup,
                password,
                first,
                attempt,
                error,
            } => {
                if matches!(attempt, Task::Running(_)) {
                    return Action::None;
                }
                match keys::Unlock::get(&key) {
                    Some(keys::Unlock::Back) => return Action::Pop,
                    Some(keys::Unlock::Clear) => *password = Secret::new(),
                    Some(keys::Unlock::Submit) => {
                        *error = None;
                        let typed = password.take();
                        let (state, user) = (ctx.state, ctx.user);
                        if !*setup {
                            attempt.spawn(async move {
                                state
                                    .vault
                                    .unlock(user, typed)
                                    .await
                                    .or_raise(|| UiError::UnlockVault)
                            });
                        } else if let Some(first) = first.take() {
                            if first == typed {
                                attempt.spawn(async move {
                                    state
                                        .vault
                                        .create(user, typed)
                                        .await
                                        .or_raise(|| UiError::CreateVault)
                                });
                            } else {
                                *error = Some(UiError::PasswordMismatch.raise());
                            }
                        } else if typed.chars().count() < MIN_PASSWORD {
                            *error = Some(UiError::PasswordTooShort(MIN_PASSWORD).raise());
                        } else {
                            *first = Some(typed);
                        }
                    }
                    None => password.input(&key),
                }
            }
            State::Unlocked {
                db,
                list,
                table,
                confirm,
            } => {
                if let Some(account) = confirm.take() {
                    if keys::Confirm::get(&key) == Some(keys::Confirm::Yes) {
                        let db = db.clone();
                        *list = Task::new_spawn(async move {
                            db.delete(&account)
                                .await
                                .or_raise(|| UiError::DeleteAccount)?;
                            db.accounts().await.or_raise(|| UiError::LoadAccounts)
                        });
                    }
                    return Action::None;
                }
                if table_nav(table, &key) {
                    return Action::None;
                }
                let Some(cmd) = keys::Vault::get(&key) else {
                    return Action::None;
                };
                match (cmd, selected(list, table)) {
                    (keys::Vault::Back, _) => return Action::Pop,
                    (keys::Vault::Reload, _) => *list = load(db),
                    (keys::Vault::Add, _) => {
                        return Action::Push(Box::new(
                            AccountForm::new(ctx, db.clone(), None).into(),
                        ));
                    }
                    (keys::Vault::Show, Some(account)) => {
                        return Action::Push(Box::new(
                            AccountView::new(db.clone(), account.clone()).into(),
                        ));
                    }
                    (keys::Vault::Edit, Some(account)) => {
                        return Action::Push(Box::new(
                            AccountForm::new(ctx, db.clone(), Some(account.clone())).into(),
                        ));
                    }
                    (keys::Vault::Delete, Some(account)) => *confirm = Some(account.clone()),
                    (keys::Vault::YankPassword, Some(account)) => return yank(&account.password),
                    (keys::Vault::YankUsername, Some(account)) => return yank(&account.username),
                    (
                        keys::Vault::Show
                        | keys::Vault::Edit
                        | keys::Vault::Delete
                        | keys::Vault::YankPassword
                        | keys::Vault::YankUsername,
                        None,
                    ) => {}
                }
            }
        }
        Action::None
    }

    fn help(&self) -> keys::Help {
        match self.state {
            State::Locked { .. } => &[keys::Unlock::KEYS],
            State::Unlocked { .. } => &[keys::Vault::KEYS],
        }
    }

    fn mode(&self) -> Mode {
        match self.state {
            State::Locked { .. } => Mode::Insert,
            State::Unlocked { .. } => Mode::Normal,
        }
    }

    fn update(&mut self, _ctx: &Ctx) -> Action {
        let State::Locked { attempt, error, .. } = &mut self.state else {
            return Action::None;
        };
        if !matches!(attempt.poll(), Task::Done(_) | Task::Panicked(_)) {
            return Action::None;
        }
        match mem::take(attempt) {
            Task::Done(Ok(db)) => {
                self.state = unlocked(db.clone());
                Action::Unlocked(db)
            }
            Task::Done(Err(e)) => {
                *error = Some(e);
                Action::None
            }
            Task::Panicked(e) => {
                *error = Some(Exn::new(e).raise(UiError::VaultPanicked));
                Action::None
            }
            Task::Unexecuted | Task::Running(_) => Action::None,
        }
    }

    fn resume(&mut self, _ctx: &Ctx) -> Action {
        if let State::Unlocked { db, list, .. } = &mut self.state {
            *list = load(db);
        }
        Action::None
    }

    fn search(&mut self, query: &str, forward: bool) {
        let State::Unlocked { list, table, .. } = &mut self.state else {
            return;
        };
        let Task::Done(Ok(list)) = list else {
            return;
        };
        let found = find_match(list.len(), table.selected(), forward, |i| {
            let a = &list[i];
            contains(&a.service, query)
                || contains(&a.username, query)
                || contains(&a.address, query)
                || contains(&a.url, query)
        });
        if found.is_some() {
            table.select(found);
        }
    }
}

pub struct AccountView {
    db: Db,
    account: Account,
}

impl AccountView {
    fn new(db: Db, account: Account) -> Self {
        Self { db, account }
    }
}

impl View for AccountView {
    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let a = &self.account;
        let field = |name: &'static str, value: &str| {
            Line::from(vec![
                Span::raw(name).fg(theme::SUBTLE),
                Span::raw(value.to_owned()),
            ])
        };
        let mut lines = vec![
            field("service:  ", &a.service),
            field("address:  ", &a.address),
            field("username: ", &a.username),
            field("password: ", &a.password),
            field("url:      ", &a.url),
            Line::from(""),
        ];
        lines.extend(a.notes.lines().map(|l| Line::from(l.to_owned())));
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(theme::block().title(format!(" {} ", a.service))),
            area,
        );
    }

    fn input(&mut self, key: KeyEvent, ctx: &Ctx) -> Action {
        match keys::Account::get(&key) {
            Some(keys::Account::Back) => Action::Pop,
            Some(keys::Account::YankPassword) => yank(&self.account.password),
            Some(keys::Account::YankUsername) => yank(&self.account.username),
            Some(keys::Account::Edit) => Action::Push(Box::new(
                AccountForm::new(ctx, self.db.clone(), Some(self.account.clone())).into(),
            )),
            None => Action::None,
        }
    }

    fn help(&self) -> keys::Help {
        &[keys::Account::KEYS]
    }

    fn resume(&mut self, _ctx: &Ctx) -> Action {
        Action::Pop
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Service,
    Address,
    Username,
    Password,
    Url,
    Notes,
}

impl Field {
    const ALL: [Field; 6] = [
        Field::Service,
        Field::Address,
        Field::Username,
        Field::Password,
        Field::Url,
        Field::Notes,
    ];

    fn step(self, forward: bool) -> Self {
        let i = Self::ALL.iter().position(|f| *f == self).unwrap_or(0);
        let n = Self::ALL.len();
        Self::ALL[if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        }]
    }
}

pub struct AccountForm {
    db: Db,
    id: Option<i64>,
    addresses: Load<Vec<String>>,
    address: Option<String>,
    service: Box<TextArea<'static>>,
    username: Box<TextArea<'static>>,
    password: Secret,
    url: Box<TextArea<'static>>,
    notes: Box<TextArea<'static>>,
    focus: Field,
    mode: Mode,
    saving: Load<()>,
}

impl AccountForm {
    fn new(ctx: &Ctx, db: Db, account: Option<Account>) -> Self {
        let (state, user) = (ctx.state, ctx.user);
        let account = account.unwrap_or_default();
        let mut username = line_input(account.username.clone());
        username.set_placeholder_text("defaults to the address");
        let mut notes = Box::new(TextArea::from(account.notes.lines()));
        notes.set_cursor_line_style(Style::new());
        Self {
            db,
            id: account.id,
            addresses: Task::new_spawn(async move {
                state
                    .stalwart
                    .addresses(user)
                    .await
                    .or_raise(|| UiError::LoadAddresses)
            }),
            address: (!account.address.is_empty()).then(|| account.address.clone()),
            service: line_input(account.service.clone()),
            username,
            password: match account.id {
                Some(_) => Secret::from(&account.password),
                None => Secret::generate(),
            },
            url: line_input(account.url.clone()),
            notes,
            focus: Field::Service,
            mode: Mode::Normal,
            saving: Task::new(),
        }
    }

    fn address(&self) -> Option<&String> {
        let Task::Done(Ok(list)) = &self.addresses else {
            return None;
        };
        pick(list, self.address.as_ref())
    }

    fn cycle_address(&mut self, forward: bool) {
        if let Task::Done(Ok(list)) = &self.addresses {
            self.address = cycle(list, self.address.as_ref(), forward);
        }
    }

    fn save(&mut self) {
        if matches!(self.saving, Task::Running(_)) {
            return;
        }
        let Some(address) = self.address().cloned() else {
            self.saving = Task::Done(Err(UiError::NoAddress.raise()));
            return;
        };
        let username = self.username.lines().join("");
        let account = Account {
            id: self.id,
            username: if username.trim().is_empty() {
                address.clone()
            } else {
                username
            },
            address,
            service: self.service.lines().join(""),
            password: self.password.0.to_string(),
            url: self.url.lines().join(""),
            notes: self.notes.lines().join("\n"),
        };
        let db = self.db.clone();
        self.mode = Mode::Normal;
        self.saving
            .spawn(async move { db.save(account).await.or_raise(|| UiError::SaveAccount) });
    }

    fn yank_field(&self) -> Action {
        match self.focus {
            Field::Service => yank(&self.service.lines().join("\n")),
            Field::Address => yank(self.address().map_or("", String::as_str)),
            Field::Username => yank(&self.username.lines().join("\n")),
            Field::Password => yank(&self.password.0),
            Field::Url => yank(&self.url.lines().join("\n")),
            Field::Notes => yank(&self.notes.lines().join("\n")),
        }
    }

    fn field(&mut self) -> Option<&mut TextArea<'static>> {
        match self.focus {
            Field::Service => Some(&mut self.service),
            Field::Username => Some(&mut self.username),
            Field::Url => Some(&mut self.url),
            Field::Notes => Some(&mut self.notes),
            Field::Address | Field::Password => None,
        }
    }
}

impl View for AccountForm {
    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let [service, address, username, password, url, notes, status] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .areas(area);

        let (focus, mode) = (self.focus, self.mode);
        let block = |title: &'static str, field: Field| {
            let block = theme::block().title(title);
            match (field == focus, mode) {
                (true, Mode::Insert) => block.border_style(Style::new().green()),
                (true, Mode::Normal) => block.border_style(Style::new().yellow()),
                (false, _) => block,
            }
        };

        let sender = match self.addresses.poll() {
            Task::Done(Ok(list)) if list.is_empty() => Line::from("no addresses yet").red(),
            Task::Done(Ok(_)) => Line::from(format!(
                "‹ {} ›",
                self.address().map(String::as_str).unwrap_or_default()
            )),
            Task::Done(Err(e)) => Line::from(failure(e)).red(),
            _ => Line::from("loading…").fg(theme::SUBTLE),
        };
        frame.render_widget(
            Paragraph::new(sender).block(block(" Address ", Field::Address)),
            address,
        );

        let mut secret = vec![Span::raw(self.password.0.as_str())];
        if focus == Field::Password && mode == Mode::Insert {
            secret.push(Span::raw(" ").reversed());
        }
        frame.render_widget(
            Paragraph::new(Line::from(secret))
                .block(block(" Password · ctrl-r generate ", Field::Password)),
            password,
        );

        for (area, field, title, rect) in [
            (&mut self.service, Field::Service, " Service ", service),
            (&mut self.username, Field::Username, " Username ", username),
            (&mut self.url, Field::Url, " Url ", url),
            (&mut self.notes, Field::Notes, " Notes ", notes),
        ] {
            area.set_block(block(title, field));
            area.set_cursor_style(if field == focus && mode == Mode::Insert {
                Style::new().reversed()
            } else {
                Style::new()
            });
            frame.render_widget(&**area, rect);
        }

        let line = match self.saving.poll() {
            Task::Unexecuted | Task::Done(Ok(())) => Line::from(""),
            Task::Running(_) => Line::from("saving…").fg(theme::SUBTLE),
            Task::Done(Err(e)) => Line::from(failure(e)).red(),
            Task::Panicked(e) => Line::from(format!("error: {e}")).red(),
        };
        frame.render_widget(line, status);
    }

    fn input(&mut self, key: KeyEvent, _ctx: &Ctx) -> Action {
        let on_address = self.focus == Field::Address;
        match keys::Fields::get(&key) {
            Some(keys::Fields::Submit) => {
                self.save();
                return Action::None;
            }
            Some(keys::Fields::Next) => {
                self.focus = self.focus.step(true);
                return Action::None;
            }
            Some(keys::Fields::Prev) => {
                self.focus = self.focus.step(false);
                return Action::None;
            }
            Some(keys::Fields::Left) if on_address => {
                self.cycle_address(false);
                return Action::None;
            }
            Some(keys::Fields::Right) if on_address => {
                self.cycle_address(true);
                return Action::None;
            }
            Some(keys::Fields::Left | keys::Fields::Right) | None => {}
        }
        if let Some(keys::Password::Generate) = keys::Password::get(&key) {
            self.password = Secret::generate();
            return Action::None;
        }
        match self.mode {
            Mode::Insert => match keys::Insert::get(&key) {
                Some(keys::Insert::Normal) => self.mode = Mode::Normal,
                Some(keys::Insert::Next) if self.focus != Field::Notes => {
                    self.focus = self.focus.step(true);
                }
                Some(keys::Insert::Next) | None if self.focus == Field::Password => {
                    self.password.input(&key);
                }
                Some(keys::Insert::Next) | None => {
                    if let Some(area) = self.field() {
                        area.input(key);
                    }
                }
            },
            Mode::Normal => match keys::Edit::get(&key) {
                Some(keys::Edit::Back) => return Action::Pop,
                Some(keys::Edit::Insert) if !on_address => self.mode = Mode::Insert,
                Some(keys::Edit::Down) => self.focus = self.focus.step(true),
                Some(keys::Edit::Up) => self.focus = self.focus.step(false),
                Some(keys::Edit::Left) if on_address => self.cycle_address(false),
                Some(keys::Edit::Right) if on_address => self.cycle_address(true),
                Some(keys::Edit::Yank) => return self.yank_field(),
                Some(keys::Edit::Insert | keys::Edit::Left | keys::Edit::Right) | None => {}
            },
        }
        Action::None
    }

    fn help(&self) -> keys::Help {
        match self.mode {
            Mode::Normal => &[keys::Edit::KEYS, keys::Fields::KEYS, keys::Password::KEYS],
            Mode::Insert => &[keys::Insert::KEYS, keys::Fields::KEYS, keys::Password::KEYS],
        }
    }

    fn mode(&self) -> Mode {
        self.mode
    }

    fn update(&mut self, _ctx: &Ctx) -> Action {
        match self.saving.poll() {
            Task::Done(Ok(())) => Action::Pop,
            _ => Action::None,
        }
    }

    fn command(&mut self, name: &str, _ctx: &Ctx) -> Option<Action> {
        matches!(name, "w" | "write").then(|| {
            self.save();
            Action::None
        })
    }
}
