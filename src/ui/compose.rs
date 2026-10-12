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
    text::{Line, Span},
    widgets::{Clear, List, ListItem, ListState, Paragraph},
};
use ratatui_textarea::TextArea;

use super::{
    Action, Ctx, Load, Mode, View, attachments::size, cycle, failure, keys, line_input, list_nav,
    pick, status, theme, yank,
};
use crate::{
    app::KeyEvent,
    error::{InputError, UiError},
    files::StoredFile,
    mail::Attachment,
    task::Task,
};

const COOLDOWN: Duration = Duration::from_secs(2);
const OCTET_STREAM: &str = "application/octet-stream";

#[derive(Clone, PartialEq, Eq)]
pub enum Attached {
    File(StoredFile),
    Mail(Attachment),
}

impl Attached {
    fn name(&self) -> &str {
        match self {
            Attached::File(f) => &f.name,
            Attached::Mail(a) => &a.name,
        }
    }

    fn size(&self) -> u64 {
        match self {
            Attached::File(f) => f.size,
            Attached::Mail(a) => a.size,
        }
    }
}

#[derive(Default)]
pub struct Draft {
    pub from: Option<String>,
    pub to: String,
    pub subject: String,
    pub body: String,
    pub attachments: Vec<Attached>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    From,
    To,
    Subject,
    Attachments,
    Body,
}

impl Field {
    const ALL: [Field; 5] = [
        Field::From,
        Field::To,
        Field::Subject,
        Field::Attachments,
        Field::Body,
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

    fn editable(self) -> bool {
        matches!(self, Field::To | Field::Subject | Field::Body)
    }
}

struct Picker {
    files: Load<Vec<StoredFile>>,
    list: ListState,
}

pub struct Compose {
    addresses: Load<Vec<String>>,
    from: Option<String>,
    to: Box<TextArea<'static>>,
    subject: Box<TextArea<'static>>,
    body: Box<TextArea<'static>>,
    attachments: Vec<Attached>,
    attachment: usize,
    picker: Option<Picker>,
    focus: Field,
    mode: Mode,
    sending: Load<()>,
    port: u16,
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
            attachments: draft.attachments,
            attachment: 0,
            picker: None,
            focus,
            mode: Mode::Normal,
            sending: Task::new(),
            port: state.server.listen.port(),
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

    fn option(&mut self, forward: bool) {
        match self.focus {
            Field::From => self.cycle_sender(forward),
            Field::Attachments => {
                let last = self.attachments.len().saturating_sub(1);
                self.attachment = if forward {
                    (self.attachment + 1).min(last)
                } else {
                    self.attachment.saturating_sub(1)
                };
            }
            Field::To | Field::Subject | Field::Body => {}
        }
    }

    fn open_picker(&mut self, ctx: &Ctx) {
        let (state, user) = (ctx.state, ctx.user);
        self.picker = Some(Picker {
            files: Task::new_spawn(async move {
                state.files.list(user).await.or_raise(|| UiError::LoadFiles)
            }),
            list: ListState::default().with_selected(Some(0)),
        });
    }

    fn pick_file(&mut self) {
        let Some(picker) = self.picker.take() else {
            return;
        };
        let (Task::Done(Ok(files)), Some(i)) = (&picker.files, picker.list.selected()) else {
            return;
        };
        if let Some(file) = available(&self.attachments, files).get(i) {
            self.attachments.push(Attached::File(file.clone()));
            self.attachment = self.attachments.len() - 1;
        }
    }

    fn picker_input(&mut self, key: KeyEvent) {
        let Some(picker) = &mut self.picker else {
            return;
        };
        if list_nav(&mut picker.list, &key) {
            return;
        }
        match keys::Picker::get(&key) {
            Some(keys::Picker::Pick) => self.pick_file(),
            Some(keys::Picker::Cancel) => self.picker = None,
            None => {}
        }
    }

    fn detach(&mut self) {
        if self.attachment < self.attachments.len() {
            self.attachments.remove(self.attachment);
            self.attachment = self
                .attachment
                .min(self.attachments.len().saturating_sub(1));
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
        let (state, user, mail) = (ctx.state, ctx.user, ctx.mail.clone());
        let name = ctx.user.name.to_string();
        let subject = self.subject.lines().join(" ");
        let body = self.body.lines().join("\n");
        let attachments = self.attachments.clone();
        self.mode = Mode::Normal;
        ctx.sent_at.set(Some(now));
        self.sending.spawn(async move {
            let mut parts = Vec::new();
            let mut uploaded = Vec::new();
            for attached in attachments {
                match attached {
                    Attached::Mail(attachment) => parts.push(attachment),
                    Attached::File(file) => {
                        let bytes = state
                            .files
                            .read(user, &file.name)
                            .await
                            .or_raise(|| UiError::Attach(file.name.clone()))?;
                        let kind = mime_guess::from_path(&file.name)
                            .first_raw()
                            .unwrap_or(OCTET_STREAM);
                        let part = mail
                            .upload(&file.name, kind, bytes)
                            .await
                            .or_raise(|| UiError::Attach(file.name.clone()))?;
                        parts.push(part);
                        uploaded.push(file.name);
                    }
                }
            }
            mail.send(&name, &from, &to, &subject, &body, &parts)
                .await
                .or_raise(|| UiError::SendMessage)?;
            state
                .files
                .remove(user, &uploaded)
                .await
                .or_raise(|| UiError::RemoveSent)
        });
    }

    fn text(&self) -> String {
        match self.focus {
            Field::From => self.sender().cloned().unwrap_or_default(),
            Field::To => self.to.lines().join("\n"),
            Field::Subject => self.subject.lines().join("\n"),
            Field::Attachments => self
                .attachments
                .get(self.attachment)
                .map(|a| a.name().to_owned())
                .unwrap_or_default(),
            Field::Body => self.body.lines().join("\n"),
        }
    }

    fn field(&mut self) -> Option<&mut TextArea<'static>> {
        match self.focus {
            Field::From | Field::Attachments => None,
            Field::To => Some(&mut self.to),
            Field::Subject => Some(&mut self.subject),
            Field::Body => Some(&mut self.body),
        }
    }

    fn attachments_line(&self) -> Line<'static> {
        if self.attachments.is_empty() {
            let hint = match self.focus {
                Field::Attachments => "none · a to attach",
                _ => "none",
            };
            return Line::from(hint).fg(theme::SUBTLE);
        }
        let mut spans = Vec::new();
        for (i, attached) in self.attachments.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(" · ").fg(theme::SUBTLE));
            }
            let span = Span::raw(format!("{} {}", attached.name(), size(attached.size())));
            spans.push(
                if self.focus == Field::Attachments && i == self.attachment {
                    span.style(theme::SELECTED)
                } else {
                    span
                },
            );
        }
        Line::from(spans)
    }

    fn draw_picker(&mut self, frame: &mut Frame, area: Rect) {
        let Some(picker) = &mut self.picker else {
            return;
        };
        let available = match picker.files.poll() {
            Task::Done(Ok(files)) => Some(available(&self.attachments, files)),
            _ => None,
        };
        let width = 60.min(area.width);
        let height = 14.min(area.height);
        let rect = Rect {
            x: area.x + (area.width - width) / 2,
            y: area.y + (area.height - height) / 2,
            width,
            height,
        };
        let block = theme::block().title(" attach file ");
        let inner = block.inner(rect);
        frame.render_widget(Clear, rect);
        frame.render_widget(block, rect);
        match available {
            Some(files) if files.is_empty() => frame.render_widget(
                Paragraph::new(vec![
                    Line::from("no files to attach"),
                    Line::from(""),
                    Line::from("upload with:").fg(theme::SUBTLE),
                    Line::from(format!("scp -P {} <file> <host>:", self.port)).fg(theme::SUBTLE),
                ]),
                inner,
            ),
            Some(files) => {
                let items: Vec<ListItem> = files
                    .iter()
                    .map(|f| ListItem::new(format!("{} {}", f.name, size(f.size))))
                    .collect();
                frame.render_stateful_widget(
                    List::new(items).highlight_style(theme::SELECTED),
                    inner,
                    &mut picker.list,
                );
            }
            None => status(frame, inner, &picker.files),
        }
    }
}

impl View for Compose {
    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let [from, to, subject, attachments, body, status] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .areas(area);

        if matches!(self.sending.poll(), Task::Done(Ok(()))) {
            self.attachments.clear();
        }

        let (focus, mode) = (self.focus, self.mode);
        let block = |title: String, field: Field| {
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
            Paragraph::new(sender).block(block(" From ".into(), Field::From)),
            from,
        );

        let total: u64 = self.attachments.iter().map(Attached::size).sum();
        let title = match total {
            0 => " Attachments ".to_owned(),
            total => format!(" Attachments · {} ", size(total)),
        };
        frame.render_widget(
            Paragraph::new(self.attachments_line()).block(block(title, Field::Attachments)),
            attachments,
        );

        for (area, field, title, rect) in [
            (&mut self.to, Field::To, " To ", to),
            (&mut self.subject, Field::Subject, " Subject ", subject),
            (&mut self.body, Field::Body, " Body ", body),
        ] {
            area.set_block(block(title.into(), field));
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

        self.draw_picker(frame, area);
    }

    fn input(&mut self, key: KeyEvent, ctx: &Ctx) -> Action {
        if self.picker.is_some() {
            self.picker_input(key);
            return Action::None;
        }
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
            Some(keys::Fields::Left) if !self.focus.editable() => {
                self.option(false);
                return Action::None;
            }
            Some(keys::Fields::Right) if !self.focus.editable() => {
                self.option(true);
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
            Mode::Normal => {
                if self.focus == Field::Attachments {
                    match keys::Files::get(&key) {
                        Some(keys::Files::Attach) => {
                            self.open_picker(ctx);
                            return Action::None;
                        }
                        Some(keys::Files::Detach) => {
                            self.detach();
                            return Action::None;
                        }
                        None => {}
                    }
                }
                match keys::Edit::get(&key) {
                    Some(keys::Edit::Back) => return Action::Pop,
                    Some(keys::Edit::Insert) if self.focus.editable() => self.mode = Mode::Insert,
                    Some(keys::Edit::Down) => self.focus = self.focus.step(true),
                    Some(keys::Edit::Up) => self.focus = self.focus.step(false),
                    Some(keys::Edit::Left) => self.option(false),
                    Some(keys::Edit::Right) => self.option(true),
                    Some(keys::Edit::Yank) => return yank(&self.text()),
                    Some(keys::Edit::Insert) | None => {}
                }
            }
        }
        Action::None
    }

    fn help(&self) -> keys::Help {
        match (self.mode, self.focus, &self.picker) {
            (_, _, Some(_)) => &[keys::Picker::KEYS],
            (Mode::Normal, Field::Attachments, None) => {
                &[keys::Files::KEYS, keys::Edit::KEYS, keys::Fields::KEYS]
            }
            (Mode::Normal, _, None) => &[keys::Edit::KEYS, keys::Fields::KEYS],
            (Mode::Insert, _, None) => &[keys::Insert::KEYS, keys::Fields::KEYS],
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

fn available(attachments: &[Attached], files: &[StoredFile]) -> Vec<StoredFile> {
    files
        .iter()
        .filter(|f| !attachments.contains(&Attached::File((*f).clone())))
        .cloned()
        .collect()
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
