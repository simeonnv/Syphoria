mod app;
mod backend;
mod config;
mod mail;
mod ssh;
mod stalwart;
mod state;
mod task;
mod ui;
mod vault;

use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use clap::Parser;
use exn_anyhow::into_anyhow;

use crate::{config::Config, ssh::SshServer, state::AppState, ui::Pages};

#[derive(Parser)]
#[command(version, about)]
struct Args {
    #[arg(short, long, default_value = "syphoria.toml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let state = AppState::new(Config::load(&args.config)?)
        .with_context(|| format!("invalid config {}", args.config.display()))?;

    println!(
        "loaded {}: {} domain(s), {} user(s), {} key(s)",
        args.config.display(),
        state.domains.len(),
        state.users.len(),
        state.key_count()
    );

    state
        .stalwart
        .provision(&state.domains, state.users.values())
        .await
        .map_err(into_anyhow)
        .context("provisioning stalwart")?;

    println!("provisioned stalwart");

    let host_key = ssh::load_or_create_host_key(&state.server.host_key)?;
    let server = SshServer {
        state,
        factory: |state, user| Box::new(Pages::new(state, user)),
        refresh_rate: Some(Duration::from_millis(100)),
    };

    println!("listening on {}", state.server.listen);
    server.run(ssh::config(host_key), state.server.listen).await
}
