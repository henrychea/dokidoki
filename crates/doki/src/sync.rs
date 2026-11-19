use crate::write_quiet;
use anyhow::{Context, Result, bail};
use doki_core::config::Config;
use doki_core::discovery;
use doki_core::lock::{LockAttempt, lock_path};
use doki_core::notify::{Desktop, Notifications, Notifier, Off};
use doki_core::pool::{self, EventBus};
use doki_core::scheduler;
use doki_core::store::Store;
use doki_core::sync::{self, SyncOptions, SyncReport, SyncState};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub use doki_agent::messenger::AgentMessenger;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    Repo(PathBuf),
    All,
    Due,
}

pub fn run(
    cfg: &Config,
    db: &Path,
    state_dir: &Path,
    selection: Selection,
    dry_run: bool,
) -> Result<()> {
    match selection {
        Selection::Repo(repo) => run_one(cfg, db, state_dir, &repo, dry_run),
        Selection::All => run_many(cfg, db, state_dir, Selection::All, dry_run),
        Selection::Due => run_many(cfg, db, state_dir, Selection::Due, dry_run),
    }
}

fn options(state_dir: &Path, dry_run: bool) -> SyncOptions {
    SyncOptions {
        dry_run,
        state_dir: Some(state_dir.to_path_buf()),
    }
}

fn run_one(cfg: &Config, db: &Path, state_dir: &Path, repo: &Path, dry_run: bool) -> Result<()> {
    let store = Store::open(db)?;
    let messenger = AgentMessenger::new(cfg, repo)?;
    let report = sync::sync_with(cfg, &store, repo, &messenger, options(state_dir, dry_run))?;
    let mut notifications = notifier(cfg);
    if let Some(row) = store.repo_by_path(&report.repo)? {
        notifications.notify_report(row.id, &row.name(), &report, now_unix());
    }
    write_quiet(io::stdout().lock(), &format!("{}\n", report_text(&report)))?;
    match report.state {
        SyncState::Blocked | SyncState::Failed | SyncState::Busy | SyncState::WaitingUser => {
            bail!(
                "{}: {}",
                report.state.label(),
                report.detail.clone().unwrap_or_default()
            )
        }
        _ => Ok(()),
    }
}

fn run_many(
    cfg: &Config,
    db: &Path,
    state_dir: &Path,
    selection: Selection,
    dry_run: bool,
) -> Result<()> {
    let _scheduler_lock = match selection {
        Selection::Due => match scheduler::try_lock(state_dir)? {
            LockAttempt::Acquired(lock) => Some(lock),
            LockAttempt::Busy => bail!(
                "busy: another doki scheduler holds {}",
                lock_path(state_dir).display()
            ),
        },
        _ => None,
    };
    let mut store = Store::open(db)?;
    store.upsert_detected(&discovery::scan(&cfg.roots, &cfg.discovery)?)?;
    let now = now_unix();
    let paths: Vec<PathBuf> = match selection {
        Selection::Due => scheduler::due(cfg, &store, now)?
            .into_iter()
            .map(|row| row.path)
            .collect(),
        _ => store
            .repos()?
            .into_iter()
            .filter(|row| row.enabled)
            .map(|row| row.path)
            .collect(),
    };
    if paths.is_empty() {
        let message = if selection == Selection::Due {
            "no repos are due\n"
        } else {
            "no enabled repos\n"
        };
        return write_quiet(io::stdout().lock(), message).map_err(Into::into);
    }

    let bus = EventBus::new();
    let mut notifications = notifier(cfg);
    let results = pool::run(paths, cfg.sync.concurrency, &bus, |path| {
        let store = Store::open(db)?;
        let messenger = AgentMessenger::new(cfg, path)?;
        sync::sync_with(cfg, &store, path, &messenger, options(state_dir, dry_run))
    });

    let mut out = String::new();
    let mut attention = 0;
    for (path, result) in results {
        match result {
            Ok(report) => {
                if let Some(row) = store.repo_by_path(&report.repo)? {
                    notifications.notify_report(row.id, &row.name(), &report, now_unix());
                }
                attention += usize::from(report.exit_code() != 0);
                out.push_str(&report_text(&report));
                out.push('\n');
            }
            Err(error) => {
                attention += 1;
                out.push_str(&format!(
                    "repo:   {}\nstate:  failed\nerror:  {error:#}\n\n",
                    path.display()
                ));
            }
        }
    }
    let total = out.lines().filter(|line| line.starts_with("repo:")).count();
    out.push_str(&format!(
        "{total} repo(s) synced, {attention} need attention\n"
    ));
    write_quiet(io::stdout().lock(), &out)?;
    if attention > 0 {
        bail!("{attention} repo(s) need attention");
    }
    Ok(())
}

fn notifier(cfg: &Config) -> Notifications<Box<dyn Notifier>> {
    let notifier: Box<dyn Notifier> = if cfg.notify.desktop {
        Box::new(Desktop)
    } else {
        Box::new(Off)
    };
    Notifications::new(notifier, cfg.notify.coalesce_seconds)
}

pub fn report_text(report: &SyncReport) -> String {
    let mut out = format!(
        "repo:   {}\nstate:  {}\n",
        report.repo.display(),
        report.state.label()
    );
    if let Some(scope) = report.scope {
        out.push_str(&format!("scope:  {scope:?}\n"));
    }
    if let Some(branch) = &report.branch {
        out.push_str(&format!("branch: {branch}\n"));
    }
    if let (Some(before), Some(after)) = (&report.head_before, &report.head_after) {
        out.push_str(&format!("head:   {} -> {}\n", short(before), short(after)));
    }
    if let Some(reference) = &report.snapshot_ref {
        out.push_str(&format!("snapshot: {reference}\n"));
    }
    if report.push != doki_core::push::PushState::Skipped {
        out.push_str(&format!("push:   {}\n", report.push.label()));
    }
    if let Some(agent) = &report.agent {
        let model = if report.model.is_empty() {
            "cli default".to_string()
        } else {
            report.model.clone()
        };
        out.push_str(&format!("agent:  {agent} [{model}]\n"));
    } else if report.state == SyncState::Committed {
        out.push_str("agent:  fallback (no agent produced a message)\n");
    }
    if let Some(message) = &report.message {
        for (index, line) in message.lines().enumerate() {
            if index == 0 {
                out.push_str(&format!("message: {line}\n"));
            } else {
                out.push_str(&format!("         {line}\n"));
            }
        }
    }
    if let Some(bundle) = &report.bundled {
        out.push_str(&format!("bundle: {}\n", bundle.display()));
    }
    if report.quarantined {
        out.push_str("quarantined: manual reset required\n");
    }
    if let Some(warning) = &report.warning {
        out.push_str(&format!("warning: {warning}\n"));
    }
    if let Some(detail) = &report.detail {
        out.push_str(&format!("detail: {detail}\n"));
    }
    for argv in &report.argv {
        out.push_str(&format!("argv:   git {}\n", argv.join(" ")));
    }
    if !report.agent_argv.is_empty() {
        out.push_str(&format!("argv:   {}\n", report.agent_argv.join(" ")));
    }
    if let Some(kind) = &report.incident_kind {
        out.push_str(&format!("incident: {kind}\n"));
    }
    out
}

fn short(sha: &str) -> String {
    sha.chars().take(12).collect()
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

pub fn reset_quarantine(cfg: &Config, db: &Path, repo: &Path) -> Result<()> {
    let store = Store::open(db)?;
    let row = store.repo_by_path(repo)?.with_context(|| {
        format!(
            "{} is not a known repo; run `doki status` first",
            repo.display()
        )
    })?;
    store.reset_quarantine(row.id)?;
    let next = scheduler::next_for_repo(cfg, &row, now_unix())?;
    store.set_next_run(row.id, Some(next))?;
    write_quiet(
        io::stdout().lock(),
        &format!(
            "quarantine cleared for {}\nnext run in {}s\n",
            row.path.display(),
            next - now_unix()
        ),
    )
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use doki_core::config::CommitScope;
    use doki_core::sync::{ChangedFile, MessageContext, Messenger};

    fn repo() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn messenger_exposes_the_resolved_agent_and_preview_argv() {
        let tmp = repo();
        let shim = tmp.path().join("omp-shim");
        std::fs::write(&shim, "#!/bin/sh\nexit 0\n").unwrap();
        let mut perms = std::fs::metadata(&shim).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&shim, perms).unwrap();

        let cfg = Config::parse(&format!(
            "[agent]\norder = [\"omp\"]\n\n[agents.omp]\ncli = \"{}\"\nmodel = \"m/n\"\n",
            shim.display()
        ))
        .unwrap();
        let messenger = AgentMessenger::new(&cfg, tmp.path()).unwrap();
        assert_eq!(messenger.agent(), "omp");
        assert_eq!(messenger.model(), "m/n");

        let ctx = MessageContext {
            repo_name: "repo".into(),
            branch: "main".into(),
            files: vec![ChangedFile {
                path: "a.txt".into(),
                added: Some(1),
                deleted: Some(0),
            }],
            diff: "+one\n".into(),
            ..MessageContext::default()
        };
        let argv = messenger.preview(&ctx);
        assert_eq!(argv[0], shim.display().to_string());
        assert!(argv.contains(&"--model".to_string()));
        assert!(argv.contains(&"m/n".to_string()));
    }

    #[test]
    fn dry_run_text_prints_scope_argv_and_state() {
        let mut report = SyncReport::new("/tmp/repo", true);
        report.state = SyncState::DryRun;
        report.scope = Some(CommitScope::Tracked);
        report.branch = Some("main".into());
        report.head_before = Some("0123456789abcdef".into());
        report.head_after = Some("0123456789abcdef".into());
        report.snapshot_ref = Some("refs/doki/pre-sync/1".into());
        report.argv = vec![vec!["add".into(), "-u".into()]];
        report.agent_argv = vec!["omp".into(), "-p".into()];
        let text = report_text(&report);
        assert!(text.contains("state:  dry-run"), "{text}");
        assert!(text.contains("scope:  Tracked"), "{text}");
        assert!(text.contains("branch: main"), "{text}");
        assert!(text.contains("argv:   git add -u"), "{text}");
        assert!(text.contains("argv:   omp -p"), "{text}");
    }

    #[test]
    fn committed_text_shows_agent_and_message() {
        let mut report = SyncReport::new("/tmp/repo", false);
        report.state = SyncState::Committed;
        report.agent = Some("codex".into());
        report.model = "gpt-5.2".into();
        report.message = Some("doki: add widget\n\nBody.\n".into());
        report.push = doki_core::push::PushState::Pushed;
        let text = report_text(&report);
        assert!(text.contains("agent:  codex [gpt-5.2]"), "{text}");
        assert!(text.contains("message: doki: add widget"), "{text}");
        assert!(text.contains("         Body."), "{text}");
        assert!(text.contains("push:   pushed"), "{text}");
    }
}
