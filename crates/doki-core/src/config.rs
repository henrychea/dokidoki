use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const KNOWN_AGENTS: &[&str] = &["omp", "codex", "claude", "opencode", "pi"];

/// A model name is passed as one argv element, never through a shell, and this
/// keeps it from being read as an option or a second argument.
pub fn valid_model(model: &str) -> bool {
    model
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._:/@+-".contains(c))
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub discovery: Discovery,
    pub roots: Vec<Root>,
    pub sync: Sync,
    pub guards: Guards,
    pub agent: AgentSettings,
    pub agents: BTreeMap<String, AgentEntry>,
    pub overrides: Vec<Override>,
    pub ui: Ui,
    pub notify: Notify,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AgentSettings {
    pub order: Vec<String>,
    pub timeout_seconds: u64,
    pub fallback_on_error: bool,
    pub max_attempts: u32,
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self {
            order: ["omp", "codex"].map(String::from).to_vec(),
            timeout_seconds: 90,
            fallback_on_error: true,
            max_attempts: 2,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AgentEntry {
    pub cli: Option<String>,
    pub model: String,
    pub extra_args: Vec<String>,
    pub timeout_seconds: Option<u64>,
    pub command: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Override {
    pub path: PathBuf,
    pub agent: String,
    #[serde(default)]
    pub model: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Discovery {
    pub rescan_minutes: u32,
    pub skip_names: Vec<String>,
    pub follow_symlinks: bool,
}

impl Default for Discovery {
    fn default() -> Self {
        Self {
            rescan_minutes: 15,
            skip_names: [".git", "node_modules", "target", "vendor", ".venv"]
                .map(String::from)
                .to_vec(),
            follow_symlinks: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Root {
    pub name: String,
    pub path: PathBuf,
    #[serde(default = "default_depth")]
    pub max_depth: usize,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "default_schedule")]
    pub schedule: String,
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CommitScope {
    Tracked,
    All,
    Staged,
}

impl CommitScope {
    fn rank(self) -> u8 {
        match self {
            Self::Staged => 0,
            Self::Tracked => 1,
            Self::All => 2,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Tracked => "tracked",
            Self::All => "all",
            Self::Staged => "staged",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        [Self::Staged, Self::Tracked, Self::All]
            .into_iter()
            .find(|scope| scope.label() == text)
    }

    pub fn narrows(self, base: Self) -> bool {
        self.rank() <= base.rank()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PushMode {
    Push,
    CommitOnly,
    Bundle,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Sync {
    pub concurrency: u32,
    pub commit_scope: CommitScope,
    pub push_mode: PushMode,
    pub protected: Vec<String>,
    pub fetch_timeout_seconds: u64,
    pub push_timeout_seconds: u64,
    pub backoff_max_minutes: u64,
    pub quarantine_after: u32,
}

impl Default for Sync {
    fn default() -> Self {
        Self {
            concurrency: 2,
            commit_scope: CommitScope::Tracked,
            push_mode: PushMode::Push,
            protected: ["main", "master"].map(String::from).to_vec(),
            fetch_timeout_seconds: 120,
            push_timeout_seconds: 300,
            backoff_max_minutes: 360,
            quarantine_after: 5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SecretScanner {
    Gitleaks,
    Trufflehog,
    Off,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Guards {
    pub secret_scan: bool,
    pub secret_scanner: SecretScanner,
    pub max_deleted_lines: u64,
    pub max_changed_files: usize,
    pub max_diff_bytes_for_model: usize,
}

impl Default for Guards {
    fn default() -> Self {
        Self {
            secret_scan: true,
            secret_scanner: SecretScanner::Gitleaks,
            max_deleted_lines: 2000,
            max_changed_files: 200,
            max_diff_bytes_for_model: 60000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Sumi,
    Washi,
    System,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Ui {
    pub theme: Theme,
    pub editor: String,
    pub window: [u32; 2],
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            theme: Theme::Sumi,
            editor: "zed".into(),
            window: [1280, 900],
        }
    }
}

fn default_depth() -> usize {
    2
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Notify {
    pub desktop: bool,
    pub coalesce_seconds: i64,
}

impl Default for Notify {
    fn default() -> Self {
        Self {
            desktop: true,
            coalesce_seconds: 30,
        }
    }
}
fn yes() -> bool {
    true
}
fn default_schedule() -> String {
    "*/20 * * * *".into()
}

pub fn config_path() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config").join("doki/config.toml")
}

pub fn state_dir() -> PathBuf {
    xdg_dir("XDG_STATE_HOME", ".local/state").join("doki")
}

fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home().join(fallback),
    }
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

pub fn expand_tilde(p: &Path) -> PathBuf {
    match p.strip_prefix("~") {
        Ok(rest) => home().join(rest),
        Err(_) => p.to_path_buf(),
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Self> {
        let mut cfg: Config = toml::from_str(text)?;
        for root in &mut cfg.roots {
            root.path = expand_tilde(&root.path);
            if root.name.trim().is_empty() {
                bail!("root name must not be empty");
            }
        }
        let mut names: Vec<_> = cfg.roots.iter().map(|r| r.name.as_str()).collect();
        names.sort_unstable();
        if let Some(w) = names.windows(2).find(|w| w[0] == w[1]) {
            bail!("duplicate root name `{}`", w[0]);
        }
        cfg.validate_agents()?;
        if cfg.notify.coalesce_seconds < 0 {
            bail!("[notify] coalesce_seconds must not be negative");
        }
        Ok(cfg)
    }

    fn validate_agents(&mut self) -> Result<()> {
        if self.agent.max_attempts == 0 {
            bail!("[agent] max_attempts must be at least 1");
        }
        if self.agent.timeout_seconds == 0 {
            bail!("[agent] timeout_seconds must be at least 1");
        }
        for id in &self.agent.order {
            if id.trim().is_empty() {
                bail!("[agent] order must not contain an empty agent id");
            }
            if self.agent_entry(id).is_none() {
                bail!("[agent] order names unknown agent `{id}`");
            }
        }
        for (id, entry) in &self.agents {
            if !valid_model(&entry.model) {
                bail!(
                    "[agents.{id}] model `{}` must match ^[A-Za-z0-9._:/@+-]+$",
                    entry.model
                );
            }
            if entry.cli.as_ref().is_some_and(|c| c.trim().is_empty()) {
                bail!("[agents.{id}] cli must not be empty");
            }
            match &entry.command {
                Some(command) => {
                    if KNOWN_AGENTS.contains(&id.as_str()) {
                        bail!("[agents.{id}] command is only allowed for unknown agent ids");
                    }
                    if command.first().is_none_or(|c| c.trim().is_empty()) {
                        bail!("[agents.{id}] command must start with a binary");
                    }
                }
                None if !KNOWN_AGENTS.contains(&id.as_str()) => {
                    bail!("[agents.{id}] unknown agent id needs command = [\"...\"]");
                }
                None => {}
            }
        }
        for o in &mut self.overrides {
            o.path = expand_tilde(&o.path);
            if o.agent.trim().is_empty() {
                bail!("override for {} must name an agent", o.path.display());
            }
            if !valid_model(&o.model) {
                bail!(
                    "override for {} has model `{}` that must match ^[A-Za-z0-9._:/@+-]+$",
                    o.path.display(),
                    o.model
                );
            }
        }
        Ok(())
    }

    /// A known CLI with no `[agents.<id>]` entry still resolves: the binary is
    /// the id and the CLI picks its own default model.
    pub fn agent_entry(&self, id: &str) -> Option<AgentEntry> {
        self.agents.get(id).cloned().or_else(|| {
            KNOWN_AGENTS.contains(&id).then(|| AgentEntry {
                cli: Some(id.to_string()),
                ..AgentEntry::default()
            })
        })
    }

    pub fn agent_ids(&self) -> Vec<String> {
        let mut ids = self.agent.order.clone();
        for id in self
            .agents
            .keys()
            .map(String::as_str)
            .chain(KNOWN_AGENTS.iter().copied())
        {
            if !ids.iter().any(|known| known == id) {
                ids.push(id.to_string());
            }
        }
        ids
    }

    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).with_context(|| format!("invalid {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_roots_with_defaults() {
        let cfg = Config::parse("[[roots]]\nname = \"dev\"\npath = \"/tmp/dev\"\n").unwrap();
        assert_eq!(cfg.roots[0].max_depth, 2);
        assert!(cfg.roots[0].enabled);
        assert_eq!(cfg.ui.theme, Theme::Sumi);
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(Config::parse("bogus = 1").is_err());
        assert!(Config::parse("[discovery]\nnope = true").is_err());
    }

    #[test]
    fn rejects_duplicate_root_names() {
        let t = "[[roots]]\nname=\"a\"\npath=\"/x\"\n[[roots]]\nname=\"a\"\npath=\"/y\"\n";
        assert!(Config::parse(t).is_err());
    }

    #[test]
    fn parses_sync_and_guards_sections() {
        let cfg = Config::parse(
            "[sync]\nconcurrency = 4\ncommit_scope = \"staged\"\npush_mode = \"commit-only\"\n\
             protected = [\"trunk\"]\n\n[guards]\nsecret_scan = false\nmax_changed_files = 5\n",
        )
        .unwrap();
        assert_eq!(cfg.sync.concurrency, 4);
        assert_eq!(cfg.sync.commit_scope, CommitScope::Staged);
        assert_eq!(cfg.sync.push_mode, PushMode::CommitOnly);
        assert_eq!(cfg.sync.protected, ["trunk"]);
        assert_eq!(cfg.sync.fetch_timeout_seconds, 120);
        assert!(!cfg.guards.secret_scan);
        assert_eq!(cfg.guards.max_changed_files, 5);
        assert_eq!(cfg.guards.max_deleted_lines, 2000);
        assert_eq!(cfg.guards.secret_scanner, SecretScanner::Gitleaks);
    }

    #[test]
    fn rejects_unknown_sync_and_guard_keys() {
        assert!(Config::parse("[sync]\nnope = 1").is_err());
        assert!(Config::parse("[guards]\nnope = 1").is_err());
        assert!(Config::parse("[sync]\ncommit_scope = \"sideways\"").is_err());
    }

    #[test]
    fn parses_agent_sections_and_defaults() {
        let cfg = Config::parse("[agent]\norder = [\"codex\"]\ntimeout_seconds = 30\n").unwrap();
        assert_eq!(cfg.agent.order, ["codex"]);
        assert_eq!(cfg.agent.timeout_seconds, 30);
        assert_eq!(cfg.agent.max_attempts, 2);
        assert!(cfg.agent.fallback_on_error);
        assert!(cfg.agents.is_empty());

        let cfg = Config::parse(
            "[agents.omp]\ncli = \"omp\"\nmodel = \"anthropic/claude-sonnet-4.5\"\n\
             extra_args = [\"--no-session\"]\ntimeout_seconds = 10\n\n\
             [[overrides]]\npath = \"~/Developer/billing\"\nagent = \"codex\"\nmodel = \"gpt-5.2\"\n",
        )
        .unwrap();
        let omp = cfg.agent_entry("omp").unwrap();
        assert_eq!(omp.model, "anthropic/claude-sonnet-4.5");
        assert_eq!(omp.extra_args, ["--no-session"]);
        assert_eq!(omp.timeout_seconds, Some(10));
        assert_eq!(cfg.overrides[0].agent, "codex");
        assert!(!cfg.overrides[0].path.starts_with("~"));

        assert_eq!(cfg.agent_ids()[0], "omp");
        assert!(cfg.agent_ids().contains(&"claude".to_string()));
    }

    #[test]
    fn known_agent_without_an_entry_uses_the_cli_default() {
        let cfg = Config::parse("").unwrap();
        assert_eq!(cfg.agent.order, ["omp", "codex"]);
        let omp = cfg.agent_entry("omp").unwrap();
        assert_eq!(omp.cli.as_deref(), Some("omp"));
        assert_eq!(omp.model, "");
        assert!(cfg.agent_entry("nope").is_none());
    }

    #[test]
    fn rejects_invalid_models_and_agent_commands() {
        for text in [
            "[agents.omp]\nmodel = \"bad model\"\n",
            "[agents.omp]\nmodel = \"a;rm -rf /\"\n",
            "[[overrides]]\npath = \"/tmp/x\"\nagent = \"codex\"\nmodel = \"o3 $(id)\"\n",
            "[agents.omp]\ncommand = [\"curl\"]\n",
            "[agents.mine]\nmodel = \"m\"\n",
            "[agent]\norder = [\"ghost\"]\n",
            "[agent]\norder = [\"ghost\"]\n\n[agents.ghost]\nmodel = \"m\"\n",
            "[agent]\nmax_attempts = 0\n",
            "[agents.omp]\ncli = \"  \"\n",
        ] {
            assert!(Config::parse(text).is_err(), "accepted {text:?}");
        }
        assert!(Config::parse("[agents.omp]\nmodel = \"\"\n").is_ok());
        assert!(Config::parse("[agents.omp]\nmodel = \"a/b-c.d:e@f+1\"\n").is_ok());
        assert!(Config::parse("[agents.mine]\ncommand = [\"mine-cli\", \"{prompt}\"]\n").is_ok());
    }

    #[test]
    fn parses_notify_and_rejects_bad_coalescing() {
        let cfg = Config::parse("[notify]\ndesktop = false\ncoalesce_seconds = 5\n").unwrap();
        assert!(!cfg.notify.desktop);
        assert_eq!(cfg.notify.coalesce_seconds, 5);
        assert!(Config::default().notify.desktop);
        assert_eq!(Config::default().notify.coalesce_seconds, 30);
        assert!(Config::parse("[notify]\ncoalesce_seconds = -1\n").is_err());
        assert!(Config::parse("[notify]\nnope = 1\n").is_err());
    }

    #[test]
    fn expands_tilde() {
        let p = expand_tilde(Path::new("~/Developer"));
        assert!(p.ends_with("Developer") && !p.starts_with("~"));
    }
}
