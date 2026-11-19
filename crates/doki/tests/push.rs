use doki::sync::AgentMessenger;
use doki_core::config::Config;
use doki_core::discovery::DetectedRepo;
use doki_core::push::PushState;
use doki_core::store::{RepoState, Store};
use doki_core::sync::{self, SyncOptions, SyncReport, SyncState};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;
use tempfile::TempDir;

static ISOLATE: Once = Once::new();

/// Keep a developer's real git config out of every assertion in this file.
fn isolate_git() {
    ISOLATE.call_once(|| {
        // SAFETY: set once to the same value before any git work starts.
        unsafe {
            std::env::set_var("GIT_CONFIG_GLOBAL", "/dev/null");
            std::env::set_var("GIT_CONFIG_SYSTEM", "/dev/null");
            std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
        }
    });
}

fn git(dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "doki-test")
        .env("GIT_AUTHOR_EMAIL", "doki@example.com")
        .env("GIT_COMMITTER_NAME", "doki-test")
        .env("GIT_COMMITTER_EMAIL", "doki@example.com")
        .env("GIT_TERMINAL_PROMPT", "0");
    command
}

fn git_ok(dir: &Path, args: &[&str]) -> String {
    let out = git(dir, args).output().expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

fn chmod(path: &Path) {
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

struct Fixture {
    _tmp: TempDir,
    root: PathBuf,
    bare: PathBuf,
    state: PathBuf,
    store: Store,
    cfg: Config,
    branch: String,
}

impl Fixture {
    fn new() -> Self {
        Self::build("main", "[sync]\nprotected = [\"main\", \"master\"]\n", true)
    }

    fn on_branch(branch: &str) -> Self {
        Self::build(branch, "[sync]\nprotected = []\n", true)
    }

    fn build(branch: &str, sync_section: &str, upstream: bool) -> Self {
        isolate_git();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let bare = tmp.path().join("remote.git");
        let state = tmp.path().join("state");
        fs::create_dir_all(&state).unwrap();
        git_ok(tmp.path(), &["init", "-q", "--bare", "remote.git"]);
        git_ok(
            &bare,
            &["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")],
        );
        git_ok(tmp.path(), &["init", "-q", "-b", branch, "repo"]);
        git_ok(&root, &["config", "user.name", "doki-test"]);
        git_ok(&root, &["config", "user.email", "doki@example.com"]);
        git_ok(&root, &["config", "commit.gpgsign", "false"]);
        let hooks = root.join(".git/hooks");
        let hooks = hooks.to_str().unwrap();
        git_ok(&root, &["config", "core.hooksPath", hooks]);
        fs::write(root.join("a.txt"), "one\n").unwrap();
        git_ok(&root, &["add", "a.txt"]);
        git_ok(&root, &["commit", "-qm", "initial"]);
        let url = format!("file://{}", bare.display());
        git_ok(&root, &["remote", "add", "origin", &url]);
        if upstream {
            git_ok(&root, &["push", "-q", "-u", "origin", branch]);
        }

        let shim = tmp.path().join("omp");
        let script = "#!/bin/sh\ncat >/dev/null 2>&1 || true\nprintf 'add widget\\n'\n";
        fs::write(&shim, script).unwrap();
        chmod(&shim);

        let cfg = Config::parse(&format!(
            "[agent]\norder = [\"omp\"]\nfallback_on_error = false\nmax_attempts = 1\n\n\
             [agents.omp]\ncli = \"{}\"\nmodel = \"test/model\"\n\n\
             [notify]\ndesktop = false\n\n{sync_section}",
            shim.display()
        ))
        .unwrap();

        let mut store = Store::open_in_memory().unwrap();
        store
            .upsert_detected(&[DetectedRepo {
                root: "test".into(),
                path: root.canonicalize().unwrap(),
                git_common_dir: None,
            }])
            .unwrap();
        let id = store.repos().unwrap()[0].id;
        store.set_enabled(id, true).unwrap();

        Self {
            _tmp: tmp,
            root,
            bare,
            state,
            store,
            cfg,
            branch: branch.to_string(),
        }
    }

    fn with_sync(sync_section: &str) -> Self {
        Self::build("topic", sync_section, true)
    }

    fn repo_id(&self) -> i64 {
        self.store.repos().unwrap()[0].id
    }

    fn head(&self) -> String {
        git_ok(&self.root, &["rev-parse", "HEAD"])
    }

    fn remote_head(&self, branch: &str) -> String {
        git_ok(&self.bare, &["rev-parse", branch])
    }

    fn dirty(&self, body: &str) {
        fs::write(self.root.join("a.txt"), body).unwrap();
    }

    fn sync(&self) -> SyncReport {
        self.sync_with(SyncOptions {
            dry_run: false,
            state_dir: Some(self.state.clone()),
        })
    }

    fn sync_with(&self, opts: SyncOptions) -> SyncReport {
        let messenger = AgentMessenger::new(&self.cfg, &self.root).unwrap();
        sync::sync_with(&self.cfg, &self.store, &self.root, &messenger, opts).unwrap()
    }

    fn repo(&self) -> doki_core::store::RepoRow {
        self.store.repo_by_path(&self.root).unwrap().unwrap()
    }

    fn last_run(&self) -> doki_core::store::RunRow {
        self.store
            .last_run(self.repo_id())
            .unwrap()
            .expect("a run row")
    }

    fn add_remote_hook(&self, name: &str, body: &str) {
        let path = self.bare.join("hooks").join(name);
        fs::write(&path, body).unwrap();
        chmod(&path);
    }

    fn push_elsewhere(&self) {
        let other = self.bare.parent().unwrap().join("other");
        let url = format!("file://{}", self.bare.display());
        git_ok(self.bare.parent().unwrap(), &["clone", "-q", &url, "other"]);
        fs::write(other.join("b.txt"), "remote\n").unwrap();
        git_ok(&other, &["add", "b.txt"]);
        git_ok(&other, &["commit", "-qm", "remote work"]);
        git_ok(&other, &["push", "-q", "origin", &self.branch]);
    }
}

#[test]
fn a_dirty_repo_commits_and_the_push_lands_in_the_bare_remote() {
    let f = Fixture::on_branch("topic");
    let before = f.head();
    f.dirty("one\ntwo\n");

    let report = f.sync();
    assert_eq!(report.state, SyncState::Committed, "{report:?}");
    assert_eq!(report.push, PushState::Pushed, "{report:?}");
    assert!(report.pushed);
    assert_eq!(
        git_ok(&f.root, &["rev-parse", "HEAD"]),
        f.remote_head(&f.branch)
    );
    assert_ne!(f.head(), before);
    assert_eq!(f.repo().state, Some(RepoState::Clean));

    let run = f.last_run();
    assert_eq!(run.state, "committed");
    assert!(run.pushed, "run.pushed must be true after a push");
    assert!(run.error_kind.is_none());
    assert!(report.argv.iter().any(|argv| argv[0] == "fetch"));
    assert!(
        report
            .argv
            .iter()
            .any(|argv| argv[0] == "push" && argv.contains(&f.branch))
    );
    assert!(
        !report
            .argv
            .iter()
            .flatten()
            .any(|arg| arg.contains("--force"))
    );
}

#[test]
fn a_first_push_sets_the_upstream_with_u() {
    let f = Fixture::build("topic", "[sync]\nprotected = []\n", false);
    f.dirty("one\ntwo\n");

    let report = f.sync();
    assert_eq!(report.state, SyncState::Committed, "{report:?}");
    assert_eq!(report.push, PushState::Pushed, "{report:?}");
    assert_eq!(
        git_ok(&f.root, &["rev-parse", "HEAD"]),
        f.remote_head("topic")
    );
    assert_eq!(
        git_ok(&f.root, &["rev-parse", "--abbrev-ref", "topic@{u}"]),
        "origin/topic"
    );
    assert!(
        report
            .argv
            .iter()
            .any(|argv| argv == &["push", "-u", "origin", "topic"])
    );
}

#[test]
fn a_diverged_branch_waits_for_a_human_and_never_forces() {
    let f = Fixture::on_branch("topic");
    f.push_elsewhere();
    let remote_before = f.remote_head("topic");
    f.dirty("one\ntwo\n");

    let report = f.sync();
    assert_eq!(report.state, SyncState::WaitingUser, "{report:?}");
    assert_eq!(report.push, PushState::Diverged, "{report:?}");
    assert!(!report.pushed);
    assert_eq!(f.remote_head("topic"), remote_before);
    assert_eq!(
        git_ok(&f.bare, &["rev-list", "--count", "topic"]),
        "2",
        "the remote must not receive the local commit"
    );
    assert!(report.head_after.is_some(), "the local commit is kept");

    let incidents = f.store.incidents(f.repo_id()).unwrap();
    assert_eq!(incidents.len(), 1);
    assert_eq!(incidents[0].kind, "diverged");
    assert_eq!(f.last_run().state, "waiting");
    assert!(!f.last_run().pushed);
    assert_eq!(f.repo().state, Some(RepoState::WaitingUser));
    assert!(
        !report
            .argv
            .iter()
            .flatten()
            .any(|arg| arg == "--force" || arg == "--force-with-lease")
    );
}

#[test]
fn a_behind_only_branch_waits_for_a_human() {
    let f = Fixture::on_branch("topic");
    f.push_elsewhere();
    let local_before = f.head();
    let remote_before = f.remote_head("topic");

    let report = f.sync();
    assert_eq!(report.state, SyncState::WaitingUser, "{report:?}");
    assert_eq!(report.push, PushState::Diverged);
    assert_eq!(f.head(), local_before, "no merge and no rebase");
    assert_eq!(f.remote_head("topic"), remote_before);
    assert_eq!(f.store.incidents(f.repo_id()).unwrap()[0].kind, "diverged");
    assert_eq!(f.repo().state, Some(RepoState::WaitingUser));
}

#[test]
fn a_protected_branch_commits_but_does_not_push_without_opt_in() {
    let f = Fixture::new();
    assert_eq!(f.branch, "main");
    let remote_before = f.remote_head("main");
    f.dirty("one\ntwo\n");

    let report = f.sync();
    assert_eq!(report.state, SyncState::Committed, "{report:?}");
    assert_eq!(report.push, PushState::Protected, "{report:?}");
    assert!(!report.pushed);
    assert_ne!(f.head(), remote_before);
    assert_eq!(f.remote_head("main"), remote_before);
    assert!(f.store.incidents(f.repo_id()).unwrap().is_empty());
    assert!(!f.repo().allow_protected_push);

    f.store.set_allow_protected_push(f.repo_id(), true).unwrap();
    f.dirty("one\nthree\n");
    let report = f.sync();
    assert_eq!(report.state, SyncState::Committed, "{report:?}");
    assert_eq!(report.push, PushState::Pushed, "{report:?}");
    assert_eq!(f.head(), f.remote_head("main"));
    assert!(f.repo().allow_protected_push);
}

#[test]
fn a_rejected_push_fails_the_run_and_counts_a_failure() {
    let f = Fixture::on_branch("topic");
    f.add_remote_hook(
        "pre-receive",
        "#!/bin/sh\necho 'policy says no' >&2\nexit 1\n",
    );
    let remote_before = f.remote_head("topic");
    f.dirty("one\ntwo\n");

    let report = f.sync();
    assert_eq!(report.state, SyncState::Failed, "{report:?}");
    assert_eq!(report.push, PushState::Failed);
    assert_eq!(f.remote_head("topic"), remote_before);
    assert!(
        git_ok(&f.root, &["log", "-1", "--format=%s"]).starts_with("doki: "),
        "the local commit is kept"
    );

    let incidents = f.store.incidents(f.repo_id()).unwrap();
    assert_eq!(incidents.len(), 1);
    assert_eq!(incidents[0].kind, "push");
    assert!(incidents[0].detail.contains("policy says no"));
    let run = f.last_run();
    assert_eq!(run.state, "failed");
    assert_eq!(run.error_kind.as_deref(), Some("push"));
    assert!(!run.pushed);
    assert_eq!(run.head_before, Some(f.remote_head("topic")));
    assert!(run.head_after.is_some());

    let repo = f.repo();
    assert_eq!(repo.fail_count, 1);
    assert_eq!(repo.state, Some(RepoState::Blocked));
    assert_eq!(repo.last_sync_at.map(|at| at + 120), repo.next_run_at);
}

#[test]
fn a_slow_push_is_killed_by_the_timeout() {
    let f = Fixture::with_sync(
        "[sync]\nprotected = []\npush_timeout_seconds = 1\nfetch_timeout_seconds = 30\n",
    );
    f.add_remote_hook("pre-receive", "#!/bin/sh\nsleep 5\nexit 1\n");
    let remote_before = f.remote_head("topic");
    f.dirty("one\ntwo\n");

    let start = std::time::Instant::now();
    let report = f.sync();
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
    assert_eq!(report.state, SyncState::Failed, "{report:?}");
    assert_eq!(report.incident_kind.as_deref(), Some("push-timeout"));
    assert_eq!(f.remote_head("topic"), remote_before);
    assert_eq!(
        f.store.incidents(f.repo_id()).unwrap()[0].kind,
        "push-timeout"
    );
}

#[test]
fn repeated_failures_quarantine_until_a_manual_reset() {
    let f = Fixture::with_sync("[sync]\nprotected = []\nquarantine_after = 2\n");
    f.add_remote_hook("pre-receive", "#!/bin/sh\necho 'still no' >&2\nexit 1\n");
    f.dirty("one\ntwo\n");

    assert_eq!(f.sync().state, SyncState::Failed);
    assert_eq!(f.repo().fail_count, 1);
    assert_eq!(f.repo().state, Some(RepoState::Blocked));

    let second = f.sync();
    assert_eq!(second.state, SyncState::Failed, "{second:?}");
    assert!(second.quarantined);
    assert_eq!(second.fail_count, Some(2));
    let repo = f.repo();
    assert_eq!(repo.state, Some(RepoState::Quarantined));
    assert_eq!(repo.fail_count, 2);
    assert!(repo.quarantine_until.is_some());
    assert_eq!(repo.next_run_at, None, "quarantine stops the schedule");
    assert_eq!(
        f.store.incidents(f.repo_id()).unwrap().last().unwrap().kind,
        "quarantined"
    );

    let blocked = f.sync();
    assert_eq!(blocked.state, SyncState::Blocked, "{blocked:?}");
    assert!(blocked.detail.unwrap().contains("quarantined"));
    assert_eq!(f.repo().fail_count, 2, "a quarantined repo does not retry");

    f.store.reset_quarantine(f.repo_id()).unwrap();
    let repo = f.repo();
    assert_eq!(repo.fail_count, 0);
    assert_eq!(repo.quarantine_until, None);
    assert_eq!(repo.state, Some(RepoState::Clean));
    assert_eq!(repo.next_run_at, None);
}

#[test]
fn a_repo_without_a_remote_commits_and_records_no_remote() {
    let f = Fixture::on_branch("topic");
    git_ok(&f.root, &["remote", "remove", "origin"]);
    f.dirty("one\ntwo\n");

    let report = f.sync();
    assert_eq!(report.state, SyncState::Committed, "{report:?}");
    assert_eq!(report.push, PushState::NoRemote, "{report:?}");
    assert!(!report.pushed);
    assert!(report.head_after.is_some());
    assert_eq!(f.repo().state, Some(RepoState::NoRemote));
    let incidents = f.store.incidents(f.repo_id()).unwrap();
    assert_eq!(incidents.len(), 1);
    assert_eq!(incidents[0].kind, "no-remote");
    assert_eq!(incidents[0].severity, "warning");
    assert!(!f.last_run().pushed);
}

#[test]
fn bundle_mode_writes_a_bundle_and_records_a_snapshot() {
    let f = Fixture::with_sync("[sync]\nprotected = []\npush_mode = \"bundle\"\n");
    let remote_before = f.remote_head("topic");
    f.dirty("one\ntwo\n");

    let report = f.sync();
    assert_eq!(report.state, SyncState::Committed, "{report:?}");
    assert_eq!(report.push, PushState::Bundled, "{report:?}");
    let bundle = report.bundled.clone().expect("a bundle path");
    assert!(bundle.exists(), "{}", bundle.display());
    assert!(bundle.starts_with(&f.state));
    assert_eq!(
        f.remote_head("topic"),
        remote_before,
        "bundle mode never pushes"
    );
    let text = fs::read(&bundle).unwrap();
    assert!(String::from_utf8_lossy(&text).contains("# v2 git bundle"));

    let run = f.last_run();
    let snapshot = f.store.last_snapshot(run.id).unwrap().unwrap();
    assert_eq!(snapshot.reference, bundle.display().to_string());
    assert_eq!(snapshot.sha, f.head());
}

#[test]
fn commit_only_mode_never_fetches_or_pushes() {
    let f = Fixture::with_sync("[sync]\nprotected = []\npush_mode = \"commit-only\"\n");
    let remote_before = f.remote_head("topic");
    f.dirty("one\ntwo\n");

    let report = f.sync();
    assert_eq!(report.state, SyncState::Committed, "{report:?}");
    assert_eq!(report.push, PushState::CommitOnly);
    assert_eq!(f.remote_head("topic"), remote_before);
    assert!(!report.argv.iter().any(|argv| argv[0] == "fetch"));
    assert!(!report.argv.iter().any(|argv| argv[0] == "push"));
}

#[test]
fn dry_run_against_a_remote_executes_no_fetch_or_push() {
    let f = Fixture::on_branch("topic");
    f.dirty("one\ntwo\n");
    let head = f.head();
    let remote_before = f.remote_head("topic");

    let report = f.sync_with(SyncOptions {
        dry_run: true,
        state_dir: Some(f.state.clone()),
    });
    assert_eq!(report.state, SyncState::DryRun, "{report:?}");
    assert_eq!(f.head(), head);
    assert_eq!(f.remote_head("topic"), remote_before);
    assert_eq!(git_ok(&f.root, &["for-each-ref", "refs/doki"]), "");
    assert_eq!(git_ok(&f.root, &["diff", "--cached", "--name-only"]), "");
    assert!(f.store.runs(f.repo_id()).unwrap().is_empty());
    assert!(
        report
            .argv
            .iter()
            .any(|argv| argv == &["fetch", "--prune", "origin"]),
        "the dry run must preview the fetch: {report:?}"
    );
    assert!(
        report
            .argv
            .iter()
            .any(|argv| argv == &["push", "origin", "topic"]),
        "the dry run must preview the push: {report:?}"
    );
    assert!(report.detail.unwrap().contains("dry run"));
}
