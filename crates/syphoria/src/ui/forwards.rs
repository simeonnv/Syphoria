use exn::{Exn, ResultExt};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Style, Stylize},
    text::Line,
    widgets::{Paragraph, Row, Table, TableState},
};
use ratatui_textarea::TextArea;

use super::{
    Action, Ctx, Load, Mode, View, confirm_popup, contains, cycle, failure, find_match, keys,
    line_input, pick, status, table_nav, theme, yank,
};
use crate::{
    app::KeyEvent,
    error::{InputError, UiError},
    mail::{Forward, Header},
    state::User,
    task::Task,
};

enum State {
    Browse,
    Confirm(Forward),
}

pub struct Forwards {
    list: Load<Vec<Forward>>,
    table: TableState,
    state: State,
}

impl Forwards {
    pub fn new(ctx: &Ctx) -> Self {
        Self {
            list: load(ctx),
            table: TableState::default().with_selected(0),
            state: State::Browse,
        }
    }

    fn selected(&self) -> Option<&Forward> {
        let Task::Done(Ok(list)) = &self.list else {
            return None;
        };
        self.table.selected().and_then(|i| list.get(i))
    }
}

fn load(ctx: &Ctx) -> Load<Vec<Forward>> {
    let mail = ctx.mail.clone();
    Task::new_spawn(async move { mail.forwards().await.or_raise(|| UiError::LoadForwards) })
}

fn filter_text(forward: &Forward) -> String {
    forward
        .filter()
        .map(|f| format!("{} contains {}", f.header, f.text))
        .unwrap_or_default()
}

impl View for Forwards {
    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let block = theme::block().title(" Forwards · first match wins, spam is never forwarded ");
        let inner = block.inner(area);
        frame.render_widget(block, area);
        match self.list.poll() {
            Task::Done(Ok(list)) if list.is_empty() => {
                frame.render_widget(
                    Line::from("no forwards yet, press a").fg(theme::SUBTLE),
                    inner,
                );
            }
            Task::Done(Ok(list)) => {
                let rows = list.iter().map(|f| {
                    Row::new([
                        match f.address() {
                            Some(address) => Line::from(address.to_owned()),
                            None => Line::from("all").fg(theme::SUBTLE),
                        },
                        Line::from(filter_text(f)),
                        Line::from(f.target().to_owned()),
                        if f.keep() {
                            Line::from("copy").fg(theme::SUBTLE)
                        } else {
                            Line::from("move").yellow()
                        },
                    ])
                });
                let widget = Table::new(
                    rows,
                    [
                        Constraint::Fill(1),
                        Constraint::Fill(1),
                        Constraint::Fill(1),
                        Constraint::Length(6),
                    ],
                )
                .header(Row::new(["address", "filter", "forward to", "mode"]).style(theme::HEADER))
                .row_highlight_style(theme::SELECTED);
                frame.render_stateful_widget(widget, inner, &mut self.table);
            }
            other => status(frame, inner, other),
        }
        if let State::Confirm(_) = self.state {
            confirm_popup(frame, area, " delete forward? ");
        }
    }

    fn input(&mut self, key: KeyEvent, ctx: &Ctx) -> Action {
        if let State::Confirm(forward) = &self.state {
            let forward = forward.clone();
            self.state = State::Browse;
            if keys::Confirm::get(&key) == Some(keys::Confirm::Yes) {
                let mail = ctx.mail.clone();
                self.list.spawn(async move {
                    mail.remove_forward(&forward)
                        .await
                        .or_raise(|| UiError::RemoveForward)
                });
            }
            return Action::None;
        }
        if table_nav(&mut self.table, &key) {
            return Action::None;
        }
        match keys::Forwards::get(&key) {
            Some(keys::Forwards::Add) => {
                return Action::Push(Box::new(ForwardForm::new(ctx, None).into()));
            }
            Some(keys::Forwards::Delete) => {
                if let Some(forward) = self.selected().cloned() {
                    self.state = State::Confirm(forward);
                }
            }
            Some(keys::Forwards::Reload) => self.list = load(ctx),
            Some(keys::Forwards::Back) => return Action::Pop,
            None => {}
        }
        Action::None
    }

    fn help(&self) -> keys::Help {
        match self.state {
            State::Browse => &[keys::Forwards::KEYS],
            State::Confirm(_) => &[keys::Confirm::KEYS],
        }
    }

    fn resume(&mut self, ctx: &Ctx) -> Action {
        self.list = load(ctx);
        Action::None
    }

    fn search(&mut self, query: &str, forward: bool) {
        let Task::Done(Ok(list)) = &self.list else {
            return;
        };
        let found = find_match(list.len(), self.table.selected(), forward, |i| {
            let f = &list[i];
            contains(f.address().unwrap_or("all"), query)
                || contains(f.target(), query)
                || contains(&filter_text(f), query)
        });
        if found.is_some() {
            self.table.select(found);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Address,
    Filter,
    Text,
    Target,
    Keep,
}

impl Field {
    const ALL: [Field; 5] = [
        Field::Address,
        Field::Filter,
        Field::Text,
        Field::Target,
        Field::Keep,
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

    fn text(self) -> bool {
        matches!(self, Field::Text | Field::Target)
    }
}

pub struct ForwardForm {
    user: &'static User,
    addresses: Load<Vec<String>>,
    address: Option<String>,
    header: Option<Header>,
    text: Box<TextArea<'static>>,
    target: Box<TextArea<'static>>,
    keep: bool,
    focus: Field,
    mode: Mode,
    error: Option<Exn<InputError>>,
    saving: Load<Vec<Forward>>,
}

impl ForwardForm {
    pub fn new(ctx: &Ctx, address: Option<String>) -> Self {
        let (state, user) = (ctx.state, ctx.user);
        let mut text = line_input(String::new());
        text.set_placeholder_text("text to match, pick a filter above");
        let mut target = line_input(String::new());
        target.set_placeholder_text("someone@example.org");
        Self {
            user,
            addresses: Task::new_spawn(async move {
                let mut list = state
                    .stalwart
                    .addresses(user)
                    .await
                    .or_raise(|| UiError::LoadAddresses)?;
                list.insert(0, String::new());
                Ok(list)
            }),
            address,
            header: None,
            text,
            target,
            keep: true,
            focus: Field::Target,
            mode: Mode::Normal,
            error: None,
            saving: Task::new(),
        }
    }

    fn address(&self) -> Option<&String> {
        let Task::Done(Ok(list)) = &self.addresses else {
            return None;
        };
        pick(list, self.address.as_ref())
    }

    fn cycle(&mut self, forward: bool) {
        match self.focus {
            Field::Address => {
                if let Task::Done(Ok(list)) = &self.addresses {
                    self.address = cycle(list, self.address.as_ref(), forward);
                }
            }
            Field::Filter => {
                self.header = match (self.header, forward) {
                    (None, true) | (Some(Header::Subject), false) => Some(Header::From),
                    (Some(Header::From), true) | (None, false) => Some(Header::Subject),
                    (Some(Header::Subject), true) | (Some(Header::From), false) => None,
                };
            }
            Field::Keep => self.keep = !self.keep,
            Field::Text | Field::Target => {}
        }
    }

    fn save(&mut self, ctx: &Ctx) {
        if matches!(self.saving, Task::Running(_)) {
            return;
        }
        let Some(address) = self.address().cloned() else {
            return;
        };
        let address = (!address.is_empty()).then_some(address);
        let filter = self.header.map(|h| (h, self.text.lines().join("")));
        let target = self.target.lines().join("");
        match Forward::new(self.user, address, filter, &target, self.keep) {
            Ok(forward) => {
                let mail = ctx.mail.clone();
                self.mode = Mode::Normal;
                self.saving.spawn(async move {
                    mail.add_forward(forward)
                        .await
                        .or_raise(|| UiError::AddForward)
                });
            }
            Err(e) => self.error = Some(e),
        }
    }

    fn field(&mut self) -> Option<&mut TextArea<'static>> {
        match self.focus {
            Field::Text => Some(&mut self.text),
            Field::Target => Some(&mut self.target),
            Field::Address | Field::Filter | Field::Keep => None,
        }
    }

    fn yank_field(&self) -> Action {
        match self.focus {
            Field::Address => yank(self.address().map_or("", String::as_str)),
            Field::Text => yank(&self.text.lines().join("")),
            Field::Target => yank(&self.target.lines().join("")),
            Field::Filter | Field::Keep => Action::None,
        }
    }
}

impl View for ForwardForm {
    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let [address, filter, text, target, keep, status] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
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

        let source = match self.addresses.poll() {
            Task::Done(Ok(_)) => match self.address().map(String::as_str) {
                Some("") | None => Line::from("‹ all addresses ›"),
                Some(address) => Line::from(format!("‹ {address} ›")),
            },
            Task::Done(Err(e)) => Line::from(failure(e)).red(),
            _ => Line::from("loading…").fg(theme::SUBTLE),
        };
        frame.render_widget(
            Paragraph::new(source).block(block(" Mail to ", Field::Address)),
            address,
        );

        let header = match self.header {
            Some(header) => Line::from(format!("‹ {header} contains ›")),
            None => Line::from("‹ no filter ›").fg(theme::SUBTLE),
        };
        frame.render_widget(
            Paragraph::new(header).block(block(" Filter ", Field::Filter)),
            filter,
        );

        let mode_line = if self.keep {
            Line::from("‹ copy · keep it here too ›")
        } else {
            Line::from("‹ move · only forward ›").yellow()
        };
        frame.render_widget(
            Paragraph::new(mode_line).block(block(" Mode ", Field::Keep)),
            keep,
        );

        for (input, field, title, rect) in [
            (&mut self.text, Field::Text, " Filter text ", text),
            (&mut self.target, Field::Target, " Forward to ", target),
        ] {
            input.set_block(block(title, field));
            input.set_cursor_style(if field == focus && mode == Mode::Insert {
                Style::new().reversed()
            } else {
                Style::new()
            });
            frame.render_widget(&**input, rect);
        }

        let line = match (&self.error, self.saving.poll()) {
            (Some(e), _) => Line::from(failure(e)).red(),
            (None, Task::Running(_)) => Line::from("saving…").fg(theme::SUBTLE),
            (None, Task::Done(Err(e))) => Line::from(failure(e)).red(),
            (None, Task::Panicked(e)) => Line::from(format!("error: {e}")).red(),
            (None, Task::Unexecuted | Task::Done(Ok(_))) => Line::from(""),
        };
        frame.render_widget(line, status);
    }

    fn input(&mut self, key: KeyEvent, ctx: &Ctx) -> Action {
        self.error = None;
        match keys::Fields::get(&key) {
            Some(keys::Fields::Submit) => {
                self.save(ctx);
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
            Some(keys::Fields::Left) if !self.focus.text() => {
                self.cycle(false);
                return Action::None;
            }
            Some(keys::Fields::Right) if !self.focus.text() => {
                self.cycle(true);
                return Action::None;
            }
            Some(keys::Fields::Left | keys::Fields::Right) | None => {}
        }
        match self.mode {
            Mode::Insert => match keys::Insert::get(&key) {
                Some(keys::Insert::Normal) => self.mode = Mode::Normal,
                Some(keys::Insert::Next) => self.focus = self.focus.step(true),
                None => {
                    if let Some(input) = self.field() {
                        input.input(key);
                    }
                }
            },
            Mode::Normal => match keys::Edit::get(&key) {
                Some(keys::Edit::Back) => return Action::Pop,
                Some(keys::Edit::Insert) if self.focus.text() => self.mode = Mode::Insert,
                Some(keys::Edit::Down) => self.focus = self.focus.step(true),
                Some(keys::Edit::Up) => self.focus = self.focus.step(false),
                Some(keys::Edit::Left) => self.cycle(false),
                Some(keys::Edit::Right) => self.cycle(true),
                Some(keys::Edit::Yank) => return self.yank_field(),
                Some(keys::Edit::Insert) | None => {}
            },
        }
        Action::None
    }

    fn help(&self) -> keys::Help {
        match self.mode {
            Mode::Normal => &[keys::Edit::KEYS, keys::Fields::KEYS],
            Mode::Insert => &[keys::Insert::KEYS, keys::Fields::KEYS],
        }
    }

    fn mode(&self) -> Mode {
        self.mode
    }

    fn update(&mut self, _ctx: &Ctx) -> Action {
        match self.saving.poll() {
            Task::Done(Ok(_)) => Action::Pop,
            _ => Action::None,
        }
    }

    fn command(&mut self, name: &str, ctx: &Ctx) -> Option<Action> {
        matches!(name, "w" | "write").then(|| {
            self.save(ctx);
            Action::None
        })
    }
}
