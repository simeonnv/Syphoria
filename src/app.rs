use anyhow::Result;
use ratatui::Terminal;
pub use termwiz::input::{InputEvent, KeyCode, KeyEvent};

use crate::backend::SshBackend;

pub type SshTerminal = Terminal<SshBackend>;

pub trait App: Send {
    fn render(&mut self, terminal: &mut SshTerminal) -> Result<Option<String>>;

    fn input(&mut self, event: InputEvent);

    fn on_tick(&mut self) {}
}
