use exn_anyhow::into_anyhow;
use ratatui::{
    Frame,
    layout::Rect,
    style::Stylize,
    text::{Line, Text},
    widgets::{Block, Paragraph, Wrap},
};

use super::{Action, Compose, Ctx, Draft, Load, View, date, status};
use crate::{
    app::{KeyCode, KeyEvent},
    mail::{Addr, Message},
    task::Task,
};

pub struct Reader {
    message: Load<Message>,
    scroll: u16,
}

impl Reader {
    pub fn new(ctx: &Ctx, id: String) -> Self {
        let mail = ctx.mail.clone();
        Self {
            message: Task::new_spawn(async move { mail.message(&id).await.map_err(into_anyhow) }),
            scroll: 0,
        }
    }
}

impl View for Reader {
    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let block = Block::bordered();
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
        text.extend(message.body.lines().map(Line::from));

        frame.render_widget(
            Paragraph::new(text)
                .block(block.title(format!(" {} ", message.subject)))
                .wrap(Wrap { trim: false })
                .scroll((self.scroll, 0)),
            area,
        );
    }

    fn input(&mut self, key: KeyEvent, ctx: &Ctx) -> Action {
        match key.key {
            KeyCode::Char('j') | KeyCode::DownArrow => self.scroll = self.scroll.saturating_add(1),
            KeyCode::Char('k') | KeyCode::UpArrow => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll = self.scroll.saturating_add(20),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(20),
            KeyCode::Escape => return Action::Pop,
            KeyCode::Char('r') => {
                if let Task::Done(Ok(message)) = &self.message {
                    return Action::Push(Compose::new(ctx, reply(ctx, message)).into());
                }
            }
            _ => {}
        }
        Action::None
    }

    fn help(&self) -> &'static str {
        "j/k scroll · r reply · esc back"
    }
}

fn list(addrs: &[Addr]) -> String {
    addrs
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn reply(ctx: &Ctx, message: &Message) -> Draft {
    let subject = if message.subject.to_lowercase().starts_with("re:") {
        message.subject.clone()
    } else {
        format!("Re: {}", message.subject)
    };

    let quoted = message
        .body
        .lines()
        .map(|l| format!("> {l}"))
        .collect::<Vec<_>>()
        .join("\n");

    Draft {
        from: message
            .to
            .iter()
            .find(|a| ctx.user.owns(&a.email))
            .map(|a| a.email.clone()),
        to: message
            .from
            .iter()
            .map(|a| a.email.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        subject,
        body: format!("\n\n{quoted}"),
    }
}
