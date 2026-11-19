use doki_core::lock::LockAttempt;
use doki_core::scheduler;
use doki_core::store::{RepoState, Store};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
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
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git runs");
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
    config_home: PathBuf,
    state_home: PathBuf,
    repo: PathBuf,
    remote: PathBuf,
}

impl Fixture {
    fn new(dirty: bool) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let config_home = base.join("config");
        let state_home = base.join("state");
        let repos = base.join("repos");
        let repo = repos.join("repo");
        let remote = base.join("remote.git");
        fs::create_dir_all(&config_home).unwrap();
        fs::create_dir_all(&state_home).unwrap();
        fs::create_dir_all(&repos).unwrap();

        git(base, &["init", "-q", "--bare", "remote.git"]);
        git(&remote, &["symbolic-ref", "HEAD", "refs/heads/topic"]);
        git(base, &["init", "-q", "-b", "topic", "repos/repo"]);
        git(&repo, &["config", "user.name", "doki-test"]);
        git(&repo, &["config", "user.email", "doki@example.com"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        let hooks = repo.join(".git/hooks");
        git(
            &repo,
            &["config", "core.hooksPath", hooks.to_str().unwrap()],
        );
        fs::write(repo.join("a.txt"), "one\n").unwrap();
        git(&repo, &["add", "a.txt"]);
        git(&repo, &["commit", "-qm", "initial"]);
        let url = format!("file://{}", remote.display());
        git(&repo, &["remote", "add", "origin", &url]);
        git(&repo, &["push", "-q", "-u", "origin", "topic"]);
        if dirty {
            fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
        }

        let shim = base.join("omp");
        fs::write(
            &shim,
            "#!/bin/sh\ncat >/dev/null 2>&1 || true\nprintf 'add widget\\n'\n",
        )
        .unwrap();
        chmod(&shim);

        fs::create_dir_all(config_home.join("doki")).unwrap();
        fs::write(
            config_home.join("doki/config.toml"),
            format!(
                "[[roots]]\nname = \"dev\"\npath = \"{}\"\nmax_depth = 2\nschedule = \"*/20 * * * *\"\n\n\
                 [sync]\nprotected = []\n\n\
                 [notify]\ndesktop = false\n\n\
                 [agent]\norder = [\"omp\"]\nfallback_on_error = false\nmax_attempts = 1\n\n\
                 [agents.omp]\ncli = \"{}\"\nmodel = \"test/model\"\n",
                repos.display(),
                shim.display()
            ),
        )
        .unwrap();

        Self {
            _tmp: tmp,
            config_home,
            state_home,
            repo,
            remote,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_doki"))
            .args(args)
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env("XDG_STATE_HOME", &self.state_home)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("doki runs")
    }

    fn store(&self) -> Store {
        Store::open(&self.state_home.join("doki/doki.db")).unwrap()
    }

    fn enable(&self) {
        let out = self.run(&["status"]);
        assert!(out.status.success(), "{}", stderr(&out));
        let store = self.store();
        let row = store.repo_by_path(&self.repo).unwrap().expect("discovered");
        store.set_enabled(row.id, true).unwrap();
    }

    fn head(&self) -> String {
        git(&self.repo, &["rev-parse", "HEAD"])
    }

    fn remote_head(&self) -> String {
        git(&self.remote, &["rev-parse", "topic"])
    }

    fn repo_row(&self) -> doki_core::store::RepoRow {
        self.store()
            .repo_by_path(&self.repo)
            .unwrap()
            .expect("a repo row")
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout)
}

#[test]
fn status_lists_the_persisted_state_and_the_next_run() {
    let f = Fixture::new(false);
    let first = f.run(&["status"]);
    assert!(first.status.success(), "{}", stderr(&first));
    let text = String::from_utf8_lossy(&first.stdout).into_owned();
    assert!(text.contains("disabled"), "{text}");

    f.enable();
    let second = f.run(&["status"]);
    assert!(second.status.success(), "{}", stderr(&second));
    let text = String::from_utf8_lossy(&second.stdout).into_owned();
    assert!(text.contains("enabled"), "{text}");
    assert!(text.contains(&f.repo.display().to_string()), "{text}");
    assert!(
        text.contains("next "),
        "status must show the next run: {text}"
    );
    assert!(f.repo_row().next_run_at.is_some());
}

#[test]
fn sync_all_commits_pushes_and_settles_the_repo_state() {
    let f = Fixture::new(true);
    f.enable();
    let before = f.remote_head();

    let out = f.run(&["sync", "--all"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("state:  committed"), "{text}");
    assert!(text.contains("push:   pushed"), "{text}");
    assert_eq!(f.head(), f.remote_head());
    assert_ne!(f.remote_head(), before);

    let row = f.repo_row();
    assert_eq!(row.state, Some(RepoState::Clean));
    assert_eq!(row.fail_count, 0);
    assert!(row.last_sync_at.is_some());
    assert!(row.next_run_at.unwrap() > row.last_sync_at.unwrap());
    assert!(!row.allow_protected_push);

    let store = f.store();
    let run = store.last_run(row.id).unwrap().unwrap();
    assert!(run.pushed);
    assert_eq!(run.state, "committed");
}

#[test]
fn sync_all_dry_run_leaves_remote_head_and_refs_alone() {
    let f = Fixture::new(true);
    f.enable();
    let before = f.head();
    let remote_before = f.remote_head();

    let out = f.run(&["sync", "--all", "--dry-run"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("state:  dry-run"), "{text}");
    assert!(text.contains("argv:   git fetch --prune origin"), "{text}");
    assert!(text.contains("argv:   git push origin topic"), "{text}");
    assert_eq!(f.head(), before);
    assert_eq!(f.remote_head(), remote_before);
    assert_eq!(git(&f.repo, &["for-each-ref", "refs/doki"]), "");
    assert_eq!(git(&f.repo, &["diff", "--cached", "--name-only"]), "");
}

#[test]
fn sync_due_exits_busy_when_the_scheduler_lock_is_held() {
    let f = Fixture::new(true);
    f.enable();
    let state = f.state_home.join("doki");
    let held = scheduler::try_lock(&state).unwrap();
    assert!(matches!(held, LockAttempt::Acquired(_)));

    let out = f.run(&["sync", "--due"]);
    assert!(!out.status.success(), "a second --due must not run");
    assert!(
        stderr(&out).contains("busy"),
        "expected a busy message: {}",
        stderr(&out)
    );
    drop(held);

    let again = f.run(&["sync", "--due"]);
    assert!(
        again.status.success() || !stderr(&again).contains("busy"),
        "{}",
        stderr(&again)
    );
}

#[test]
fn reset_quarantine_clears_the_repo_and_plans_the_next_run() {
    let f = Fixture::new(false);
    let discovered = f.run(&["status"]);
    assert!(discovered.status.success(), "{}", stderr(&discovered));
    f.enable();

    let store = f.store();
    let row = store.repo_by_path(&f.repo).unwrap().unwrap();
    store
        .record_failure(row.id, 100, RepoState::Blocked, "boom", None, 1)
        .unwrap();
    assert_eq!(f.repo_row().state, Some(RepoState::Quarantined));

    let out = f.run(&["reset-quarantine", f.repo.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("quarantine cleared"), "{text}");

    let row = f.repo_row();
    assert_eq!(row.state, Some(RepoState::Clean));
    assert_eq!(row.fail_count, 0);
    assert_eq!(row.quarantine_until, None);
    assert!(row.next_run_at.is_some());

    let missing = f.run(&["reset-quarantine", "/tmp/doki-not-a-repo"]);
    assert!(!missing.status.success());
}

#[test]
fn sync_requires_exactly_one_selection() {
    let f = Fixture::new(false);
    for args in [
        vec!["sync"],
        vec!["sync", "--all", "--due"],
        vec!["sync", "--repo", "/tmp/x", "--all"],
    ] {
        let out = f.run(&args);
        assert!(!out.status.success(), "{args:?} should be rejected");
    }
}

#[test]
fn sync_due_commits_and_pushes_once_then_waits_for_the_next_slot() {
    let f = Fixture::new(true);
    f.enable();
    let row = f.repo_row();
    f.store().set_next_run(row.id, Some(0)).unwrap();
    let before = f.remote_head();

    let out = f.run(&["sync", "--due"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("state:  committed"), "{text}");
    assert!(text.contains("push:   pushed"), "{text}");
    assert_eq!(f.head(), f.remote_head());
    assert_ne!(f.remote_head(), before);

    let head = f.head();
    let again = f.run(&["sync", "--due"]);
    assert!(again.status.success(), "{}", stderr(&again));
    let text = String::from_utf8_lossy(&again.stdout).into_owned();
    assert!(text.contains("no repos are due"), "{text}");
    assert_eq!(f.head(), head);
    assert_eq!(f.remote_head(), head);
}

#[test]
fn install_timer_refuses_foreign_units_then_backs_up_with_force_write() {
    let f = Fixture::new(false);
    let unit_dir = f.config_home.join("systemd/user");
    fs::create_dir_all(&unit_dir).unwrap();
    let timer = unit_dir.join("doki.timer");
    fs::write(&timer, "hand written by the user\n").unwrap();

    let refused = f.run(&["install-timer"]);
    assert!(
        !refused.status.success(),
        "a differing unit must be refused"
    );
    assert!(
        stderr(&refused).contains("--force-write"),
        "{}",
        stderr(&refused)
    );
    assert_eq!(
        fs::read_to_string(&timer).unwrap(),
        "hand written by the user\n"
    );
    assert!(!unit_dir.join("doki.service").exists());

    let forced = f.run(&["install-timer", "--force-write"]);
    assert!(forced.status.success(), "{}", stderr(&forced));
    let text = String::from_utf8_lossy(&forced.stdout).into_owned();
    assert!(
        text.contains("systemctl --user daemon-reload && systemctl --user enable --now doki.timer"),
        "{text}"
    );
    assert!(
        text.contains("OnCalendar") || text.contains("calendar: *:0/20"),
        "{text}"
    );
    let units = fs::read_to_string(&timer).unwrap();
    assert!(units.contains("# doki-managed"), "{units}");
    assert!(units.contains("OnCalendar=*:0/20"), "{units}");
    let service = fs::read_to_string(unit_dir.join("doki.service")).unwrap();
    assert!(service.contains("# doki-managed"), "{service}");
    assert!(
        service.contains(&format!(
            "ExecStart={} sync --due",
            fs::canonicalize(env!("CARGO_BIN_EXE_doki"))
                .unwrap()
                .display()
        )),
        "{service}"
    );
    assert_eq!(
        fs::read_to_string(unit_dir.join("doki.timer.bak")).unwrap(),
        "hand written by the user\n"
    );

    let removed = f.run(&["uninstall-timer"]);
    assert!(removed.status.success(), "{}", stderr(&removed));
    let text = String::from_utf8_lossy(&removed.stdout).into_owned();
    assert!(
        text.contains("systemctl --user disable --now doki.timer"),
        "{text}"
    );
    assert!(!timer.exists());
    assert!(!unit_dir.join("doki.service").exists());
    assert_eq!(
        fs::read_to_string(unit_dir.join("doki.timer.bak")).unwrap(),
        "hand written by the user\n"
    );
}

#[test]
fn uninstall_timer_keeps_units_it_did_not_write() {
    let f = Fixture::new(false);
    let unit_dir = f.config_home.join("systemd/user");
    fs::create_dir_all(&unit_dir).unwrap();
    let service = unit_dir.join("doki.service");
    fs::write(&service, "[Service]\nExecStart=/usr/bin/false\n").unwrap();

    let out = f.run(&["uninstall-timer"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("not written by doki"), "{text}");
    assert!(service.exists());
}

#[test]
fn doctor_prints_every_probe_and_names_the_timer_state() {
    let f = Fixture::new(false);
    let out = f.run(&["doctor"]);
    let text = stderr(&out);
    for row in [
        "STATUS",
        "config",
        "git",
        "gitleaks",
        "agent omp",
        "state dir",
        "inotify",
        "notify",
        "systemd timer",
    ] {
        assert!(text.contains(row), "missing {row}: {text}");
    }
    assert!(
        text.contains("OK") && (text.contains("WARN") || text.contains("FAIL")),
        "{text}"
    );
}

#[test]
fn background_runs_a_due_repo_and_stops_on_sigterm() {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let f = Fixture::new(true);
    f.enable();
    let row = f.repo_row();
    f.store().set_next_run(row.id, Some(0)).unwrap();
    let before = f.remote_head();

    let mut child = Command::new(env!("CARGO_BIN_EXE_doki"))
        .args(["run", "--background", "--no-tray"])
        .env("XDG_CONFIG_HOME", &f.config_home)
        .env("XDG_STATE_HOME", &f.state_home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("background doki starts");

    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if f.remote_head() != before {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "background sync did not push in time"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(f.head(), f.remote_head());

    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "background did not stop on SIGTERM"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "background exit status {status:?}");
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("background stopped"), "{text}");
}

#[test]
fn doctor_exits_non_zero_when_a_probe_fails() {
    let f = Fixture::new(false);
    fs::write(f.config_home.join("doki/config.toml"), "bogus = true\n").unwrap();
    let out = f.run(&["doctor"]);
    assert!(!out.status.success(), "invalid config must fail doctor");
    let text = stderr(&out);
    assert!(text.contains("FAIL"), "{text}");
    assert!(text.contains("config"), "{text}");
}
