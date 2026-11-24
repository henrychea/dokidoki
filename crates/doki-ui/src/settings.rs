use crate::fonts;
use crate::theme;
use crate::{DokiApp, dot, fade};
use doki_agent::adapter::on_path;
use doki_agent::parse;
use doki_agent::resolve::resolve_id;
use doki_agent::run::run;
use doki_core::config::{
    CommitScope, Config, KNOWN_AGENTS, PushMode, SecretScanner, Theme, valid_model,
};
use doki_core::config_edit;
use doki_core::scheduler;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

const TEST_PROMPT: &str = "Reply with exactly one line and nothing else: doki: agent test";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsError {
    pub field: String,
    pub message: String,
}

impl SettingsError {
    pub fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootDraft {
    pub name: String,
    pub path: String,
    pub depth: String,
    pub schedule: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentDraft {
    pub id: String,
    pub cli: String,
    pub model: String,
    pub extra_args: String,
    pub timeout: String,
    pub known: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsDraft {
    pub roots: Vec<RootDraft>,
    pub concurrency: String,
    pub commit_scope: CommitScope,
    pub push_mode: PushMode,
    pub fetch_timeout: String,
    pub push_timeout: String,
    pub quarantine_after: String,
    pub secret_scan: bool,
    pub secret_scanner: SecretScanner,
    pub max_deleted_lines: String,
    pub max_changed_files: String,
    pub max_diff_kb: String,
    pub agents: Vec<AgentDraft>,
    pub theme: Theme,
    pub editor: String,
}

impl SettingsDraft {
    pub fn from_config(cfg: &Config) -> Self {
        let agents = cfg
            .agent_ids()
            .into_iter()
            .filter_map(|id| {
                let entry = cfg.agent_entry(&id)?;
                Some(AgentDraft {
                    cli: entry.cli.unwrap_or_else(|| id.clone()),
                    id: id.clone(),
                    model: entry.model,
                    extra_args: entry.extra_args.join(" "),
                    timeout: entry
                        .timeout_seconds
                        .map(|seconds| seconds.to_string())
                        .unwrap_or_default(),
                    known: KNOWN_AGENTS.contains(&id.as_str()),
                })
            })
            .collect();
        Self {
            roots: cfg
                .roots
                .iter()
                .map(|root| RootDraft {
                    name: root.name.clone(),
                    path: tilde_path(&root.path),
                    depth: root.max_depth.to_string(),
                    schedule: root.schedule.clone(),
                    enabled: root.enabled,
                })
                .collect(),
            concurrency: cfg.sync.concurrency.to_string(),
            commit_scope: cfg.sync.commit_scope,
            push_mode: cfg.sync.push_mode,
            fetch_timeout: cfg.sync.fetch_timeout_seconds.to_string(),
            push_timeout: cfg.sync.push_timeout_seconds.to_string(),
            quarantine_after: cfg.sync.quarantine_after.to_string(),
            secret_scan: cfg.guards.secret_scan,
            secret_scanner: cfg.guards.secret_scanner,
            max_deleted_lines: cfg.guards.max_deleted_lines.to_string(),
            max_changed_files: cfg.guards.max_changed_files.to_string(),
            max_diff_kb: (cfg.guards.max_diff_bytes_for_model / 1000).to_string(),
            agents,
            theme: cfg.ui.theme,
            editor: cfg.ui.editor.clone(),
        }
    }

    pub fn apply(&self, original: &str) -> Result<String, SettingsError> {
        let mut table: toml::Table = if original.trim().is_empty() {
            toml::Table::new()
        } else {
            toml::from_str(original).map_err(|error| {
                SettingsError::new("form", format!("config does not parse: {error}"))
            })?
        };
        let original_roots = table
            .get("roots")
            .and_then(toml::Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut roots = Vec::with_capacity(self.roots.len());
        for root in &self.roots {
            let field = format!("root.{}.depth", root.name);
            let depth = positive(root.depth.trim(), &field)? as i64;
            scheduler::parse(root.schedule.trim()).map_err(|error| {
                SettingsError::new(format!("root.{}.schedule", root.name), format!("{error:#}"))
            })?;
            if root.path.trim().is_empty() {
                return Err(SettingsError::new(
                    format!("root.{}.path", root.name),
                    "path must not be empty",
                ));
            }
            let mut row = original_roots
                .iter()
                .find(|value| {
                    value
                        .as_table()
                        .and_then(|row| row.get("name"))
                        .and_then(toml::Value::as_str)
                        == Some(root.name.as_str())
                })
                .and_then(toml::Value::as_table)
                .cloned()
                .unwrap_or_default();
            row.insert("name".into(), toml::Value::String(root.name.clone()));
            row.insert(
                "path".into(),
                toml::Value::String(root.path.trim().to_string()),
            );
            row.insert("max_depth".into(), toml::Value::Integer(depth));
            row.insert("enabled".into(), toml::Value::Boolean(root.enabled));
            row.insert(
                "schedule".into(),
                toml::Value::String(root.schedule.trim().to_string()),
            );
            roots.push(toml::Value::Table(row));
        }
        table.insert("roots".into(), toml::Value::Array(roots));

        let sync = table_at(&mut table, "sync");
        sync.insert(
            "concurrency".into(),
            toml::Value::Integer(positive(&self.concurrency, "sync.concurrency")? as i64),
        );
        sync.insert(
            "commit_scope".into(),
            toml::Value::String(self.commit_scope.label().into()),
        );
        sync.insert(
            "push_mode".into(),
            toml::Value::String(push_mode_label(self.push_mode).into()),
        );
        sync.insert(
            "fetch_timeout_seconds".into(),
            toml::Value::Integer(positive(&self.fetch_timeout, "sync.fetch_timeout")? as i64),
        );
        sync.insert(
            "push_timeout_seconds".into(),
            toml::Value::Integer(positive(&self.push_timeout, "sync.push_timeout")? as i64),
        );
        sync.insert(
            "quarantine_after".into(),
            toml::Value::Integer(positive(&self.quarantine_after, "sync.quarantine_after")? as i64),
        );

        let guards = table_at(&mut table, "guards");
        guards.insert("secret_scan".into(), toml::Value::Boolean(self.secret_scan));
        guards.insert(
            "secret_scanner".into(),
            toml::Value::String(scanner_label(self.secret_scanner).into()),
        );
        guards.insert(
            "max_deleted_lines".into(),
            toml::Value::Integer(
                positive(&self.max_deleted_lines, "guards.max_deleted_lines")? as i64,
            ),
        );
        guards.insert(
            "max_changed_files".into(),
            toml::Value::Integer(
                positive(&self.max_changed_files, "guards.max_changed_files")? as i64,
            ),
        );
        let kb = positive(&self.max_diff_kb, "guards.max_diff_kb")?;
        guards.insert(
            "max_diff_bytes_for_model".into(),
            toml::Value::Integer((kb.saturating_mul(1000)) as i64),
        );

        let agents = table_at(&mut table, "agents");
        for agent in &self.agents {
            let field = format!("agent.{}.model", agent.id);
            let model = agent.model.trim();
            if !valid_model(model) {
                return Err(SettingsError::new(
                    field,
                    format!("`{model}` must match ^[A-Za-z0-9._:/@+-]+$"),
                ));
            }
            let mut row = agents
                .get(&agent.id)
                .and_then(toml::Value::as_table)
                .cloned()
                .unwrap_or_default();
            if agent.cli.trim().is_empty() {
                row.remove("cli");
            } else {
                row.insert(
                    "cli".into(),
                    toml::Value::String(agent.cli.trim().to_string()),
                );
            }
            row.insert("model".into(), toml::Value::String(model.to_string()));
            let extra: Vec<toml::Value> = agent
                .extra_args
                .split_whitespace()
                .map(|arg| toml::Value::String(arg.to_string()))
                .collect();
            if extra.is_empty() {
                row.remove("extra_args");
            } else {
                row.insert("extra_args".into(), toml::Value::Array(extra));
            }
            if agent.timeout.trim().is_empty() {
                row.remove("timeout_seconds");
            } else {
                row.insert(
                    "timeout_seconds".into(),
                    toml::Value::Integer(positive(
                        agent.timeout.trim(),
                        format!("agent.{}.timeout", agent.id),
                    )? as i64),
                );
            }
            agents.insert(agent.id.clone(), toml::Value::Table(row));
        }

        let editor = self.editor.trim();
        if editor.is_empty() {
            return Err(SettingsError::new("ui.editor", "editor must not be empty"));
        }
        let ui = table_at(&mut table, "ui");
        ui.insert(
            "theme".into(),
            toml::Value::String(theme_name(self.theme).into()),
        );
        ui.insert("editor".into(), toml::Value::String(editor.to_string()));

        let text = toml::to_string_pretty(&table)
            .map_err(|error| SettingsError::new("form", format!("{error}")))?;
        Config::parse(&text).map_err(|error| SettingsError::new("form", format!("{error:#}")))?;
        Ok(text)
    }
}

/// Settings shows roots the way the header does; `Config::parse` expands them.
fn tilde_path(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    if let Some(home) = home
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() {
            "~".into()
        } else {
            format!("~/{}", rest.display())
        };
    }
    path.display().to_string()
}

fn positive(value: &str, field: impl Into<String>) -> Result<u64, SettingsError> {
    let field = field.into();
    match value.trim().parse::<u64>() {
        Ok(number) if number > 0 => Ok(number),
        Ok(_) => Err(SettingsError::new(field, "must be at least 1")),
        Err(_) => Err(SettingsError::new(field, "must be a whole number")),
    }
}

fn table_at<'a>(root: &'a mut toml::Table, key: &str) -> &'a mut toml::Table {
    if !root.get(key).is_some_and(toml::Value::is_table) {
        root.insert(key.into(), toml::Value::Table(toml::Table::new()));
    }
    root.get_mut(key)
        .and_then(toml::Value::as_table_mut)
        .expect("just inserted a table")
}

pub fn push_mode_label(mode: PushMode) -> &'static str {
    match mode {
        PushMode::Push => "push",
        PushMode::CommitOnly => "commit-only",
        PushMode::Bundle => "bundle",
    }
}

pub fn push_modes() -> [PushMode; 3] {
    [PushMode::Push, PushMode::CommitOnly, PushMode::Bundle]
}

pub fn scanner_label(scanner: SecretScanner) -> &'static str {
    match scanner {
        SecretScanner::Gitleaks => "gitleaks",
        SecretScanner::Trufflehog => "trufflehog",
        SecretScanner::Off => "off",
    }
}

pub fn scanners() -> [SecretScanner; 3] {
    [
        SecretScanner::Gitleaks,
        SecretScanner::Trufflehog,
        SecretScanner::Off,
    ]
}

pub fn theme_name(theme: Theme) -> &'static str {
    match theme {
        Theme::Sumi => "sumi",
        Theme::Washi => "washi",
        Theme::System => "system",
    }
}

pub fn themes() -> [Theme; 3] {
    [Theme::Sumi, Theme::Washi, Theme::System]
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TestOutcome {
    Ok { message: String, latency: String },
    Err(String),
}

impl TestOutcome {
    pub fn label(&self) -> String {
        match self {
            Self::Ok { message, latency } => format!("{message} · {latency}"),
            Self::Err(error) => error.clone(),
        }
    }

    pub fn failed(&self) -> bool {
        matches!(self, Self::Err(_))
    }
}

/// One fixed prompt through one CLI, on a worker thread; the UI thread only
/// renders the answer.
pub fn test_agent(cfg: &Config, id: &str) -> TestOutcome {
    let resolved = match resolve_id(cfg, id) {
        Ok(resolved) => resolved,
        Err(error) => return TestOutcome::Err(format!("{error:#}")),
    };
    if !on_path(&resolved.cli) {
        return TestOutcome::Err(format!("`{}` is not on PATH", resolved.cli));
    }
    match run(&resolved, TEST_PROMPT) {
        Ok(result) if !result.status.success() => TestOutcome::Err(format!(
            "`{}` exited {}: {}",
            resolved.cli,
            result.status,
            result.stderr_tail()
        )),
        Ok(result) => match parse::parse(result.output()) {
            Some(message) => TestOutcome::Ok {
                message: message.subject,
                latency: format!("{:.2}s", result.latency.as_secs_f64()),
            },
            None => TestOutcome::Err(format!(
                "no usable message in output: {}",
                result.output().chars().take(120).collect::<String>()
            )),
        },
        Err(error) => TestOutcome::Err(format!("{error:#}")),
    }
}

pub struct SettingsView {
    pub draft: SettingsDraft,
    fields: HashMap<String, Entity<InputState>>,
    pub error: Option<SettingsError>,
    pub saved: bool,
    pub testing: Option<String>,
    pub results: BTreeMap<String, TestOutcome>,
    pub found: HashMap<String, bool>,
    pub add_root: Entity<InputState>,
}

impl SettingsView {
    pub fn new(cfg: &Config, window: &mut Window, cx: &mut Context<DokiApp>) -> Self {
        let add_root = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("~/Projects")
                .submit_on_enter(true)
        });
        let mut view = Self {
            draft: SettingsDraft::from_config(cfg),
            fields: HashMap::new(),
            error: None,
            saved: false,
            testing: None,
            results: BTreeMap::new(),
            found: HashMap::new(),
            add_root,
        };
        view.ensure_fields(window, cx);
        view.refresh_found();
        view
    }

    fn refresh_found(&mut self) {
        self.found.clear();
        for agent in &self.draft.agents {
            self.found.insert(agent.id.clone(), on_path(&agent.cli));
        }
    }

    fn values(&self) -> Vec<(String, String)> {
        let mut fields = Vec::new();
        for root in &self.draft.roots {
            fields.push((format!("root.{}.path", root.name), root.path.clone()));
            fields.push((format!("root.{}.depth", root.name), root.depth.clone()));
            fields.push((
                format!("root.{}.schedule", root.name),
                root.schedule.clone(),
            ));
        }
        fields.push(("sync.concurrency".into(), self.draft.concurrency.clone()));
        fields.push((
            "sync.fetch_timeout".into(),
            self.draft.fetch_timeout.clone(),
        ));
        fields.push(("sync.push_timeout".into(), self.draft.push_timeout.clone()));
        fields.push((
            "sync.quarantine_after".into(),
            self.draft.quarantine_after.clone(),
        ));
        fields.push((
            "guards.max_deleted_lines".into(),
            self.draft.max_deleted_lines.clone(),
        ));
        fields.push((
            "guards.max_changed_files".into(),
            self.draft.max_changed_files.clone(),
        ));
        fields.push(("guards.max_diff_kb".into(), self.draft.max_diff_kb.clone()));
        for agent in &self.draft.agents {
            fields.push((format!("agent.{}.cli", agent.id), agent.cli.clone()));
            fields.push((format!("agent.{}.model", agent.id), agent.model.clone()));
            fields.push((
                format!("agent.{}.extra_args", agent.id),
                agent.extra_args.clone(),
            ));
            fields.push((format!("agent.{}.timeout", agent.id), agent.timeout.clone()));
        }
        fields.push(("ui.editor".into(), self.draft.editor.clone()));
        fields
    }

    fn ensure_fields(&mut self, window: &mut Window, cx: &mut Context<DokiApp>) {
        for (key, value) in self.values() {
            if self.fields.contains_key(&key) {
                continue;
            }
            let input = cx.new(|cx| InputState::new(window, cx).submit_on_enter(false));
            input.update(cx, |state, cx| state.set_value(value, window, cx));
            self.fields.insert(key, input);
        }
    }

    pub fn field(&self, key: &str) -> Option<&Entity<InputState>> {
        self.fields.get(key)
    }

    /// Reads the inputs back into the draft, so typing needs no change event.
    fn collect(&self, cx: &App) -> SettingsDraft {
        let value = |key: &str, fallback: &str| {
            self.fields
                .get(key)
                .map(|input| input.read(cx).value().to_string())
                .unwrap_or_else(|| fallback.to_string())
        };
        let mut draft = self.draft.clone();
        for root in &mut draft.roots {
            root.path = value(&format!("root.{}.path", root.name), &root.path);
            root.depth = value(&format!("root.{}.depth", root.name), &root.depth);
            root.schedule = value(&format!("root.{}.schedule", root.name), &root.schedule);
        }
        draft.concurrency = value("sync.concurrency", &draft.concurrency);
        draft.fetch_timeout = value("sync.fetch_timeout", &draft.fetch_timeout);
        draft.push_timeout = value("sync.push_timeout", &draft.push_timeout);
        draft.quarantine_after = value("sync.quarantine_after", &draft.quarantine_after);
        draft.max_deleted_lines = value("guards.max_deleted_lines", &draft.max_deleted_lines);
        draft.max_changed_files = value("guards.max_changed_files", &draft.max_changed_files);
        draft.max_diff_kb = value("guards.max_diff_kb", &draft.max_diff_kb);
        for agent in &mut draft.agents {
            agent.cli = value(&format!("agent.{}.cli", agent.id), &agent.cli);
            agent.model = value(&format!("agent.{}.model", agent.id), &agent.model);
            agent.extra_args = value(&format!("agent.{}.extra_args", agent.id), &agent.extra_args);
            agent.timeout = value(&format!("agent.{}.timeout", agent.id), &agent.timeout);
        }
        draft.editor = value("ui.editor", &draft.editor);
        draft
    }
}

impl DokiApp {
    #[doc(hidden)]
    pub fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_palette(window, cx);
        self.settings = Some(SettingsView::new(&self.cfg, window, cx));
        self.avatar_open = false;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    pub(crate) fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.take().is_some() {
            self.apply_theme(self.cfg.ui.theme, cx);
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    #[doc(hidden)]
    pub fn focus_settings_field(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = self
            .settings
            .as_ref()
            .and_then(|view| view.field(key))
            .cloned()
        {
            input.update(cx, |state, cx| state.focus(window, cx));
        }
    }

    fn rebuild_settings(&mut self, cfg: &Config, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.settings.as_mut() {
            view.draft = SettingsDraft::from_config(cfg);
            view.ensure_fields(window, cx);
            view.refresh_found();
            view.error = None;
            view.saved = false;
        }
    }

    pub(crate) fn save_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.settings.as_ref() else {
            return;
        };
        let draft = view.collect(cx);
        let original = match std::fs::read_to_string(&self.cfg_path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => {
                self.settings_error(SettingsError::new(
                    "form",
                    format!("read {}: {error}", self.cfg_path.display()),
                ));
                cx.notify();
                return;
            }
        };
        match draft.apply(&original).and_then(|text| {
            config_edit::write_config(&self.cfg_path, &text)
                .map(|()| text)
                .map_err(|error| SettingsError::new("form", format!("{error:#}")))
        }) {
            Ok(_) => match Config::load(&self.cfg_path) {
                Ok(cfg) => {
                    let cfg2 = cfg.clone();
                    self.cfg = cfg;
                    self.apply_theme(self.cfg.ui.theme, cx);
                    self.rebuild_settings(&cfg2, window, cx);
                    if let Some(view) = self.settings.as_mut() {
                        view.saved = true;
                    }
                    self.relabel_roots();
                    self.reload();
                    self.rescan(cx);
                    self.reload_journal();
                }
                Err(error) => {
                    self.settings_error(SettingsError::new("form", format!("{error:#}")));
                }
            },
            Err(error) => self.settings_error(error),
        }
        cx.notify();
    }

    fn settings_error(&mut self, error: SettingsError) {
        if let Some(view) = self.settings.as_mut() {
            view.error = Some(error);
            view.saved = false;
        }
    }

    fn relabel_roots(&mut self) {
        for repo in &mut self.repos {
            if let Some(root) = self.cfg.roots.iter().find(|root| root.name == repo.root) {
                repo.root_path = root.path.clone();
            }
        }
    }

    pub(crate) fn add_settings_root(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self
            .settings
            .as_ref()
            .map(|view| view.add_root.read(cx).value().to_string())
            .unwrap_or_default();
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return;
        }
        let path = doki_core::config::expand_tilde(Path::new(trimmed));
        if !path.is_dir() {
            self.settings_error(SettingsError::new(
                "root.new",
                format!("{} is not a directory", path.display()),
            ));
            cx.notify();
            return;
        }
        let base = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "root".into());
        if let Some(view) = self.settings.as_mut() {
            let mut name = base.clone();
            let mut suffix = 2;
            while view.draft.roots.iter().any(|root| root.name == name) {
                name = format!("{base}-{suffix}");
                suffix += 1;
            }
            view.draft.roots.push(RootDraft {
                name,
                path: tilde_path(&path),
                depth: "2".into(),
                schedule: scheduler::DEFAULT_SCHEDULE.into(),
                enabled: true,
            });
            view.add_root
                .update(cx, |state, cx| state.set_value("", window, cx));
            view.ensure_fields(window, cx);
            view.error = None;
        }
        cx.notify();
    }

    pub(crate) fn remove_settings_root(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(view) = self.settings.as_mut()
            && index < view.draft.roots.len()
        {
            view.draft.roots.remove(index);
            view.error = None;
        }
        cx.notify();
    }

    pub(crate) fn test_settings_agent(&mut self, id: String, cx: &mut Context<Self>) {
        if let Some(view) = self.settings.as_mut() {
            view.testing = Some(id.clone());
            view.results.remove(&id);
        }
        let cfg = self.cfg.clone();
        let work_id = id.clone();
        self.off_thread(
            cx,
            move || test_agent(&cfg, &work_id),
            move |this, result, cx| {
                if let Some(view) = this.settings.as_mut() {
                    view.testing = None;
                    view.results.insert(id, result);
                }
                cx.notify();
            },
        );
        cx.notify();
    }

    pub(crate) fn settings_center(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = self.p;
        let Some(draft) = self.settings.as_ref().map(|view| view.draft.clone()) else {
            return div().into_any_element();
        };
        let error = self.settings.as_ref().and_then(|view| view.error.clone());
        let saved = self.settings.as_ref().is_some_and(|view| view.saved);
        let found = self
            .settings
            .as_ref()
            .map(|view| view.found.clone())
            .unwrap_or_default();
        let testing = self.settings.as_ref().and_then(|view| view.testing.clone());
        let results = self
            .settings
            .as_ref()
            .map(|view| view.results.clone())
            .unwrap_or_default();
        let add_root = self.settings.as_ref().map(|view| view.add_root.clone());
        let cfg_path = self.cfg_path.display().to_string();

        let mut body = v_flex().gap(px(16.)).p(px(24.)).w_full();
        if let Some(error) = error {
            body = body.child(
                div()
                    .id("settings-error")
                    .test_support()
                    .aria_label(error.message.clone())
                    .px(px(10.))
                    .py(px(6.))
                    .rounded(px(4.))
                    .bg(p.danger_soft)
                    .border_1()
                    .border_color(p.danger_border)
                    .text_size(px(11.))
                    .font_family(fonts::MONO)
                    .text_color(p.danger)
                    .child(format!("{}: {}", error.field, error.message)),
            );
        }
        if saved {
            body = body.child(
                div()
                    .id("settings-saved")
                    .test_support()
                    .aria_label("Config saved")
                    .text_size(px(11.))
                    .font_family(fonts::MONO)
                    .text_color(p.success)
                    .child("Config saved · config.toml.bak kept"),
            );
        }

        let mut roots =
            self.settings_card("Roots", "Every root is scanned depth-first, then scheduled");
        for (index, root) in draft.roots.iter().enumerate() {
            let name = root.name.clone();
            let path = root.path.clone();
            let enabled = root.enabled;
            roots = roots.child(
                v_flex()
                    .gap(px(6.))
                    .p(px(8.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(p.hairline_soft)
                    .child(
                        h_flex()
                            .justify_between()
                            .gap(px(8.))
                            .child(
                                h_flex()
                                    .gap(px(8.))
                                    .min_w_0()
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .font_family(fonts::MONO)
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(p.text)
                                            .child(name.clone()),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(px(10.))
                                            .font_family(fonts::MONO)
                                            .text_color(p.text_dim)
                                            .child(path),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .gap(px(6.))
                                    .child(
                                        Switch::new(("root-enabled", index as u64))
                                            .checked(enabled)
                                            .small()
                                            .color(p.success)
                                            .on_change(cx.listener(
                                                move |this, on: &bool, _, cx| {
                                                    if let Some(view) = this.settings.as_mut()
                                                        && let Some(root) =
                                                            view.draft.roots.get_mut(index)
                                                    {
                                                        root.enabled = *on;
                                                    }
                                                    cx.notify();
                                                },
                                            )),
                                    )
                                    .child(self.settings_text_button(
                                        format!("root-remove-{index}"),
                                        "Remove",
                                        p.danger,
                                        cx.listener(move |this, _, _, cx| {
                                            this.remove_settings_root(index, cx)
                                        }),
                                    )),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .child(self.settings_field(
                                format!("root.{name}.depth"),
                                "Depth",
                                px(52.),
                            ))
                            .child(self.settings_field(
                                format!("root.{name}.schedule"),
                                "Schedule",
                                px(150.),
                            )),
                    ),
            );
        }
        roots = roots.child(
            h_flex()
                .gap(px(6.))
                .pt(px(4.))
                .when_some(add_root, |row, input| {
                    row.child(self.settings_input(&input, "settings-add-root", px(240.)))
                })
                .child(self.settings_text_button(
                    "settings-add-confirm".into(),
                    "Add root",
                    p.text_muted,
                    cx.listener(|this, _, window, cx| this.add_settings_root(window, cx)),
                )),
        );
        body = body.child(roots);

        let mut sync = self.settings_card("Sync", "How a run commits, fetches and backs off");
        sync = sync
            .child(self.settings_field("sync.concurrency".into(), "Concurrency", px(90.)))
            .child(self.settings_choice_row(
                "Commit scope",
                ["Staged", "Tracked", "All"].map(String::from).to_vec(),
                draft.commit_scope.label(),
                cx,
                |this, label, cx| {
                    let scope = match label {
                        "Staged" => CommitScope::Staged,
                        "Tracked" => CommitScope::Tracked,
                        _ => CommitScope::All,
                    };
                    if let Some(view) = this.settings.as_mut() {
                        view.draft.commit_scope = scope;
                    }
                    cx.notify();
                },
            ))
            .child(
                self.settings_choice_row(
                    "Push mode",
                    push_modes()
                        .iter()
                        .map(|mode| push_mode_label(*mode).to_string())
                        .collect(),
                    push_mode_label(draft.push_mode),
                    cx,
                    |this, label, cx| {
                        let mode = match label {
                            "commit-only" => PushMode::CommitOnly,
                            "bundle" => PushMode::Bundle,
                            _ => PushMode::Push,
                        };
                        if let Some(view) = this.settings.as_mut() {
                            view.draft.push_mode = mode;
                        }
                        cx.notify();
                    },
                ),
            )
            .child(self.settings_field("sync.fetch_timeout".into(), "Fetch timeout (s)", px(90.)))
            .child(self.settings_field("sync.push_timeout".into(), "Push timeout (s)", px(90.)))
            .child(self.settings_field(
                "sync.quarantine_after".into(),
                "Quarantine after",
                px(90.),
            ));
        body = body.child(sync);

        let mut guards = self.settings_card("Guards", "The pre-flight checks that can stop a run");
        let scan = draft.secret_scan;
        guards = guards
            .child(
                h_flex()
                    .justify_between()
                    .py(px(2.))
                    .child(self.settings_label("Secret scan"))
                    .child(
                        Switch::new("guard-secret-scan")
                            .checked(scan)
                            .small()
                            .color(p.success)
                            .on_change(cx.listener(|this, on: &bool, _, cx| {
                                if let Some(view) = this.settings.as_mut() {
                                    view.draft.secret_scan = *on;
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(
                self.settings_choice_row(
                    "Scanner",
                    scanners()
                        .iter()
                        .map(|scanner| scanner_label(*scanner).to_string())
                        .collect(),
                    scanner_label(draft.secret_scanner),
                    cx,
                    |this, label, cx| {
                        let scanner = match label {
                            "trufflehog" => SecretScanner::Trufflehog,
                            "off" => SecretScanner::Off,
                            _ => SecretScanner::Gitleaks,
                        };
                        if let Some(view) = this.settings.as_mut() {
                            view.draft.secret_scanner = scanner;
                        }
                        cx.notify();
                    },
                ),
            )
            .child(self.settings_field(
                "guards.max_deleted_lines".into(),
                "Max deleted lines",
                px(90.),
            ))
            .child(self.settings_field(
                "guards.max_changed_files".into(),
                "Max changed files",
                px(90.),
            ))
            .child(self.settings_field("guards.max_diff_kb".into(), "Model context (KB)", px(90.)));
        body = body.child(guards);

        let mut agents = self.settings_card("Agents", "One row per CLI, with a fixed-prompt test");
        for agent in &draft.agents {
            let id = agent.id.clone();
            let known = agent.known;
            let found = found.get(&id).copied().unwrap_or(false);
            let testing = testing.as_deref() == Some(id.as_str());
            let result = results.get(&id).cloned();
            let test_id = id.clone();
            let mut row = v_flex()
                .gap(px(6.))
                .p(px(8.))
                .rounded(px(4.))
                .border_1()
                .border_color(p.hairline_soft)
                .child(
                    h_flex()
                        .justify_between()
                        .gap(px(8.))
                        .child(
                            h_flex()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .text_size(px(12.))
                                        .font_family(fonts::MONO)
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(p.text)
                                        .child(id.clone()),
                                )
                                .child(dot(px(6.), if found { p.success } else { p.text_dim }))
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .font_family(fonts::MONO)
                                        .text_color(if found { p.text_muted } else { p.text_dim })
                                        .child(if found {
                                            "found on PATH"
                                        } else {
                                            "missing from PATH"
                                        }),
                                )
                                .when(!known, |line| {
                                    line.child(
                                        div()
                                            .text_size(px(10.))
                                            .font_family(fonts::MONO)
                                            .text_color(p.text_dim)
                                            .child("custom command"),
                                    )
                                }),
                        )
                        .child(self.settings_text_button(
                            format!("agent-test-{id}"),
                            if testing { "Testing…" } else { "Test" },
                            if testing { p.text_dim } else { p.text_muted },
                            cx.listener(move |this, _, _, cx| {
                                this.test_settings_agent(test_id.clone(), cx)
                            }),
                        )),
                )
                .child(
                    h_flex()
                        .gap(px(8.))
                        .child(self.settings_field(format!("agent.{id}.cli"), "Binary", px(110.)))
                        .child(self.settings_field(format!("agent.{id}.model"), "Model", px(150.)))
                        .child(self.settings_field(
                            format!("agent.{id}.timeout"),
                            "Timeout",
                            px(70.),
                        )),
                )
                .child(self.settings_field(
                    format!("agent.{id}.extra_args"),
                    "Extra args",
                    px(260.),
                ));
            if let Some(result) = result {
                row = row.child(
                    div()
                        .id(ElementId::Name(format!("agent-test-result-{id}").into()))
                        .test_support()
                        .aria_label(result.label())
                        .text_size(px(10.))
                        .font_family(fonts::MONO)
                        .text_color(if result.failed() { p.danger } else { p.success })
                        .child(result.label()),
                );
            }
            agents = agents.child(row);
        }
        body = body.child(agents);

        let selected_theme = theme::label(draft.theme).to_string();
        let appearance = self
            .settings_card("Appearance", "Sumi ink, Washi paper, or the window")
            .child(
                self.settings_choice_row(
                    "Theme",
                    themes()
                        .iter()
                        .map(|theme| theme::label(*theme).to_string())
                        .collect(),
                    &selected_theme,
                    cx,
                    |this, label, cx| {
                        let theme = match label {
                            "Washi" => Theme::Washi,
                            "System" => Theme::System,
                            _ => Theme::Sumi,
                        };
                        if let Some(view) = this.settings.as_mut() {
                            view.draft.theme = theme;
                        }
                        this.apply_theme(theme, cx);
                        cx.notify();
                    },
                ),
            );
        body = body.child(appearance);

        let editor = self
            .settings_card("Editor", "The program behind Open in <editor>")
            .child(self.settings_field("ui.editor".into(), "Command", px(220.)));
        body = body.child(editor);

        v_flex()
            .id("settings-view")
            .test_support()
            .size_full()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .flex_shrink_0()
                    .px(px(24.))
                    .py(px(16.))
                    .gap(px(8.))
                    .border_b_1()
                    .border_color(p.hairline)
                    .bg(fade(p.pane_raised, 0.4))
                    .child(
                        h_flex()
                            .justify_between()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(2.))
                                    .child(
                                        div()
                                            .text_size(px(18.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(p.text)
                                            .child("Settings"),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(px(11.))
                                            .font_family(fonts::MONO)
                                            .text_color(p.text_dim)
                                            .child(format!(
                                                "{cfg_path} · validated before every write"
                                            )),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .flex_shrink_0()
                                    .gap(px(6.))
                                    .child(self.settings_text_button(
                                        "settings-save".into(),
                                        "Save",
                                        p.text,
                                        cx.listener(|this, _, window, cx| {
                                            this.save_settings(window, cx)
                                        }),
                                    ))
                                    .child(self.settings_text_button(
                                        "settings-close".into(),
                                        "Close",
                                        p.text_muted,
                                        cx.listener(|this, _, window, cx| {
                                            this.close_settings(window, cx)
                                        }),
                                    )),
                            ),
                    ),
            )
            .child(body)
            .into_any_element()
    }

    fn settings_card(&self, title: &str, hint: &str) -> Div {
        let p = self.p;
        v_flex()
            .w_full()
            .gap(px(8.))
            .p(px(12.))
            .rounded(px(8.))
            .bg(fade(p.pane_raised, 0.6))
            .border_1()
            .border_color(p.hairline)
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.text)
                            .child(title.to_string()),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .font_family(fonts::MONO)
                            .text_color(p.text_dim)
                            .child(hint.to_string()),
                    ),
            )
    }

    fn settings_label(&self, label: &str) -> AnyElement {
        let p = self.p;
        div()
            .text_size(px(11.))
            .font_family(fonts::MONO)
            .text_color(p.text_muted)
            .child(label.to_string())
            .into_any_element()
    }

    fn settings_field(&self, key: String, label: &str, width: Pixels) -> AnyElement {
        let p = self.p;
        let invalid = self
            .settings
            .as_ref()
            .and_then(|view| view.error.as_ref())
            .is_some_and(|error| error.field == key);
        let input = self
            .settings
            .as_ref()
            .and_then(|view| view.field(&key))
            .cloned();
        let mut value = h_flex().gap(px(8.)).items_center().child(
            div()
                .text_size(px(11.))
                .font_family(fonts::MONO)
                .text_color(p.text_dim)
                .child(label.to_string()),
        );
        if let Some(input) = input {
            value = value.child(self.settings_input(&input, &format!("settings-{key}"), width));
        } else {
            value = value.child(
                div()
                    .w(width)
                    .text_size(px(11.))
                    .font_family(fonts::MONO)
                    .text_color(p.text_faint)
                    .child("—"),
            );
        }
        v_flex()
            .gap(px(2.))
            .child(value)
            .when(invalid, |column| {
                column.child(
                    div()
                        .text_size(px(10.))
                        .font_family(fonts::MONO)
                        .text_color(p.danger)
                        .child("invalid value"),
                )
            })
            .into_any_element()
    }

    fn settings_input(&self, input: &Entity<InputState>, id: &str, width: Pixels) -> AnyElement {
        let p = self.p;
        h_flex()
            .w(width)
            .px(px(6.))
            .py(px(2.))
            .rounded(px(4.))
            .bg(p.canvas)
            .border_1()
            .border_color(p.hairline)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .font_family(fonts::MONO)
                    .text_size(px(11.))
                    .child(
                        Input::new(input)
                            .id(ElementId::Name(id.to_string().into()))
                            .appearance(false)
                            .bordered(false)
                            .focus_bordered(false)
                            .text_size(px(11.))
                            .font_family(fonts::MONO),
                    ),
            )
            .into_any_element()
    }

    fn settings_text_button(
        &self,
        id: String,
        label: &str,
        color: Rgba,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        let p = self.p;
        div()
            .id(ElementId::Name(id.into()))
            .test_support()
            .aria_label(label.to_string())
            .px(px(8.))
            .py(px(3.))
            .rounded(px(4.))
            .bg(p.button)
            .border_1()
            .border_color(p.hairline)
            .cursor_pointer()
            .hover(|style| style.bg(p.button_hover))
            .on_click(handler)
            .text_size(px(10.))
            .font_family(fonts::MONO)
            .text_color(color)
            .child(label.to_string())
            .into_any_element()
    }

    fn settings_choice_row(
        &self,
        label: &str,
        options: Vec<String>,
        selected: &str,
        cx: &mut Context<Self>,
        on_pick: impl Fn(&mut Self, &str, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let p = self.p;
        let on_pick = std::rc::Rc::new(on_pick);
        let mut row = h_flex()
            .gap(px(8.))
            .items_center()
            .child(self.settings_label(label));
        for option in options {
            let active = option == selected;
            let picked = option.clone();
            let on_pick = on_pick.clone();
            row = row.child(
                div()
                    .id(ElementId::Name(
                        format!("settings-choice-{label}-{option}").into(),
                    ))
                    .test_support()
                    .aria_label(option.clone())
                    .px(px(6.))
                    .py(px(2.))
                    .rounded(px(4.))
                    .when(active, |chip| {
                        chip.bg(p.surface).border_1().border_color(p.hairline)
                    })
                    .when(!active, |chip| chip.hover(|style| style.bg(p.button_hover)))
                    .cursor_pointer()
                    .text_size(px(10.))
                    .font_family(fonts::MONO)
                    .text_color(if active { p.text } else { p.text_dim })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        on_pick(this, &picked, cx);
                    }))
                    .child(option),
            );
        }
        row.into_any_element()
    }
}
