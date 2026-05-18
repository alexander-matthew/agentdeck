use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;

mod agent;
mod app;
mod config;
mod keymap;
mod state;
mod ui;

#[derive(Parser, Debug)]
#[command(
    name = "agentdeck",
    version,
    about = "Terminal control room for multiple AI-agent CLIs."
)]
struct Cli {
    /// Path to a config file. Defaults to ~/.config/agentdeck/config.toml.
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Print the resolved config (writing defaults if missing) then exit.
    #[arg(long)]
    print_config: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    install_tracing()?;

    let cfg_path = match cli.config {
        Some(p) => p,
        None => config::default_config_path()?,
    };

    if cli.print_config {
        let resolved = config::load_or_init(&cfg_path)?;
        println!("# config path: {}", cfg_path.display());
        println!(
            "{}",
            toml::to_string_pretty(&resolved).context("serialize config")?
        );
        return Ok(());
    }

    let cfg = config::load_or_init(&cfg_path)?;
    tracing::info!(path = %cfg_path.display(), n_agents = cfg.agents.len(), "loaded config");

    app::run(cfg)
}

fn install_tracing() -> Result<()> {
    let state_dir = dirs_state_home()?.join("agentdeck");
    std::fs::create_dir_all(&state_dir).ok();
    let file_appender = tracing_appender::rolling::never(state_dir, "agentdeck.log");
    let env_filter = tracing_subscriber::EnvFilter::try_from_env("AGENTDECK_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(file_appender)
        .with_ansi(false)
        .init();
    Ok(())
}

fn dirs_state_home() -> Result<PathBuf> {
    if let Ok(s) = std::env::var("XDG_STATE_HOME")
        && !s.is_empty()
    {
        return Ok(PathBuf::from(s));
    }
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".local").join("state"))
}
