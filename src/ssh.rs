use std::{fs, net::SocketAddr, path::Path, sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow};
use ratatui::Terminal;
use russh::{
    Channel, ChannelId, MethodKind, MethodSet, Pty,
    keys::{
        Algorithm, HashAlg, PrivateKey, PublicKey,
        ssh_key::{LineEnding, rand_core::OsRng},
    },
    server::{Auth, Config, Handle, Handler, Msg, Server, Session},
};
use termwiz::input::{InputEvent, InputParser, KeyCode, KeyEvent, Modifiers};
use tokio::{net::ToSocketAddrs, sync::mpsc};

use crate::{
    app::App,
    backend::SshBackend,
    state::{AppState, User},
};

pub type AppFactory = fn(AppState, &'static User) -> Box<dyn App>;

const ENTER_ALT_SCREEN: &str = "\x1b[?1049h\x1b[H\x1b[?25l";
const LEAVE_ALT_SCREEN: &str = "\x1b[?1049l\x1b[?25h\x1b[0m";

pub struct SshServer {
    pub state: AppState,
    pub factory: AppFactory,
    pub refresh_rate: Option<Duration>,
}

pub fn config(host_key: PrivateKey) -> Config {
    let mut methods = MethodSet::empty();
    methods.push(MethodKind::PublicKey);

    Config {
        keys: vec![host_key],
        methods,
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    }
}

pub fn load_or_create_host_key(path: &Path) -> Result<PrivateKey> {
    if path.exists() {
        return PrivateKey::read_openssh_file(path)
            .with_context(|| format!("reading host key {}", path.display()));
    }

    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        fs::create_dir_all(dir)
            .with_context(|| format!("creating host key directory {}", dir.display()))?;
    }

    let key = PrivateKey::random(&mut OsRng, Algorithm::Ed25519)?;
    key.write_openssh_file(path, LineEnding::LF)
        .with_context(|| format!("writing host key {}", path.display()))?;

    println!("generated new host key at {}", path.display());

    Ok(key)
}

impl SshServer {
    pub async fn run(self, config: Config, addr: impl ToSocketAddrs + Send) -> Result<()> {
        let mut server = self;
        server.run_on_address(Arc::new(config), addr).await?;
        Ok(())
    }
}

impl Server for SshServer {
    type Handler = SessionHandler;

    fn new_client(&mut self, peer: Option<SocketAddr>) -> SessionHandler {
        SessionHandler {
            state: self.state,
            factory: self.factory,
            refresh_rate: self.refresh_rate,
            peer,
            user: None,
            app: None,
            size: (80, 24),
            events: None,
            parser: InputParser::new(),
        }
    }
}

enum Event {
    Input(InputEvent),
    Resize(u16, u16),
    Quit,
}

pub struct SessionHandler {
    state: AppState,
    factory: AppFactory,
    refresh_rate: Option<Duration>,
    peer: Option<SocketAddr>,
    user: Option<&'static User>,
    app: Option<Box<dyn App>>,
    size: (u16, u16),
    events: Option<mpsc::UnboundedSender<Event>>,
    parser: InputParser,
}

impl SessionHandler {
    fn send(&self, event: Event) {
        if let Some(tx) = &self.events {
            let _ = tx.send(event);
        }
    }

    fn peer(&self) -> String {
        self.peer.map_or_else(|| "?".into(), |p| p.to_string())
    }

    fn lookup(&self, key: &PublicKey) -> Option<&'static User> {
        let user = self.state.user_by_key(key);

        if user.is_none() {
            println!(
                "rejected key from {}: {} {}",
                self.peer(),
                key.fingerprint(HashAlg::Sha256),
                key.to_openssh().unwrap_or_default()
            );
        }

        user
    }

    fn reject_try_publickey() -> Auth {
        let mut methods = MethodSet::empty();
        methods.push(MethodKind::PublicKey);
        Auth::Reject {
            proceed_with_methods: Some(methods),
            partial_success: false,
        }
    }
}

impl Handler for SessionHandler {
    type Error = anyhow::Error;

    async fn auth_none(&mut self, _user: &str) -> Result<Auth> {
        Ok(Self::reject_try_publickey())
    }

    async fn auth_password(&mut self, _user: &str, _password: &str) -> Result<Auth> {
        Ok(Self::reject_try_publickey())
    }

    async fn auth_publickey_offered(&mut self, _user: &str, key: &PublicKey) -> Result<Auth> {
        Ok(match self.lookup(key) {
            Some(_) => Auth::Accept,
            None => Self::reject_try_publickey(),
        })
    }

    async fn auth_publickey(&mut self, _user: &str, key: &PublicKey) -> Result<Auth> {
        let Some(user) = self.lookup(key) else {
            return Ok(Self::reject_try_publickey());
        };
        self.user = Some(user);
        Ok(Auth::Accept)
    }

    async fn auth_succeeded(&mut self, _session: &mut Session) -> Result<()> {
        let user = self
            .user
            .ok_or_else(|| anyhow!("auth succeeded without a public key"))?;

        println!("connected: {} from {}", user.name, self.peer());

        self.app = Some((self.factory)(self.state, user));
        Ok(())
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool> {
        Ok(true)
    }

    #[allow(clippy::too_many_arguments)]
    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        cols: u32,
        rows: u32,
        _px_w: u32,
        _px_h: u32,
        _modes: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<()> {
        self.size = (cols as u16, rows as u16);
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<()> {
        let app = self
            .app
            .take()
            .ok_or_else(|| anyhow!("shell requested before app was built"))?;

        session.channel_success(channel)?;
        session.data(channel, ENTER_ALT_SCREEN.into())?;

        let (tx, rx) = mpsc::unbounded_channel();
        self.events = Some(tx);

        let runner = SessionTask {
            app,
            terminal: Terminal::new(SshBackend::new(self.size.0, self.size.1))?,
            handle: session.handle(),
            channel,
            refresh_rate: self.refresh_rate,
        };
        tokio::spawn(runner.run(rx));
        Ok(())
    }

    async fn data(
        &mut self,
        _channel: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> Result<()> {
        let mut events = Vec::new();
        self.parser.parse(data, |e| events.push(e), false);

        for event in events {
            if let InputEvent::Key(KeyEvent {
                key: KeyCode::Char('c'),
                modifiers: Modifiers::CTRL,
            }) = event
            {
                self.send(Event::Quit);
                return Ok(());
            }
            self.send(Event::Input(event));
        }
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        _channel: ChannelId,
        cols: u32,
        rows: u32,
        _px_w: u32,
        _px_h: u32,
        _session: &mut Session,
    ) -> Result<()> {
        self.size = (cols as u16, rows as u16);
        self.send(Event::Resize(cols as u16, rows as u16));
        Ok(())
    }
}

impl Drop for SessionHandler {
    fn drop(&mut self) {
        if let Some(user) = self.user {
            println!("disconnected: {} from {}", user.name, self.peer());
        }
    }
}

struct SessionTask {
    app: Box<dyn App>,
    terminal: Terminal<SshBackend>,
    handle: Handle,
    channel: ChannelId,
    refresh_rate: Option<Duration>,
}

impl SessionTask {
    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Event>) {
        let mut ticker = self.refresh_rate.map(|rate| {
            let mut i = tokio::time::interval(rate);
            i.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            i
        });

        if let Err(e) = self.render().await {
            eprintln!("render error: {e}");
            return;
        }

        loop {
            let tick = async {
                match ticker.as_mut() {
                    Some(t) => {
                        t.tick().await;
                    }
                    None => std::future::pending().await,
                }
            };

            tokio::select! {
                event = rx.recv() => match event {
                    Some(Event::Input(e)) => self.app.input(e),
                    Some(Event::Resize(cols, rows)) => self.terminal.backend_mut().resize(cols, rows),
                    Some(Event::Quit) | None => {
                        close(&self.handle, self.channel, None).await;
                        return;
                    }
                },
                _ = tick => self.app.on_tick(),
            }

            match self.render().await {
                Ok(None) => {}
                Ok(Some(message)) => {
                    close(&self.handle, self.channel, Some(message)).await;
                    return;
                }
                Err(e) => {
                    eprintln!("render error: {e}");
                    close(&self.handle, self.channel, Some("internal error".into())).await;
                    return;
                }
            }
        }
    }

    async fn render(&mut self) -> Result<Option<String>> {
        let exit = self.app.render(&mut self.terminal)?;
        let bytes = self.terminal.backend_mut().take();
        if !bytes.is_empty() {
            self.handle
                .data(self.channel, bytes.into())
                .await
                .map_err(|_| anyhow!("channel closed"))?;
        }
        Ok(exit)
    }
}

async fn close(handle: &Handle, channel: ChannelId, message: Option<String>) {
    let bye = format!(
        "{LEAVE_ALT_SCREEN}{}\r\n",
        message.as_deref().unwrap_or("bye")
    );
    let _ = handle.data(channel, bye.into()).await;
    let _ = handle.exit_status_request(channel, 0).await;
    let _ = handle.eof(channel).await;
    let _ = handle.close(channel).await;
}
