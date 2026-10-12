mod app;
mod backend;
mod config;
mod error;
mod files;
mod mail;
mod sftp;
mod ssh;
mod stalwart;
mod state;
mod task;
mod ui;
mod vault;

use std::{path::PathBuf, time::Duration};

use backon::{ExponentialBuilder, Retryable};
use clap::Parser;
use exn::ResultExt;

use crate::{
    config::Config,
    error::{MainError, chain, is_temporary},
    ssh::SshServer,
    state::AppState,
    ui::Pages,
};

#[derive(Parser)]
#[command(version, about)]
struct Args {
    #[arg(short, long, default_value = "syphoria.toml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> exn::Result<(), MainError> {
    let args = Args::parse();
    let config = Config::load(&args.config).or_raise(|| MainError::LoadConfig)?;
    let state = AppState::new(config).or_raise(|| MainError::InvalidConfig)?;

    println!(
        "loaded {}: {} domain(s), {} user(s), {} key(s)",
        args.config.display(),
        state.domains.len(),
        state.users.len(),
        state.key_count()
    );

    (|| {
        state
            .stalwart
            .provision(&state.domains, state.users.values())
    })
    .retry(
        ExponentialBuilder::default()
            .with_min_delay(Duration::from_millis(500))
            .with_max_delay(Duration::from_secs(10))
            .with_max_times(10),
    )
    .when(|e| is_temporary(e.frame()))
    .notify(|e, delay| {
        println!(
            "stalwart unavailable, retrying in {delay:?}: {}",
            chain(e.frame())
        )
    })
    .await
    .or_raise(|| MainError::Provision)?;

    println!("provisioned stalwart");

    let host_key =
        ssh::load_or_create_host_key(&state.server.host_key).or_raise(|| MainError::HostKey)?;
    let server = SshServer {
        state,
        factory: |state, user| Box::new(Pages::new(state, user)),
        refresh_rate: Some(Duration::from_millis(100)),
    };

    println!("listening on {}", state.server.listen);
    server
        .run(ssh::config(host_key), state.server.listen)
        .await
        .or_raise(|| MainError::Serve)
}
