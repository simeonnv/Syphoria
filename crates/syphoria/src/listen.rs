use std::{net::SocketAddr, sync::Arc, time::Duration};

use exn::ResultExt;
use log::warn;
use russh::server::Config;
use tokio::net::{TcpListener, TcpStream};

use crate::{api::Api, error::MainError, ssh::SshServer};

const PEEK: Duration = Duration::from_secs(3);
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);
const TLS_HANDSHAKE: u8 = 0x16;

pub async fn run(
    addr: SocketAddr,
    ssh: SshServer,
    config: Config,
    api: Option<Api>,
) -> exn::Result<(), MainError> {
    let listener = TcpListener::bind(addr)
        .await
        .or_raise(|| MainError::Listen)?;
    let config = Arc::new(config);
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(e) => {
                warn!("accept failed: {e}");
                tokio::time::sleep(ACCEPT_BACKOFF).await;
                continue;
            }
        };
        let config = config.clone();
        let api = api.clone();
        tokio::spawn(async move {
            match api {
                Some(api) if is_tls(&stream).await => api.serve(stream, peer).await,
                _ => ssh.serve(config, stream, peer).await,
            }
        });
    }
}

async fn is_tls(stream: &TcpStream) -> bool {
    let mut byte = [0u8; 1];
    matches!(
        tokio::time::timeout(PEEK, stream.peek(&mut byte)).await,
        Ok(Ok(1)) if byte[0] == TLS_HANDSHAKE
    )
}
