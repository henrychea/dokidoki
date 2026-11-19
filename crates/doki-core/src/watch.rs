use notify::{RecursiveMode, Watcher as _};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub const DEFAULT_WATCH_CAP: usize = 2048;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WatchMode {
    Native,
    #[default]
    Polling,
}

/// A native watcher with a hard budget; overflow or a watch failure means the
/// caller polls instead, so discovery never depends on inotify availability.
pub struct Watch {
    mode: WatchMode,
    changed: Arc<AtomicBool>,
    _watcher: Option<notify::RecommendedWatcher>,
}

impl Default for Watch {
    fn default() -> Self {
        Self::polling()
    }
}

impl Watch {
    pub fn polling() -> Self {
        Self {
            mode: WatchMode::Polling,
            changed: Arc::new(AtomicBool::new(false)),
            _watcher: None,
        }
    }

    pub fn mode(&self) -> WatchMode {
        self.mode
    }

    pub fn changed(&self) -> bool {
        self.changed.swap(false, Ordering::Relaxed)
    }
}

pub fn watch_repos(repos: &[PathBuf], cap: usize) -> Watch {
    let targets = targets(repos);
    if targets.len() > cap {
        return Watch::polling();
    }
    let changed = Arc::new(AtomicBool::new(false));
    let flag = changed.clone();
    let mut watcher =
        match notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if event.is_ok() {
                flag.store(true, Ordering::Relaxed);
            }
        }) {
            Ok(watcher) => watcher,
            Err(_) => return Watch::polling(),
        };
    for (path, recursive) in targets {
        if watcher.watch(&path, mode(recursive)).is_err() {
            return Watch::polling();
        }
    }
    Watch {
        mode: WatchMode::Native,
        changed,
        _watcher: Some(watcher),
    }
}

fn mode(recursive: bool) -> RecursiveMode {
    if recursive {
        RecursiveMode::Recursive
    } else {
        RecursiveMode::NonRecursive
    }
}

fn targets(repos: &[PathBuf]) -> Vec<(PathBuf, bool)> {
    let mut targets = Vec::new();
    for repo in repos {
        targets.push((repo.clone(), false));
        let git = repo.join(".git");
        if !git.is_dir() {
            continue;
        }
        for name in ["HEAD", "index"] {
            let path = git.join(name);
            if path.exists() {
                targets.push((path, false));
            }
        }
        let refs = git.join("refs");
        if refs.exists() {
            targets.push((refs, true));
        }
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn overflow_or_a_broken_target_falls_back_to_polling() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir_all(repo.join(".git/refs/heads")).unwrap();
        fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(
            watch_repos(std::slice::from_ref(&repo), 1).mode(),
            WatchMode::Polling
        );
        assert_eq!(
            watch_repos(std::slice::from_ref(&repo), DEFAULT_WATCH_CAP).mode(),
            WatchMode::Native
        );
        assert_eq!(
            watch_repos(&[temp.path().join("missing")], 0).mode(),
            WatchMode::Polling
        );
    }

    #[test]
    fn a_repo_change_sets_the_flag() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir_all(repo.join(".git/refs/heads")).unwrap();
        fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let watch = watch_repos(std::slice::from_ref(&repo), DEFAULT_WATCH_CAP);
        assert_eq!(watch.mode(), WatchMode::Native);
        fs::write(repo.join("work.txt"), "x\n").unwrap();
        for _ in 0..40 {
            if watch.changed() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        panic!("a file write did not wake the watcher");
    }
}
