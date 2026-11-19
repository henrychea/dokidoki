use crate::config::{PushMode, Sync};
use crate::git::Git;
use crate::status::PorcelainStatus;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PushState {
    #[default]
    Skipped,
    Pushed,
    UpToDate,
    CommitOnly,
    Protected,
    NoRemote,
    Diverged,
    Failed,
    Bundled,
}

impl PushState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Skipped => "skipped",
            Self::Pushed => "pushed",
            Self::UpToDate => "up-to-date",
            Self::CommitOnly => "commit-only",
            Self::Protected => "protected",
            Self::NoRemote => "no-remote",
            Self::Diverged => "diverged",
            Self::Failed => "failed",
            Self::Bundled => "bundled",
        }
    }

    pub fn incident(self) -> Option<Incident> {
        match self {
            Self::NoRemote => Some(Incident::warning(
                "no-remote",
                "the commit is local only: this repo has no origin remote",
            )),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PushOutcome {
    pub state: PushState,
    pub detail: Option<String>,
    pub warning: Option<String>,
    pub incident: Option<Incident>,
    pub bundle: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incident {
    pub severity: &'static str,
    pub kind: &'static str,
    pub detail: String,
}

impl Incident {
    fn warning(kind: &'static str, detail: impl Into<String>) -> Self {
        Self {
            severity: "warning",
            kind,
            detail: detail.into(),
        }
    }

    fn error(kind: &'static str, detail: impl Into<String>) -> Self {
        Self {
            severity: "error",
            kind,
            detail: detail.into(),
        }
    }

    fn blocking(kind: &'static str, detail: impl Into<String>) -> Self {
        Self {
            severity: "blocking",
            kind,
            detail: detail.into(),
        }
    }

    fn outcome(self) -> PushOutcome {
        PushOutcome {
            state: PushState::Failed,
            detail: Some(self.detail.clone()),
            incident: Some(self),
            ..PushOutcome::default()
        }
    }
}

pub fn origin_url(git: &Git) -> Result<Option<String>> {
    let out = git.run_unchecked(&["config", "--get", "remote.origin.url"])?;
    let url = out.stdout_lossy().trim().to_string();
    Ok(out.success().then_some(url).filter(|url| !url.is_empty()))
}

pub fn has_origin(git: &Git) -> Result<bool> {
    Ok(origin_url(git)?.is_some())
}

fn upstream_remote(git: &Git, branch: &str) -> Result<Option<String>> {
    let key = format!("branch.{branch}.remote");
    let out = git.run_unchecked(&["config", "--get", &key])?;
    let remote = out.stdout_lossy().trim().to_string();
    Ok(out.success().then_some(remote).filter(|r| !r.is_empty()))
}

pub struct PushPolicy<'a> {
    pub cfg: &'a Sync,
    pub branch: &'a str,
    pub allow_protected_push: bool,
    pub committed: bool,
    pub bundle_dir: &'a Path,
    pub now: i64,
}

impl PushPolicy<'_> {
    pub fn protected(&self) -> bool {
        self.cfg.protected.iter().any(|name| name == self.branch)
    }
}

/// Ahead on a non-protected branch fast-forwards; behind or diverged waits for a human.
pub fn check(git: &Git, policy: &PushPolicy<'_>) -> Result<PushOutcome> {
    if policy.cfg.push_mode == PushMode::Bundle {
        if !policy.committed {
            return Ok(PushOutcome::default());
        }
        return bundle(git, policy);
    }
    if !has_origin(git)? {
        let mut outcome = PushOutcome {
            state: PushState::NoRemote,
            ..PushOutcome::default()
        };
        if policy.committed {
            outcome.detail = Some("no origin remote: the commit stays local".into());
            outcome.warning = Some("no origin remote".into());
            outcome.incident = PushState::NoRemote.incident();
        }
        return Ok(outcome);
    }
    if policy.protected() && !policy.allow_protected_push {
        return Ok(PushOutcome {
            state: PushState::Protected,
            detail: Some(format!(
                "`{}` is protected: committed locally, push skipped",
                policy.branch
            )),
            ..PushOutcome::default()
        });
    }
    if policy.cfg.push_mode == PushMode::CommitOnly {
        return Ok(PushOutcome {
            state: PushState::CommitOnly,
            detail: Some("push_mode = commit-only".into()),
            ..PushOutcome::default()
        });
    }

    let fetch = git.run_timeout(
        &["fetch", "--prune", "origin"],
        timeout(policy.cfg.fetch_timeout_seconds),
    )?;
    if fetch.timed_out {
        return Ok(Incident::error(
            "fetch-timeout",
            format!(
                "fetch --prune origin timed out after {}s",
                policy.cfg.fetch_timeout_seconds
            ),
        )
        .outcome());
    }
    if !fetch.success() {
        return Ok(Incident::error(
            "fetch",
            format!("fetch --prune origin failed: {}", tail(&fetch)),
        )
        .outcome());
    }

    let parsed = status(git)?;
    if parsed.upstream.is_none() {
        let out = push(git, policy, true)?;
        return Ok(match out.state {
            PushState::Pushed => out,
            _ => out,
        });
    }
    if parsed.behind > 0 {
        let detail = format!(
            "origin is {} commit(s) ahead and this branch is {} commit(s) ahead: \
             doki never merges, rebases or force-pushes, so a human decides",
            parsed.behind, parsed.ahead
        );
        return Ok(PushOutcome {
            state: PushState::Diverged,
            detail: Some(detail.clone()),
            incident: Some(Incident::blocking("diverged", detail)),
            ..PushOutcome::default()
        });
    }
    if parsed.ahead == 0 {
        return Ok(PushOutcome {
            state: PushState::UpToDate,
            detail: Some("nothing to push: origin already has this commit".into()),
            ..PushOutcome::default()
        });
    }
    push(git, policy, false)
}

pub fn preview(
    git: &Git,
    cfg: &Sync,
    branch: &str,
    allow_protected_push: bool,
) -> Result<PushOutcome> {
    if cfg.push_mode == PushMode::Bundle {
        return Ok(PushOutcome {
            state: PushState::Bundled,
            detail: Some("dry run: no bundle would be written".into()),
            ..PushOutcome::default()
        });
    }
    if !has_origin(git)? {
        return Ok(PushOutcome {
            state: PushState::NoRemote,
            ..PushOutcome::default()
        });
    }
    if cfg.protected.iter().any(|name| name == branch) && !allow_protected_push {
        return Ok(PushOutcome {
            state: PushState::Protected,
            detail: Some(format!(
                "`{branch}` is protected: committed locally, push skipped"
            )),
            ..PushOutcome::default()
        });
    }
    if cfg.push_mode == PushMode::CommitOnly {
        return Ok(PushOutcome {
            state: PushState::CommitOnly,
            detail: Some("push_mode = commit-only".into()),
            ..PushOutcome::default()
        });
    }
    let set_upstream = upstream_remote(git, branch)?.is_none();
    let _ = git.run_unchecked(&["fetch", "--prune", "origin"]);
    let mut args = vec!["push"];
    if set_upstream {
        args.push("-u");
    }
    args.extend(["origin", branch]);
    let _ = git.run_unchecked(&args);
    Ok(PushOutcome {
        state: PushState::Skipped,
        detail: Some("dry run: fetch and push were not executed".into()),
        ..PushOutcome::default()
    })
}

fn push(git: &Git, policy: &PushPolicy<'_>, set_upstream: bool) -> Result<PushOutcome> {
    let mut args = vec!["push"];
    if set_upstream {
        args.push("-u");
    }
    args.extend(["origin", policy.branch]);
    let out = git.run_timeout(&args, timeout(policy.cfg.push_timeout_seconds))?;
    let command = format!("push {}", args[1..].join(" "));
    if out.timed_out {
        return Ok(Incident::error(
            "push-timeout",
            format!(
                "{command} timed out after {}s",
                policy.cfg.push_timeout_seconds
            ),
        )
        .outcome());
    }
    if !out.success() {
        return Ok(
            Incident::error("push", format!("{command} was rejected: {}", tail(&out))).outcome(),
        );
    }
    Ok(PushOutcome {
        state: PushState::Pushed,
        detail: Some(format!(
            "pushed {} to origin{}",
            policy.branch,
            if set_upstream {
                " and set the upstream"
            } else {
                ""
            }
        )),
        ..PushOutcome::default()
    })
}

fn bundle(git: &Git, policy: &PushPolicy<'_>) -> Result<PushOutcome> {
    std::fs::create_dir_all(policy.bundle_dir)
        .with_context(|| format!("create {}", policy.bundle_dir.display()))?;
    let name = git
        .repo_path()
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    let path = allocate_bundle(policy.bundle_dir, &name, policy.now);
    let out = git.run_timeout(
        &["bundle", "create", &path.to_string_lossy(), "--all"],
        timeout(policy.cfg.push_timeout_seconds),
    )?;
    if out.timed_out || !out.success() {
        let why = if out.timed_out {
            format!(
                "git bundle create timed out after {}s",
                policy.cfg.push_timeout_seconds
            )
        } else {
            format!("git bundle create failed: {}", tail(&out))
        };
        return Ok(Incident::error("bundle", why).outcome());
    }
    Ok(PushOutcome {
        state: PushState::Bundled,
        detail: Some(format!("bundled every ref into {}", path.display())),
        bundle: Some(path),
        ..PushOutcome::default()
    })
}

fn allocate_bundle(dir: &Path, name: &str, now: i64) -> PathBuf {
    let mut path = dir.join(format!("{name}-{now}.bundle"));
    for n in 1..1000 {
        if !path.exists() {
            break;
        }
        path = dir.join(format!("{name}-{now}-{n}.bundle"));
    }
    path
}

fn status(git: &Git) -> Result<PorcelainStatus> {
    let out = git.run(&["status", "--porcelain=v2", "-z", "--branch"])?;
    crate::status::parse(&out.stdout)
}

fn tail(out: &crate::git::GitOutput) -> String {
    let lines: Vec<String> = out
        .stderr_lossy()
        .lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(4)
        .map(|line| line.trim().to_string())
        .collect();
    if lines.is_empty() {
        return "no stderr".into();
    }
    lines
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("; ")
        .chars()
        .take(500)
        .collect()
}

fn timeout(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}
