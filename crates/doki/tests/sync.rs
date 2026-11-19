use doki::sync::AgentMessenger;
use doki_core::config::Config;
use doki_core::discovery::DetectedRepo;
use doki_core::store::Store;
use doki_core::sync::{self, SyncReport, SyncState};
use doki_core::undo::{self, UndoReport, UndoState};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

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
    marker: PathBuf,
    store: Store,
    cfg: Config,
}

impl Fixture {
    fn new() -> Self {
        Self::with_agent("printf 'add widget\\n'")
    }

    fn with_agent(body: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        git_ok(tmp.path(), &["init", "-q", "-b", "main", "repo"]);
        git_ok(&root, &["config", "user.name", "doki-test"]);
        git_ok(&root, &["config", "user.email", "doki@example.com"]);
        git_ok(&root, &["config", "commit.gpgsign", "false"]);
        let hooks = root.join(".git/hooks");
        git_ok(
            &root,
            &["config", "core.hooksPath", hooks.to_str().unwrap()],
        );
        fs::write(root.join("a.txt"), "one\n").unwrap();
        git_ok(&root, &["add", "a.txt"]);
        git_ok(&root, &["commit", "-qm", "initial"]);

        let marker = tmp.path().join("agent-calls");
        let shim = tmp.path().join("omp");
        let script = format!(
            "#!/bin/sh\necho called >> '{}'\ncat >/dev/null 2>&1 || true\n{body}\n",
            marker.display()
        );
        fs::write(&shim, script).unwrap();
        chmod(&shim);

        let cfg = Config::parse(&format!(
            "[agent]\norder = [\"omp\"]\nfallback_on_error = false\nmax_attempts = 1\n\n\
             [agents.omp]\ncli = \"{}\"\nmodel = \"test/model\"\n",
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
            marker,
            store,
            cfg,
        }
    }

    fn repo_id(&self) -> i64 {
        self.store.repos().unwrap()[0].id
    }

    fn head(&self) -> String {
        git_ok(&self.root, &["rev-parse", "HEAD"])
    }

    fn dirty(&self, body: &str) {
        fs::write(self.root.join("a.txt"), body).unwrap();
    }

    fn sync(&self, dry_run: bool) -> SyncReport {
        let messenger = AgentMessenger::new(&self.cfg, &self.root).unwrap();
        sync::sync(&self.cfg, &self.store, &self.root, &messenger, dry_run).unwrap()
    }

    fn undo(&self) -> UndoReport {
        undo::undo(&self.store, &self.root).unwrap()
    }

    fn shim(&self) -> PathBuf {
        self.root.parent().unwrap().join("omp")
    }

    fn add_hook(&self, name: &str, body: &str) {
        let path = self.root.join(".git/hooks").join(name);
        fs::write(&path, body).unwrap();
        chmod(&path);
    }

    fn last_run(&self) -> doki_core::store::RunRow {
        self.store
            .last_run(self.repo_id())
            .unwrap()
            .expect("a run row")
    }
}

fn agent_calls(marker: &Path) -> usize {
    fs::read_to_string(marker)
        .map(|text| text.lines().count())
        .unwrap_or(0)
}

fn status_lines(root: &Path) -> Vec<String> {
    git_ok(root, &["status", "--porcelain"])
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn dirty_repo_commits_the_shim_subject_and_snapshots_first() {
    let f = Fixture::new();
    let snapshot_probe = f.root.parent().unwrap().join("presync-at-hook");
    f.add_hook(
        "pre-commit",
        &format!(
            "#!/bin/sh\ngit for-each-ref --format='%(objectname)' refs/doki/pre-sync/ > '{}'\n",
            snapshot_probe.display()
        ),
    );
    f.dirty("one\ntwo\n");
    let head_before = f.head();

    let report = f.sync(false);
    assert_eq!(report.state, SyncState::Committed, "{report:?}");
    assert_eq!(report.head_before.as_deref(), Some(head_before.as_str()));
    assert_ne!(report.head_after.as_deref(), Some(head_before.as_str()));
    assert_eq!(report.agent.as_deref(), Some("omp"));
    assert_eq!(report.model, "test/model");
    assert_eq!(report.message.as_deref(), Some("doki: add widget\n"));
    assert!(agent_calls(&f.marker) == 1);

    assert_eq!(
        git_ok(&f.root, &["log", "-1", "--format=%s"]),
        "doki: add widget"
    );
    let reference = report.snapshot_ref.clone().expect("snapshot ref");
    assert!(reference.starts_with("refs/doki/pre-sync/"));
    assert_eq!(git_ok(&f.root, &["rev-parse", &reference]), head_before);

    let probe = fs::read_to_string(&snapshot_probe).unwrap();
    assert_eq!(probe.trim(), head_before, "pre-sync ref missing in hook");

    let run = f.last_run();
    assert_eq!(run.state, "committed");
    assert_eq!(run.head_before.as_deref(), Some(head_before.as_str()));
    assert_eq!(run.head_after.as_deref(), report.head_after.as_deref());
    assert_eq!(run.agent.as_deref(), Some("omp"));
    assert_eq!(run.model.as_deref(), Some("test/model"));
    assert_eq!(run.message.as_deref(), Some("doki: add widget\n"));
    assert_eq!(
        f.store.last_snapshot(run.id).unwrap().unwrap().sha,
        head_before
    );
}

#[test]
fn dry_run_reports_argv_and_mutates_nothing() {
    let f = Fixture::new();
    f.dirty("one\ntwo\n");
    let head = f.head();

    let report = f.sync(true);
    assert_eq!(report.state, SyncState::DryRun, "{report:?}");
    assert!(
        report
            .argv
            .iter()
            .any(|argv| argv.first() == Some(&"commit".to_string()))
    );
    assert!(
        report
            .argv
            .iter()
            .any(|argv| argv.first() == Some(&"add".to_string()))
    );
    assert!(!report.agent_argv.is_empty());
    assert!(agent_calls(&f.marker) == 0, "dry run called the agent");

    assert_eq!(f.head(), head);
    assert_eq!(git_ok(&f.root, &["diff", "--cached", "--name-only"]), "");
    assert_eq!(git_ok(&f.root, &["for-each-ref", "refs/doki"]), "");
    assert!(f.store.runs(f.repo_id()).unwrap().is_empty());
}

#[test]
fn undo_restores_head_and_keeps_every_change() {
    let f = Fixture::new();
    f.dirty("one\ntwo\n");
    let head_before = f.head();
    let report = f.sync(false);
    assert_eq!(report.state, SyncState::Committed);

    let undone = f.undo();
    assert_eq!(undone.state, UndoState::Undone, "{undone:?}");
    assert_eq!(f.head(), head_before);
    assert_eq!(
        fs::read_to_string(f.root.join("a.txt")).unwrap(),
        "one\ntwo\n"
    );
    assert_eq!(
        git_ok(&f.root, &["diff", "--cached", "--name-only"]),
        "a.txt"
    );
    assert_eq!(git_ok(&f.root, &["diff", "--name-only"]), "");
    assert!(
        status_lines(&f.root)
            .iter()
            .any(|line| line.ends_with("a.txt"))
    );
}

#[test]
fn undo_refuses_when_head_moved() {
    let f = Fixture::new();
    f.dirty("one\ntwo\n");
    f.sync(false);

    fs::write(f.root.join("b.txt"), "later\n").unwrap();
    git_ok(&f.root, &["add", "b.txt"]);
    git_ok(&f.root, &["commit", "-qm", "later work"]);
    let moved = f.head();

    let undone = f.undo();
    assert_eq!(undone.state, UndoState::Refused);
    assert!(undone.detail.contains("HEAD moved"), "{}", undone.detail);
    assert_eq!(f.head(), moved);
}

#[test]
fn undo_refuses_after_the_run_pushed() {
    let f = Fixture::new();
    f.dirty("one\ntwo\n");
    let report = f.sync(false);
    let run = f.last_run();
    f.store.mark_run_pushed(run.id, true).unwrap();

    let undone = f.undo();
    assert_eq!(undone.state, UndoState::Refused);
    assert!(undone.detail.contains("pushed"), "{}", undone.detail);
    assert_eq!(f.head(), report.head_after.unwrap());
}

#[test]
fn a_secret_never_reaches_the_agent_and_raises_an_incident() {
    let f = Fixture::new();
    f.dirty("aws = AKIA0000000000000000\n");
    let head = f.head();

    let report = f.sync(false);
    assert_eq!(report.state, SyncState::Blocked, "{report:?}");
    assert_eq!(
        agent_calls(&f.marker),
        0,
        "the secret diff reached the agent"
    );
    assert_eq!(f.head(), head);

    let run = f.last_run();
    assert_eq!(run.state, "blocked");
    assert_eq!(run.error_kind.as_deref(), Some("secret"));
    assert!(run.agent.is_none() && run.message.is_none());

    let incidents = f.store.incidents(f.repo_id()).unwrap();
    assert_eq!(incidents.len(), 1);
    assert_eq!(incidents[0].kind, "secret");
    assert_eq!(incidents[0].severity, "blocking");
    assert!(!report.detail.unwrap_or_default().is_empty());
    assert_eq!(
        git_ok(&f.root, &["diff", "--cached", "--name-only"]),
        "a.txt"
    );
}

#[test]
fn a_failing_pre_commit_hook_fails_the_run_without_bypass() {
    let f = Fixture::new();
    f.add_hook("pre-commit", "#!/bin/sh\necho 'hook says no' >&2\nexit 1\n");
    f.dirty("one\ntwo\n");
    let head = f.head();

    let report = f.sync(false);
    assert_eq!(report.state, SyncState::Failed, "{report:?}");
    assert!(!report.argv.iter().flatten().any(|arg| arg == "--no-verify"));
    assert_eq!(f.head(), head);

    let run = f.last_run();
    assert_eq!(run.state, "failed");
    assert_eq!(run.error_kind.as_deref(), Some("hook"));
    assert!(
        run.finished_at.is_some(),
        "the failed run must be closed in the journal"
    );

    let incidents = f.store.incidents(f.repo_id()).unwrap();
    assert_eq!(incidents.len(), 1);
    assert_eq!(incidents[0].kind, "hook");
    assert!(incidents[0].detail.contains("hook says no"));
}

#[test]
fn a_clean_repo_is_a_skip_without_a_run_or_agent_call() {
    let f = Fixture::new();
    let report = f.sync(false);
    assert_eq!(report.state, SyncState::Skipped, "{report:?}");
    assert_eq!(agent_calls(&f.marker), 0);
    assert!(f.store.runs(f.repo_id()).unwrap().is_empty());
    assert_eq!(git_ok(&f.root, &["for-each-ref", "refs/doki"]), "");
}

#[test]
fn a_cached_message_skips_the_agent_on_rerun() {
    let f = Fixture::new();
    f.dirty("one\ntwo\n");
    let first = f.sync(false);
    assert_eq!(first.state, SyncState::Committed);
    assert_eq!(agent_calls(&f.marker), 1);

    assert_eq!(f.undo().state, UndoState::Undone);
    let second = f.sync(false);
    assert_eq!(second.state, SyncState::Committed, "{second:?}");
    assert_eq!(second.message, first.message);
    assert_eq!(
        agent_calls(&f.marker),
        1,
        "cache hit still called the agent"
    );
    assert_eq!(second.agent.as_deref(), Some("omp"));
    assert_eq!(second.model, "test/model");
}

#[test]
fn an_index_change_during_message_generation_blocks_the_commit() {
    let f = Fixture::new();
    f.dirty("one\ntwo\n");
    let script = format!(
        "#!/bin/sh\necho called >> '{}'\ncat >/dev/null 2>&1 || true\n\
         printf 'add widget\\n'\nprintf 'sneaky\\n' > '{}/sneaky.txt'\n\
         git -C '{}' add sneaky.txt\n",
        f.marker.display(),
        f.root.display(),
        f.root.display()
    );
    fs::write(f.shim(), script).unwrap();
    chmod(&f.shim());
    let head = f.head();

    let report = f.sync(false);
    assert_eq!(report.state, SyncState::Blocked, "{report:?}");
    assert_eq!(f.head(), head, "a changed index must not be committed");
    let run = f.last_run();
    assert_eq!(run.state, "blocked");
    assert_eq!(run.error_kind.as_deref(), Some("changed"));
    assert_eq!(f.store.incidents(f.repo_id()).unwrap()[0].kind, "changed");
    assert!(
        git_ok(&f.root, &["diff", "--cached", "--name-only"])
            .lines()
            .any(|line| line == "sneaky.txt")
    );
}

#[test]
fn ignore_globs_exclude_paths_from_the_commit() {
    let f = Fixture::new();
    fs::write(f.root.join("secret.txt"), "v1\n").unwrap();
    git_ok(&f.root, &["add", "secret.txt"]);
    git_ok(&f.root, &["commit", "-qm", "add secret"]);

    fs::write(f.root.join(".doki.toml"), "ignore = [\"secret.txt\"]\n").unwrap();
    f.dirty("one\ntwo\n");
    fs::write(f.root.join("secret.txt"), "v2\n").unwrap();

    let report = f.sync(false);
    assert_eq!(report.state, SyncState::Committed, "{report:?}");
    let committed = git_ok(&f.root, &["show", "--name-only", "--format=", "HEAD"]);
    assert_eq!(committed, "a.txt");
    let status = status_lines(&f.root);
    assert!(
        status.iter().any(|line| line.ends_with("secret.txt")),
        "ignored file should stay in the worktree: {status:?}"
    );
}

#[test]
fn a_repo_must_be_enabled_and_a_repo_file_can_only_disable_it() {
    let f = Fixture::new();
    f.dirty("one\ntwo\n");
    f.store.set_enabled(f.repo_id(), false).unwrap();
    let messenger = AgentMessenger::new(&f.cfg, &f.root).unwrap();
    assert!(sync::sync(&f.cfg, &f.store, &f.root, &messenger, false).is_err());

    f.store.set_enabled(f.repo_id(), true).unwrap();
    fs::write(f.root.join(".doki.toml"), "enabled = false\n").unwrap();
    let report = sync::sync(&f.cfg, &f.store, &f.root, &messenger, false).unwrap();
    assert_eq!(report.state, SyncState::Disabled);
    assert_eq!(agent_calls(&f.marker), 0);
}

#[test]
fn mass_delete_guard_blocks_before_the_agent() {
    let mut f = Fixture::new();
    let lines: String = (1..=6).map(|n| format!("line {n}\n")).collect();
    fs::write(f.root.join("a.txt"), &lines).unwrap();
    git_ok(&f.root, &["add", "a.txt"]);
    git_ok(&f.root, &["commit", "-qm", "six lines"]);
    f.cfg.guards.max_deleted_lines = 2;
    f.dirty("line 1\n");
    let head = f.head();

    let report = f.sync(false);
    assert_eq!(report.state, SyncState::Blocked, "{report:?}");
    assert_eq!(
        agent_calls(&f.marker),
        0,
        "guard blocked after a model call"
    );
    assert_eq!(f.head(), head);
    let run = f.last_run();
    assert_eq!(run.state, "blocked");
    assert_eq!(run.error_kind.as_deref(), Some("guard"));
    assert_eq!(f.store.incidents(f.repo_id()).unwrap()[0].kind, "guard");
    assert!(
        fs::read_to_string(f.root.join("a.txt"))
            .unwrap()
            .contains("line 1")
    );
}

#[test]
fn an_in_progress_rebase_is_refused_without_touching_the_tree() {
    let f = Fixture::new();
    f.dirty("one\ntwo\n");
    fs::create_dir(f.root.join(".git/rebase-merge")).unwrap();
    let head = f.head();
    let before = git_ok(&f.root, &["status", "--porcelain"]);

    let report = f.sync(false);
    assert_eq!(report.state, SyncState::Blocked, "{report:?}");
    assert_eq!(
        agent_calls(&f.marker),
        0,
        "a refused repo reached the agent"
    );
    assert_eq!(f.head(), head);
    assert_eq!(git_ok(&f.root, &["status", "--porcelain"]), before);
    let run = f.last_run();
    assert_eq!(run.state, "blocked");
    assert_eq!(run.error_kind.as_deref(), Some("refused"));
    assert_eq!(f.store.incidents(f.repo_id()).unwrap()[0].kind, "refused");
}
