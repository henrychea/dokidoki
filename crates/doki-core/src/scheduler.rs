use crate::config::Config;
use crate::lock::{self, LockAttempt};
use crate::store::{RepoRow, Store};
use anyhow::{Context, Result};
use chrono::{TimeZone, Utc};
use cron::Schedule;
use std::path::Path;
use std::str::FromStr;

pub const DEFAULT_SCHEDULE: &str = "*/20 * * * *";
pub const MAX_JITTER_SECONDS: i64 = 60;

pub fn parse(expression: &str) -> Result<Schedule> {
    // The cron crate starts at seconds; a crontab expression starts at minutes.
    let normalized = if expression.split_whitespace().count() == 5 {
        format!("0 {}", expression.trim())
    } else {
        expression.trim().to_string()
    };
    Schedule::from_str(&normalized).with_context(|| format!("invalid cron `{expression}`"))
}

/// Wall-clock seconds, so a suspended machine catches up instead of stalling.
pub fn next_occurrence(expression: &str, after: i64) -> Result<i64> {
    let schedule = parse(expression)?;
    let from = Utc
        .timestamp_opt(after, 0)
        .single()
        .with_context(|| format!("invalid timestamp {after}"))?;
    schedule
        .after(&from)
        .next()
        .map(|at| at.timestamp())
        .with_context(|| format!("cron `{expression}` has no next run"))
}

/// Stable per repo and scheduled minute, so tests see the same offset.
pub fn jitter(repo_id: i64, tick: i64) -> i64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in repo_id
        .to_le_bytes()
        .iter()
        .chain(tick.to_le_bytes().iter())
    {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    (hash % (MAX_JITTER_SECONDS as u64 + 1)) as i64
}

pub fn backoff_seconds(fail_count: u32, max_minutes: u64) -> i64 {
    let minutes = 1u64.checked_shl(fail_count).unwrap_or(u64::MAX);
    let capped = minutes.min(max_minutes.max(1));
    capped.saturating_mul(60).min(i64::MAX as u64) as i64
}

pub fn schedule_for<'a>(cfg: &'a Config, root: &str) -> &'a str {
    cfg.roots
        .iter()
        .find(|r| r.name == root)
        .map_or(DEFAULT_SCHEDULE, |r| r.schedule.as_str())
}

pub fn next_for_repo(cfg: &Config, repo: &RepoRow, after: i64) -> Result<i64> {
    let occurrence = next_occurrence(schedule_for(cfg, &repo.root), after)?;
    Ok(occurrence + jitter(repo.id, occurrence / 60))
}

/// Repos that never ran get a plan instead of an immediate sync.
pub fn plan_missing(cfg: &Config, store: &Store, now: i64) -> Result<usize> {
    let mut planned = 0;
    for repo in store.repos()? {
        if !repo.enabled || repo.quarantined() || repo.next_run_at.is_some() {
            continue;
        }
        store.set_next_run(repo.id, Some(next_for_repo(cfg, &repo, now)?))?;
        planned += 1;
    }
    Ok(planned)
}

pub fn due(cfg: &Config, store: &Store, now: i64) -> Result<Vec<RepoRow>> {
    plan_missing(cfg, store, now)?;
    Ok(store
        .repos()?
        .into_iter()
        .filter(|repo| {
            repo.enabled && !repo.quarantined() && repo.next_run_at.is_some_and(|at| at <= now)
        })
        .collect())
}

pub fn try_lock(state_dir: &Path) -> Result<LockAttempt> {
    lock::try_scheduler(state_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cron_accepts_crontab_and_seconds_forms() {
        let five = next_occurrence("*/20 * * * *", 0).unwrap();
        assert_eq!(five, 20 * 60);
        assert_eq!(next_occurrence("30 */20 * * * *", 0).unwrap(), 30);
        assert_eq!(
            next_occurrence("30 */20 * * * *", 100).unwrap(),
            20 * 60 + 30
        );
        assert!(parse("*/20 * * * *").is_ok());
        assert!(parse("not a cron").is_err());
        assert!(next_occurrence("0 0 0 1 1 * 2000", 1_600_000_000).is_err());
    }

    #[test]
    fn jitter_is_stable_bounded_and_repo_scoped() {
        for repo in 1..8 {
            let j = jitter(repo, 1234);
            assert_eq!(j, jitter(repo, 1234));
            assert!((0..=MAX_JITTER_SECONDS).contains(&j));
        }
        let values: Vec<i64> = (1..8).map(|repo| jitter(repo, 99)).collect();
        assert!(values.iter().any(|v| *v != values[0]), "{values:?}");
        let ticks: Vec<i64> = (100..108).map(|tick| jitter(1, tick)).collect();
        assert!(ticks.iter().any(|v| *v != ticks[0]), "{ticks:?}");
    }

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(backoff_seconds(1, 360), 120);
        assert_eq!(backoff_seconds(2, 360), 240);
        assert_eq!(backoff_seconds(3, 360), 480);
        assert_eq!(backoff_seconds(4, 360), 960);
        assert_eq!(backoff_seconds(9, 360), 360 * 60);
        assert_eq!(backoff_seconds(9, 0), 60);
        assert_eq!(backoff_seconds(u32::MAX, 10), 600);
    }

    #[test]
    fn missing_schedule_falls_back_to_the_default() {
        let cfg = Config::default();
        assert_eq!(schedule_for(&cfg, "gone"), DEFAULT_SCHEDULE);
    }
}
