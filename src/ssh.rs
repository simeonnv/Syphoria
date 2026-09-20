use std::{net::SocketAddr, sync::Arc, time::Duration};

use anyhow::{Result, anyhow};
use ratatui::Terminal;
use russh::{
    Channel, ChannelId, MethodKind, MethodSet, Pty,
    keys::PublicKey,
    server::{Auth, Config, Handle, Handler, Msg, Server, Session},
};
use termwiz::input::{InputEvent, InputParser, KeyCode, KeyEvent, Modifiers};
use tokio::{net::ToSocketAddrs, sync::mpsc};

use crate::{
    app::{App, Identity},
    backend::SshBackend,
};

pub type Authorizer = Arc<dyn Fn(&str, &PublicKey) -> bool + Send + Sync>;
pub type AppFactory = Arc<dyn Fn(&Identity) -> Box<dyn App> + Send + Sync>;

const ENTER_ALT_SCREEN: &str = "\x1b[?1049h\x1b[H\x1b[?25l";
const LEAVE_ALT_SCREEN: &str = "\x1b[?1049l\x1b[?25h\x1b[0m";

pub struct SshServer {
    pub authorize: Authorizer,
    pub factory: AppFactory,
    pub refresh_rate: Option<Duration>,
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
            authorize: self.authorize.clone(),
            factory: self.factory.clone(),
            refresh_rate: self.refresh_rate,
            peer,
            identity: None,
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
    authorize: Authorizer,
    factory: AppFactory,
    refresh_rate: Option<Duration>,
    peer: Option<SocketAddr>,
    identity: Option<Identity>,
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

    async fn auth_publickey_offered(&mut self, user: &str, key: &PublicKey) -> Result<Auth> {
        Ok(if (self.authorize)(user, key) {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn auth_publickey(&mut self, user: &str, key: &PublicKey) -> Result<Auth> {
        if !(self.authorize)(user, key) {
            return Ok(Auth::reject());
        }
        self.identity = Some(Identity::new(user, key));
        Ok(Auth::Accept)
    }

    async fn auth_succeeded(&mut self, _session: &mut Session) -> Result<()> {
        let identity = self
            .identity
            .as_ref()
            .ok_or_else(|| anyhow!("auth succeeded without a public key"))?;
        println!(
            "connected: {}@{} ({})",
            identity.user,
            self.peer.map_or("?".into(), |p| p.to_string()),
            identity.fingerprint
        );
        self.app = Some((self.factory)(identity));
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
        if let Some(identity) = &self.identity {
            println!("disconnected: {} ({})", identity.user, identity.fingerprint);
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
