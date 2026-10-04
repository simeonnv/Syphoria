use exn_anyhow::into_anyhow;
use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::{Style, Stylize},
    text::Line,
    widgets::{Block, Row, Table, TableState},
};

use super::{Action, Ctx, Load, Reader, View, date, emails, names, status};
use crate::{
    app::{KeyCode, KeyEvent},
    mail::Summary,
    task::Task,
};

pub struct Messages {
    title: String,
    mailbox: String,
    list: Load<Vec<Summary>>,
    table: TableState,
}

impl Messages {
    pub fn new(ctx: &Ctx, title: impl Into<String>, mailbox: String) -> Self {
        let mut page = Self {
            title: title.into(),
            mailbox,
            list: Task::new(),
            table: TableState::default().with_selected(0),
        };
        page.reload(ctx);
        page
    }

    fn reload(&mut self, ctx: &Ctx) {
        let (mail, mailbox) = (ctx.mail.clone(), self.mailbox.clone());
        self.list
            .spawn(async move { mail.summaries(&mailbox).await.map_err(into_anyhow) });
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
    }

    fn input(&mut self, key: KeyEvent, ctx: &Ctx) -> Action {
        match key.key {
            KeyCode::Char('j') | KeyCode::DownArrow => self.table.select_next(),
            KeyCode::Char('k') | KeyCode::UpArrow => self.table.select_previous(),
            KeyCode::Char('r') => self.reload(ctx),
            KeyCode::Escape => return Action::Pop,
            KeyCode::Enter => {
                if let Task::Done(Ok(list)) = &mut self.list
                    && let Some(message) = self.table.selected().and_then(|i| list.get_mut(i))
                {
                    message.seen = true;
                    return Action::Push(Reader::new(ctx, message.id.clone()).into());
                }
            }
            _ => {}
        }
        Action::None
    }

    fn help(&self) -> &'static str {
        "j/k move · enter open · r reload · esc back"
    }
}
