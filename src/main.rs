//! Agentdeck: a small Rust TUI that wraps multiple AI-agent CLIs (Claude Code,
//! Codex CLI, Gemini CLI, Aider, ...) in a single split-pane view, running each
//! in its own PTY tile so you can drive them side by side.

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;

mod agent;
mod app;
mod config;
mod keymap;
mod state;
mod ui;
mod usage;

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

    let cfg_path = resolve_config_path(cli.config, std::env::var("AGENTDECK_CONFIG").ok())?;

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

    for warning in cfg.validate() {
        tracing::warn!(target: "agentdeck::config", "{warning}");
        eprintln!("agentdeck: config warning: {warning}");
    }

    app::run(cfg)
}

/// Pure precedence: `--config` flag, then a non-empty `AGENTDECK_CONFIG`, else
/// none (caller falls back to [`config::default_config_path`]).
fn pick_config_source(
    cli_override: Option<PathBuf>,
    env_value: Option<String>,
) -> Option<(PathBuf, &'static str)> {
    if let Some(p) = cli_override {
        return Some((p, "cli"));
    }
    if let Some(v) = env_value
        && !v.is_empty()
    {
        return Some((PathBuf::from(v), "env"));
    }
    None
}

fn resolve_config_path(
    cli_override: Option<PathBuf>,
    env_value: Option<String>,
) -> Result<PathBuf> {
    let (path, source) = match pick_config_source(cli_override, env_value) {
        Some(picked) => picked,
        None => (config::default_config_path()?, "default"),
    };
    tracing::info!(source, path = %path.display(), "config path from CLI/env/default");
    Ok(path)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_flag_wins_over_env() {
        let picked = pick_config_source(
            Some(PathBuf::from("/tmp/y.toml")),
            Some("/tmp/x.toml".into()),
        );
        assert_eq!(picked, Some((PathBuf::from("/tmp/y.toml"), "cli")));
    }

    #[test]
    fn env_used_when_no_cli_flag() {
        let picked = pick_config_source(None, Some("/tmp/x.toml".into()));
        assert_eq!(picked, Some((PathBuf::from("/tmp/x.toml"), "env")));
    }

    #[test]
    fn empty_env_falls_back_to_default() {
        assert_eq!(pick_config_source(None, Some(String::new())), None);
        assert_eq!(pick_config_source(None, None), None);
    }
}
