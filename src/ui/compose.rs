use std::str::FromStr;

use anyhow::{Context, Result, anyhow, ensure};
use email_address::EmailAddress;
use exn_anyhow::into_anyhow;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Style, Stylize},
    text::Line,
    widgets::{Block, Paragraph},
};
use ratatui_textarea::TextArea;
use termwiz::input::Modifiers;

use super::{Action, Ctx, Load, Mode, View, ctrl, cycle, line_input, pick};
use crate::{
    app::{KeyCode, KeyEvent},
    task::Task,
};

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
                state.stalwart.addresses(user).await.map_err(into_anyhow)
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
        let Some(from) = self.sender().cloned() else {
            self.sending = Task::Done(Err(anyhow!(
                "you have no address to send from, create one in Addresses"
            )));
            return;
        };
        if !ctx.user.owns(&from) {
            self.sending = Task::Done(Err(anyhow!("`{from}` is not on one of your domains")));
            return;
        }
        let to = match recipients(&self.to.lines().join(",")) {
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
        self.sending.spawn(async move {
            mail.send(&name, &from, &to, &subject, &body)
                .await
                .map_err(into_anyhow)
        });
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
            let block = Block::bordered().title(title);
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
            Task::Done(Err(e)) => Line::from(format!("error: {e:#}")).red(),
            _ => Line::from("loading…").dim(),
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
            Task::Running(_) => Line::from("sending…").dim(),
            Task::Done(Ok(())) => Line::from("sent ✓").green(),
            Task::Done(Err(e)) => Line::from(format!("error: {e:#}")).red(),
            Task::Panicked => Line::from("send task panicked").red(),
        };
        frame.render_widget(line, status);
    }

    fn input(&mut self, key: KeyEvent, ctx: &Ctx) -> Action {
        let shift = key.modifiers.contains(Modifiers::SHIFT);
        match (self.mode, key.key) {
            (_, KeyCode::Char('s')) if ctrl(&key) => self.send(ctx),
            (_, KeyCode::Tab) => self.focus = self.focus.step(!shift),
            (_, KeyCode::LeftArrow | KeyCode::RightArrow) if self.focus == Field::From => {
                self.cycle_sender(key.key == KeyCode::RightArrow);
            }
            (Mode::Insert, KeyCode::Escape) => self.mode = Mode::Normal,
            (Mode::Insert, KeyCode::Enter) if self.focus != Field::Body => {
                self.focus = self.focus.step(true);
            }
            (Mode::Insert, _) => {
                if let Some(area) = self.field() {
                    area.input(key);
                }
            }
            (Mode::Normal, KeyCode::Escape) => return Action::Pop,
            (Mode::Normal, KeyCode::Char('i')) if self.focus != Field::From => {
                self.mode = Mode::Insert;
            }
            (Mode::Normal, KeyCode::Char('j') | KeyCode::DownArrow) => {
                self.focus = self.focus.step(true);
            }
            (Mode::Normal, KeyCode::Char('k') | KeyCode::UpArrow) => {
                self.focus = self.focus.step(false);
            }
            (Mode::Normal, KeyCode::Char(c @ ('h' | 'l'))) if self.focus == Field::From => {
                self.cycle_sender(c == 'l');
            }
            _ => {}
        }
        Action::None
    }

    fn help(&self) -> &'static str {
        match self.mode {
            Mode::Normal => "i insert · j/k field · h/l sender · ctrl-s or :w send · esc back",
            Mode::Insert => "esc normal · tab field · ←/→ sender · ctrl-s send",
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

fn recipients(raw: &str) -> Result<Vec<String>> {
    let to = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            EmailAddress::from_str(s)
                .map(|_| s.to_owned())
                .with_context(|| format!("invalid recipient `{s}`"))
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(!to.is_empty(), "add at least one recipient");
    Ok(to)
}
