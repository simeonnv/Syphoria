use exn::{ErrorExt, Exn, ResultExt};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Style, Stylize},
    text::Line,
    widgets::{Clear, Paragraph, Row, Table, TableState, Wrap},
};
use ratatui_textarea::TextArea;
use zeroize::Zeroizing;

use super::{
    Action, Ctx, Load, Mode, View, confirm_popup, date, failure, key_line, keys, line_input,
    status, table_nav, theme, yank,
};
use crate::{app::KeyEvent, error::UiError, task::Task, tokens::Token};

enum State {
    Browse,
    Naming(Box<TextArea<'static>>),
    Confirm(Token),
}

pub struct Tokens {
    list: Load<Vec<Token>>,
    table: TableState,
    state: State,
    created: Load<Zeroizing<String>>,
    fresh: bool,
    error: Option<Exn<UiError>>,
}

impl Tokens {
    pub fn new(ctx: &Ctx) -> Self {
        Self {
            list: load(ctx),
            table: TableState::default().with_selected(0),
            state: State::Browse,
            created: Task::new(),
            fresh: false,
            error: None,
        }
    }

    fn selected(&self) -> Option<&Token> {
        let Task::Done(Ok(list)) = &self.list else {
            return None;
        };
        self.table.selected().and_then(|i| list.get(i))
    }
}

fn load(ctx: &Ctx) -> Load<Vec<Token>> {
    let (state, user) = (ctx.state, ctx.user);
    Task::new_spawn(async move {
        state
            .tokens
            .list(user)
            .await
            .or_raise(|| UiError::LoadTokens)
    })
}

impl View for Tokens {
    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let [main, prompt] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(3)]).areas(area);
        let block = theme::block().title(" API tokens · browser extension ");
        let inner = block.inner(main);
        frame.render_widget(block, main);
        match self.list.poll() {
            Task::Done(Ok(list)) if list.is_empty() => {
                frame.render_widget(
                    Line::from("no tokens yet, press a").fg(theme::SUBTLE),
                    inner,
                );
            }
            Task::Done(Ok(list)) => {
                let rows = list.iter().map(|t| {
                    Row::new([
                        t.name.clone(),
                        date(Some(t.created)),
                        t.used.map_or_else(|| "never".to_owned(), |d| date(Some(d))),
                    ])
                });
                let widget = Table::new(
                    rows,
                    [
                        Constraint::Fill(1),
                        Constraint::Length(16),
                        Constraint::Length(16),
                    ],
                )
                .header(Row::new(["name", "created", "last used"]).style(theme::HEADER))
                .row_highlight_style(theme::SELECTED);
                frame.render_stateful_widget(widget, inner, &mut self.table);
            }
            other => status(frame, inner, other),
        }

        match &mut self.state {
            State::Naming(input) => {
                let block = theme::block().title(" token name, e.g. laptop firefox ");
                input.set_block(match &self.error {
                    Some(error) => block
                        .title_bottom(Line::from(format!(" {} ", failure(error))).red())
                        .border_style(Style::new().red()),
                    None => block.border_style(Style::new().green()),
                });
                input.set_cursor_style(Style::new().reversed());
                frame.render_widget(&**input, prompt);
            }
            State::Confirm(_) => confirm_popup(frame, area, " revoke token? "),
            State::Browse => {
                if let Some(error) = &self.error {
                    frame.render_widget(
                        Paragraph::new(failure(error)).red().block(theme::block()),
                        prompt,
                    );
                }
            }
        }

        match self.created.poll() {
            Task::Running(_) => {
                frame.render_widget(Line::from("creating…").fg(theme::SUBTLE), prompt);
            }
            Task::Done(Ok(token)) => {
                let lines = vec![
                    Line::from("copy it now, it is shown only once").fg(theme::SUBTLE),
                    Line::from(""),
                    Line::from(token.as_str().to_owned()).bold(),
                    Line::from(""),
                    key_line("y", "yank token"),
                    key_line("esc", "close"),
                ];
                let width = (token.len() as u16 + 4).max(44).min(area.width);
                let height = (lines.len() as u16 + 2).min(area.height);
                let rect = Rect {
                    x: area.x + (area.width - width) / 2,
                    y: area.y + (area.height - height) / 2,
                    width,
                    height,
                };
                frame.render_widget(Clear, rect);
                frame.render_widget(
                    Paragraph::new(lines)
                        .wrap(Wrap { trim: false })
                        .block(theme::block().title(" new token ").green()),
                    rect,
                );
            }
            Task::Done(Err(e)) => {
                frame.render_widget(
                    Paragraph::new(failure(e)).red().block(theme::block()),
                    prompt,
                );
            }
            Task::Panicked(e) => {
                frame.render_widget(
                    Paragraph::new(format!("error: {e}"))
                        .red()
                        .block(theme::block()),
                    prompt,
                );
            }
            Task::Unexecuted => {}
        }
    }

    fn input(&mut self, key: KeyEvent, ctx: &Ctx) -> Action {
        if let Task::Done(Ok(token)) = &self.created {
            let action = match keys::ShowToken::get(&key) {
                Some(keys::ShowToken::Yank) => yank(token),
                Some(keys::ShowToken::Close) => Action::None,
                None => return Action::None,
            };
            self.created = Task::new();
            return action;
        }
        if matches!(self.created, Task::Running(_)) {
            return Action::None;
        }
        self.created = Task::new();
        self.error = None;
        match &mut self.state {
            State::Naming(input) => match keys::NewToken::get(&key) {
                Some(keys::NewToken::Cancel) => self.state = State::Browse,
                Some(keys::NewToken::Create) => {
                    let name = input.lines().join("").trim().to_owned();
                    if name.is_empty() {
                        self.error = Some(UiError::EmptyTokenName.raise());
                        return Action::None;
                    }
                    let (state, user) = (ctx.state, ctx.user);
                    self.fresh = true;
                    self.created.spawn(async move {
                        state
                            .tokens
                            .create(user, name)
                            .await
                            .or_raise(|| UiError::CreateToken)
                    });
                    self.state = State::Browse;
                }
                None => {
                    input.input(key);
                }
            },
            State::Confirm(token) => {
                let id = token.id;
                self.state = State::Browse;
                if keys::Confirm::get(&key) == Some(keys::Confirm::Yes) {
                    let (state, user) = (ctx.state, ctx.user);
                    state.sessions.revoke(user, id);
                    self.list = Task::new_spawn(async move {
                        state
                            .tokens
                            .revoke(user, id)
                            .await
                            .or_raise(|| UiError::RevokeToken)?;
                        state
                            .tokens
                            .list(user)
                            .await
                            .or_raise(|| UiError::LoadTokens)
                    });
                }
            }
            State::Browse => {
                if table_nav(&mut self.table, &key) {
                    return Action::None;
                }
                match keys::Tokens::get(&key) {
                    Some(keys::Tokens::Add) => {
                        self.state = State::Naming(line_input(String::new()))
                    }
                    Some(keys::Tokens::Revoke) => {
                        if let Some(token) = self.selected().cloned() {
                            self.state = State::Confirm(token);
                        }
                    }
                    Some(keys::Tokens::Reload) => self.list = load(ctx),
                    Some(keys::Tokens::Back) => return Action::Pop,
                    None => {}
                }
            }
        }
        Action::None
    }

    fn help(&self) -> keys::Help {
        match (&self.created, &self.state) {
            (Task::Done(Ok(_)), _) => &[keys::ShowToken::KEYS],
            (_, State::Naming(_)) => &[keys::NewToken::KEYS],
            (_, State::Confirm(_)) => &[keys::Confirm::KEYS],
            (_, State::Browse) => &[keys::Tokens::KEYS],
        }
    }

    fn mode(&self) -> Mode {
        match self.state {
            State::Naming(_) => Mode::Insert,
            State::Browse | State::Confirm(_) => Mode::Normal,
        }
    }

    fn update(&mut self, ctx: &Ctx) -> Action {
        if self.fresh && matches!(self.created.poll(), Task::Done(_) | Task::Panicked(_)) {
            self.fresh = false;
            self.list = load(ctx);
        }
        Action::None
    }
}
