use exn::{Exn, ResultExt};
use rand::{RngExt, distr::Alphanumeric};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Style, Stylize},
    text::Line,
    widgets::{List, ListState, Paragraph},
};
use ratatui_textarea::TextArea;

use super::{
    Action, Ctx, Load, Mode, View, contains, failure, find_match, keys, line_input, list_nav,
    status, theme, yank,
};
use crate::{
    app::KeyEvent,
    error::{InputError, UiError},
    stalwart::Address,
    state::{Grant, User},
    task::Task,
};

enum State {
    Browse,
    Create {
        local: Box<TextArea<'static>>,
        domain: usize,
    },
}

pub struct Addresses {
    user: &'static User,
    list: Load<Vec<String>>,
    selected: ListState,
    state: State,
    error: Option<Exn<InputError>>,
}

impl Addresses {
    pub fn new(ctx: &Ctx) -> Self {
        let mut page = Self {
            user: ctx.user,
            list: Task::new(),
            selected: ListState::default().with_selected(Some(0)),
            state: State::Browse,
            error: None,
        };
        page.reload(ctx);
        page
    }

    fn reload(&mut self, ctx: &Ctx) {
        let (state, user) = (ctx.state, ctx.user);
        self.list.spawn(async move {
            state
                .stalwart
                .addresses(user)
                .await
                .or_raise(|| UiError::LoadAddresses)
        });
    }

    fn selected(&self) -> Option<&String> {
        let Task::Done(Ok(list)) = &self.list else {
            return None;
        };
        self.selected.selected().and_then(|i| list.get(i))
    }
}

impl View for Addresses {
    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let [main, prompt] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(3)]).areas(area);

        let fixed = matches!(self.user.grant, Grant::Addresses(_));
        let block = theme::block().title(if fixed {
            " Addresses \u{b7} assigned by admin "
        } else {
            " Addresses "
        });
        let inner = block.inner(main);
        frame.render_widget(block, main);
        match self.list.poll() {
            Task::Done(Ok(list)) if list.is_empty() => {
                let empty = if fixed {
                    "no addresses assigned"
                } else {
                    "no addresses yet, press a"
                };
                frame.render_widget(Line::from(empty).fg(theme::SUBTLE), inner);
            }
            Task::Done(Ok(list)) => {
                let list =
                    List::new(list.iter().map(String::as_str)).highlight_style(theme::SELECTED);
                frame.render_stateful_widget(list, inner, &mut self.selected);
            }
            other => status(frame, inner, other),
        }

        match &mut self.state {
            State::Browse => {
                if let Some(error) = &self.error {
                    frame.render_widget(
                        Paragraph::new(failure(error)).red().block(theme::block()),
                        prompt,
                    );
                }
            }
            State::Create { local, domain } => {
                let suffix = self
                    .user
                    .domains()
                    .into_iter()
                    .nth(*domain)
                    .map(|d| format!("@{d}"))
                    .unwrap_or_default();
                let [left, right] = Layout::horizontal([
                    Constraint::Fill(1),
                    Constraint::Length(suffix.len() as u16 + 2),
                ])
                .areas(prompt);
                let block = theme::block().title(" new address · addresses are permanent ");
                local.set_block(match &self.error {
                    Some(error) => block
                        .title_bottom(Line::from(format!(" {} ", failure(error))).red())
                        .border_style(Style::new().red()),
                    None => block.border_style(Style::new().green()),
                });
                local.set_cursor_style(Style::new().reversed());
                frame.render_widget(&**local, left);
                frame.render_widget(
                    Paragraph::new(suffix).block(theme::block().title(" tab ")),
                    right,
                );
            }
        }
    }

    fn input(&mut self, key: KeyEvent, ctx: &Ctx) -> Action {
        self.error = None;
        match &mut self.state {
            State::Browse => {
                if list_nav(&mut self.selected, &key) {
                    return Action::None;
                }
                match keys::Addresses::get(&key) {
                    Some(keys::Addresses::Reload) => self.reload(ctx),
                    Some(keys::Addresses::Add) if !ctx.user.domains().is_empty() => {
                        self.state = State::Create {
                            local: line_input(random_local_part()),
                            domain: 0,
                        };
                    }
                    Some(keys::Addresses::Yank) => {
                        if let Some(address) = self.selected() {
                            return yank(address);
                        }
                    }
                    Some(keys::Addresses::Back) => return Action::Pop,
                    Some(keys::Addresses::Add) | None => {}
                }
            }
            State::Create { local, domain } => {
                let domains = ctx.user.domains().len();
                match keys::NewAddress::get(&key) {
                    Some(keys::NewAddress::Cancel) => self.state = State::Browse,
                    Some(keys::NewAddress::Prev) => *domain = (*domain + domains - 1) % domains,
                    Some(keys::NewAddress::Next) => *domain = (*domain + 1) % domains,
                    Some(keys::NewAddress::Random) => *local = line_input(random_local_part()),
                    Some(keys::NewAddress::Create) => {
                        let (state, user) = (ctx.state, ctx.user);
                        let Some(domain) = user.domains().into_iter().nth(*domain) else {
                            return Action::None;
                        };
                        let address = format!("{}@{domain}", local.lines().join(""));
                        match Address::new(user, &address) {
                            Ok(address) => {
                                self.list.spawn(async move {
                                    state
                                        .stalwart
                                        .add_address(&address)
                                        .await
                                        .or_raise(|| UiError::AddAddress)?;
                                    state
                                        .stalwart
                                        .addresses(user)
                                        .await
                                        .or_raise(|| UiError::LoadAddresses)
                                });
                                self.state = State::Browse;
                            }
                            Err(e) => self.error = Some(e),
                        }
                    }
                    None => {
                        local.input(key);
                    }
                }
            }
        }
        Action::None
    }

    fn help(&self) -> keys::Help {
        match self.state {
            State::Browse => &[keys::Addresses::KEYS],
            State::Create { .. } => &[keys::NewAddress::KEYS],
        }
    }

    fn mode(&self) -> Mode {
        match self.state {
            State::Browse => Mode::Normal,
            State::Create { .. } => Mode::Insert,
        }
    }

    fn search(&mut self, query: &str, forward: bool) {
        let Task::Done(Ok(list)) = &self.list else {
            return;
        };
        let found = find_match(list.len(), self.selected.selected(), forward, |i| {
            contains(&list[i], query)
        });
        if found.is_some() {
            self.selected.select(found);
        }
    }
}

fn random_local_part() -> String {
    rand::rng()
        .sample_iter(Alphanumeric)
        .take(12)
        .map(|b| char::from(b).to_ascii_lowercase())
        .collect()
}
