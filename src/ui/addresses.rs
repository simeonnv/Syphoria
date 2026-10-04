use exn_anyhow::into_anyhow;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Style, Stylize},
    text::Line,
    widgets::{Block, List, ListState, Paragraph},
};
use ratatui_textarea::TextArea;
use termwiz::input::Modifiers;

use super::{Action, Ctx, Load, View, line_input, status};
use crate::{
    app::{KeyCode, KeyEvent},
    stalwart::Stalwart,
    state::User,
    task::Task,
};

enum Mode {
    Browse,
    Create {
        local: Box<TextArea<'static>>,
        domain: usize,
    },
    Delete(String),
}

pub struct Addresses {
    user: &'static User,
    list: Load<Vec<String>>,
    selected: ListState,
    mode: Mode,
}

impl Addresses {
    pub fn new(ctx: &Ctx) -> Self {
        let mut page = Self {
            user: ctx.user,
            list: Task::new(),
            selected: ListState::default().with_selected(Some(0)),
            mode: Mode::Browse,
        };
        page.reload(ctx);
        page
    }

    fn reload(&mut self, ctx: &Ctx) {
        let (state, user) = (ctx.state, ctx.user);
        self.list
            .spawn(async move { state.stalwart.addresses(user).await.map_err(into_anyhow) });
    }

    fn selected(&self) -> Option<String> {
        let Task::Done(Ok(list)) = &self.list else {
            return None;
        };
        self.selected.selected().and_then(|i| list.get(i)).cloned()
    }
}

impl View for Addresses {
    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let [main, prompt] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(3)]).areas(area);

        let block = Block::bordered().title(" Addresses ");
        let inner = block.inner(main);
        frame.render_widget(block, main);
        match self.list.poll() {
            Task::Done(Ok(list)) if list.is_empty() => {
                frame.render_widget(Line::from("no addresses yet, press n").dim(), inner);
            }
            Task::Done(Ok(list)) => {
                let list = List::new(list.iter().map(String::as_str))
                    .highlight_style(Style::new().reversed());
                frame.render_stateful_widget(list, inner, &mut self.selected);
            }
            other => status(frame, inner, other),
        }

        match &mut self.mode {
            Mode::Browse => {}
            Mode::Delete(address) => frame.render_widget(
                Paragraph::new(format!("delete {address}? y/n"))
                    .red()
                    .block(Block::bordered()),
                prompt,
            ),
            Mode::Create { local, domain } => {
                let suffix = self
                    .user
                    .domains
                    .iter()
                    .nth(*domain)
                    .map(|d| format!("@{d}"))
                    .unwrap_or_default();
                let [left, right] = Layout::horizontal([
                    Constraint::Fill(1),
                    Constraint::Length(suffix.len() as u16 + 2),
                ])
                .areas(prompt);
                local.set_block(
                    Block::bordered()
                        .title(" new address ")
                        .border_style(Style::new().yellow()),
                );
                frame.render_widget(&**local, left);
                frame.render_widget(
                    Paragraph::new(suffix).block(Block::bordered().title(" tab ")),
                    right,
                );
            }
        }
    }

    fn input(&mut self, key: KeyEvent, ctx: &Ctx) -> Action {
        match &mut self.mode {
            Mode::Browse => match key.key {
                KeyCode::Char('j') | KeyCode::DownArrow => self.selected.select_next(),
                KeyCode::Char('k') | KeyCode::UpArrow => self.selected.select_previous(),
                KeyCode::Char('r') => self.reload(ctx),
                KeyCode::Char('n') if !ctx.user.domains.is_empty() => {
                    self.mode = Mode::Create {
                        local: line_input(Stalwart::random_local_part()),
                        domain: 0,
                    };
                }
                KeyCode::Char('d') => {
                    if let Some(address) = self.selected() {
                        self.mode = Mode::Delete(address);
                    }
                }
                KeyCode::Escape => return Action::Pop,
                _ => {}
            },
            Mode::Delete(address) => {
                if key.key == KeyCode::Char('y') {
                    let (state, user, address) = (ctx.state, ctx.user, address.clone());
                    self.list.spawn(async move {
                        state
                            .stalwart
                            .remove_address(user, &address)
                            .await
                            .map_err(into_anyhow)
                    });
                }
                self.mode = Mode::Browse;
            }
            Mode::Create { local, domain } => {
                let domains = ctx.user.domains.len();
                match key.key {
                    KeyCode::Escape => self.mode = Mode::Browse,
                    KeyCode::Tab if key.modifiers.contains(Modifiers::SHIFT) => {
                        *domain = (*domain + domains - 1) % domains;
                    }
                    KeyCode::Tab => *domain = (*domain + 1) % domains,
                    KeyCode::Enter => {
                        let (state, user) = (ctx.state, ctx.user);
                        let local = local.lines().join("");
                        if let Some(domain) = user.domains.iter().nth(*domain).cloned() {
                            self.list.spawn(async move {
                                state
                                    .stalwart
                                    .add_address(user, &local, &domain)
                                    .await
                                    .map_err(into_anyhow)
                            });
                        }
                        self.mode = Mode::Browse;
                    }
                    _ => {
                        local.input(key);
                    }
                }
            }
        }
        Action::None
    }

    fn help(&self) -> &'static str {
        match self.mode {
            Mode::Browse => "n new · d delete · r reload",
            Mode::Create { .. } => "type local part · tab domain · enter create · esc cancel",
            Mode::Delete(_) => "y confirm · any key cancel",
        }
    }

    fn typing(&self) -> bool {
        matches!(self.mode, Mode::Create { .. })
    }
}
