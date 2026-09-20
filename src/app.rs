use anyhow::Result;
use ratatui::Terminal;
use russh::keys::{HashAlg, PublicKey};
pub use termwiz::input::{InputEvent, KeyCode, KeyEvent};

use crate::backend::SshBackend;

pub type SshTerminal = Terminal<SshBackend>;

#[derive(Debug, Clone)]
pub struct Identity {
    pub user: String,
    pub key: PublicKey,
    pub fingerprint: String,
}

impl Identity {
    pub fn new(user: &str, key: &PublicKey) -> Self {
        Self {
            user: user.to_owned(),
            key: key.clone(),
            fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
        }
    }
}

pub trait App: Send {
    fn render(&mut self, terminal: &mut SshTerminal) -> Result<Option<String>>;

    fn input(&mut self, event: InputEvent);

    fn on_tick(&mut self) {}
}
