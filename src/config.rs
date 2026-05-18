use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub settings: Settings,
    #[serde(default, rename = "agent")]
    pub agents: Vec<AgentConfig>,
    /// Per-provider shell commands the usage dashboard runs periodically.
    /// Keys are provider tags (`claude`, `codex`, `gemini`, `aider`,
    /// `shell`, `other`); empty/missing entries skip that provider.
    #[serde(default)]
    pub usage_commands: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Settings {
    /// Detach prefix key as a control byte (default 0x01 = Ctrl-A).
    #[serde(default = "default_prefix")]
    pub prefix_byte: u8,
    /// Key (ASCII) following the prefix that triggers detach (default 'd').
    #[serde(default = "default_detach_key")]
    pub detach_key: char,
    /// Key to toggle focus between deck and agent (default "ctrl-space").
    #[serde(default = "default_toggle_key")]
    pub toggle_key: String,
    /// Rows in the multi-pane grid view (default 2).
    #[serde(default = "default_grid_rows")]
    pub grid_rows: u16,
    /// Cols in the multi-pane grid view (default 2).
    #[serde(default = "default_grid_cols")]
    pub grid_cols: u16,
    /// How often to re-run each provider's usage command, in seconds (default 60).
    #[serde(default = "default_usage_refresh_secs")]
    pub usage_refresh_secs: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            prefix_byte: default_prefix(),
            detach_key: default_detach_key(),
            toggle_key: default_toggle_key(),
            grid_rows: default_grid_rows(),
            grid_cols: default_grid_cols(),
            usage_refresh_secs: default_usage_refresh_secs(),
        }
    }
}

fn default_prefix() -> u8 {
    0x01
}
fn default_detach_key() -> char {
    'd'
}
fn default_toggle_key() -> String {
    "ctrl-space".into()
}
fn default_grid_rows() -> u16 {
    2
}
fn default_grid_cols() -> u16 {
    2
}
fn default_usage_refresh_secs() -> u64 {
    60
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentConfig {
    /// Stable id, used in logs and as a default display name.
    pub id: String,
    /// Optional human-readable name; falls back to `id`.
    #[serde(default)]
    pub name: Option<String>,
    /// Provider tag for display + future provider-specific niceties.
    pub provider: Provider,
    /// Executable to spawn. Looked up on PATH if not absolute.
    pub command: String,
    /// Arguments to pass to the executable.
    #[serde(default)]
    pub args: Vec<String>,
    /// Working directory; `~` and env vars are expanded.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Extra env vars to merge with the parent environment.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// If true, don't auto-spawn at startup; user opens it manually later.
    #[serde(default)]
    pub manual: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Claude,
    Codex,
    Gemini,
    Aider,
    Shell,
    Other,
}

impl Provider {
    pub fn tag(&self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
            Provider::Gemini => "gemini",
            Provider::Aider => "aider",
            Provider::Shell => "shell",
            Provider::Other => "other",
        }
    }
}

impl AgentConfig {
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }

    pub fn resolved_cwd(&self) -> Option<PathBuf> {
        self.cwd.as_ref().map(|raw| {
            let expanded = shellexpand::full(raw)
                .map(|c| c.into_owned())
                .unwrap_or_else(|_| raw.clone());
            PathBuf::from(expanded)
        })
    }
}

pub fn default_config_path() -> Result<PathBuf> {
    if let Ok(s) = std::env::var("XDG_CONFIG_HOME")
        && !s.is_empty()
    {
        return Ok(PathBuf::from(s).join("agentdeck").join("config.toml"));
    }
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("agentdeck")
        .join("config.toml"))
}

pub fn load_or_init(path: &Path) -> Result<Config> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).context("create config dir")?;
        }
        let default = default_config();
        let text = toml::to_string_pretty(&default).context("serialize default config")?;
        let header = "\
# agentdeck config
# Each [[agent]] section spawns one provider CLI in its own PTY.
# These commands run under your shell user, so they reuse whatever
# subscription / OAuth login the native CLI already has — no API keys here.
#
# Press a number to attach to that agent.
# Use [settings] toggle_key to change the key that switches between sidebar and agent.
# Default is \"ctrl-space\". Other examples: \"f1\", \"ctrl-p\", \"alt-d\", \"esc\".
#
# `g` (in deck focus) toggles the multi-pane grid view; tune grid_rows /
# grid_cols below. `u` opens the usage dashboard, which runs each entry
# under [usage_commands] as a shell command and shows the output. Set the
# refresh cadence with usage_refresh_secs.
#
";
        std::fs::write(path, format!("{header}{text}")).context("write default config")?;
        tracing::info!(path = %path.display(), "wrote default config");
        return Ok(default);
    }
    let text = std::fs::read_to_string(path).context("read config")?;
    let cfg: Config = toml::from_str(&text).context("parse config")?;
    Ok(cfg)
}

fn default_config() -> Config {
    let mut usage_commands = BTreeMap::new();
    // ccusage parses Claude Code's local session logs to summarize spend.
    // Default to `npx -y` so it works on a fresh box without a global install.
    usage_commands.insert(
        "claude".to_string(),
        "npx -y ccusage@latest --json".to_string(),
    );
    Config {
        settings: Settings::default(),
        usage_commands,
        agents: vec![
            AgentConfig {
                id: "claude".into(),
                name: Some("Claude".into()),
                provider: Provider::Claude,
                command: "claude".into(),
                args: vec![],
                cwd: Some("~".into()),
                env: BTreeMap::new(),
                manual: false,
            },
            AgentConfig {
                id: "codex".into(),
                name: Some("Codex".into()),
                provider: Provider::Codex,
                command: "codex".into(),
                args: vec![],
                cwd: Some("~".into()),
                env: BTreeMap::new(),
                manual: false,
            },
            AgentConfig {
                id: "gemini".into(),
                name: Some("Gemini".into()),
                provider: Provider::Gemini,
                command: "gemini".into(),
                args: vec![],
                cwd: Some("~".into()),
                env: BTreeMap::new(),
                manual: false,
            },
            AgentConfig {
                id: "aider".into(),
                name: Some("Aider".into()),
                provider: Provider::Aider,
                command: "aider".into(),
                args: vec![],
                cwd: Some("~".into()),
                env: BTreeMap::new(),
                manual: false,
            },
            AgentConfig {
                id: "shell".into(),
                name: Some("Shell".into()),
                provider: Provider::Shell,
                command: std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into()),
                args: vec![],
                cwd: Some("~".into()),
                env: BTreeMap::new(),
                manual: false,
            },
        ],
    }
}
