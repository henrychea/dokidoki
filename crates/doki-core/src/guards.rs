use crate::config::Guards;
use crate::git::Git;
use anyhow::{Result, bail};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Merge,
    RebaseMerge,
    RebaseApply,
    CherryPick,
    IndexLock,
}

impl Refusal {
    pub fn reason(self) -> &'static str {
        match self {
            Self::Merge => "MERGE_HEAD present",
            Self::RebaseMerge => "rebase-merge/ present",
            Self::RebaseApply => "rebase-apply/ present",
            Self::CherryPick => "CHERRY_PICK_HEAD present",
            Self::IndexLock => "index.lock present",
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.reason())
    }
}

pub fn refusal(git_dir: &Path) -> Option<Refusal> {
    let present = |name: &str| git_dir.join(name).exists();
    if present("MERGE_HEAD") {
        Some(Refusal::Merge)
    } else if present("rebase-merge") {
        Some(Refusal::RebaseMerge)
    } else if present("rebase-apply") {
        Some(Refusal::RebaseApply)
    } else if present("CHERRY_PICK_HEAD") {
        Some(Refusal::CherryPick)
    } else if present("index.lock") {
        Some(Refusal::IndexLock)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MassChange {
    pub changed_files: usize,
    pub deleted_lines: u64,
}

impl MassChange {
    pub fn parse(numstat: &[u8]) -> Self {
        let mut change = Self::default();
        for line in String::from_utf8_lossy(numstat).lines() {
            let mut fields = line.split('\t');
            let (Some(added), Some(deleted)) = (fields.next(), fields.next()) else {
                continue;
            };
            if added.is_empty() {
                continue;
            }
            change.changed_files += 1;
            if deleted != "-" {
                change.deleted_lines += deleted.parse().unwrap_or(0);
            }
        }
        change
    }

    pub fn check(&self, guards: &Guards) -> Result<()> {
        if self.changed_files > guards.max_changed_files {
            bail!(
                "mass change: {} changed files exceeds max_changed_files = {}",
                self.changed_files,
                guards.max_changed_files
            );
        }
        if self.deleted_lines > guards.max_deleted_lines {
            bail!(
                "mass change: {} deleted lines exceeds max_deleted_lines = {}",
                self.deleted_lines,
                guards.max_deleted_lines
            );
        }
        Ok(())
    }
}

pub fn measure(git: &Git) -> Result<MassChange> {
    let out = git.run(&["diff", "--numstat"])?;
    Ok(MassChange::parse(&out.stdout))
}
