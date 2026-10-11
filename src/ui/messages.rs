use exn::ResultExt;
use jmap_client::mailbox::Role;
use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::{Style, Stylize},
    text::Line,
    widgets::{Row, Table, TableState},
};

use super::{
    Action, Ctx, Load, Op, Reader, View, confirm_popup, contains, date, emails, find_match,
    junk_target, keys, keys_popup, move_target, names, status, table_nav, theme, yank,
};
use crate::{app::KeyEvent, error::UiError, mail::Summary, task::Task};

enum Pending {
    Move,
    Destroy(String),
}

pub struct Messages {
    title: String,
    mailbox: String,
    role: Role,
    list: Load<Vec<Summary>>,
    table: TableState,
    pending: Option<Pending>,
    undo: Option<(String, Role)>,
}

impl Messages {
    pub fn new(ctx: &Ctx, title: impl Into<String>, mailbox: String, role: Role) -> Self {
        let mut page = Self {
            title: title.into(),
            mailbox,
            role,
            list: Task::new(),
            table: TableState::default().with_selected(0),
            pending: None,
            undo: None,
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

    fn move_selected(&mut self, ctx: &Ctx, to: Role) {
        if to == self.role {
            return;
        }
        let Some(id) = self.selected().map(|m| m.id.clone()) else {
            return;
        };
        self.undo = Some((id.clone(), to.clone()));
        let from = self.role.clone();
        self.run(ctx, id, Op::Move { from, to });
    }

    fn undo(&mut self, ctx: &Ctx) {
        if let Some((id, from)) = self.undo.take() {
            let op = Op::Undo {
                mailbox: self.mailbox.clone(),
                from,
                to: self.role.clone(),
            };
            self.run(ctx, id, op);
        }
    }

    fn mail(&mut self, cmd: keys::Mail, ctx: &Ctx) -> Action {
        match cmd {
            keys::Mail::Back => return Action::Pop,
            keys::Mail::Trash => self.move_selected(ctx, Role::Trash),
            keys::Mail::Archive => self.move_selected(ctx, Role::Archive),
            keys::Mail::Junk => self.move_selected(ctx, junk_target(&self.role)),
            keys::Mail::Delete => {
                if let Some(message) = self.selected() {
                    self.pending = Some(Pending::Destroy(message.id.clone()));
                }
            }
            keys::Mail::Move => self.pending = Some(Pending::Move),
            keys::Mail::Unread => self.run_selected(ctx, Op::Seen(false)),
            keys::Mail::YankSender => {
                if let Some(message) = self.selected() {
                    return yank(&emails(&message.from));
                }
            }
            keys::Mail::YankSubject => {
                if let Some(message) = self.selected() {
                    return yank(&message.subject);
                }
            }
        }
        Action::None
    }
}

impl View for Messages {
    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let block = theme::block().title(format!(" {} ", self.title));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        match self.list.poll() {
            Task::Done(Ok(list)) if list.is_empty() => {
                frame.render_widget(Line::from("no messages").fg(theme::SUBTLE), inner);
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
                .header(Row::new(["date", "from", "to", "subject"]).style(theme::HEADER))
                .row_highlight_style(theme::SELECTED);
                frame.render_stateful_widget(table, inner, &mut self.table);
            }
            other => status(frame, inner, other),
        }

        match self.pending {
            Some(Pending::Move) => keys_popup(frame, area, " move to ", &[keys::MoveTo::KEYS]),
            Some(Pending::Destroy(_)) => confirm_popup(frame, area, " delete permanently? "),
            None => {}
        }
    }

    fn input(&mut self, key: KeyEvent, ctx: &Ctx) -> Action {
        if let Some(pending) = self.pending.take() {
            match pending {
                Pending::Move => {
                    if let Some(to) = move_target(&key) {
                        self.move_selected(ctx, to);
                    }
                }
                Pending::Destroy(id) => {
                    if keys::Confirm::get(&key) == Some(keys::Confirm::Yes) {
                        self.run(ctx, id, Op::Destroy);
                    }
                }
            }
            return Action::None;
        }
        if table_nav(&mut self.table, &key) {
            return Action::None;
        }
        if let Some(cmd) = keys::Mail::get(&key) {
            return self.mail(cmd, ctx);
        }
        match keys::Messages::get(&key) {
            Some(keys::Messages::Reload) => self.reload(ctx),
            Some(keys::Messages::Open) => {
                if let Task::Done(Ok(list)) = &mut self.list
                    && let Some(message) = self.table.selected().and_then(|i| list.get_mut(i))
                {
                    message.seen = true;
                    return Action::Push(Box::new(
                        Reader::new(ctx, message.id.clone(), self.role.clone()).into(),
                    ));
                }
            }
            Some(keys::Messages::Read) => self.run_selected(ctx, Op::Seen(true)),
            Some(keys::Messages::Undo) => self.undo(ctx),
            None => {}
        }
        Action::None
    }

    fn help(&self) -> keys::Help {
        &[keys::Messages::KEYS, keys::Mail::KEYS]
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
