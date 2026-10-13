use std::{
    mem,
    time::{Duration, Instant},
};

use exn::{Exn, ResultExt};
use jmap_client::mailbox::Role;
use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Style, Stylize},
    text::Line,
    widgets::{Row, Table, TableState},
};
use ratatui_textarea::TextArea;
use unicode_width::UnicodeWidthStr;

use super::{
    Action, Ctx, Load, Mode, Op, Reader, View, confirm_popup, date, emails, failure, keys,
    keys_popup, line_input, move_target, names, spam_target, suggest, suggestions_popup, table_nav,
    theme, yank,
};
use crate::{
    app::KeyEvent,
    error::{InputError, UiError},
    mail::{Folder, Found, Summary, Term},
    task::Task,
};

const DEBOUNCE: Duration = Duration::from_millis(250);
const BATCH: usize = 100;
const PRELOAD: usize = 10;
const HINT: &str = " me: with: from: to: subject: in: unread read ";

enum Pending {
    Move,
    Destroy(String),
}

pub struct Search {
    input: Box<TextArea<'static>>,
    typing: bool,
    folders: Vec<Folder>,
    mine: Load<Vec<String>>,
    others: Load<Vec<String>>,
    terms: Option<Vec<Term>>,
    edited: Option<Instant>,
    results: Vec<Summary>,
    total: usize,
    fetch: Load<(usize, Found)>,
    table: TableState,
    choice: usize,
    pending: Option<Pending>,
    error: Option<Exn<InputError>>,
    matcher: Matcher,
}

impl Search {
    pub fn new(ctx: &Ctx, folders: Vec<Folder>) -> Self {
        let (state, user) = (ctx.state, ctx.user);
        let mail = ctx.mail.clone();
        let mut page = Self {
            input: line_input(String::new()),
            typing: true,
            folders,
            mine: Task::new_spawn(async move {
                state
                    .stalwart
                    .addresses(user)
                    .await
                    .or_raise(|| UiError::LoadAddresses)
            }),
            others: Task::new_spawn(async move {
                let list = mail
                    .correspondents()
                    .await
                    .or_raise(|| UiError::LoadCorrespondents)?;
                Ok(list.into_iter().filter(|a| !user.owns(a)).collect())
            }),
            terms: None,
            edited: None,
            results: Vec::new(),
            total: 0,
            fetch: Task::new(),
            table: TableState::default().with_selected(0),
            choice: 0,
            pending: None,
            error: None,
            matcher: Matcher::new(Config::DEFAULT),
        };
        page.run(ctx);
        page
    }

    pub fn focus(&mut self) {
        self.typing = true;
    }

    fn line(&self) -> String {
        self.input.lines().join(" ")
    }

    fn run(&mut self, ctx: &Ctx) {
        let line = self.line();
        let (tokens, _) = split(&line);
        let terms = match tokens
            .into_iter()
            .filter_map(|token| parse(token, &self.folders).transpose())
            .collect::<exn::Result<Vec<_>, InputError>>()
        {
            Ok(terms) => terms,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        if self.terms.as_ref() == Some(&terms) {
            return;
        }
        self.terms = Some(terms);
        self.table.select_first();
        self.fetch(ctx, 0, None);
    }

    fn fetch(&mut self, ctx: &Ctx, position: usize, op: Option<(String, Op)>) {
        let Some(terms) = self.terms.clone() else {
            return;
        };
        let mail = ctx.mail.clone();
        self.fetch.spawn(async move {
            if let Some((id, op)) = op {
                op.run(&mail, &id).await?;
            }
            let found = mail
                .query(&terms, position, BATCH)
                .await
                .or_raise(|| UiError::Search)?;
            Ok((position, found))
        });
    }

    fn more(&mut self, ctx: &Ctx) {
        let shown = self.visible().len();
        let near_end = self.table.selected().is_some_and(|i| i + PRELOAD >= shown);
        if near_end && self.results.len() < self.total && !matches!(self.fetch, Task::Running(_)) {
            self.fetch(ctx, self.results.len(), None);
        }
    }

    fn needle(&self) -> Option<String> {
        let line = self.line();
        let pending = split(&line).1?;
        (key(pending).is_none() && flag(pending).is_none()).then(|| pending.to_owned())
    }

    fn visible(&mut self) -> Vec<usize> {
        let Some(needle) = self.needle() else {
            return (0..self.results.len()).collect();
        };
        let pattern = Pattern::parse(&needle, CaseMatching::Ignore, Normalization::Smart);
        let mut buf = Vec::new();
        self.results
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                let haystack = format!(
                    "{} {} {} {}",
                    m.subject,
                    names(&m.from),
                    emails(&m.from),
                    emails(&m.to)
                );
                pattern
                    .score(Utf32Str::new(&haystack, &mut buf), &mut self.matcher)
                    .is_some()
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn selected(&mut self) -> Option<usize> {
        let visible = self.visible();
        self.table.selected().and_then(|i| visible.get(i).copied())
    }

    fn suggestions(&mut self) -> Vec<String> {
        let line = self.line();
        let Some((key, value)) = split(&line).1.and_then(key) else {
            return Vec::new();
        };
        let loaded = |task: &Load<Vec<String>>| match task {
            Task::Done(Ok(list)) => list.clone(),
            _ => Vec::new(),
        };
        let candidates = match key {
            "me" => loaded(&self.mine),
            "with" => loaded(&self.others),
            "from" | "to" => [loaded(&self.mine), loaded(&self.others)].concat(),
            "in" => self.folders.iter().flat_map(labels).collect(),
            _ => return Vec::new(),
        };
        suggest(&mut self.matcher, value, candidates)
    }

    fn accept(&mut self, choice: &str) {
        let line = self.line();
        let Some(pending) = split(&line).1 else {
            return;
        };
        let Some((key, _)) = key(pending) else {
            return;
        };
        let start = line.len() - pending.len();
        self.input = line_input(format!("{}{key}:{choice} ", &line[..start]));
        self.edited();
    }

    fn edited(&mut self) {
        self.edited = Some(Instant::now());
        self.error = None;
        self.choice = 0;
        self.table.select_first();
    }

    fn role(&self, summary: &Summary) -> Role {
        self.folders
            .iter()
            .find(|f| Some(&f.id) == summary.mailbox.as_ref())
            .map_or(Role::None, |f| f.role.clone())
    }

    fn folder(&self, summary: &Summary) -> String {
        self.folders
            .iter()
            .find(|f| Some(&f.id) == summary.mailbox.as_ref())
            .map(|f| f.name.clone())
            .unwrap_or_default()
    }

    fn move_selected(&mut self, ctx: &Ctx, to: Role) {
        let Some(i) = self.selected() else {
            return;
        };
        let from = self.role(&self.results[i]);
        if from != to {
            let id = self.results[i].id.clone();
            self.fetch(ctx, 0, Some((id, Op::Move { from, to })));
        }
    }

    fn run_selected(&mut self, ctx: &Ctx, op: Op) {
        if let Some(i) = self.selected() {
            let id = self.results[i].id.clone();
            self.fetch(ctx, 0, Some((id, op)));
        }
    }

    fn typing(&mut self, key: KeyEvent, ctx: &Ctx) {
        let suggestions = self.suggestions();
        let len = suggestions.len().max(1);
        match keys::SearchInput::get(&key) {
            Some(keys::SearchInput::Results) => self.typing = false,
            Some(keys::SearchInput::Run) => {
                match suggestions.get(self.choice) {
                    Some(choice) => self.accept(choice),
                    None if !self.line().ends_with(' ') && !self.line().is_empty() => {
                        self.input = line_input(format!("{} ", self.line()));
                    }
                    None => {}
                }
                self.typing = false;
                self.edited = None;
                self.run(ctx);
            }
            Some(keys::SearchInput::Complete) => {
                if let Some(choice) = suggestions.get(self.choice) {
                    self.accept(choice);
                }
            }
            Some(keys::SearchInput::Next) => self.choice = (self.choice + 1) % len,
            Some(keys::SearchInput::Prev) => self.choice = (self.choice + len - 1) % len,
            None => {
                if self.input.input(key) {
                    self.edited();
                }
            }
        }
    }

    fn mail(&mut self, cmd: keys::Mail, ctx: &Ctx) -> Action {
        match cmd {
            keys::Mail::Back => return Action::Pop,
            keys::Mail::Trash => self.move_selected(ctx, Role::Trash),
            keys::Mail::Archive => self.move_selected(ctx, Role::Archive),
            keys::Mail::Spam => {
                if let Some(i) = self.selected() {
                    let to = spam_target(&self.role(&self.results[i]));
                    self.move_selected(ctx, to);
                }
            }
            keys::Mail::Delete => {
                if let Some(i) = self.selected() {
                    self.pending = Some(Pending::Destroy(self.results[i].id.clone()));
                }
            }
            keys::Mail::Move => self.pending = Some(Pending::Move),
            keys::Mail::Unread => self.run_selected(ctx, Op::Seen(false)),
            keys::Mail::YankSender => {
                if let Some(i) = self.selected() {
                    return yank(&emails(&self.results[i].from));
                }
            }
            keys::Mail::YankSubject => {
                if let Some(i) = self.selected() {
                    return yank(&self.results[i].subject);
                }
            }
        }
        Action::None
    }

    fn draw_suggestions(&mut self, frame: &mut Frame, input: Rect) {
        let suggestions = self.suggestions();
        let line = self.line();
        let offset = split(&line)
            .1
            .and_then(|pending| {
                let (key, _) = key(pending)?;
                Some(line.width() - pending.width() + key.len() + 1)
            })
            .unwrap_or(0) as u16;
        suggestions_popup(
            frame,
            input.x + offset,
            input.bottom(),
            &suggestions,
            self.choice,
        );
    }
}

impl View for Search {
    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let [input, results] =
            Layout::vertical([Constraint::Length(3), Constraint::Fill(1)]).areas(area);

        let mut block = theme::block().title(" search ").title_bottom(HINT);
        block = match (&self.error, self.typing) {
            (Some(error), _) => block
                .title_bottom(Line::from(format!(" {} ", failure(error))).red())
                .border_style(Style::new().red()),
            (None, true) => block.border_style(Style::new().green()),
            (None, false) => block,
        };
        for task in [&self.mine, &self.others] {
            if let Task::Done(Err(e)) = task {
                block = block.title_bottom(Line::from(format!(" {} ", failure(e))).red());
            }
        }
        self.input.set_block(block);
        self.input.set_cursor_style(if self.typing {
            Style::new().reversed()
        } else {
            Style::new()
        });
        frame.render_widget(&*self.input, input);

        let visible = self.visible();
        let mut block = theme::block().title(format!(
            " {} shown · {} of {} loaded ",
            visible.len(),
            self.results.len(),
            self.total
        ));
        match self.fetch.poll() {
            Task::Running(_) => {
                block = block.title_bottom(Line::from(" searching… ").fg(theme::SUBTLE));
            }
            Task::Done(Err(e)) => {
                block = block.title_bottom(Line::from(format!(" {} ", failure(e))).red());
            }
            Task::Panicked(e) => {
                block = block.title_bottom(Line::from(format!(" error: {e} ")).red());
            }
            Task::Done(Ok(_)) | Task::Unexecuted => {}
        }
        if self
            .table
            .selected()
            .is_some_and(|i| i >= visible.len() && !visible.is_empty())
        {
            self.table.select_last();
        }
        let rows = visible.iter().map(|&i| {
            let m = &self.results[i];
            let row = Row::new([
                date(m.received),
                self.folder(m),
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
                Constraint::Length(14),
                Constraint::Length(24),
                Constraint::Length(28),
                Constraint::Fill(1),
            ],
        )
        .header(Row::new(["date", "folder", "from", "to", "subject"]).style(theme::HEADER))
        .row_highlight_style(if self.typing {
            Style::new()
        } else {
            theme::SELECTED
        })
        .block(block);
        frame.render_stateful_widget(table, results, &mut self.table);

        if self.typing {
            self.draw_suggestions(frame, input);
        }
        match self.pending {
            Some(Pending::Move) => keys_popup(frame, area, " move to ", &[keys::MoveTo::KEYS]),
            Some(Pending::Destroy(_)) => confirm_popup(frame, area, " delete permanently? "),
            None => {}
        }
    }

    fn input(&mut self, key: KeyEvent, ctx: &Ctx) -> Action {
        if self.typing {
            self.typing(key, ctx);
            return Action::None;
        }
        if let Some(pending) = self.pending.take() {
            match pending {
                Pending::Move => {
                    if let Some(to) = move_target(&key) {
                        self.move_selected(ctx, to);
                    }
                }
                Pending::Destroy(id) => {
                    if keys::Confirm::get(&key) == Some(keys::Confirm::Yes) {
                        self.fetch(ctx, 0, Some((id, Op::Destroy)));
                    }
                }
            }
            return Action::None;
        }
        if table_nav(&mut self.table, &key) {
            self.more(ctx);
            return Action::None;
        }
        if let Some(cmd) = keys::Mail::get(&key) {
            return self.mail(cmd, ctx);
        }
        match keys::SearchResults::get(&key) {
            Some(keys::SearchResults::Open) => {
                if let Some(i) = self.selected() {
                    let role = self.role(&self.results[i]);
                    let message = &mut self.results[i];
                    message.seen = true;
                    return Action::Push(Box::new(
                        Reader::new(ctx, message.id.clone(), role).into(),
                    ));
                }
            }
            Some(keys::SearchResults::Read) => self.run_selected(ctx, Op::Seen(true)),
            Some(keys::SearchResults::Reload) => self.fetch(ctx, 0, None),
            None => {}
        }
        Action::None
    }

    fn help(&self) -> keys::Help {
        if self.typing {
            &[keys::SearchInput::KEYS]
        } else {
            &[keys::SearchResults::KEYS, keys::Mail::KEYS]
        }
    }

    fn mode(&self) -> Mode {
        if self.typing {
            Mode::Insert
        } else {
            Mode::Normal
        }
    }

    fn update(&mut self, ctx: &Ctx) -> Action {
        self.mine.poll();
        self.others.poll();
        if self.edited.is_some_and(|t| t.elapsed() >= DEBOUNCE) {
            self.edited = None;
            self.run(ctx);
        }
        if matches!(self.fetch.poll(), Task::Done(Ok(_)))
            && let Task::Done(Ok((position, found))) = mem::take(&mut self.fetch)
        {
            if position == 0 {
                self.results = found.summaries;
            } else {
                self.results.extend(found.summaries);
            }
            self.total = found.total;
        }
        Action::None
    }

    fn resume(&mut self, ctx: &Ctx) -> Action {
        self.fetch(ctx, 0, None);
        Action::None
    }
}

fn split(line: &str) -> (Vec<&str>, Option<&str>) {
    let mut tokens: Vec<&str> = line.split_whitespace().collect();
    let pending = if line.ends_with(char::is_whitespace) {
        None
    } else {
        tokens.pop()
    };
    (tokens, pending)
}

fn key(token: &str) -> Option<(&str, &str)> {
    token
        .split_once(':')
        .filter(|(key, _)| matches!(*key, "me" | "with" | "from" | "to" | "subject" | "in"))
}

fn flag(token: &str) -> Option<Term> {
    match token.to_lowercase().as_str() {
        "unread" => Some(Term::Unread),
        "read" => Some(Term::Read),
        _ => None,
    }
}

fn parse(token: &str, folders: &[Folder]) -> exn::Result<Option<Term>, InputError> {
    let Some((key, value)) = key(token) else {
        return Ok(Some(
            flag(token).unwrap_or_else(|| Term::Text(token.to_owned())),
        ));
    };
    if value.is_empty() {
        return Ok(None);
    }
    let value = value.to_owned();
    Ok(Some(match key {
        "me" => Term::Me(value),
        "with" => Term::With(value),
        "from" => Term::From(value),
        "to" => Term::To(value),
        "subject" => Term::Subject(value),
        _ => {
            let folder = folders
                .iter()
                .find(|f| labels(f).contains(&value.to_lowercase()))
                .ok_or_else(|| Exn::new(InputError::UnknownFolder(value)))?;
            Term::In(folder.id.clone())
        }
    }))
}

fn labels(folder: &Folder) -> Vec<String> {
    let role = match folder.role {
        Role::Inbox => "inbox",
        Role::Drafts => "drafts",
        Role::Sent => "sent",
        Role::Archive => "archive",
        Role::Junk => "spam",
        Role::Trash => "trash",
        _ => "",
    };
    let name = folder.name.to_lowercase().replace(char::is_whitespace, "-");
    if role.is_empty() || role == name {
        vec![name]
    } else {
        vec![role.to_owned(), name]
    }
}
