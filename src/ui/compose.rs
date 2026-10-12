use std::{
    str::FromStr,
    time::{Duration, Instant},
};

use email_address::EmailAddress;
use exn::{ErrorExt, ResultExt, ensure};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Style, Stylize},
    text::Line,
    widgets::Paragraph,
};
use ratatui_textarea::TextArea;

use super::{Action, Ctx, Load, Mode, View, cycle, failure, keys, line_input, pick, theme, yank};
use crate::{
    app::KeyEvent,
    error::{InputError, UiError},
    task::Task,
};

const COOLDOWN: Duration = Duration::from_secs(2);

#[derive(Default)]
pub struct Draft {
    pub from: Option<String>,
    pub to: String,
    pub subject: String,
    pub body: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    From,
    To,
    Subject,
    Body,
}

impl Field {
    const ALL: [Field; 4] = [Field::From, Field::To, Field::Subject, Field::Body];

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

pub struct Compose {
    addresses: Load<Vec<String>>,
    from: Option<String>,
    to: Box<TextArea<'static>>,
    subject: Box<TextArea<'static>>,
    body: Box<TextArea<'static>>,
    focus: Field,
    mode: Mode,
    sending: Load<()>,
}

impl Compose {
    pub fn new(ctx: &Ctx, draft: Draft) -> Self {
        let (state, user) = (ctx.state, ctx.user);
        let focus = if draft.to.is_empty() {
            Field::To
        } else {
            Field::Body
        };
        let mut body = Box::new(TextArea::from(draft.body.lines()));
        body.set_cursor_line_style(Style::new());
        Self {
            addresses: Task::new_spawn(async move {
                state
                    .stalwart
                    .addresses(user)
                    .await
                    .or_raise(|| UiError::LoadAddresses)
            }),
            from: draft.from,
            to: line_input(draft.to),
            subject: line_input(draft.subject),
            body,
            focus,
            mode: Mode::Normal,
            sending: Task::new(),
        }
    }

    fn sender(&self) -> Option<&String> {
        let Task::Done(Ok(list)) = &self.addresses else {
            return None;
        };
        pick(list, self.from.as_ref())
    }

    fn cycle_sender(&mut self, forward: bool) {
        if let Task::Done(Ok(list)) = &self.addresses {
            self.from = cycle(list, self.from.as_ref(), forward);
        }
    }

    fn send(&mut self, ctx: &Ctx) {
        if matches!(self.sending, Task::Running(_)) {
            return;
        }
        let now = Instant::now();
        if ctx
            .sent_at
            .get()
            .is_some_and(|at| now.duration_since(at) < COOLDOWN)
        {
            self.sending = Task::Done(Err(UiError::SendCooldown(COOLDOWN.as_secs()).raise()));
            return;
        }
        let Some(from) = self.sender().cloned() else {
            self.sending = Task::Done(Err(UiError::NoAddress.raise()));
            return;
        };
        if !ctx.user.owns(&from) {
            self.sending = Task::Done(Err(UiError::ForeignSender.raise()));
            return;
        }
        let to =
            match recipients(&self.to.lines().join(",")).or_raise(|| UiError::InvalidRecipients) {
                Ok(to) => to,
                Err(e) => {
                    self.sending = Task::Done(Err(e));
                    return;
                }
            };
        let mail = ctx.mail.clone();
        let name = ctx.user.name.to_string();
        let subject = self.subject.lines().join(" ");
        let body = self.body.lines().join("\n");
        self.mode = Mode::Normal;
        ctx.sent_at.set(Some(now));
        self.sending.spawn(async move {
            mail.send(&name, &from, &to, &subject, &body)
                .await
                .or_raise(|| UiError::SendMessage)
        });
    }

    fn text(&self) -> String {
        match self.focus {
            Field::From => self.sender().cloned().unwrap_or_default(),
            Field::To => self.to.lines().join("\n"),
            Field::Subject => self.subject.lines().join("\n"),
            Field::Body => self.body.lines().join("\n"),
        }
    }

    fn field(&mut self) -> Option<&mut TextArea<'static>> {
        match self.focus {
            Field::From => None,
            Field::To => Some(&mut self.to),
            Field::Subject => Some(&mut self.subject),
            Field::Body => Some(&mut self.body),
        }
    }
}

impl View for Compose {
    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let [from, to, subject, body, status] = Layout::vertical([
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
                self.sender().map(String::as_str).unwrap_or_default()
            )),
            Task::Done(Err(e)) => Line::from(failure(e)).red(),
            _ => Line::from("loading…").fg(theme::SUBTLE),
        };
        frame.render_widget(
            Paragraph::new(sender).block(block(" From ", Field::From)),
            from,
        );

        for (area, field, title, rect) in [
            (&mut self.to, Field::To, " To ", to),
            (&mut self.subject, Field::Subject, " Subject ", subject),
            (&mut self.body, Field::Body, " Body ", body),
        ] {
            area.set_block(block(title, field));
            area.set_cursor_style(if field == focus && mode == Mode::Insert {
                Style::new().reversed()
            } else {
                Style::new()
            });
            frame.render_widget(&**area, rect);
        }

        let line = match self.sending.poll() {
            Task::Unexecuted => Line::from(""),
            Task::Running(_) => Line::from("sending…").fg(theme::SUBTLE),
            Task::Done(Ok(())) => Line::from("sent ✓").green(),
            Task::Done(Err(e)) => Line::from(failure(e)).red(),
            Task::Panicked(e) => Line::from(format!("error: {e}")).red(),
        };
        frame.render_widget(line.bold(), status);
    }

    fn input(&mut self, key: KeyEvent, ctx: &Ctx) -> Action {
        let on_from = self.focus == Field::From;
        match keys::Fields::get(&key) {
            Some(keys::Fields::Submit) => {
                self.send(ctx);
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
            Some(keys::Fields::Left) if on_from => {
                self.cycle_sender(false);
                return Action::None;
            }
            Some(keys::Fields::Right) if on_from => {
                self.cycle_sender(true);
                return Action::None;
            }
            Some(keys::Fields::Left | keys::Fields::Right) | None => {}
        }
        match self.mode {
            Mode::Insert => match keys::Insert::get(&key) {
                Some(keys::Insert::Normal) => self.mode = Mode::Normal,
                Some(keys::Insert::Next) if self.focus != Field::Body => {
                    self.focus = self.focus.step(true);
                }
                Some(keys::Insert::Next) | None => {
                    if let Some(area) = self.field() {
                        area.input(key);
                    }
                }
            },
            Mode::Normal => match keys::Edit::get(&key) {
                Some(keys::Edit::Back) => return Action::Pop,
                Some(keys::Edit::Insert) if !on_from => self.mode = Mode::Insert,
                Some(keys::Edit::Down) => self.focus = self.focus.step(true),
                Some(keys::Edit::Up) => self.focus = self.focus.step(false),
                Some(keys::Edit::Left) if on_from => self.cycle_sender(false),
                Some(keys::Edit::Right) if on_from => self.cycle_sender(true),
                Some(keys::Edit::Yank) => return yank(&self.text()),
                Some(keys::Edit::Insert | keys::Edit::Left | keys::Edit::Right) | None => {}
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

    fn command(&mut self, name: &str, ctx: &Ctx) -> Option<Action> {
        matches!(name, "w" | "send").then(|| {
            self.send(ctx);
            Action::None
        })
    }
}

fn recipients(raw: &str) -> exn::Result<Vec<String>, InputError> {
    let to = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            EmailAddress::from_str(s)
                .map(|_| s.to_owned())
                .or_raise(|| InputError::InvalidRecipient(s.to_owned()))
        })
        .collect::<exn::Result<Vec<_>, InputError>>()?;
    ensure!(!to.is_empty(), InputError::NoRecipients);
    Ok(to)
}
