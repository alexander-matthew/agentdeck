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

impl Config {
    /// Pure check for foot-guns that parse fine but produce confusing
    /// runtime behavior. Returns a list of human-readable warning strings;
    /// an empty Vec means the config is clean. Does no I/O.
    pub fn validate(&self) -> Vec<String> {
        let mut warnings = Vec::new();

        let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
        for agent in &self.agents {
            *seen.entry(agent.id.as_str()).or_insert(0) += 1;
        }
        for (id, count) in &seen {
            if *count > 1 {
                warnings.push(format!(
                    "duplicate agent id {id:?} appears {count} times; logs and the sidebar cannot distinguish these entries"
                ));
            }
        }

        if self.settings.grid_rows == 0 {
            warnings.push(
                "settings.grid_rows = 0; clamped to 1 (set grid_rows >= 1 to silence this)".into(),
            );
        }
        if self.settings.grid_cols == 0 {
            warnings.push(
                "settings.grid_cols = 0; clamped to 1 (set grid_cols >= 1 to silence this)".into(),
            );
        }

        if self.settings.usage_refresh_secs < 5 {
            warnings.push(format!(
                "settings.usage_refresh_secs = {} is below the 5s minimum; clamped to 5",
                self.settings.usage_refresh_secs
            ));
        }

        let known_tags = [
            Provider::Claude.tag(),
            Provider::Codex.tag(),
            Provider::Gemini.tag(),
            Provider::Aider.tag(),
            Provider::Shell.tag(),
            Provider::Other.tag(),
        ];
        for key in self.usage_commands.keys() {
            if !known_tags.contains(&key.as_str()) {
                warnings.push(format!(
                    "usage_commands key {key:?} does not match any known provider tag; entry will be ignored"
                ));
            }
        }

        warnings
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_dir(suffix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "agentdeck-config-test-{}-{}",
            std::process::id(),
            suffix
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn settings_default_matches_documented_values() {
        let s = Settings::default();
        assert_eq!(s.prefix_byte, 0x01);
        assert_eq!(s.detach_key, 'd');
        assert_eq!(s.toggle_key, "ctrl-space");
        assert_eq!(s.grid_rows, 2);
        assert_eq!(s.grid_cols, 2);
        assert_eq!(s.usage_refresh_secs, 60);
    }

    #[test]
    fn default_config_lists_five_agents_in_order() {
        let cfg = default_config();
        let expected: &[(&str, Provider)] = &[
            ("claude", Provider::Claude),
            ("codex", Provider::Codex),
            ("gemini", Provider::Gemini),
            ("aider", Provider::Aider),
            ("shell", Provider::Shell),
        ];
        assert_eq!(cfg.agents.len(), expected.len());
        for (agent, (expected_id, expected_provider)) in cfg.agents.iter().zip(expected) {
            assert_eq!(&agent.id, expected_id);
            assert_eq!(agent.provider, *expected_provider);
            assert!(!agent.display_name().is_empty());
            assert!(!agent.manual);
            assert_eq!(agent.cwd.as_deref(), Some("~"));
        }
        let claude_cmd = cfg
            .usage_commands
            .get("claude")
            .expect("claude usage command present");
        assert!(claude_cmd.contains("ccusage"));
    }

    #[test]
    fn resolved_cwd_expands_tilde_and_passes_through_none() {
        // safety: no other test in this module reads or mutates HOME, so the
        // env-var write here cannot race with another test thread.
        unsafe {
            std::env::set_var("HOME", "/tmp/agentdeck-fake-home");
        }
        let agent = AgentConfig {
            id: "x".into(),
            name: None,
            provider: Provider::Shell,
            command: "sh".into(),
            args: vec![],
            cwd: Some("~".into()),
            env: BTreeMap::new(),
            manual: false,
        };
        assert_eq!(
            agent.resolved_cwd(),
            Some(PathBuf::from("/tmp/agentdeck-fake-home"))
        );

        let no_cwd = AgentConfig { cwd: None, ..agent };
        assert_eq!(no_cwd.resolved_cwd(), None);
    }

    #[test]
    fn provider_tag_round_trips_every_variant() {
        let cases = [
            (Provider::Claude, "claude"),
            (Provider::Codex, "codex"),
            (Provider::Gemini, "gemini"),
            (Provider::Aider, "aider"),
            (Provider::Shell, "shell"),
            (Provider::Other, "other"),
        ];
        for (variant, expected_tag) in cases {
            assert_eq!(variant.tag(), expected_tag);
            let doc = format!("id = \"x\"\ncommand = \"c\"\nprovider = \"{expected_tag}\"\n");
            let parsed: AgentConfig = toml::from_str(&doc).expect("parse agent with provider tag");
            assert_eq!(parsed.provider, variant);
        }
    }

    #[test]
    fn load_or_init_writes_seed_on_first_run() {
        let dir = make_dir("first-run");
        let path = dir.join("config.toml");

        let cfg = load_or_init(&path).expect("first-run load");
        assert!(path.exists(), "config file should be written");
        let raw = std::fs::read_to_string(&path).expect("read written config");
        assert!(raw.contains("[settings]"), "expected [settings] block");
        assert!(
            raw.contains("[[agent]]"),
            "expected at least one [[agent]] block"
        );

        // Round-trip: re-parse the file we just wrote and confirm it matches.
        let reparsed: Config = toml::from_str(&raw).expect("re-parse written config");
        let default = default_config();
        let ids: Vec<&str> = cfg.agents.iter().map(|a| a.id.as_str()).collect();
        let default_ids: Vec<&str> = default.agents.iter().map(|a| a.id.as_str()).collect();
        let reparsed_ids: Vec<&str> = reparsed.agents.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, default_ids);
        assert_eq!(reparsed_ids, default_ids);

        let providers: Vec<Provider> = cfg.agents.iter().map(|a| a.provider).collect();
        let default_providers: Vec<Provider> = default.agents.iter().map(|a| a.provider).collect();
        let reparsed_providers: Vec<Provider> =
            reparsed.agents.iter().map(|a| a.provider).collect();
        assert_eq!(providers, default_providers);
        assert_eq!(reparsed_providers, default_providers);

        assert_eq!(cfg.settings.prefix_byte, default.settings.prefix_byte);
        assert_eq!(cfg.settings.detach_key, default.settings.detach_key);
        assert_eq!(cfg.settings.toggle_key, default.settings.toggle_key);
        assert_eq!(cfg.settings.grid_rows, default.settings.grid_rows);
        assert_eq!(cfg.settings.grid_cols, default.settings.grid_cols);
        assert_eq!(
            cfg.settings.usage_refresh_secs,
            default.settings.usage_refresh_secs
        );
        assert_eq!(reparsed.settings.prefix_byte, default.settings.prefix_byte);
        assert_eq!(reparsed.settings.toggle_key, default.settings.toggle_key);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_or_init_round_trips_custom_settings() {
        let dir = make_dir("custom-grid");
        let path = dir.join("config.toml");
        let body = "\
[settings]
grid_rows = 3

[[agent]]
id = \"only\"
provider = \"shell\"
command = \"sh\"
";
        std::fs::write(&path, body).expect("seed config file");
        let cfg = load_or_init(&path).expect("parse pre-written config");
        assert_eq!(cfg.settings.grid_rows, 3);
        // Unmentioned settings keep their defaults.
        assert_eq!(cfg.settings.grid_cols, 2);
        assert_eq!(cfg.settings.toggle_key, "ctrl-space");
        assert_eq!(cfg.agents.len(), 1);
        assert_eq!(cfg.agents[0].id, "only");
        assert_eq!(cfg.agents[0].provider, Provider::Shell);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn agent(id: &str) -> AgentConfig {
        AgentConfig {
            id: id.into(),
            name: None,
            provider: Provider::Shell,
            command: "sh".into(),
            args: vec![],
            cwd: None,
            env: BTreeMap::new(),
            manual: false,
        }
    }

    #[test]
    fn validate_clean_config_returns_no_warnings() {
        let cfg = Config {
            settings: Settings::default(),
            agents: vec![agent("a"), agent("b")],
            usage_commands: BTreeMap::new(),
        };
        assert!(cfg.validate().is_empty());
    }

    #[test]
    fn validate_default_config_is_clean() {
        assert!(default_config().validate().is_empty());
    }

    #[test]
    fn validate_flags_duplicate_agent_ids() {
        let cfg = Config {
            settings: Settings::default(),
            agents: vec![agent("dup"), agent("dup"), agent("solo")],
            usage_commands: BTreeMap::new(),
        };
        let warnings = cfg.validate();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("duplicate agent id"));
        assert!(warnings[0].contains("\"dup\""));
        assert!(warnings[0].contains("2 times"));
    }

    #[test]
    fn validate_flags_zero_grid_dims() {
        let cfg = Config {
            settings: Settings {
                grid_rows: 0,
                grid_cols: 0,
                ..Settings::default()
            },
            agents: vec![],
            usage_commands: BTreeMap::new(),
        };
        let warnings = cfg.validate();
        assert!(warnings.iter().any(|w| w.contains("grid_rows = 0")));
        assert!(warnings.iter().any(|w| w.contains("grid_cols = 0")));
    }

    #[test]
    fn validate_flags_sub_five_second_usage_refresh() {
        let cfg = Config {
            settings: Settings {
                usage_refresh_secs: 1,
                ..Settings::default()
            },
            agents: vec![],
            usage_commands: BTreeMap::new(),
        };
        let warnings = cfg.validate();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("usage_refresh_secs = 1"));
        assert!(warnings[0].contains("clamped to 5"));
    }

    #[test]
    fn validate_flags_unknown_usage_commands_key() {
        let mut usage_commands = BTreeMap::new();
        usage_commands.insert("claude".into(), "ccusage".into());
        usage_commands.insert("bogus".into(), "echo".into());
        let cfg = Config {
            settings: Settings::default(),
            agents: vec![],
            usage_commands,
        };
        let warnings = cfg.validate();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("\"bogus\""));
    }

    #[test]
    fn load_or_init_returns_err_on_invalid_toml() {
        let dir = make_dir("invalid");
        let path = dir.join("config.toml");
        std::fs::write(&path, "this is = not = valid = toml").expect("write invalid toml");
        assert!(load_or_init(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
