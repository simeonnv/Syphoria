use std::{collections::HashMap, sync::Arc};

use bytesize::ByteSize;
use exn::ResultExt;
use image::imageops::FilterType;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Stylize,
    text::Line,
    widgets::{Block, Paragraph, Row, Table, TableState, Wrap},
};
use ratatui_image::{Resize, StatefulImage, picker::Picker, protocol::StatefulProtocol};

use super::{Action, Ctx, Load, PAGE, View, failure, keys, status, table_nav, theme};
use crate::{
    app::KeyEvent,
    error::UiError,
    mail::{Attachment, Mail},
    task::Task,
};

const MAX_PIXELS: u32 = 1600;
const AUTO_PREVIEW: u64 = 10_000_000;
const SIDE_BY_SIDE: u16 = 100;

pub fn size(bytes: u64) -> String {
    ByteSize(bytes).display().si().to_string()
}

fn save(ctx: &Ctx, attachment: &Attachment) -> Load<String> {
    let (state, user, mail) = (ctx.state, ctx.user, ctx.mail.clone());
    let attachment = attachment.clone();
    Task::new_spawn(async move {
        let bytes = mail
            .download(&attachment.blob_id)
            .await
            .or_raise(|| UiError::LoadAttachment)?;
        state
            .files
            .save(user, &attachment.name, &bytes)
            .await
            .or_raise(|| UiError::SaveAttachment)
    })
}

fn save_status(saving: &mut Load<String>, port: u16) -> Option<Line<'static>> {
    Some(match saving.poll() {
        Task::Unexecuted => return None,
        Task::Running(_) => Line::from(" saving… ").fg(theme::SUBTLE),
        Task::Done(Ok(name)) => Line::from(format!(
            " saved to your files · download: scp -P {port} <host>:'{name}' . "
        ))
        .green(),
        Task::Done(Err(e)) => Line::from(format!(" {} ", failure(e))).red(),
        Task::Panicked(e) => Line::from(format!(" error: {e} ")).red(),
    })
}

fn saved(block: Block<'static>, saving: &mut Load<String>, port: u16) -> Block<'static> {
    match save_status(saving, port) {
        Some(line) => block.title_bottom(line),
        None => block,
    }
}

pub enum Content {
    Image(Box<StatefulProtocol>),
    Text(String),
    Binary,
}

fn load(mail: Arc<Mail>, attachment: &Attachment) -> Load<Content> {
    let (blob_id, kind) = (attachment.blob_id.clone(), attachment.kind.clone());
    Task::new_spawn(async move {
        let bytes = mail
            .download(&blob_id)
            .await
            .or_raise(|| UiError::LoadAttachment)?;
        tokio::task::spawn_blocking(move || decode(&kind, bytes))
            .await
            .or_raise(|| UiError::LoadAttachment)?
    })
}

fn decode(kind: &str, bytes: Vec<u8>) -> exn::Result<Content, UiError> {
    if kind.starts_with("image/") || image::guess_format(&bytes).is_ok() {
        let image = image::load_from_memory(&bytes)
            .or_raise(|| UiError::LoadAttachment)?
            .thumbnail(MAX_PIXELS, MAX_PIXELS);
        return Ok(Content::Image(Box::new(
            Picker::halfblocks().new_resize_protocol(image),
        )));
    }
    Ok(match String::from_utf8(bytes) {
        Ok(text) => Content::Text(text),
        Err(_) => Content::Binary,
    })
}

fn render(frame: &mut Frame, area: Rect, content: &mut Load<Content>, scroll: u16, hint: &str) {
    let content = match content {
        Task::Done(Ok(content)) => content,
        content => {
            status(frame, area, content.poll());
            return;
        }
    };
    match content {
        Content::Image(protocol) => frame.render_stateful_widget(
            StatefulImage::default().resize(Resize::Fit(Some(FilterType::Triangle))),
            area,
            &mut **protocol,
        ),
        Content::Text(text) => frame.render_widget(
            Paragraph::new(text.as_str())
                .wrap(Wrap { trim: false })
                .scroll((scroll, 0)),
            area,
        ),
        Content::Binary => frame.render_widget(
            Paragraph::new(vec![
                Line::from("no preview for this file type"),
                Line::from(""),
                Line::from(hint.to_owned()).fg(theme::SUBTLE),
            ])
            .wrap(Wrap { trim: false }),
            area,
        ),
    }
}

pub struct Attachments {
    list: Vec<Attachment>,
    table: TableState,
    previews: HashMap<String, Load<Content>>,
    saving: Load<String>,
    port: u16,
}

impl Attachments {
    pub fn new(ctx: &Ctx, list: Vec<Attachment>) -> Self {
        Self {
            list,
            table: TableState::default().with_selected(0),
            previews: HashMap::new(),
            saving: Task::new(),
            port: ctx.state.server.listen.port(),
        }
    }

    fn selected(&self) -> Option<&Attachment> {
        self.table.selected().and_then(|i| self.list.get(i))
    }

    fn draw_preview(&mut self, frame: &mut Frame, area: Rect) {
        let Some(attachment) = self.table.selected().and_then(|i| self.list.get(i)) else {
            frame.render_widget(theme::block(), area);
            return;
        };
        let block =
            theme::block().title(format!(" {} · {} ", attachment.name, size(attachment.size)));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        match self.previews.get_mut(&attachment.blob_id) {
            Some(content) => render(frame, inner, content, 0, "s saves it to your files"),
            None => frame.render_widget(
                Paragraph::new(vec![
                    Line::from("too large for a quick preview"),
                    Line::from(""),
                    Line::from("enter opens the full preview").fg(theme::SUBTLE),
                ])
                .wrap(Wrap { trim: false }),
                inner,
            ),
        }
    }
}

impl View for Attachments {
    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let area = match save_status(&mut self.saving, self.port) {
            Some(line) => {
                let [body, footer] =
                    Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
                frame.render_widget(line, footer);
                body
            }
            None => area,
        };
        let [list, preview] = if area.width >= SIDE_BY_SIDE {
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(area)
        } else {
            Layout::vertical([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(area)
        };

        let rows = self
            .list
            .iter()
            .map(|a| Row::new([a.name.clone(), a.kind.clone(), size(a.size)]));
        let block = theme::block().title(" Attachments ");
        let table = Table::new(
            rows,
            [
                Constraint::Fill(1),
                Constraint::Length(18),
                Constraint::Length(9),
            ],
        )
        .header(Row::new(["name", "type", "size"]).fg(theme::SUBTLE))
        .row_highlight_style(theme::SELECTED)
        .block(block);
        frame.render_stateful_widget(table, list, &mut self.table);

        self.draw_preview(frame, preview);
    }

    fn input(&mut self, key: KeyEvent, ctx: &Ctx) -> Action {
        if table_nav(&mut self.table, &key) {
            return Action::None;
        }
        match keys::Attachments::get(&key) {
            Some(keys::Attachments::Open) => match self.selected() {
                Some(a) => Action::Push(Box::new(Preview::new(ctx, a.clone()).into())),
                None => Action::None,
            },
            Some(keys::Attachments::Save) => {
                if let Some(a) = self.selected()
                    && !matches!(self.saving, Task::Running(_))
                {
                    self.saving = save(ctx, a);
                }
                Action::None
            }
            Some(keys::Attachments::Back) => Action::Pop,
            None => Action::None,
        }
    }

    fn update(&mut self, ctx: &Ctx) -> Action {
        if let Some(attachment) = self.selected()
            && attachment.size <= AUTO_PREVIEW
            && !self.previews.contains_key(&attachment.blob_id)
        {
            let content = load(ctx.mail.clone(), attachment);
            self.previews.insert(attachment.blob_id.clone(), content);
        }
        Action::None
    }

    fn help(&self) -> keys::Help {
        &[keys::Attachments::KEYS]
    }
}

pub struct Preview {
    attachment: Attachment,
    content: Load<Content>,
    scroll: u16,
    saving: Load<String>,
    port: u16,
}

impl Preview {
    pub fn new(ctx: &Ctx, attachment: Attachment) -> Self {
        Self {
            content: load(ctx.mail.clone(), &attachment),
            attachment,
            scroll: 0,
            saving: Task::new(),
            port: ctx.state.server.listen.port(),
        }
    }
}

impl View for Preview {
    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let title = format!(
            " {} · {} · {} ",
            self.attachment.name,
            self.attachment.kind,
            size(self.attachment.size)
        );
        let block = saved(theme::block().title(title), &mut self.saving, self.port);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        render(
            frame,
            inner,
            &mut self.content,
            self.scroll,
            "s saves it to your files, the footer shows how to download it",
        );
    }

    fn input(&mut self, key: KeyEvent, ctx: &Ctx) -> Action {
        if let Some(nav) = keys::Nav::get(&key) {
            self.scroll = match nav {
                keys::Nav::Down => self.scroll.saturating_add(1),
                keys::Nav::Up => self.scroll.saturating_sub(1),
                keys::Nav::PageDown => self.scroll.saturating_add(PAGE),
                keys::Nav::PageUp => self.scroll.saturating_sub(PAGE),
                keys::Nav::Top => 0,
                keys::Nav::Bottom => match &self.content {
                    Task::Done(Ok(Content::Text(text))) => text.lines().count() as u16,
                    _ => 0,
                },
            };
            return Action::None;
        }
        match keys::Preview::get(&key) {
            Some(keys::Preview::Save) => {
                if !matches!(self.saving, Task::Running(_)) {
                    self.saving = save(ctx, &self.attachment);
                }
                Action::None
            }
            Some(keys::Preview::Back) => Action::Pop,
            None => Action::None,
        }
    }

    fn help(&self) -> keys::Help {
        &[keys::Preview::KEYS]
    }
}
