use ratatui::{
    style::{Color, Modifier, Style},
    widgets::Block,
};

pub const SUBTLE: Color = Color::Indexed(20);
pub const HEADER: Style = Style::new().fg(Color::Cyan);
pub const SELECTED: Style = Style::new()
    .bg(Color::Indexed(19))
    .add_modifier(Modifier::BOLD);
const BORDER: Style = Style::new().fg(Color::Blue);

pub fn block<'a>() -> Block<'a> {
    Block::bordered().border_style(BORDER).title_style(BORDER)
}
