use doki_core::config::Config;
use doki_core::discovery::DetectedRepo;
use doki_core::lock::LockAttempt;
use doki_core::scheduler;
use doki_core::store::{RepoState, Store};

fn config(schedule: &str) -> Config {
    Config::parse(&format!(
        "[[roots]]\nname = \"dev\"\npath = \"/tmp/dev\"\nschedule = \"{schedule}\"\n"
    ))
    .unwrap()
}

fn store_with_repo(name: &str) -> (Store, i64) {
    let mut store = Store::open_in_memory().unwrap();
    store
        .upsert_detected(&[DetectedRepo {
            root: "dev".into(),
            path: format!("/tmp/dev/{name}").into(),
            git_common_dir: None,
        }])
        .unwrap();
    let id = store.repos().unwrap()[0].id;
    store.set_enabled(id, true).unwrap();
    (store, id)
}

#[test]
fn due_plans_the_root_schedule_and_catches_up_exactly_once() {
    let cfg = config("*/20 * * * *");
    let (store, id) = store_with_repo("repo");
    let now = 1_700_000_000;

    assert!(scheduler::due(&cfg, &store, now).unwrap().is_empty());
    let row = store.repo(id).unwrap().unwrap();
    let first = row.next_run_at.expect("a planned first run");
    assert!(first > now);
    assert!(first <= now + 20 * 60 + 61, "{first} vs {now}");

    let due = scheduler::due(&cfg, &store, first).unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].id, id);

    let gap = now + 3 * 3600;
    let catch_up = scheduler::due(&cfg, &store, gap).unwrap();
    assert_eq!(catch_up.len(), 1, "a long gap is one catch-up run");
    let next = scheduler::next_for_repo(&cfg, &row, gap).unwrap();
    store
        .record_success(id, gap, RepoState::Clean, Some(next))
        .unwrap();
    assert!(next > gap && next <= gap + 20 * 60 + 61);
    assert!(scheduler::due(&cfg, &store, gap).unwrap().is_empty());
    assert!(
        scheduler::due(&cfg, &store, next - 1).unwrap().is_empty(),
        "a settled repo waits for its jittered slot"
    );
    assert_eq!(scheduler::due(&cfg, &store, next).unwrap().len(), 1);
}

#[test]
fn plan_missing_skips_disabled_and_quarantined_repos() {
    let cfg = config("0 */2 * * *");
    let mut store = Store::open_in_memory().unwrap();
    store
        .upsert_detected(&[
            DetectedRepo {
                root: "dev".into(),
                path: "/tmp/dev/on".into(),
                git_common_dir: None,
            },
            DetectedRepo {
                root: "dev".into(),
                path: "/tmp/dev/off".into(),
                git_common_dir: None,
            },
            DetectedRepo {
                root: "dev".into(),
                path: "/tmp/dev/held".into(),
                git_common_dir: None,
            },
        ])
        .unwrap();
    let ids: Vec<i64> = store.repos().unwrap().iter().map(|row| row.id).collect();
    store.set_enabled(ids[0], true).unwrap();
    store.set_enabled(ids[2], true).unwrap();

    let now = 1_700_000_000;
    let (_, quarantined) = store
        .record_failure(ids[2], now, RepoState::Blocked, "boom", None, 1)
        .unwrap();
    assert!(quarantined);

    assert_eq!(scheduler::plan_missing(&cfg, &store, now).unwrap(), 1);
    let rows = store.repos().unwrap();
    let on = rows.iter().find(|row| row.id == ids[0]).unwrap();
    let off = rows.iter().find(|row| row.id == ids[1]).unwrap();
    let held = rows.iter().find(|row| row.id == ids[2]).unwrap();
    assert!(on.next_run_at.is_some());
    assert_eq!(off.next_run_at, None, "a disabled repo is never scheduled");
    assert_eq!(held.next_run_at, None, "a quarantined repo never retries");
    assert_eq!(held.state, Some(RepoState::Quarantined));
}

#[test]
fn failures_back_off_then_quarantine_and_reset_clears_everything() {
    let (store, id) = store_with_repo("repo");

    let (count, quarantined) = store
        .record_failure(id, 10, RepoState::Blocked, "boom", Some(130), 2)
        .unwrap();
    assert_eq!(count, 1);
    assert!(!quarantined);
    let row = store.repo(id).unwrap().unwrap();
    assert_eq!(row.fail_count, 1);
    assert_eq!(row.next_run_at, Some(130));
    assert_eq!(row.last_sync_at, Some(10));

    let (count, quarantined) = store
        .record_failure(id, 20, RepoState::Blocked, "boom again", Some(140), 2)
        .unwrap();
    assert_eq!(count, 2);
    assert!(quarantined);
    let row = store.repo(id).unwrap().unwrap();
    assert_eq!(row.state, Some(RepoState::Quarantined));
    assert_eq!(row.fail_count, 2);
    assert_eq!(row.quarantine_until, Some(20));
    assert_eq!(row.next_run_at, None);
    assert_eq!(row.last_error.as_deref(), Some("boom again"));

    store.reset_quarantine(id).unwrap();
    let row = store.repo(id).unwrap().unwrap();
    assert_eq!(row.fail_count, 0);
    assert_eq!(row.quarantine_until, None);
    assert_eq!(row.next_run_at, None);
    assert_eq!(row.last_error, None);
    assert_eq!(row.state, Some(RepoState::Clean));
}

#[test]
fn the_scheduler_lock_is_exclusive_across_handles() {
    let dir = tempfile::tempdir().unwrap();
    let first = scheduler::try_lock(dir.path()).unwrap();
    assert!(matches!(first, LockAttempt::Acquired(_)));
    assert!(matches!(
        scheduler::try_lock(dir.path()).unwrap(),
        LockAttempt::Busy
    ));
    assert!(dir.path().join("scheduler.lock").exists());
    drop(first);
    assert!(matches!(
        scheduler::try_lock(dir.path()).unwrap(),
        LockAttempt::Acquired(_)
    ));
}

#[test]
fn a_bad_schedule_is_an_error_not_a_silent_default() {
    let cfg = config("61 * * * *");
    let (store, _) = store_with_repo("repo");
    assert!(scheduler::plan_missing(&cfg, &store, 1_700_000_000).is_err());
}
