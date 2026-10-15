use std::mem;

use exn::ResultExt;
use jmap_client::mailbox::Role;
use linkify::{LinkFinder, LinkKind};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Position, Rect},
    style::Stylize,
    text::{Line, Text},
    widgets::{Clear, List, ListState, Paragraph, Widget, Wrap},
};
use unicode_width::UnicodeWidthStr;
use url::Url;

use super::{
    Action, Attached, Attachments, Compose, Ctx, Draft, Load, Op, PAGE, View, attachments::size,
    confirm_popup, date, emails, failure, keys, keys_popup, list_nav, move_target, spam_target,
    status, theme, yank,
};
use crate::{
    app::KeyEvent,
    backend::Link,
    error::UiError,
    mail::{Addr, Message},
    task::Task,
};

struct Href {
    text: String,
    url: String,
}

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
    menu: Option<ListState>,
    links: Vec<Link>,
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
            menu: None,
            links: Vec::new(),
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
            keys::Mail::Spam => self.move_to(ctx, spam_target(&self.role)),
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

    fn menu_input(&mut self, key: &KeyEvent) -> Action {
        let Some(menu) = &mut self.menu else {
            return Action::None;
        };
        if list_nav(menu, key) {
            return Action::None;
        }
        match keys::Links::get(key) {
            Some(keys::Links::Yank) => {
                let selected = menu.selected();
                self.menu = None;
                if let (Task::Done(Ok(message)), Some(i)) = (&self.message, selected)
                    && let Some(href) = hrefs(&message.body).get(i)
                {
                    return yank(&href.text);
                }
            }
            Some(keys::Links::Close) => self.menu = None,
            None => {}
        }
        Action::None
    }

    fn lines(&self) -> u16 {
        match &self.message {
            Task::Done(Ok(message)) => {
                let cc = usize::from(!message.cc.is_empty());
                (message.body.lines().count() + message.attachments.len() + cc + 5) as u16
            }
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
        let at = text.lines.len() - 1;
        for (i, a) in message.attachments.iter().enumerate() {
            let label = if i == 0 { "files:   " } else { "         " };
            text.lines.insert(
                at + i,
                header(label, format!("{} ({})", a.name, size(a.size))),
            );
        }
        text.extend(message.body.lines().map(Line::from));

        let hrefs = hrefs(&message.body);
        let block = block.title(format!(" {} ", message.subject));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let longest = hrefs.iter().map(|h| h.text.width()).max().unwrap_or(0);
        let span = (longest / usize::from(inner.width.max(1))) as u16 + 1;
        let start = self.scroll.saturating_sub(span);
        let skip = self.scroll - start;
        let rect = Rect::new(0, 0, inner.width, inner.height.saturating_add(skip + span));
        let mut rendered = Buffer::empty(rect);
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .scroll((start, 0))
            .render(rect, &mut rendered);
        let buf = frame.buffer_mut();
        for y in 0..inner.height {
            for x in 0..inner.width {
                buf[(inner.x + x, inner.y + y)] = rendered[(x, y + skip)].clone();
            }
        }

        self.links = match (&self.pending, &mut self.menu) {
            (Some(Pending::Move), _) => {
                keys_popup(frame, area, " move to ", &[keys::MoveTo::KEYS]);
                Vec::new()
            }
            (Some(Pending::Destroy), _) => {
                confirm_popup(frame, area, " delete permanently? ");
                Vec::new()
            }
            (None, Some(menu)) => {
                draw_menu(frame, area, &hrefs, menu);
                Vec::new()
            }
            (None, None) => locate(&rendered, &hrefs, skip, inner),
        };
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
        if self.menu.is_some() {
            return self.menu_input(&key);
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
            Some(keys::Reader::Attachments) if !message.attachments.is_empty() => Action::Push(
                Box::new(Attachments::new(ctx, message.attachments.clone()).into()),
            ),
            Some(keys::Reader::YankBody) => yank(&message.body),
            Some(keys::Reader::Links) => {
                self.menu = Some(ListState::default().with_selected(Some(0)));
                Action::None
            }
            Some(keys::Reader::Attachments) | None => Action::None,
        }
    }

    fn help(&self) -> keys::Help {
        match self.menu {
            Some(_) => &[keys::Links::KEYS],
            None => &[keys::Reader::KEYS, keys::Mail::KEYS],
        }
    }

    fn links(&mut self) -> Vec<Link> {
        mem::take(&mut self.links)
    }

    fn update(&mut self, _ctx: &Ctx) -> Action {
        match self.op.poll() {
            Task::Done(Ok(())) => Action::Pop,
            _ => Action::None,
        }
    }
}

fn hrefs(body: &str) -> Vec<Href> {
    let mut finder = LinkFinder::new();
    finder.kinds(&[LinkKind::Url]);
    let mut hrefs: Vec<Href> = Vec::new();
    for link in finder.links(body) {
        let text = link.as_str();
        if let Ok(url) = Url::parse(text)
            && matches!(url.scheme(), "http" | "https")
            && !hrefs.iter().any(|h| h.text == text)
        {
            hrefs.push(Href {
                text: text.to_owned(),
                url: url.into(),
            });
        }
    }
    hrefs
}

fn locate(rendered: &Buffer, hrefs: &[Href], skip: u16, inner: Rect) -> Vec<Link> {
    let area = rendered.area;
    let mut text = String::new();
    let mut cells = Vec::new();
    for y in 0..area.height {
        let row: Vec<_> = (0..area.width)
            .map(|x| (x, rendered[(x, y)].symbol()))
            .collect();
        let end = row
            .iter()
            .rposition(|(_, s)| *s != " ")
            .map_or(0, |i| i + 1);
        for &(x, symbol) in &row[..end] {
            cells.push((text.len(), Position::new(x, y)));
            text.push_str(symbol);
        }
    }
    hrefs
        .iter()
        .map(|href| Link {
            url: href.url.clone(),
            cells: text
                .match_indices(&href.text)
                .flat_map(|(at, found)| {
                    let from = cells.partition_point(|(b, _)| *b < at);
                    let to = cells.partition_point(|(b, _)| *b < at + found.len());
                    &cells[from..to]
                })
                .filter(|(_, p)| p.y >= skip && p.y - skip < inner.height)
                .map(|(_, p)| Position::new(inner.x + p.x, inner.y + p.y - skip))
                .collect(),
        })
        .filter(|link| !link.cells.is_empty())
        .collect()
}

fn draw_menu(frame: &mut Frame, area: Rect, hrefs: &[Href], menu: &mut ListState) {
    let width = area.width.saturating_sub(4);
    let height = (hrefs.len().max(1) as u16 + 2).min(area.height.saturating_sub(2));
    let rect = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    let block = theme::block().title(" links ");
    let inner = block.inner(rect);
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);
    if hrefs.is_empty() {
        frame.render_widget(Line::from("no links").fg(theme::SUBTLE), inner);
        return;
    }
    frame.render_stateful_widget(
        List::new(hrefs.iter().map(|h| h.text.as_str())).highlight_style(theme::SELECTED),
        inner,
        menu,
    );
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
        attachments: Vec::new(),
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
        attachments: message
            .attachments
            .iter()
            .cloned()
            .map(Attached::Mail)
            .collect(),
    }
}
