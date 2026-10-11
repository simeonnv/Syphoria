use exn::ResultExt;
use jmap_client::mailbox::Role;
use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::{Style, Stylize},
    text::Line,
    widgets::{Block, Row, Table, TableState},
};

use super::{
    Action, Ctx, Load, MOVE, Op, Reader, View, alt, contains, date, emails, find_match, keys_popup,
    move_target, names, status, table_nav, yank,
};
use crate::{
    app::{KeyCode, KeyEvent},
    error::UiError,
    mail::Summary,
    task::Task,
};

enum Pending {
    Move,
    Destroy(String),
}

pub struct Messages {
    title: String,
    mailbox: String,
    list: Load<Vec<Summary>>,
    table: TableState,
    pending: Option<Pending>,
}

impl Messages {
    pub fn new(ctx: &Ctx, title: impl Into<String>, mailbox: String) -> Self {
        let mut page = Self {
            title: title.into(),
            mailbox,
            list: Task::new(),
            table: TableState::default().with_selected(0),
            pending: None,
        };
        page.reload(ctx);
        page
    }

    fn reload(&mut self, ctx: &Ctx) {
        let (mail, mailbox) = (ctx.mail.clone(), self.mailbox.clone());
        self.list.spawn(async move {
            mail.summaries(&mailbox)
                .await
                .or_raise(|| UiError::LoadMessages)
        });
    }

    fn selected(&self) -> Option<&Summary> {
        let Task::Done(Ok(list)) = &self.list else {
            return None;
        };
        self.table.selected().and_then(|i| list.get(i))
    }

    fn run(&mut self, ctx: &Ctx, id: String, op: Op) {
        let (mail, mailbox) = (ctx.mail.clone(), self.mailbox.clone());
        self.list.spawn(async move {
            op.run(&mail, &id).await?;
            mail.summaries(&mailbox)
                .await
                .or_raise(|| UiError::LoadMessages)
        });
    }

    fn run_selected(&mut self, ctx: &Ctx, op: Op) {
        if let Some(id) = self.selected().map(|m| m.id.clone()) {
            self.run(ctx, id, op);
        }
    }
}

impl View for Messages {
    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let block = Block::bordered().title(format!(" {} ", self.title));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        match self.list.poll() {
            Task::Done(Ok(list)) if list.is_empty() => {
                frame.render_widget(Line::from("no messages").dim(), inner);
            }
            Task::Done(Ok(list)) => {
                let rows = list.iter().map(|m| {
                    let row = Row::new([
                        date(m.received),
                        names(&m.from),
                        emails(&m.to),
                        m.subject.clone(),
                    ]);
                    if m.seen {
                        row
                    } else {
                        row.style(Style::new().bold())
                    }
                });
                let table = Table::new(
                    rows,
                    [
                        Constraint::Length(16),
                        Constraint::Length(24),
                        Constraint::Length(28),
                        Constraint::Fill(1),
                    ],
                )
                .header(Row::new(["date", "from", "to", "subject"]).style(Style::new().dim()))
                .row_highlight_style(Style::new().reversed());
                frame.render_stateful_widget(table, inner, &mut self.table);
            }
            other => status(frame, inner, other),
        }

        match self.pending {
            Some(Pending::Move) => keys_popup(frame, area, " move to ", MOVE),
            Some(Pending::Destroy(_)) => keys_popup(
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
                        self.run_selected(ctx, op);
                    }
                }
                Pending::Destroy(id) => {
                    if key.key == KeyCode::Char('y') {
                        self.run(ctx, id, Op::Destroy);
                    }
                }
            }
            return Action::None;
        }
        if table_nav(&mut self.table, &key) {
            return Action::None;
        }
        match key.key {
            KeyCode::Escape => return Action::Pop,
            KeyCode::Char('r') => self.reload(ctx),
            KeyCode::Enter => {
                if let Task::Done(Ok(list)) = &mut self.list
                    && let Some(message) = self.table.selected().and_then(|i| list.get_mut(i))
                {
                    message.seen = true;
                    return Action::Push(Box::new(Reader::new(ctx, message.id.clone()).into()));
                }
            }
            KeyCode::Char('d') if alt(&key) => {
                if let Some(message) = self.selected() {
                    self.pending = Some(Pending::Destroy(message.id.clone()));
                }
            }
            KeyCode::Char('d') => self.run_selected(ctx, Op::Move(Role::Trash)),
            KeyCode::Char('m') => self.pending = Some(Pending::Move),
            KeyCode::Char('u') => self.run_selected(ctx, Op::Seen(false)),
            KeyCode::Char('U') => self.run_selected(ctx, Op::Seen(true)),
            KeyCode::Char('y') if alt(&key) => {
                if let Some(message) = self.selected() {
                    return yank(&message.subject);
                }
            }
            KeyCode::Char('y') => {
                if let Some(message) = self.selected() {
                    return yank(&emails(&message.from));
                }
            }
            _ => {}
        }
        Action::None
    }

    fn help(&self) -> &'static str {
        "enter open · d trash · alt-d delete · m move · u/U unread/read · y sender · alt-y subject · r reload"
    }

    fn resume(&mut self, ctx: &Ctx) -> Action {
        self.reload(ctx);
        Action::None
    }

    fn search(&mut self, query: &str, forward: bool) {
        let Task::Done(Ok(list)) = &self.list else {
            return;
        };
        let found = find_match(list.len(), self.table.selected(), forward, |i| {
            let m = &list[i];
            contains(&m.subject, query)
                || contains(&names(&m.from), query)
                || contains(&emails(&m.from), query)
                || contains(&emails(&m.to), query)
        });
        if found.is_some() {
            self.table.select(found);
        }
    }
}
