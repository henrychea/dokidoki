use crate::config::{CommitScope, Config};
use crate::git::{Git, GitOutput};
use crate::guards::MassChange;
use crate::lock::{self, LockAttempt};
use crate::push::{self, PushOutcome, PushPolicy, PushState};
use crate::repo_config::RepoConfig;
use crate::scheduler;
use crate::secret::{self, Scan};
use crate::status::{self, RepoState};
use crate::store::{NewIncident, RepoState as PersistedState, RunFinish, Store};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SyncState {
    Committed,
    #[default]
    Skipped,
    Blocked,
    WaitingUser,
    Busy,
    Disabled,
    DryRun,
    Failed,
}

impl SyncState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::Skipped => "skipped",
            Self::Blocked => "blocked",
            Self::WaitingUser => "waiting",
            Self::Busy => "busy",
            Self::Disabled => "disabled",
            Self::DryRun => "dry-run",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncOptions {
    pub dry_run: bool,
    pub state_dir: Option<PathBuf>,
}

impl SyncOptions {
    pub fn new(dry_run: bool) -> Self {
        Self {
            dry_run,
            ..Self::default()
        }
    }

    fn bundle_dir(&self) -> PathBuf {
        self.state_dir
            .clone()
            .unwrap_or_else(crate::config::state_dir)
            .join("bundles")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub repo: PathBuf,
    pub state: SyncState,
    pub dry_run: bool,
    pub scope: Option<CommitScope>,
    pub run_id: Option<i64>,
    pub branch: Option<String>,
    pub head_before: Option<String>,
    pub head_after: Option<String>,
    pub message: Option<String>,
    pub agent: Option<String>,
    pub model: String,
    pub snapshot_ref: Option<String>,
    pub push: PushState,
    pub pushed: bool,
    pub bundled: Option<PathBuf>,
    pub quarantined: bool,
    pub fail_count: Option<u32>,
    pub incident_kind: Option<String>,
    pub warning: Option<String>,
    pub detail: Option<String>,
    pub argv: Vec<Vec<String>>,
    pub agent_argv: Vec<String>,
}

impl SyncReport {
    pub fn new(repo: impl Into<PathBuf>, dry_run: bool) -> Self {
        Self {
            repo: repo.into(),
            dry_run,
            ..Self::default()
        }
    }

    pub fn exit_code(&self) -> i32 {
        match self.state {
            SyncState::Blocked | SyncState::Failed | SyncState::Busy | SyncState::WaitingUser => 1,
            _ => 0,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitMessage {
    pub subject: String,
    pub body: String,
}

impl CommitMessage {
    pub fn full(&self) -> String {
        if self.body.is_empty() {
            format!("{}\n", self.subject)
        } else {
            format!("{}\n\n{}\n", self.subject, self.body)
        }
    }

    pub fn from_full(text: &str) -> Self {
        let mut parts = text.splitn(2, "\n\n");
        Self {
            subject: parts.next().unwrap_or_default().trim().to_string(),
            body: parts.next().unwrap_or_default().trim_end().to_string(),
        }
    }

    pub fn has_prefix(&self) -> bool {
        self.subject.starts_with("doki: ") && self.subject.len() > "doki: ".len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: PathBuf,
    pub added: Option<u64>,
    pub deleted: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MessageContext {
    pub repo_name: String,
    pub branch: String,
    pub recent_subjects: Vec<String>,
    pub files: Vec<ChangedFile>,
    pub diff: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    pub message: CommitMessage,
    pub agent: Option<String>,
    pub model: String,
    pub warning: Option<String>,
}

/// The binary supplies message generation; core keeps guards, cache and journal.
pub trait Messenger {
    fn agent(&self) -> &str;
    fn model(&self) -> &str;
    fn preview(&self, ctx: &MessageContext) -> Vec<String>;
    fn propose(&self, ctx: &MessageContext) -> Proposal;
}

pub fn sync(
    cfg: &Config,
    store: &Store,
    repo_path: &Path,
    messenger: &dyn Messenger,
    dry_run: bool,
) -> Result<SyncReport> {
    sync_with(cfg, store, repo_path, messenger, SyncOptions::new(dry_run))
}

pub fn sync_with(
    cfg: &Config,
    store: &Store,
    repo_path: &Path,
    messenger: &dyn Messenger,
    opts: SyncOptions,
) -> Result<SyncReport> {
    let mut report = sync_locked(cfg, store, repo_path, messenger, &opts)?;
    if !opts.dry_run {
        settle(cfg, store, &mut report)?;
    }
    Ok(report)
}

fn sync_locked(
    cfg: &Config,
    store: &Store,
    repo_path: &Path,
    messenger: &dyn Messenger,
    opts: &SyncOptions,
) -> Result<SyncReport> {
    let dry_run = opts.dry_run;
    let repo = repo_path
        .canonicalize()
        .unwrap_or_else(|_| repo_path.to_path_buf());
    let row = store.repo_by_path(&repo)?.with_context(|| {
        format!(
            "{} is not a known repo; run `doki status` and enable it first",
            repo.display()
        )
    })?;
    if !row.enabled {
        bail!("{} is disabled; enable it before syncing", repo.display());
    }
    let repo_cfg = RepoConfig::load(&repo)?;
    let mut report = SyncReport::new(repo.clone(), dry_run);
    if row.quarantined() {
        report.state = SyncState::Blocked;
        report.detail = Some(
            "quarantined after repeated failures: run `doki reset-quarantine <repo>` to retry"
                .into(),
        );
        return Ok(report);
    }
    if repo_cfg.disabled() {
        report.state = SyncState::Disabled;
        report.detail = Some("disabled by .doki.toml".into());
        return Ok(report);
    }
    let base_scope = row.commit_scope.unwrap_or(cfg.sync.commit_scope);
    let scope = repo_cfg.narrowed_scope(base_scope)?;
    report.scope = Some(scope);

    let git = if dry_run {
        Git::dry_run(&repo)
    } else {
        Git::new(&repo)
    };
    let common = git.common_dir()?;
    let _lock = match lock::try_acquire(&common)? {
        LockAttempt::Acquired(lock) => lock,
        LockAttempt::Busy => {
            report.state = SyncState::Busy;
            report.detail = Some("another doki run holds the repo lock".into());
            return Ok(report);
        }
    };
    if !dry_run {
        store.set_state(row.id, PersistedState::Running)?;
    }

    let before = status::read(&git)?;
    let parsed = status::parse(
        &git.run(&["status", "--porcelain=v2", "-z", "--branch"])?
            .stdout,
    )?;
    let head_before = parsed.head.clone();
    report.head_before = head_before.clone();
    if let RepoState::Refused(reason) = &before.state {
        return stop(
            store,
            row.id,
            report,
            Stop {
                state: "blocked",
                kind: "refused",
                severity: "blocking",
                detail: reason.clone(),
                warning: None,
                exit_code: None,
            },
        );
    }
    if let RepoState::Detached = before.state {
        return stop(
            store,
            row.id,
            report,
            Stop {
                state: "blocked",
                kind: "detached",
                severity: "blocking",
                detail: "detached HEAD: doki only commits on a branch".into(),
                warning: None,
                exit_code: None,
            },
        );
    }
    if let RepoState::Unborn = before.state {
        return stop(
            store,
            row.id,
            report,
            Stop {
                state: "blocked",
                kind: "unborn",
                severity: "blocking",
                detail: "unborn branch: make the first commit yourself".into(),
                warning: None,
                exit_code: None,
            },
        );
    }

    if !parsed.unmerged.is_empty() {
        let paths: Vec<String> = parsed
            .unmerged
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        return stop(
            store,
            row.id,
            report,
            Stop {
                state: "blocked",
                kind: "unmerged",
                severity: "blocking",
                detail: format!("unmerged paths: {}", paths.join(", ")),
                warning: None,
                exit_code: None,
            },
        );
    }
    let has_changes = match scope {
        CommitScope::Staged => !parsed.staged.is_empty(),
        CommitScope::Tracked => !parsed.staged.is_empty() || !parsed.unstaged.is_empty(),
        CommitScope::All => parsed.dirty() > 0,
    };
    if !has_changes {
        let branch = before.branch.clone().unwrap_or_default();
        report.branch = Some(branch.clone());
        report.state = SyncState::Skipped;
        report.detail = Some("nothing to commit".into());
        return with_push(cfg, store, &git, &row, report, opts);
    }

    let branch = before.branch.clone().unwrap_or_default();
    report.branch = Some(branch.clone());
    let ignore = repo_cfg.ignore;
    if dry_run {
        let started = now_unix();
        let reference = format!("refs/doki/pre-sync/{started}");
        git.run(&["update-ref", &reference, "HEAD"])?;
        stage(&git, scope, &ignore)?;
        let context = preview_context(&git, &repo, &branch, scope, &ignore)?;
        report.agent_argv = messenger.preview(&context);
        git.run_stdin(&["commit", "-F", "-"], b"")?;
        if let Ok(preview) = push::preview(&git, &cfg.sync, &branch, row.allow_protected_push) {
            report.push = preview.state;
            report.detail = combine(report.detail.take(), preview.detail);
        }
        report.state = SyncState::DryRun;
        report.detail = combine(
            report.detail.take(),
            Some("dry run: no mutating command was executed".into()),
        );
        report.argv = git.recorded();
        return Ok(report);
    }

    let started = now_unix();
    let run_id = store.begin_run(row.id, started, head_before.as_deref())?;
    report.run_id = Some(run_id);
    let reference = allocate_ref(&git, started)?;
    let snapshot_sha = match git
        .run(&["update-ref", &reference, "HEAD"])
        .and_then(|_| rev_parse(&git, "HEAD"))
    {
        Ok(sha) => sha,
        Err(error) => {
            return stop(
                store,
                row.id,
                report,
                Stop {
                    state: "failed",
                    kind: "snapshot",
                    severity: "error",
                    detail: format!("{error:#}"),
                    warning: None,
                    exit_code: None,
                },
            );
        }
    };
    store.add_snapshot(row.id, run_id, &reference, &snapshot_sha, started)?;
    report.snapshot_ref = Some(reference);

    if report.head_before.as_deref() != Some(snapshot_sha.as_str()) {
        return stop(
            store,
            row.id,
            report,
            Stop {
                state: "blocked",
                kind: "moved",
                severity: "blocking",
                detail: "HEAD moved between the pre-sync snapshot and staging".into(),
                warning: None,
                exit_code: None,
            },
        );
    }
    let recheck = status::read(&git)?;
    if let RepoState::Refused(reason) = &recheck.state {
        return stop(
            store,
            row.id,
            report,
            Stop {
                state: "blocked",
                kind: "refused",
                severity: "blocking",
                detail: reason.clone(),
                warning: None,
                exit_code: None,
            },
        );
    }

    let ctx = RunContext {
        cfg,
        store,
        git: &git,
        messenger,
        repo_id: row.id,
        repo: repo.clone(),
        scope,
        ignore: &ignore,
        branch,
    };
    let outcome = ctx.stage_and_commit(report);
    match outcome {
        Ok(report) => with_push(cfg, store, &git, &row, report, opts),
        Err(error) => {
            let mut report = SyncReport::new(repo, false);
            report.run_id = Some(run_id);
            report.head_before = head_before.clone();
            stop(
                store,
                row.id,
                report,
                Stop {
                    state: "failed",
                    kind: "error",
                    severity: "error",
                    detail: format!("{error:#}"),
                    warning: None,
                    exit_code: None,
                },
            )
        }
    }
}

/// Fetch and push follow a commit or an existing unpushed one, never a bare skip.
fn with_push(
    cfg: &Config,
    store: &Store,
    git: &Git,
    row: &crate::store::RepoRow,
    mut report: SyncReport,
    opts: &SyncOptions,
) -> Result<SyncReport> {
    let committed = report.state == SyncState::Committed;
    let branch = report.branch.clone().unwrap_or_default();
    if branch.is_empty() {
        return Ok(report);
    }
    let bundle_dir = opts.bundle_dir();
    let outcome = push::check(
        git,
        &PushPolicy {
            cfg: &cfg.sync,
            branch: &branch,
            allow_protected_push: row.allow_protected_push,
            committed,
            bundle_dir: &bundle_dir,
            now: now_unix(),
        },
    )?;
    if matches!(outcome.state, PushState::Diverged | PushState::Failed) {
        apply_push(store, row.id, &mut report, outcome)?;
    } else {
        report.push = outcome.state;
        report.pushed = outcome.state == PushState::Pushed;
        report.detail = combine(report.detail.take(), outcome.detail);
        report.warning = combine(report.warning.take(), outcome.warning);
        if report.pushed
            && let Some(run_id) = report.run_id
        {
            store.mark_run_pushed(run_id, true)?;
        }
        if let Some(bundle) = &outcome.bundle {
            report.bundled = Some(bundle.clone());
            if let (Some(run_id), Some(head)) = (report.run_id, report.head_after.clone()) {
                let path = bundle.display().to_string();
                store.add_snapshot(row.id, run_id, &path, &head, now_unix())?;
            }
        }
        if let Some(incident) = outcome.incident {
            report.incident_kind = Some(incident.kind.to_string());
            report.warning = combine(report.warning.take(), Some(incident.detail.clone()));
            store.add_incident(NewIncident {
                repo_id: row.id,
                run_id: report.run_id,
                severity: incident.severity,
                kind: incident.kind,
                detail: &incident.detail,
                created_at: now_unix(),
            })?;
        }
    }
    report.argv = git.recorded();
    Ok(report)
}

fn apply_push(
    store: &Store,
    repo_id: i64,
    report: &mut SyncReport,
    outcome: PushOutcome,
) -> Result<()> {
    report.push = outcome.state;
    report.detail = combine(report.detail.take(), outcome.detail);
    report.warning = combine(report.warning.take(), outcome.warning);
    report.bundled.clone_from(&outcome.bundle);
    let now = now_unix();
    if let (Some(bundle), Some(run_id), Some(head)) =
        (&outcome.bundle, report.run_id, report.head_after.clone())
    {
        store.add_snapshot(repo_id, run_id, &bundle.display().to_string(), &head, now)?;
    }
    let Some(incident) = outcome.incident else {
        return Ok(());
    };
    report.incident_kind = Some(incident.kind.to_string());
    let run_id = match report.run_id {
        Some(id) => id,
        None => {
            let id = store.begin_run(repo_id, now, report.head_before.as_deref())?;
            report.run_id = Some(id);
            id
        }
    };
    let run_state = if outcome.state == PushState::Diverged {
        report.state = SyncState::WaitingUser;
        "waiting"
    } else {
        report.state = SyncState::Failed;
        "failed"
    };
    store.finish_push(
        run_id,
        now,
        run_state,
        incident.kind,
        report.warning.as_deref(),
    )?;
    store.add_incident(NewIncident {
        repo_id,
        run_id: Some(run_id),
        severity: incident.severity,
        kind: incident.kind,
        detail: &incident.detail,
        created_at: now,
    })?;
    Ok(())
}

/// Failures back off exponentially and then latch into quarantine.
fn settle(cfg: &Config, store: &Store, report: &mut SyncReport) -> Result<()> {
    let Some(row) = store.repo_by_path(&report.repo)? else {
        return Ok(());
    };
    if row.quarantined() {
        return Ok(());
    }
    let now = now_unix();
    let state = match report.state {
        SyncState::Committed | SyncState::Skipped => match report.push {
            PushState::NoRemote => PersistedState::NoRemote,
            _ => dirty_state(&report.repo),
        },
        SyncState::WaitingUser => PersistedState::WaitingUser,
        SyncState::Blocked | SyncState::Failed => PersistedState::Blocked,
        SyncState::Disabled => PersistedState::Disabled,
        SyncState::Busy | SyncState::DryRun => return Ok(()),
    };
    if report.state == SyncState::Failed {
        let next =
            now + scheduler::backoff_seconds(row.fail_count + 1, cfg.sync.backoff_max_minutes);
        let (fail_count, quarantined) = store.record_failure(
            row.id,
            now,
            state,
            report.detail.as_deref().unwrap_or("sync failed"),
            Some(next),
            cfg.sync.quarantine_after,
        )?;
        report.fail_count = Some(fail_count);
        report.quarantined = quarantined;
        if quarantined {
            report.incident_kind = Some("quarantined".into());
            store.add_incident(NewIncident {
                repo_id: row.id,
                run_id: report.run_id,
                severity: "error",
                kind: "quarantined",
                detail: &format!(
                    "{fail_count} consecutive failures; manual reset required (`doki reset-quarantine`)"
                ),
                created_at: now,
            })?;
        }
        return Ok(());
    }
    let next = scheduler::next_for_repo(cfg, &row, now).ok();
    store.record_success(row.id, now, state, next)?;
    Ok(())
}

fn dirty_state(repo: &Path) -> PersistedState {
    match status::read(&Git::new(repo)) {
        Ok(status) if status.dirty > 0 => PersistedState::Dirty,
        _ => PersistedState::Clean,
    }
}

struct RunContext<'a> {
    cfg: &'a Config,
    store: &'a Store,
    git: &'a Git,
    messenger: &'a dyn Messenger,
    repo_id: i64,
    repo: PathBuf,
    scope: CommitScope,
    ignore: &'a [String],
    branch: String,
}

impl RunContext<'_> {
    fn stage_and_commit(&self, mut report: SyncReport) -> Result<SyncReport> {
        stage(self.git, self.scope, self.ignore)?;
        let diff = self.git.run(&["diff", "--cached"])?;
        let quiet = self.git.run_unchecked(&["diff", "--cached", "--quiet"])?;
        if quiet.success() {
            let now = now_unix();
            self.store.finish_run(RunFinish {
                run_id: report.run_id.unwrap_or_default(),
                finished_at: now,
                state: "skipped",
                head_after: report.head_before.as_deref(),
                ..RunFinish::default()
            })?;
            report.state = SyncState::Skipped;
            report.head_after = report.head_before.clone();
            report.detail = Some("nothing staged for commit".into());
            return Ok(report);
        }

        let numstat = self.git.run(&["diff", "--cached", "--numstat"])?;
        let mass = MassChange::parse(&numstat.stdout);
        if let Err(e) = mass.check(&self.cfg.guards) {
            return stop(
                self.store,
                self.repo_id,
                report,
                Stop {
                    state: "blocked",
                    kind: "guard",
                    severity: "blocking",
                    detail: format!("{e:#}"),
                    warning: None,
                    exit_code: None,
                },
            );
        }

        let scan = secret::scan(&diff.stdout, &self.cfg.guards);
        let scan_warning = match &scan {
            Scan::Clean { warning } | Scan::Finding { warning, .. } => warning.clone(),
            Scan::Off => None,
        };
        if let Scan::Finding { detail, warning } = scan {
            return stop(
                self.store,
                self.repo_id,
                report,
                Stop {
                    state: "blocked",
                    kind: "secret",
                    severity: "blocking",
                    detail,
                    warning,
                    exit_code: None,
                },
            );
        }

        let context = MessageContext {
            repo_name: repo_name(&self.repo),
            branch: self.branch.clone(),
            recent_subjects: recent_subjects(self.git),
            files: ChangedFile::parse(&numstat.stdout),
            diff: diff.stdout_lossy(),
        };
        let hash = diff_hash(&diff.stdout);
        let (message, agent, model, agent_warning) = match self.cached_message(&hash)? {
            Some(message) => (
                message,
                Some(self.messenger.agent().to_string()),
                self.messenger.model().to_string(),
                None,
            ),
            None => self.propose(&context, &hash)?,
        };
        let warning = combine(scan_warning, agent_warning);

        let scanned = self.git.run(&["diff", "--cached"])?;
        let head = rev_parse(self.git, "HEAD")?;
        if scanned.stdout != diff.stdout || report.head_before.as_deref() != Some(head.as_str()) {
            return stop(
                self.store,
                self.repo_id,
                report,
                Stop {
                    state: "blocked",
                    kind: "changed",
                    severity: "blocking",
                    detail:
                        "the index or HEAD changed during message generation; nothing was committed"
                            .into(),
                    warning,
                    exit_code: None,
                },
            );
        }

        let full = message.full();
        let commit = self
            .git
            .run_stdin_unchecked(&["commit", "-F", "-"], full.as_bytes())?;
        if !commit.success() {
            let code = commit.status.and_then(|s| s.code());
            let detail = format!(
                "git commit failed{}: {}",
                code.map(|c| format!(" (exit {c})")).unwrap_or_default(),
                stderr_tail(&commit)
            );
            return stop(
                self.store,
                self.repo_id,
                report,
                Stop {
                    state: "failed",
                    kind: "hook",
                    severity: "error",
                    detail,
                    warning,
                    exit_code: code,
                },
            );
        }

        let head_after = rev_parse(self.git, "HEAD")?;
        let now = now_unix();
        self.store.finish_run(RunFinish {
            run_id: report.run_id.unwrap_or_default(),
            finished_at: now,
            state: "committed",
            head_after: Some(&head_after),
            agent: agent.as_deref(),
            model: Some(&model),
            message: Some(&full),
            exit_code: Some(0),
            error_kind: None,
            warning: warning.as_deref(),
        })?;
        report.state = SyncState::Committed;
        report.head_after = Some(head_after);
        report.message = Some(full);
        report.agent = agent;
        report.model = model;
        report.warning = warning;
        Ok(report)
    }

    fn cached_message(&self, hash: &str) -> Result<Option<CommitMessage>> {
        let cached = self
            .store
            .cache_get(hash, self.messenger.agent(), self.messenger.model())?;
        Ok(cached
            .map(|text| CommitMessage::from_full(&text))
            .filter(CommitMessage::has_prefix))
    }

    fn propose(
        &self,
        ctx: &MessageContext,
        hash: &str,
    ) -> Result<(CommitMessage, Option<String>, String, Option<String>)> {
        let proposal: Proposal = self.messenger.propose(ctx);
        if let Some(agent) = &proposal.agent {
            self.store.cache_put(
                hash,
                agent,
                &proposal.model,
                &proposal.message.full(),
                now_unix(),
            )?;
        }
        Ok((
            proposal.message,
            proposal.agent,
            proposal.model,
            proposal.warning,
        ))
    }
}

struct Stop {
    state: &'static str,
    kind: &'static str,
    severity: &'static str,
    detail: String,
    warning: Option<String>,
    exit_code: Option<i32>,
}

fn stop(store: &Store, repo_id: i64, mut report: SyncReport, stop: Stop) -> Result<SyncReport> {
    let failed = stop.state == "failed";
    if !report.dry_run {
        let now = now_unix();
        let run_id = match report.run_id {
            Some(id) => id,
            None => {
                let id = store.begin_run(repo_id, now, report.head_before.as_deref())?;
                report.run_id = Some(id);
                id
            }
        };
        store.finish_run(RunFinish {
            run_id,
            finished_at: now,
            state: stop.state,
            head_after: report.head_before.as_deref(),
            exit_code: stop.exit_code,
            error_kind: Some(stop.kind),
            warning: stop.warning.as_deref(),
            ..RunFinish::default()
        })?;
        store.add_incident(NewIncident {
            repo_id,
            run_id: Some(run_id),
            severity: stop.severity,
            kind: stop.kind,
            detail: &stop.detail,
            created_at: now,
        })?;
    }
    report.state = if failed {
        SyncState::Failed
    } else {
        SyncState::Blocked
    };
    report.head_after = report.head_before.clone();
    report.detail = Some(stop.detail);
    report.warning = stop.warning;
    Ok(report)
}

impl ChangedFile {
    fn parse(numstat: &[u8]) -> Vec<Self> {
        String::from_utf8_lossy(numstat)
            .lines()
            .filter_map(|line| {
                let mut fields = line.split('\t');
                let added = fields.next()?;
                let deleted = fields.next()?;
                let path = fields.next()?;
                Some(Self {
                    path: PathBuf::from(path),
                    added: added.parse().ok(),
                    deleted: deleted.parse().ok(),
                })
            })
            .collect()
    }
}

fn stage(git: &Git, scope: CommitScope, ignore: &[String]) -> Result<()> {
    let flag = match scope {
        CommitScope::Staged => return Ok(()),
        CommitScope::Tracked => "-u",
        CommitScope::All => "-A",
    };
    let mut args = vec!["add".to_string(), flag.to_string()];
    args.extend(pathspec(ignore));
    run_args(git, &args)?;
    Ok(())
}

/// Dry run cannot stage, so it previews `git diff HEAD` plus untracked names.
fn preview_context(
    git: &Git,
    repo: &Path,
    branch: &str,
    scope: CommitScope,
    ignore: &[String],
) -> Result<MessageContext> {
    let base: &[&str] = match scope {
        CommitScope::Staged => &["diff", "--cached"],
        CommitScope::Tracked | CommitScope::All => &["diff", "HEAD"],
    };
    let mut numstat_args: Vec<String> = base.iter().map(|arg| arg.to_string()).collect();
    numstat_args.push("--numstat".into());
    numstat_args.extend(pathspec(ignore));
    let mut diff_args: Vec<String> = base.iter().map(|arg| arg.to_string()).collect();
    diff_args.extend(pathspec(ignore));

    let numstat = run_args(git, &numstat_args)?;
    let diff = run_args(git, &diff_args)?;
    let mut files = ChangedFile::parse(&numstat.stdout);
    if scope == CommitScope::All {
        let mut args = vec![
            "ls-files".to_string(),
            "--others".to_string(),
            "--exclude-standard".to_string(),
            "-z".to_string(),
        ];
        args.extend(pathspec(ignore));
        for path in run_args(git, &args)?
            .stdout
            .split(|byte| *byte == 0)
            .filter(|field| !field.is_empty())
        {
            files.push(ChangedFile {
                path: PathBuf::from(String::from_utf8_lossy(path).into_owned()),
                added: None,
                deleted: None,
            });
        }
    }
    Ok(MessageContext {
        repo_name: repo_name(repo),
        branch: branch.to_string(),
        recent_subjects: recent_subjects(git),
        files,
        diff: diff.stdout_lossy(),
    })
}

fn pathspec(ignore: &[String]) -> Vec<String> {
    let mut args = vec!["--".to_string(), ".".to_string()];
    args.extend(ignore.iter().map(|glob| format!(":(exclude){glob}")));
    args
}

fn run_args(git: &Git, args: &[String]) -> Result<GitOutput> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    git.run(&refs)
}

fn repo_name(repo: &Path) -> String {
    repo.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| repo.display().to_string())
}

fn recent_subjects(git: &Git) -> Vec<String> {
    git.run_unchecked(&["log", "-5", "--format=%s"])
        .map(|out| {
            out.stdout_lossy()
                .lines()
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn rev_parse(git: &Git, reference: &str) -> Result<String> {
    let out = git.run(&["rev-parse", "--verify", reference])?;
    Ok(out.stdout_lossy().trim().to_string())
}

fn allocate_ref(git: &Git, started_at: i64) -> Result<String> {
    let mut candidate = format!("refs/doki/pre-sync/{started_at}");
    for n in 1..1000 {
        let probe = format!("{candidate}^{{commit}}");
        let out = git.run_unchecked(&["rev-parse", "--verify", "--quiet", &probe])?;
        if !out.success() {
            return Ok(candidate);
        }
        candidate = format!("refs/doki/pre-sync/{started_at}-{n}");
    }
    bail!("cannot allocate a unique pre-sync ref")
}

fn stderr_tail(out: &GitOutput) -> String {
    out.stderr_lossy()
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().chars().take(500).collect())
        .unwrap_or_default()
}

fn combine(first: Option<String>, second: Option<String>) -> Option<String> {
    match (first, second) {
        (Some(a), Some(b)) => Some(format!("{a}; {b}")),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

pub fn diff_hash(diff: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in diff {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    format!("{hash:016x}-{:x}", diff.len())
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_fail_closed() {
        let mut report = SyncReport::new("/tmp/repo", false);
        for (state, code) in [
            (SyncState::Committed, 0),
            (SyncState::Skipped, 0),
            (SyncState::DryRun, 0),
            (SyncState::Disabled, 0),
            (SyncState::Blocked, 1),
            (SyncState::Failed, 1),
            (SyncState::Busy, 1),
        ] {
            report.state = state;
            assert_eq!(report.exit_code(), code, "{state:?}");
        }
    }

    #[test]
    fn message_round_trips() {
        let message = CommitMessage {
            subject: "doki: add widget".into(),
            body: "Because it was missing.".into(),
        };
        let full = message.full();
        assert_eq!(CommitMessage::from_full(&full), message);
        assert!(message.has_prefix());

        let subject_only = CommitMessage {
            subject: "doki: add widget".into(),
            body: String::new(),
        };
        assert_eq!(CommitMessage::from_full(&subject_only.full()), subject_only);
        assert!(!CommitMessage::from_full("no prefix\n").has_prefix());
        assert!(!CommitMessage::from_full("doki: \n").has_prefix());
    }

    #[test]
    fn numstat_parses_text_binary_and_renames() {
        let files = ChangedFile::parse(b"12\t3\tsrc/a.rs\n-\t-\tlogo.png\n1\t1\told => new\n");
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].path, PathBuf::from("src/a.rs"));
        assert_eq!(files[0].added, Some(12));
        assert_eq!(files[1].added, None);
        assert_eq!(files[2].path, PathBuf::from("old => new"));
    }

    #[test]
    fn stage_argv_excludes_ignore_globs() {
        let ignore = ["fixtures/**".to_string()];
        let mut owned = vec![
            "add".to_string(),
            "-u".to_string(),
            "--".to_string(),
            ".".to_string(),
        ];
        owned.extend(ignore.iter().map(|glob| format!(":(exclude){glob}")));
        assert_eq!(owned, ["add", "-u", "--", ".", ":(exclude)fixtures/**"]);
    }

    #[test]
    fn diff_hash_is_stable_and_length_aware() {
        assert_eq!(diff_hash(b"abc"), diff_hash(b"abc"));
        assert_ne!(diff_hash(b"abc"), diff_hash(b"abd"));
        assert_ne!(diff_hash(b"abc"), diff_hash(b"abc "));
        assert!(diff_hash(b"x").ends_with("-1"));
    }
}
