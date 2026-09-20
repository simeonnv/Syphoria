mod app;
mod backend;
mod ssh;
mod task;

use std::{collections::HashMap, fs, path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Style, Stylize},
    text::Line,
    widgets::{Block, Paragraph},
};
use russh::keys::{Algorithm, HashAlg, PrivateKey, PublicKey, ssh_key::rand_core::OsRng};

use crate::{
    app::{App, Identity, InputEvent, KeyCode, KeyEvent, SshTerminal},
    ssh::SshServer,
    task::Task,
};

const SPINNER: [&str; 4] = ["|", "/", "-", "\\"];

#[derive(Default)]
struct Registry {
    by_fingerprint: HashMap<String, String>,
}

impl Registry {
    fn load(path: &PathBuf) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading authorized keys from {}", path.display()))?;

        let mut by_fingerprint = HashMap::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let key = PublicKey::from_openssh(line)
                .with_context(|| format!("{}:{}: invalid public key", path.display(), n + 1))?;
            let fp = key.fingerprint(HashAlg::Sha256).to_string();
            let name = match key.comment() {
                "" => format!("key-{}", &fp[7..15]),
                c => c.to_owned(),
            };
            by_fingerprint.insert(fp, name);
        }
        Ok(Self { by_fingerprint })
    }

    fn lookup(&self, id: &Identity) -> Option<&str> {
        self.by_fingerprint.get(&id.fingerprint).map(String::as_str)
    }
}

struct Dashboard {
    name: String,
    identity: Identity,
    users: Option<Task<Result<Vec<String>, String>>>,
    frame: usize,
    exit: bool,
}

impl Dashboard {
    fn new(name: &str, identity: &Identity) -> Self {
        Self {
            name: name.to_owned(),
            identity: identity.clone(),
            users: None,
            frame: 0,
            exit: false,
        }
    }

    fn load_users(&mut self) {
        self.users = Some(Task::spawn(fetch_users()));
    }

    fn draw(&mut self, frame: &mut Frame) {
        let [body, footer] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());

        let lines: Vec<Line> = match self.users.as_mut() {
            None => vec![Line::from("press r to load users")],
            Some(task) => match task.poll() {
                Task::Running(_) => {
                    vec![Line::from(format!(
                        "{} loading...",
                        SPINNER[self.frame % SPINNER.len()]
                    ))]
                }
                Task::Panicked => vec![Line::styled("task panicked", Style::new().fg(Color::Red))],
                Task::Done(Ok(users)) => users.iter().map(|u| Line::from(u.as_str())).collect(),
                Task::Done(Err(e)) => vec![Line::styled(
                    format!("error: {e}"),
                    Style::new().fg(Color::Red),
                )],
            },
        };

        let title = format!(" users — {} ({}) ", self.name, self.identity.user);
        frame.render_widget(
            Paragraph::new(lines).block(Block::bordered().title(title)),
            body,
        );
        frame.render_widget(Paragraph::new("q quit  r reload"), footer);
    }
}

impl App for Dashboard {
    fn render(&mut self, terminal: &mut SshTerminal) -> Result<Option<String>> {
        terminal.draw(|frame| self.draw(frame))?;
        Ok(self.exit.then(|| format!("bye {}", self.name)))
    }

    fn input(&mut self, event: InputEvent) {
        let InputEvent::Key(KeyEvent { key, .. }) = event else {
            return;
        };
        match key {
            KeyCode::Char('q') | KeyCode::Escape => self.exit = true,
            KeyCode::Char('r') => self.load_users(),
            _ => {}
        }
    }

    fn on_tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }
}

struct Guest {
    identity: Identity,
    exit: bool,
}

impl App for Guest {
    fn render(&mut self, terminal: &mut SshTerminal) -> Result<Option<String>> {
        terminal.draw(|frame| {
            let key = self.identity.key.to_openssh().unwrap_or_default();
            let lines = vec![
                Line::from(format!(
                    "hello {}, your key isn't registered.",
                    self.identity.user
                )),
                Line::from(""),
                Line::from(vec![
                    "fingerprint: ".into(),
                    self.identity.fingerprint.clone().bold(),
                ]),
                Line::from(format!("key:         {key}")),
                Line::from(""),
                Line::from("ask an admin to add it to the authorized keys file.").dim(),
                Line::from(""),
                Line::from("press any key to disconnect"),
            ];
            frame.render_widget(
                Paragraph::new(lines).block(Block::bordered().title(" unregistered ")),
                frame.area(),
            );
        })?;
        Ok(self.exit.then(|| "bye".to_string()))
    }

    fn input(&mut self, event: InputEvent) {
        if matches!(event, InputEvent::Key(_)) {
            self.exit = true;
        }
    }
}

async fn fetch_users() -> Result<Vec<String>, String> {
    tokio::time::sleep(Duration::from_secs(2)).await;
    Ok(vec!["alice".into(), "bob".into(), "carol".into()])
}

#[tokio::main]
async fn main() -> Result<()> {
    let keys_path = std::env::var_os("SYPHORIA_AUTHORIZED_KEYS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("authorized_keys"));
    let registry = Arc::new(match Registry::load(&keys_path) {
        Ok(r) => {
            println!(
                "loaded {} key(s) from {}",
                r.by_fingerprint.len(),
                keys_path.display()
            );
            r
        }
        Err(e) => {
            eprintln!("warning: {e:#}; everyone will be a guest");
            Registry::default()
        }
    });

    let host_key = PrivateKey::random(&mut OsRng, Algorithm::Ed25519)?;
    let config = russh::server::Config {
        keys: vec![host_key],
        ..Default::default()
    };

    let server = SshServer {
        authorize: Arc::new(|_user, _key| true),
        factory: Arc::new(move |id| match registry.lookup(id) {
            Some(name) => Box::new(Dashboard::new(name, id)),
            None => Box::new(Guest {
                identity: id.clone(),
                exit: false,
            }),
        }),
        refresh_rate: Some(Duration::from_millis(100)),
    };

    println!("listening on 0.0.0.0:2222");
    server.run(config, ("0.0.0.0", 2222)).await
}
