use doki_agent::messenger::AgentMessenger;
use doki_agent::prompt::{self, PromptContext};
use doki_agent::resolve::{self, Effective, Resolved};
use doki_core::config::{CommitScope, Config, Guards};
use doki_core::snapshot::Snapshot;
use doki_core::store::Store;
use doki_core::sync::{CommitMessage, MessageContext, Messenger, Proposal, diff_hash};
use std::path::Path;

pub const SUBJECT_MAX: usize = 72;
pub const SUBJECT_WARN: usize = 65;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Normal,
    Warn,
    Danger,
}

/// The 72-char budget turns sand above 65 and vermilion at the cap.
pub fn counter_level(chars: usize) -> Level {
    if chars >= SUBJECT_MAX {
        Level::Danger
    } else if chars > SUBJECT_WARN {
        Level::Warn
    } else {
        Level::Normal
    }
}

pub fn counter_label(chars: usize) -> String {
    format!("{chars} / {SUBJECT_MAX}")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentInfo {
    pub id: String,
    pub model: String,
}

impl AgentInfo {
    pub fn label(&self) -> String {
        if self.model.is_empty() {
            format!("{} (cli default)", self.id)
        } else {
            format!("{} [{}]", self.id, self.model)
        }
    }

    pub fn model_label(&self) -> String {
        if self.model.is_empty() {
            "cli default".into()
        } else {
            self.model.clone()
        }
    }

    pub fn switch_label(&self) -> String {
        format!("Switch to {} ({})", self.id, self.family_label())
    }

    /// The PNG reads `omp (Claude)`: the model family, not its exact tag.
    pub fn family_label(&self) -> String {
        let model = self.model.trim();
        if model.is_empty() {
            return "CLI default".into();
        }
        let lower = model.to_ascii_lowercase();
        for (needle, label) in [
            ("claude", "Claude"),
            ("gpt", "GPT"),
            ("gemini", "Gemini"),
            ("llama", "Llama"),
            ("mistral", "Mistral"),
            ("qwen", "Qwen"),
        ] {
            if lower.contains(needle) {
                return label.into();
            }
        }
        model.to_string()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Agents {
    pub current: Option<AgentInfo>,
    pub next: Option<AgentInfo>,
    pub error: Option<String>,
}

pub fn agents(cfg: &Config, repo: &Path) -> Agents {
    let effective = match Effective::resolve(cfg, repo) {
        Ok(effective) => effective,
        Err(error) => {
            return Agents {
                error: Some(format!("{error:#}")),
                ..Agents::default()
            };
        }
    };
    let current = AgentInfo {
        id: effective.agent.id.clone(),
        model: effective.agent.model.clone(),
    };
    let next = cfg
        .agent
        .order
        .iter()
        .filter(|id| **id != current.id)
        .find_map(|id| {
            resolve::resolve_id(cfg, id).ok().map(|resolved| AgentInfo {
                id: resolved.id,
                model: resolved.model,
            })
        });
    Agents {
        current: Some(current),
        next,
        error: None,
    }
}

/// A repo preference chosen in the center, expressed as a config override so
/// the pipeline resolves it exactly like a hand-written `[[overrides]]` row.
pub fn override_config(cfg: &Config, repo: &Path, agent: Option<&AgentInfo>) -> Config {
    let mut next = cfg.clone();
    if let Some(agent) = agent {
        let override_row = doki_core::config::Override {
            path: repo.to_path_buf(),
            agent: agent.id.clone(),
            model: agent.model.clone(),
        };
        let existing = next
            .overrides
            .iter()
            .position(|row| same_path(&row.path, repo));
        match existing {
            Some(at) => next.overrides[at] = override_row,
            None => next.overrides.insert(0, override_row),
        }
    }
    next
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Draft {
    pub subject: String,
    pub body: String,
}

impl Draft {
    pub fn from_message(text: &str) -> Self {
        let message = CommitMessage::from_full(text);
        Self {
            subject: message.subject,
            body: message.body,
        }
    }

    pub fn full(&self) -> String {
        CommitMessage {
            subject: self.subject.trim_end().to_string(),
            body: self.body.trim_end().to_string(),
        }
        .full()
    }

    pub fn chars(&self) -> usize {
        self.subject.chars().count()
    }

    pub fn ready(&self) -> bool {
        let message = CommitMessage {
            subject: self.subject.trim().to_string(),
            body: String::new(),
        };
        message.has_prefix() && self.chars() <= SUBJECT_MAX
    }
}

/// The user's edited message is committed as-is; this never calls a model.
pub struct DraftMessenger {
    resolved: Resolved,
    draft: Draft,
}

impl DraftMessenger {
    pub fn new(cfg: &Config, agent: &AgentInfo, draft: Draft) -> Result<Self, String> {
        let resolved = resolve::resolve_id(cfg, &agent.id).map_err(|error| format!("{error:#}"))?;
        Ok(Self { resolved, draft })
    }

    /// A reviewed subject needs no wrapper: committing it must not require a CLI.
    pub fn unresolved(draft: Draft) -> Self {
        Self {
            resolved: Resolved {
                id: String::new(),
                cli: String::new(),
                model: String::new(),
                extra_args: Vec::new(),
                timeout: std::time::Duration::ZERO,
                command: None,
            },
            draft,
        }
    }
}

impl Messenger for DraftMessenger {
    fn agent(&self) -> &str {
        &self.resolved.id
    }

    fn model(&self) -> &str {
        &self.resolved.model
    }

    fn preview(&self, ctx: &MessageContext) -> Vec<String> {
        let prompt_ctx = PromptContext {
            repo_name: ctx.repo_name.clone(),
            branch: ctx.branch.clone(),
            recent_subjects: ctx.recent_subjects.clone(),
            files: ctx
                .files
                .iter()
                .map(|file| prompt::FileStat {
                    path: file.path.clone(),
                    added: file.added,
                    deleted: file.deleted,
                })
                .collect(),
            diff: ctx.diff.clone(),
        };
        let text = prompt::build(&prompt_ctx, SUBJECT_MAX * 1000);
        let tmp = std::env::temp_dir().join("doki-agent-preview");
        self.resolved
            .argv(&tmp, &text)
            .map(|(argv, _, _)| argv)
            .unwrap_or_default()
    }

    fn propose(&self, _ctx: &MessageContext) -> Proposal {
        Proposal {
            message: CommitMessage {
                subject: self.draft.subject.trim().to_string(),
                body: self.draft.body.trim().to_string(),
            },
            agent: Some(self.resolved.id.clone()),
            model: self.resolved.model.clone(),
            warning: None,
        }
    }
}

pub fn cached(
    cfg: &Config,
    store: Option<&Store>,
    repo: &Path,
    snapshot: &Snapshot,
) -> Option<Draft> {
    let store = store?;
    let agents = agents(cfg, repo);
    let current = agents.current?;
    let hash = diff_hash(snapshot.diff.as_bytes());
    let text = store
        .cache_get(&hash, &current.id, &current.model)
        .ok()
        .flatten()?;
    let draft = Draft::from_message(&text);
    draft.ready().then_some(draft)
}

/// One model call, but never for a diff that tripped the secret scan.
pub fn generate(
    cfg: &Config,
    repo: &Path,
    snapshot: &Snapshot,
) -> Result<(Draft, AgentInfo), String> {
    if let Some(detail) = snapshot.scan.scan.finding() {
        return Err(format!("secret scan blocked the model call: {detail}"));
    }
    let messenger = AgentMessenger::new(cfg, repo).map_err(|error| format!("{error:#}"))?;
    let effective = messenger.resolved();
    let context = snapshot.context(doki_core::snapshot::subjects(repo));
    let proposal = messenger.propose(&context);
    if proposal.agent.is_none() {
        return Err("no agent produced a message".into());
    }
    let draft = Draft {
        subject: proposal.message.subject,
        body: proposal.message.body,
    };
    let agent = AgentInfo {
        id: effective.agent.id.clone(),
        model: effective.agent.model.clone(),
    };
    if !draft.ready() {
        return Err(format!(
            "the generated subject is not a usable `doki:` subject: {}",
            draft.subject
        ));
    }
    Ok((draft, agent))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chip {
    pub label: String,
    pub value: String,
    pub ok: bool,
    pub warning: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuardChips {
    pub secret: Chip,
    pub mass: Chip,
    pub context: Chip,
}

impl GuardChips {
    pub fn blocking(&self) -> bool {
        !self.secret.ok || !self.mass.ok || !self.context.ok
    }
}

pub fn guard_chips(snapshot: &Snapshot, guards: &Guards) -> GuardChips {
    let secret = if snapshot.scan.scan.blocks() {
        Chip {
            label: scanner_label(snapshot.scan.scanner),
            value: snapshot.scan.summary(),
            ok: false,
            warning: snapshot.scan.scan.warning().map(str::to_string),
        }
    } else {
        Chip {
            label: scanner_label(snapshot.scan.scanner),
            value: snapshot.scan.summary(),
            ok: true,
            warning: snapshot.scan.scan.warning().map(str::to_string),
        }
    };
    let files = snapshot.mass.changed_files;
    let lines = snapshot.mass.deleted_lines;
    let mass_ok = files <= guards.max_changed_files && lines <= guards.max_deleted_lines;
    let mass = Chip {
        label: "Mass-Change Guard".into(),
        value: format!(
            "{} / {lines} lines {} {}",
            plural(files as u64, "file"),
            if mass_ok { "<" } else { "over" },
            thousands(guards.max_deleted_lines)
        ),
        ok: mass_ok,
        warning: None,
    };
    let bytes = snapshot.context_bytes();
    let context_ok = bytes <= guards.max_diff_bytes_for_model;
    let context = Chip {
        label: "Context".into(),
        value: format!(
            "{:.1} KB < {} KB",
            bytes as f64 / 1000.,
            guards.max_diff_bytes_for_model / 1000
        ),
        ok: context_ok,
        warning: None,
    };
    GuardChips {
        secret,
        mass,
        context,
    }
}

fn scanner_label(scanner: &str) -> String {
    match scanner {
        "gitleaks" => "Gitleaks".into(),
        "off" => "Secret scan".into(),
        other => format!("{} scan", capitalize(other)),
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

pub fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub fn plural(count: u64, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

pub fn scope_label(scope: CommitScope, files: usize) -> String {
    let name = match scope {
        CommitScope::Staged => "Staged",
        CommitScope::Tracked => "Tracked",
        CommitScope::All => "All",
    };
    match files {
        1 => format!("{name} (1 file)"),
        n => format!("{name} ({n} files)"),
    }
}

pub fn totals(files: &[doki_core::sync::ChangedFile]) -> (u64, u64) {
    files.iter().fold((0, 0), |(added, deleted), file| {
        (
            added + file.added.unwrap_or(0),
            deleted + file.deleted.unwrap_or(0),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use doki_core::config::Config;

    #[test]
    fn counter_turns_sand_past_65_and_vermilion_at_72() {
        assert_eq!(counter_level(0), Level::Normal);
        assert_eq!(counter_level(60), Level::Normal);
        assert_eq!(counter_level(65), Level::Normal);
        assert_eq!(counter_level(66), Level::Warn);
        assert_eq!(counter_level(71), Level::Warn);
        assert_eq!(counter_level(72), Level::Danger);
        assert_eq!(counter_level(90), Level::Danger);
        assert_eq!(counter_label(60), "60 / 72");
    }

    #[test]
    fn drafts_keep_the_prefix_and_reject_a_long_subject() {
        let draft = Draft {
            subject: "doki: handle idempotent webhook retries".into(),
            body: "Body line.".into(),
        };
        assert!(draft.ready());
        assert_eq!(counter_level(draft.chars()), Level::Normal);
        assert_eq!(
            draft.full(),
            "doki: handle idempotent webhook retries\n\nBody line.\n"
        );
        assert_eq!(Draft::from_message(&draft.full()), draft);

        let long = Draft {
            subject: format!("doki: {}", "x".repeat(80)),
            body: String::new(),
        };
        assert!(!long.ready());
        assert_eq!(counter_level(long.chars()), Level::Danger);

        let unprefixed = Draft {
            subject: "no prefix".into(),
            body: String::new(),
        };
        assert!(!unprefixed.ready());
    }

    #[test]
    fn guard_chips_report_the_budget_and_the_scanner() {
        let cfg =
            Config::parse("[guards]\nmax_deleted_lines = 2000\nmax_diff_bytes_for_model = 60000\n")
                .unwrap();
        let mut snapshot = Snapshot {
            repo: "/tmp/billing-service".into(),
            branch: "main".into(),
            scope: CommitScope::Tracked,
            files: Vec::new(),
            diff: "x".repeat(14_200),
            mass: doki_core::guards::MassChange {
                changed_files: 14,
                deleted_lines: 320,
            },
            scan: doki_core::secret::ScanReport {
                scan: doki_core::secret::Scan::Clean { warning: None },
                scanner: "gitleaks",
            },
            truncated: false,
        };
        let chips = guard_chips(&snapshot, &cfg.guards);
        assert_eq!(chips.secret.label, "Gitleaks");
        assert_eq!(chips.secret.value, "0 secrets detected");
        assert!(chips.secret.ok);
        assert_eq!(chips.mass.value, "14 files / 320 lines < 2,000");
        assert_eq!(chips.mass.value.split(" / ").next(), Some("14 files"));
        assert_eq!(chips.context.value, "14.2 KB < 60 KB");
        assert!(!chips.blocking());

        snapshot.mass.deleted_lines = 2_500;
        snapshot.scan = doki_core::secret::ScanReport {
            scan: doki_core::secret::Scan::Finding {
                detail: "gitleaks: aws-access-token in a.env:2".into(),
                warning: None,
            },
            scanner: "gitleaks",
        };
        let chips = guard_chips(&snapshot, &cfg.guards);
        assert!(!chips.mass.ok);
        assert!(!chips.secret.ok);
        assert!(chips.blocking());
        assert!(chips.secret.value.contains("aws-access-token"));
    }

    #[test]
    fn built_in_scan_is_named_when_gitleaks_is_missing() {
        let guards = Guards::default();
        let snapshot = Snapshot {
            repo: "/tmp/a".into(),
            branch: "main".into(),
            scope: CommitScope::Tracked,
            files: Vec::new(),
            diff: String::from("+x\n"),
            mass: Default::default(),
            scan: doki_core::secret::ScanReport {
                scan: doki_core::secret::Scan::Clean {
                    warning: Some("gitleaks is not on PATH; used doki's built-in scan".into()),
                },
                scanner: "built-in",
            },
            truncated: false,
        };
        let chips = guard_chips(&snapshot, &guards);
        assert_eq!(chips.secret.label, "Built-in scan");
        assert_eq!(chips.secret.value, "0 secrets detected");
        assert!(chips.secret.warning.is_some());
    }

    #[test]
    fn scope_labels_and_totals_match_the_strip() {
        assert_eq!(scope_label(CommitScope::Staged, 3), "Staged (3 files)");
        assert_eq!(scope_label(CommitScope::Tracked, 1), "Tracked (1 file)");
        assert_eq!(scope_label(CommitScope::All, 0), "All (0 files)");
        let files = vec![
            doki_core::sync::ChangedFile {
                path: "a".into(),
                added: Some(42),
                deleted: Some(6),
            },
            doki_core::sync::ChangedFile {
                path: "b".into(),
                added: Some(138),
                deleted: Some(12),
            },
        ];
        assert_eq!(totals(&files), (180, 18));
        assert_eq!(thousands(2000), "2,000");
    }

    #[test]
    fn agent_labels_come_from_resolution() {
        let cfg = Config::parse(
            "[agent]\norder = [\"codex\", \"omp\"]\n\n[agents.codex]\nmodel = \"gpt-5.2\"\n\n\
             [agents.omp]\nmodel = \"claude-sonnet-4-5\"\n",
        )
        .unwrap();
        let agents = agents(&cfg, Path::new("/tmp/not-a-repo"));
        let current = agents.current.unwrap();
        assert_eq!(current.label(), "codex [gpt-5.2]");
        assert_eq!(
            agents.next.unwrap().switch_label(),
            "Switch to omp (Claude)"
        );
        assert_eq!(
            AgentInfo {
                id: "pi".into(),
                model: String::new()
            }
            .switch_label(),
            "Switch to pi (CLI default)"
        );
    }
}
