use anyhow::{Context, Result};
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

pub struct RepoLock {
    _file: File,
}

pub enum LockAttempt {
    Acquired(RepoLock),
    Busy,
}

/// The lock lives in the git common dir so every worktree of a repo serializes.
pub fn try_acquire(common_dir: &Path) -> Result<LockAttempt> {
    try_acquire_path(&common_dir.join("doki.lock"))
}

/// One global lock so the in-app scheduler and `doki sync --due` never overlap.
pub fn try_scheduler(state_dir: &Path) -> Result<LockAttempt> {
    try_acquire_path(&state_dir.join("scheduler.lock"))
}

pub fn try_acquire_path(path: &Path) -> Result<LockAttempt> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let file = open_lock(path)?;
    match file.try_lock() {
        Ok(()) => Ok(LockAttempt::Acquired(RepoLock { _file: file })),
        Err(TryLockError::WouldBlock) => Ok(LockAttempt::Busy),
        Err(TryLockError::Error(e)) => Err(e).with_context(|| format!("lock {}", path.display())),
    }
}

fn open_lock(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))
}

pub fn lock_path(state_dir: &Path) -> PathBuf {
    state_dir.join("scheduler.lock")
}
