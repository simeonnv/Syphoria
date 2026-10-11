use exn::ResultExt;
use jmap_client::mailbox::Role;
use ratatui::{
    Frame,
    layout::Rect,
    style::Stylize,
    text::{Line, Text},
    widgets::{Paragraph, Wrap},
};

use super::{
    Action, Compose, Ctx, Draft, Load, Op, PAGE, View, confirm_popup, date, emails, failure,
    junk_target, keys, keys_popup, move_target, status, theme, yank,
};
use crate::{
    app::KeyEvent,
    error::UiError,
    mail::{Addr, Message},
    task::Task,
};

enum Pending {
    Move,
    Destroy,
}

pub struct Reader {
    id: String,
    role: Role,
    message: Load<Message>,
    scroll: u16,
    op: Load<()>,
    pending: Option<Pending>,
}

impl Reader {
    pub fn new(ctx: &Ctx, id: String, role: Role) -> Self {
        let mail = ctx.mail.clone();
        let message_id = id.clone();
        Self {
            id,
            role,
            message: Task::new_spawn(async move {
                mail.message(&message_id)
                    .await
                    .or_raise(|| UiError::LoadMessage)
            }),
            scroll: 0,
            op: Task::new(),
            pending: None,
        }
    }

    fn run(&mut self, ctx: &Ctx, op: Op) {
        if matches!(self.op, Task::Running(_)) {
            return;
        }
        let (mail, id) = (ctx.mail.clone(), self.id.clone());
        self.op.spawn(async move { op.run(&mail, &id).await });
    }

    fn move_to(&mut self, ctx: &Ctx, to: Role) {
        if to != self.role {
            let from = self.role.clone();
            self.run(ctx, Op::Move { from, to });
        }
    }

    fn mail(&mut self, cmd: keys::Mail, ctx: &Ctx) -> Action {
        let message = match &self.message {
            Task::Done(Ok(message)) => Some(message),
            _ => None,
        };
        match cmd {
            keys::Mail::Back => return Action::Pop,
            keys::Mail::Trash => self.move_to(ctx, Role::Trash),
            keys::Mail::Archive => self.move_to(ctx, Role::Archive),
            keys::Mail::Junk => self.move_to(ctx, junk_target(&self.role)),
            keys::Mail::Delete => self.pending = Some(Pending::Destroy),
            keys::Mail::Move => self.pending = Some(Pending::Move),
            keys::Mail::Unread => self.run(ctx, Op::Seen(false)),
            keys::Mail::YankSender => {
                if let Some(message) = message {
                    return yank(&emails(&message.from));
                }
            }
            keys::Mail::YankSubject => {
                if let Some(message) = message {
                    return yank(&message.subject);
                }
            }
        }
        Action::None
    }

    fn lines(&self) -> u16 {
        match &self.message {
            Task::Done(Ok(message)) => (message.body.lines().count() + 5) as u16,
            _ => 0,
        }
    }
}

impl View for Reader {
    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let mut block = theme::block();
        match self.op.poll() {
            Task::Running(_) => {
                block = block.title_bottom(Line::from(" working… ").fg(theme::SUBTLE))
            }
            Task::Done(Err(e)) => {
                block = block.title_bottom(Line::from(format!(" {} ", failure(e))).red());
            }
            _ => {}
        }
        let inner = block.inner(area);

        let Task::Done(Ok(message)) = self.message.poll() else {
            frame.render_widget(block, area);
            status(frame, inner, self.message.poll());
            return;
        };

        let header = |name: &'static str, value: String| {
            Line::from(vec![name.fg(theme::SUBTLE), value.into()])
        };
        let mut text = Text::from(vec![
            header("from:    ", list(&message.from)),
            header("to:      ", list(&message.to)),
            header("date:    ", date(message.received)),
            header("subject: ", message.subject.clone()).bold(),
            Line::from(""),
        ]);
        if !message.cc.is_empty() {
            text.lines.insert(2, header("cc:      ", list(&message.cc)));
        }
        text.extend(message.body.lines().map(Line::from));

        frame.render_widget(
            Paragraph::new(text)
                .block(block.title(format!(" {} ", message.subject)))
                .wrap(Wrap { trim: false })
                .scroll((self.scroll, 0)),
            area,
        );

        match self.pending {
            Some(Pending::Move) => keys_popup(frame, area, " move to ", &[keys::MoveTo::KEYS]),
            Some(Pending::Destroy) => confirm_popup(frame, area, " delete permanently? "),
            None => {}
        }
    }

    fn input(&mut self, key: KeyEvent, ctx: &Ctx) -> Action {
        if let Some(pending) = self.pending.take() {
            match pending {
                Pending::Move => {
                    if let Some(to) = move_target(&key) {
                        self.move_to(ctx, to);
                    }
                }
                Pending::Destroy => {
                    if keys::Confirm::get(&key) == Some(keys::Confirm::Yes) {
                        self.run(ctx, Op::Destroy);
                    }
                }
            }
            return Action::None;
        }

        if let Some(nav) = keys::Nav::get(&key) {
            self.scroll = match nav {
                keys::Nav::Down => self.scroll.saturating_add(1),
                keys::Nav::Up => self.scroll.saturating_sub(1),
                keys::Nav::PageDown => self.scroll.saturating_add(PAGE),
                keys::Nav::PageUp => self.scroll.saturating_sub(PAGE),
                keys::Nav::Top => 0,
                keys::Nav::Bottom => self.lines(),
            };
            return Action::None;
        }
        if let Some(cmd) = keys::Mail::get(&key) {
            return self.mail(cmd, ctx);
        }
        let Task::Done(Ok(message)) = &self.message else {
            return Action::None;
        };
        match keys::Reader::get(&key) {
            Some(keys::Reader::Reply) => Action::Push(Box::new(
                Compose::new(ctx, reply(ctx, message, false)).into(),
            )),
            Some(keys::Reader::ReplyAll) => Action::Push(Box::new(
                Compose::new(ctx, reply(ctx, message, true)).into(),
            )),
            Some(keys::Reader::Forward) => {
                Action::Push(Box::new(Compose::new(ctx, forward(ctx, message)).into()))
            }
            Some(keys::Reader::YankBody) => yank(&message.body),
            None => Action::None,
        }
    }

    fn help(&self) -> keys::Help {
        &[keys::Reader::KEYS, keys::Mail::KEYS]
    }

    fn update(&mut self, _ctx: &Ctx) -> Action {
        match self.op.poll() {
            Task::Done(Ok(())) => Action::Pop,
            _ => Action::None,
        }
    }
}

fn list(addrs: &[Addr]) -> String {
    addrs
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn prefixed(prefix: &str, subject: &str) -> String {
    if subject.to_lowercase().starts_with(&prefix.to_lowercase()) {
        subject.to_owned()
    } else {
        format!("{prefix} {subject}")
    }
}

fn own_address(ctx: &Ctx, message: &Message) -> Option<String> {
    message
        .to
        .iter()
        .chain(&message.cc)
        .find(|a| ctx.user.owns(&a.email))
        .map(|a| a.email.clone())
}

fn reply(ctx: &Ctx, message: &Message, all: bool) -> Draft {
    let quoted = message
        .body
        .lines()
        .map(|l| format!("> {l}"))
        .collect::<Vec<_>>()
        .join("\n");

    let candidates: Vec<&Addr> = if all {
        message
            .from
            .iter()
            .chain(&message.to)
            .chain(&message.cc)
            .collect()
    } else {
        message.from.iter().collect()
    };
    let mut to: Vec<&str> = Vec::new();
    for addr in candidates {
        if !ctx.user.owns(&addr.email) && !to.contains(&addr.email.as_str()) {
            to.push(&addr.email);
        }
    }

    Draft {
        from: own_address(ctx, message),
        to: to.join(", "),
        subject: prefixed("Re:", &message.subject),
        body: format!("\n\n{quoted}"),
    }
}

fn forward(ctx: &Ctx, message: &Message) -> Draft {
    Draft {
        from: own_address(ctx, message),
        to: String::new(),
        subject: prefixed("Fwd:", &message.subject),
        body: format!(
            "\n\n---------- Forwarded message ----------\nFrom: {}\nDate: {}\nSubject: {}\nTo: {}\n\n{}",
            list(&message.from),
            date(message.received),
            message.subject,
            list(&message.to),
            message.body
        ),
    }
}
