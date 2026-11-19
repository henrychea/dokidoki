use crate::git::Git;
use crate::lock::{self, LockAttempt};
use crate::store::{RunFinish, Store};
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndoState {
    Undone,
    Refused,
}

impl UndoState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Undone => "undone",
            Self::Refused => "refused",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoReport {
    pub repo: PathBuf,
    pub state: UndoState,
    pub run_id: Option<i64>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub detail: String,
}

impl UndoReport {
    fn new(repo: impl Into<PathBuf>, detail: impl Into<String>) -> Self {
        Self {
            repo: repo.into(),
            state: UndoState::Refused,
            run_id: None,
            from: None,
            to: None,
            detail: detail.into(),
        }
    }
}

/// `reset --soft` so undo moves the branch and leaves every change staged.
pub fn undo(store: &Store, repo_path: &Path) -> Result<UndoReport> {
    let repo = repo_path
        .canonicalize()
        .unwrap_or_else(|_| repo_path.to_path_buf());
    let Some(row) = store.repo_by_path(&repo)? else {
        return Ok(UndoReport::new(
            repo.clone(),
            format!("{} is not a known repo", repo.display()),
        ));
    };
    let git = Git::new(&repo);
    let common = git.common_dir()?;
    let _lock = match lock::try_acquire(&common)? {
        LockAttempt::Acquired(lock) => lock,
        LockAttempt::Busy => {
            return Ok(UndoReport::new(
                repo.clone(),
                "another doki run holds the repo lock",
            ));
        }
    };

    let Some(run) = store.last_run(row.id)? else {
        return Ok(UndoReport::new(repo.clone(), "this repo has no doki runs"));
    };
    let mut report = UndoReport {
        run_id: Some(run.id),
        from: run.head_after.clone(),
        ..UndoReport::new(repo.clone(), "")
    };
    if run.pushed {
        report.detail = "the last run pushed, so undo would rewrite published history".into();
        return Ok(report);
    }
    if run.state != "committed" {
        report.detail = format!("the last run did not commit (state = {})", run.state);
        return Ok(report);
    }
    let Some(snapshot) = store.last_snapshot(run.id)? else {
        return Ok(refused(report, "the last run has no pre-sync snapshot"));
    };
    let Some(head_after) = run.head_after.clone() else {
        return Ok(refused(report, "the last run recorded no head_after"));
    };
    let head = rev_parse(&git, "HEAD")?;
    if head != head_after {
        return Ok(refused(
            report,
            format!(
                "HEAD moved since the run: HEAD is {}, the run left {}",
                short(&head),
                short(&head_after)
            ),
        ));
    }
    let target = format!("{}^{{commit}}", snapshot.sha);
    if !git.run_unchecked(&["cat-file", "-e", &target])?.success() {
        return Ok(refused(
            report,
            format!("the pre-sync snapshot {} is missing", short(&snapshot.sha)),
        ));
    }
    let reset = git.run_unchecked(&["reset", "--soft", &snapshot.sha])?;
    if !reset.success() {
        bail!(
            "git reset --soft {} failed: {}",
            short(&snapshot.sha),
            reset.stderr_lossy().trim()
        );
    }
    let now = now_unix();
    let journal = store.begin_run(row.id, now, Some(&head_after))?;
    store.finish_run(RunFinish {
        run_id: journal,
        finished_at: now,
        state: "undone",
        head_after: Some(&snapshot.sha),
        message: Some(&format!("undo {}", snapshot.reference)),
        exit_code: Some(0),
        ..RunFinish::default()
    })?;
    report.state = UndoState::Undone;
    report.to = Some(snapshot.sha.clone());
    report.detail = format!(
        "restored {} ({}); every change stays in the index and working tree",
        short(&snapshot.sha),
        snapshot.reference
    );
    Ok(report)
}

fn refused(mut report: UndoReport, detail: impl Into<String>) -> UndoReport {
    report.state = UndoState::Refused;
    report.detail = detail.into();
    report
}

fn rev_parse(git: &Git, reference: &str) -> Result<String> {
    let out = git.run(&["rev-parse", "--verify", reference])?;
    Ok(out.stdout_lossy().trim().to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusal_is_the_default_state() {
        let report = UndoReport::new("/tmp/repo", "nothing to undo");
        assert_eq!(report.state, UndoState::Refused);
        assert_eq!(short("0123456789abcdef"), "0123456789ab");
    }

    #[test]
    fn missing_snapshot_target_is_refused() {
        let report = refused(
            UndoReport::new("/tmp/repo", ""),
            "the last run has no pre-sync snapshot",
        );
        assert_eq!(report.state, UndoState::Refused);
        assert!(report.detail.contains("pre-sync"));
    }
}
