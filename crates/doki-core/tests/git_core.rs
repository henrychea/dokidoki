use doki_core::config::Guards;
use doki_core::git::Git;
use doki_core::guards::{self, Refusal};
use doki_core::lock::{self, LockAttempt};
use doki_core::status::{self, RepoState};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn sh(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn repo() -> (TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    sh(tmp.path(), &["init", "-q", "-b", "main", "repo"]);
    let root = tmp.path().join("repo");
    (tmp, root)
}

fn commit(root: &Path, files: &[(&str, &str)], message: &str) {
    for (name, body) in files {
        fs::write(root.join(name), body).unwrap();
    }
    sh(root, &["add", "-A"]);
    sh(root, &["commit", "-qm", message]);
}

fn read_status(root: &Path) -> status::RepoStatus {
    status::read(&Git::new(root)).unwrap()
}

#[test]
fn tracked_dirty() {
    let (_tmp, root) = repo();
    commit(&root, &[("a.txt", "one\n")], "one");
    fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();

    let st = read_status(&root);
    assert_eq!(st.branch.as_deref(), Some("main"));
    assert_eq!(st.upstream, None);
    assert_eq!((st.ahead, st.behind), (0, 0));
    assert_eq!(st.dirty, 1);
    assert_eq!(st.state, RepoState::Dirty);

    let parsed = status::parse(
        &Git::new(&root)
            .run(&["status", "--porcelain=v2", "-z"])
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(parsed.unstaged, [PathBuf::from("a.txt")]);
    assert!(parsed.staged.is_empty());
}

#[test]
fn untracked_only() {
    let (_tmp, root) = repo();
    commit(&root, &[("a.txt", "one\n")], "one");
    fs::write(root.join("new.txt"), "new\n").unwrap();

    let st = read_status(&root);
    assert_eq!(st.dirty, 1);
    assert_eq!(st.state, RepoState::Dirty);
}

#[test]
fn staged_only() {
    let (_tmp, root) = repo();
    commit(&root, &[("a.txt", "one\n")], "one");
    fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
    sh(&root, &["add", "a.txt"]);

    let st = read_status(&root);
    assert_eq!(st.dirty, 1);
    assert_eq!(st.state, RepoState::Dirty);
    let parsed = status::parse(
        &Git::new(&root)
            .run(&["status", "--porcelain=v2", "-z"])
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(parsed.staged, [PathBuf::from("a.txt")]);
    assert!(parsed.unstaged.is_empty());
}

#[test]
fn clean_repo_is_clean() {
    let (_tmp, root) = repo();
    commit(&root, &[("a.txt", "one\n")], "one");
    let st = read_status(&root);
    assert_eq!(st.dirty, 0);
    assert_eq!(st.state, RepoState::Clean);
}

#[test]
fn detached_head() {
    let (_tmp, root) = repo();
    commit(&root, &[("a.txt", "one\n")], "one");
    sh(&root, &["checkout", "-q", "--detach", "HEAD"]);

    let st = read_status(&root);
    assert_eq!(st.branch, None);
    assert_eq!(st.dirty, 0);
    assert_eq!(st.state, RepoState::Detached);
}

#[test]
fn unborn_branch() {
    let (_tmp, root) = repo();
    let st = read_status(&root);
    assert_eq!(st.branch.as_deref(), Some("main"));
    assert_eq!(st.dirty, 0);
    assert_eq!(st.state, RepoState::Unborn);
}

#[test]
fn refuses_every_in_progress_state() {
    let cases: &[(&str, bool, Refusal)] = &[
        ("MERGE_HEAD", false, Refusal::Merge),
        ("rebase-merge", true, Refusal::RebaseMerge),
        ("rebase-apply", true, Refusal::RebaseApply),
        ("CHERRY_PICK_HEAD", false, Refusal::CherryPick),
        ("index.lock", false, Refusal::IndexLock),
    ];
    for (name, is_dir, want) in cases {
        let (_tmp, root) = repo();
        commit(&root, &[("a.txt", "one\n")], "one");
        let git = Git::new(&root);
        let dir = git.git_dir().unwrap();
        let marker = dir.join(name);
        if *is_dir {
            fs::create_dir(&marker).unwrap();
        } else {
            fs::write(&marker, "x").unwrap();
        }
        assert_eq!(guards::refusal(&dir), Some(*want));
        let st = status::read(&git).unwrap();
        assert_eq!(st.state, RepoState::Refused(want.reason().into()));
    }
}

#[test]
fn lock_is_exclusive_and_released_on_drop() {
    let (_tmp, root) = repo();
    let dir = Git::new(&root).common_dir().unwrap();
    assert!(!dir.join("doki.lock").exists());

    let LockAttempt::Acquired(first) = lock::try_acquire(&dir).unwrap() else {
        panic!("first lock should be acquired");
    };
    assert!(matches!(
        lock::try_acquire(&dir).unwrap(),
        LockAttempt::Busy
    ));
    drop(first);
    // A git child forked by a parallel test holds the inherited lock fd until exec,
    // so a released flock can still read busy for a moment.
    let mut released = false;
    for _ in 0..200 {
        if matches!(lock::try_acquire(&dir).unwrap(), LockAttempt::Acquired(_)) {
            released = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(released, "lock was not released after drop");
}

#[test]
fn mass_change_guard_trips_and_passes() {
    let (_tmp, root) = repo();
    let lines: String = (1..=10).map(|n| format!("line {n}\n")).collect();
    commit(&root, &[("a.txt", &lines)], "one");
    fs::write(
        root.join("a.txt"),
        "line 1\nline 2\nline 3\nline 4\nline 5\n",
    )
    .unwrap();

    let change = guards::measure(&Git::new(&root)).unwrap();
    assert_eq!(change.changed_files, 1);
    assert_eq!(change.deleted_lines, 5);

    let tight = Guards {
        max_deleted_lines: 4,
        ..Guards::default()
    };
    assert!(change.check(&tight).is_err());

    let fits = Guards {
        max_deleted_lines: 5,
        ..Guards::default()
    };
    assert!(change.check(&fits).is_ok());

    let files = Guards {
        max_changed_files: 0,
        ..Guards::default()
    };
    assert!(change.check(&files).is_err());
}

#[test]
fn mass_change_counts_binary_file_without_line_counts() {
    let (_tmp, root) = repo();
    fs::write(root.join("bin.dat"), b"\x00\x01\x02").unwrap();
    commit(&root, &[], "binary");
    fs::write(root.join("bin.dat"), b"\x00\x01\x02\x03").unwrap();

    let change = guards::measure(&Git::new(&root)).unwrap();
    assert_eq!(change.changed_files, 1);
    assert_eq!(change.deleted_lines, 0);
}

#[test]
fn dry_run_records_argv_and_leaves_repo_unchanged() {
    let (_tmp, root) = repo();
    commit(&root, &[("a.txt", "one\n")], "one");
    fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
    let head = sh(&root, &["rev-parse", "HEAD"]);

    let git = Git::dry_run(&root);
    for args in [
        &["add", "-A"][..],
        &["commit", "-m", "x"],
        &["push"],
        &["update-ref", "refs/doki/pre-sync/1", "HEAD"],
        &["reset", "--hard"],
        &["rebase", "origin/main"],
        &["merge", "--ff-only", "@{u}"],
        &["checkout", "main"],
        &["switch", "main"],
        &["tag", "v1"],
        &["stash"],
        &["rm", "a.txt"],
    ] {
        let out = git.run(args).unwrap();
        assert!(!out.executed, "dry run executed {args:?}");
        assert!(out.stdout.is_empty());
    }

    let read = git.run(&["rev-parse", "HEAD"]).unwrap();
    assert!(read.executed);
    assert_eq!(read.stdout_lossy().trim(), head.trim());
    assert_eq!(git.recorded().len(), 13);
    assert_eq!(git.recorded()[0], ["add", "-A"]);
    assert_eq!(git.recorded()[1], ["commit", "-m", "x"]);

    assert_eq!(sh(&root, &["rev-list", "--count", "HEAD"]).trim(), "1");
    assert_eq!(sh(&root, &["diff", "--cached", "--name-only"]).trim(), "");
    let st = read_status(&root);
    assert_eq!(st.dirty, 1);
    assert_eq!(st.state, RepoState::Dirty);

    let real = Git::new(&root);
    assert!(real.run(&["add", "-A"]).unwrap().executed);
    assert_eq!(
        sh(&root, &["diff", "--cached", "--name-only"]).trim(),
        "a.txt"
    );
}

#[test]
fn dry_run_allowlist_skips_hidden_and_network_commands() {
    let (_tmp, root) = repo();
    commit(&root, &[("a.txt", "one\n")], "one");
    fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
    let head = sh(&root, &["rev-parse", "HEAD"]);
    let git = Git::dry_run(&root);

    for args in [
        &["-c", "x=y", "commit", "-m", "x"][..],
        &["-C", root.to_str().unwrap(), "commit", "-m", "x"],
        &["fetch"],
        &["fetch", "--prune", "origin"],
        &["config", "user.name", "x"],
        &["gc", "--aggressive"],
        &["submodule", "update"],
    ] {
        let out = git.run(args).unwrap();
        assert!(!out.executed, "dry run executed {args:?}");
    }

    for args in [
        &["status", "--porcelain=v2", "-z", "--branch"][..],
        &["diff", "--numstat"],
        &["rev-parse", "HEAD"],
        &["log", "-5", "--format=%s"],
        &["config", "--get", "user.name"],
        &["-c", "x=y", "status"],
        &["-C", root.to_str().unwrap(), "status"],
    ] {
        let out = git.run_unchecked(args).unwrap();
        assert!(out.executed, "dry run skipped {args:?}");
    }

    assert_eq!(git.recorded().len(), 14);
    assert_eq!(sh(&root, &["rev-parse", "HEAD"]), head);
    assert_eq!(sh(&root, &["rev-list", "--count", "HEAD"]).trim(), "1");
    assert_eq!(read_status(&root).dirty, 1);
}

#[test]
fn worktrees_of_one_repo_share_a_single_common_dir_lock() {
    let (tmp, root) = repo();
    commit(&root, &[("a.txt", "one\n")], "one");
    let wt = tmp.path().join("wt");
    sh(
        &root,
        &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "wtb"],
    );

    let main_common = Git::new(&root).common_dir().unwrap();
    let worktree_common = Git::new(&wt).common_dir().unwrap();
    assert_eq!(main_common, worktree_common);

    let LockAttempt::Acquired(first) = lock::try_acquire(&main_common).unwrap() else {
        panic!("first worktree should acquire the lock");
    };
    assert!(matches!(
        lock::try_acquire(&worktree_common).unwrap(),
        LockAttempt::Busy
    ));
    assert!(main_common.join("doki.lock").is_file());
    drop(first);

    let mut released = false;
    for _ in 0..200 {
        if matches!(
            lock::try_acquire(&worktree_common).unwrap(),
            LockAttempt::Acquired(_)
        ) {
            released = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(released, "the shared lock was not released");
}

#[test]
fn dry_run_skips_symbolic_ref_writes_but_runs_queries() {
    let (_tmp, root) = repo();
    commit(&root, &[("a.txt", "one\n")], "one");
    let git = Git::dry_run(&root);

    let write = git
        .run(&["symbolic-ref", "HEAD", "refs/heads/elsewhere"])
        .unwrap();
    assert!(!write.executed, "dry run executed symbolic-ref write");
    let delete = git.run(&["symbolic-ref", "-d", "HEAD"]).unwrap();
    assert!(!delete.executed, "dry run executed symbolic-ref -d");

    let read = git.run(&["symbolic-ref", "HEAD"]).unwrap();
    assert!(read.executed);
    assert_eq!(read.stdout_lossy().trim(), "refs/heads/main");
    assert_eq!(
        sh(&root, &["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/main"
    );
}

#[test]
fn worktree_git_dir_is_resolved_from_git_file() {
    let (tmp, root) = repo();
    commit(&root, &[("a.txt", "one\n")], "one");
    let wt = tmp.path().join("wt");
    sh(
        &root,
        &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "wtb"],
    );
    assert!(wt.join(".git").is_file());

    let git = Git::new(&wt);
    let dir = git.git_dir().unwrap();
    assert!(dir.is_absolute());
    assert!(dir.ends_with(Path::new(".git/worktrees/wt")));
    assert!(dir.is_dir());

    fs::write(dir.join("MERGE_HEAD"), "x").unwrap();
    assert_eq!(
        read_status(&wt).state,
        RepoState::Refused("MERGE_HEAD present".into())
    );
}
