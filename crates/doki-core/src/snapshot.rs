use crate::config::{CommitScope, Config};
use crate::git::Git;
use crate::guards::MassChange;
use crate::repo_config::RepoConfig;
use crate::secret::{self, ScanReport};
use crate::store::Store;
use crate::sync::{ChangedFile, MessageContext};
use anyhow::Result;
use std::path::{Path, PathBuf};

/// Everything the center shows about a repo before anything mutates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub repo: PathBuf,
    pub branch: String,
    pub scope: CommitScope,
    pub files: Vec<ChangedFile>,
    pub diff: String,
    pub mass: MassChange,
    pub scan: ScanReport,
    pub truncated: bool,
}

pub const MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;

impl Snapshot {
    pub fn context_bytes(&self) -> usize {
        self.diff.len()
    }

    pub fn blocked(&self) -> bool {
        self.scan.blocks()
    }

    pub fn context(&self, recent_subjects: Vec<String>) -> MessageContext {
        MessageContext {
            repo_name: self
                .repo
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.repo.display().to_string()),
            branch: self.branch.clone(),
            recent_subjects,
            files: self.files.clone(),
            diff: self.diff.clone(),
        }
    }
}

/// The scope the center and the pipeline must agree on: the machine-side repo
/// pref first, then config, then whatever `.doki.toml` may narrow.
pub fn scope_for(cfg: &Config, store: Option<&Store>, repo: &Path) -> Result<CommitScope> {
    let base = match store {
        Some(store) => store
            .repo_by_path(repo)?
            .and_then(|row| row.commit_scope)
            .unwrap_or(cfg.sync.commit_scope),
        None => cfg.sync.commit_scope,
    };
    RepoConfig::load(repo)?.narrowed_scope(base)
}

pub fn read(cfg: &Config, store: Option<&Store>, repo_path: &Path) -> Result<Snapshot> {
    let repo = repo_path
        .canonicalize()
        .unwrap_or_else(|_| repo_path.to_path_buf());
    let git = Git::new(&repo);
    let scope = scope_for(cfg, store, &repo)?;
    let branch = branch_name(&git);
    let ignore = RepoConfig::load(&repo)?.ignore;

    let (numstat_args, diff_args) = match scope {
        CommitScope::Staged => (
            vec!["diff", "--cached", "--numstat"],
            vec!["diff", "--cached"],
        ),
        CommitScope::Tracked | CommitScope::All => {
            (vec!["diff", "HEAD", "--numstat"], vec!["diff", "HEAD"])
        }
    };
    let mut numstat_args = with_pathspec(numstat_args, &ignore);
    let diff_args = with_pathspec(diff_args, &ignore);
    let numstat = run_args(&git, &numstat_args)?;
    let diff = run_args(&git, &diff_args)?;
    numstat_args.clear();

    let mut files = parse_numstat(&numstat);
    if scope == CommitScope::All {
        for path in untracked(&git, &ignore)? {
            files.push(ChangedFile {
                path,
                added: None,
                deleted: None,
            });
        }
    }
    let mass = MassChange::parse(&numstat);
    let truncated = diff.len() > MAX_DIFF_BYTES;
    let text = if truncated {
        String::from_utf8_lossy(&diff[..MAX_DIFF_BYTES]).into_owned()
    } else {
        String::from_utf8_lossy(&diff).into_owned()
    };
    let scan = secret::scan_report(&diff, &cfg.guards);
    Ok(Snapshot {
        repo,
        branch,
        scope,
        files,
        diff: text,
        mass,
        scan,
        truncated,
    })
}

fn with_pathspec(mut args: Vec<&str>, ignore: &[String]) -> Vec<String> {
    let mut out: Vec<String> = args.drain(..).map(str::to_string).collect();
    out.push("--".into());
    out.push(".".into());
    out.extend(ignore.iter().map(|glob| format!(":(exclude){glob}")));
    out
}

fn run_args(git: &Git, args: &[String]) -> Result<Vec<u8>> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    Ok(git.run(&refs)?.stdout)
}

fn branch_name(git: &Git) -> String {
    git.run_unchecked(&["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|out| out.stdout_lossy().trim().to_string())
        .unwrap_or_else(|_| "HEAD".into())
}

fn untracked(git: &Git, ignore: &[String]) -> Result<Vec<PathBuf>> {
    let mut args = vec![
        "ls-files".to_string(),
        "--others".to_string(),
        "--exclude-standard".to_string(),
        "-z".to_string(),
    ];
    args.extend(ignore.iter().map(|glob| format!(":(exclude){glob}")));
    let out = run_args(git, &args)?;
    Ok(out
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .map(|field| PathBuf::from(String::from_utf8_lossy(field).into_owned()))
        .collect())
}

fn parse_numstat(raw: &[u8]) -> Vec<ChangedFile> {
    String::from_utf8_lossy(raw)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let added = fields.next()?;
            let deleted = fields.next()?;
            let path = fields.next()?;
            if added.is_empty() {
                return None;
            }
            Some(ChangedFile {
                path: PathBuf::from(path),
                added: added.parse().ok(),
                deleted: deleted.parse().ok(),
            })
        })
        .collect()
}

pub fn recent_subjects(git: &Git) -> Vec<String> {
    git.run_unchecked(&["log", "-5", "--format=%s"])
        .map(|out| {
            out.stdout_lossy()
                .lines()
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub fn subjects(repo: &Path) -> Vec<String> {
    recent_subjects(&Git::new(repo))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn git(repo: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .expect("git");
        assert!(status.status.success(), "git {args:?}");
    }

    fn repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(tmp.path().join("a.txt"), "one\ntwo\n").unwrap();
        git(tmp.path(), &["add", "-A"]);
        git(tmp.path(), &["commit", "-qm", "init"]);
        tmp
    }

    #[test]
    fn scope_follows_the_repo_pref_then_config() {
        let tmp = repo();
        let mut store = Store::open_in_memory().unwrap();
        store
            .upsert_detected(&[crate::discovery::DetectedRepo {
                root: "dev".into(),
                path: tmp.path().to_path_buf(),
                git_common_dir: None,
            }])
            .unwrap();
        let id = store.repos().unwrap()[0].id;
        let cfg = Config::parse("[sync]\ncommit_scope = \"tracked\"\n").unwrap();
        assert_eq!(
            scope_for(&cfg, Some(&store), tmp.path()).unwrap(),
            CommitScope::Tracked
        );
        store.set_scope_pref(id, Some(CommitScope::All)).unwrap();
        assert_eq!(
            scope_for(&cfg, Some(&store), tmp.path()).unwrap(),
            CommitScope::All
        );
        store.set_scope_pref(id, Some(CommitScope::Staged)).unwrap();
        std::fs::write(
            tmp.path().join(".doki.toml"),
            "commit_scope = \"tracked\"\n",
        )
        .unwrap();
        assert!(scope_for(&cfg, Some(&store), tmp.path()).is_err());
    }

    #[test]
    fn snapshot_counts_tracked_changes_and_truncates_nothing_small() {
        let tmp = repo();
        std::fs::write(tmp.path().join("a.txt"), "one\nthree\nfour\n").unwrap();
        let cfg = Config::parse("").unwrap();
        let snap = read(&cfg, None, tmp.path()).unwrap();
        assert_eq!(snap.files.len(), 1);
        assert_eq!(snap.files[0].added, Some(2));
        assert_eq!(snap.files[0].deleted, Some(1));
        assert_eq!(snap.mass.changed_files, 1);
        assert!(!snap.truncated);
        assert!(snap.diff.contains("+three"));
        assert!(!snap.blocked());
    }

    #[test]
    fn untracked_files_appear_only_in_the_all_scope() {
        let tmp = repo();
        std::fs::write(tmp.path().join("new.txt"), "hello\n").unwrap();
        let cfg = Config::parse("[sync]\ncommit_scope = \"all\"\n").unwrap();
        let mut store = Store::open_in_memory().unwrap();
        store
            .upsert_detected(&[crate::discovery::DetectedRepo {
                root: "dev".into(),
                path: tmp.path().to_path_buf(),
                git_common_dir: None,
            }])
            .unwrap();
        let snap = read(&cfg, Some(&store), tmp.path()).unwrap();
        assert_eq!(snap.files.len(), 1);
        assert_eq!(snap.files[0].path, PathBuf::from("new.txt"));
        assert_eq!(snap.files[0].added, None);
    }

    #[test]
    fn a_secret_in_the_diff_blocks_the_snapshot() {
        let tmp = repo();
        std::fs::write(
            tmp.path().join("a.txt"),
            "one\ntwo\naws = \"AKIA0000000000000000\"\n",
        )
        .unwrap();
        let cfg = Config::parse("").unwrap();
        let snap = read(&cfg, None, tmp.path()).unwrap();
        assert!(snap.blocked());
        assert!(snap.scan.summary().contains("blocked"));
    }
}
