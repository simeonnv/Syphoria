use exn_anyhow::into_anyhow;
use jmap_client::mailbox::Role;
use ratatui::{
    Frame,
    layout::Rect,
    style::Stylize,
    text::{Line, Text},
    widgets::{Block, Paragraph, Wrap},
};

use super::{
    Action, Compose, Ctx, Draft, Load, MOVE, Op, PAGE, View, alt, date, emails, keys_popup,
    move_target, status, yank,
};
use crate::{
    app::{KeyCode, KeyEvent},
    mail::{Addr, Message},
    task::Task,
};

enum Pending {
    Move,
    Destroy,
}

pub struct Reader {
    id: String,
    message: Load<Message>,
    scroll: u16,
    op: Load<()>,
    pending: Option<Pending>,
}

impl Reader {
    pub fn new(ctx: &Ctx, id: String) -> Self {
        let mail = ctx.mail.clone();
        let message_id = id.clone();
        Self {
            id,
            message: Task::new_spawn(async move {
                mail.message(&message_id).await.map_err(into_anyhow)
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

    fn lines(&self) -> u16 {
        match &self.message {
            Task::Done(Ok(message)) => (message.body.lines().count() + 5) as u16,
            _ => 0,
        }
    }
}

impl View for Reader {
    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let mut block = Block::bordered();
        match self.op.poll() {
            Task::Running(_) => block = block.title_bottom(Line::from(" working… ").dim()),
            Task::Done(Err(e)) => {
                block = block.title_bottom(Line::from(format!(" error: {e:#} ")).red());
            }
            _ => {}
        }
        let inner = block.inner(area);

        let Task::Done(Ok(message)) = self.message.poll() else {
            frame.render_widget(block, area);
            status(frame, inner, self.message.poll());
            return;
        };

        let header = |name: &'static str, value: String| Line::from(vec![name.dim(), value.into()]);
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
            Some(Pending::Move) => keys_popup(frame, area, " move to ", MOVE),
            Some(Pending::Destroy) => keys_popup(
                frame,
                area,
                " delete permanently? ",
                &[("y", "yes"), ("any", "cancel")],
            ),
            None => {}
        }
    }

    fn input(&mut self, key: KeyEvent, ctx: &Ctx) -> Action {
        if let Some(pending) = self.pending.take() {
            match pending {
                Pending::Move => {
                    if let Some(op) = move_target(&key) {
                        self.run(ctx, op);
                    }
                }
                Pending::Destroy => {
                    if key.key == KeyCode::Char('y') {
                        self.run(ctx, Op::Destroy);
                    }
                }
            }
            return Action::None;
        }

        let message = match &self.message {
            Task::Done(Ok(message)) => Some(message),
            _ => None,
        };
        match key.key {
            KeyCode::Char('j') | KeyCode::DownArrow => self.scroll = self.scroll.saturating_add(1),
            KeyCode::Char('k') | KeyCode::UpArrow => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(PAGE),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(PAGE),
            KeyCode::Home => self.scroll = 0,
            KeyCode::End => self.scroll = self.lines(),
            KeyCode::Escape => return Action::Pop,
            KeyCode::Char('d') if alt(&key) => self.pending = Some(Pending::Destroy),
            KeyCode::Char('d') => self.run(ctx, Op::Move(Role::Trash)),
            KeyCode::Char('m') => self.pending = Some(Pending::Move),
            KeyCode::Char('u') => self.run(ctx, Op::Seen(false)),
            KeyCode::Char('r') => {
                if let Some(message) = message {
                    return Action::Push(Box::new(
                        Compose::new(ctx, reply(ctx, message, false)).into(),
                    ));
                }
            }
            KeyCode::Char('R') => {
                if let Some(message) = message {
                    return Action::Push(Box::new(
                        Compose::new(ctx, reply(ctx, message, true)).into(),
                    ));
                }
            }
            KeyCode::Char('f') => {
                if let Some(message) = message {
                    return Action::Push(Box::new(Compose::new(ctx, forward(ctx, message)).into()));
                }
            }
            KeyCode::Char('y') if alt(&key) => {
                if let Some(message) = message {
                    return yank(&emails(&message.from));
                }
            }
            KeyCode::Char('y') => {
                if let Some(message) = message {
                    return yank(&message.body);
                }
            }
            _ => {}
        }
        Action::None
    }

    fn help(&self) -> &'static str {
        "r reply · R reply all · f forward · d trash · alt-d delete · m move · u unread · y body · alt-y sender"
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
