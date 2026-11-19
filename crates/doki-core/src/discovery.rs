use crate::config::{Discovery, Root};
use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::WalkDir;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedRepo {
    pub root: String,
    pub path: PathBuf,
    pub git_common_dir: Option<PathBuf>,
}

pub fn scan(roots: &[Root], opts: &Discovery) -> Result<Vec<DetectedRepo>> {
    let mut found = BTreeMap::new();
    for root in roots.iter().filter(|r| r.enabled) {
        for path in scan_root(root, opts)? {
            found.entry(path.clone()).or_insert_with(|| DetectedRepo {
                root: root.name.clone(),
                git_common_dir: git_common_dir(&path),
                path,
            });
        }
    }
    Ok(found.into_values().collect())
}

fn scan_root(root: &Root, opts: &Discovery) -> Result<Vec<PathBuf>> {
    let base = match root.path.canonicalize() {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("root {}", root.path.display())),
    };
    let exclude = globs(&root.exclude)?;
    let mut repos = Vec::new();
    let mut it = WalkDir::new(&base)
        .max_depth(root.max_depth)
        .follow_links(opts.follow_symlinks)
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 {
                return true;
            }
            let skipped = e
                .file_name()
                .to_str()
                .is_some_and(|n| opts.skip_names.iter().any(|s| s == n));
            let excluded = e
                .path()
                .strip_prefix(&base)
                .is_ok_and(|rel| exclude.is_match(rel));
            e.file_type().is_dir() && !skipped && !excluded
        });
    while let Some(entry) = it.next() {
        let Ok(entry) = entry else { continue };
        if entry.path().join(".git").exists() {
            repos.push(entry.into_path());
            it.skip_current_dir();
        }
    }
    Ok(repos)
}

fn globs(patterns: &[String]) -> Result<GlobSet> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        b.add(Glob::new(p).with_context(|| format!("bad exclude glob `{p}`"))?);
    }
    Ok(b.build()?)
}

fn git_common_dir(repo: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn git_init(p: &Path) {
        fs::create_dir_all(p).unwrap();
        let ok = Command::new("git")
            .args(["init", "-q"])
            .arg(p)
            .status()
            .unwrap()
            .success();
        assert!(ok);
    }

    fn root(path: &Path) -> Root {
        Root {
            name: "t".into(),
            path: path.into(),
            max_depth: 2,
            enabled: true,
            schedule: String::new(),
            exclude: vec!["archive/**".into()],
        }
    }

    fn names(repos: &[DetectedRepo], base: &Path) -> Vec<String> {
        let base = base.canonicalize().unwrap();
        repos
            .iter()
            .map(|r| r.path.strip_prefix(&base).unwrap().display().to_string())
            .collect()
    }

    #[test]
    fn finds_depth_two_not_three_and_respects_skips() {
        let t = tempfile::tempdir().unwrap();
        let b = t.path();
        git_init(&b.join("one"));
        git_init(&b.join("one/nested"));
        git_init(&b.join("group/two"));
        git_init(&b.join("group/deep/three"));
        git_init(&b.join("node_modules/pkg"));
        git_init(&b.join("archive/old"));
        #[cfg(unix)]
        std::os::unix::fs::symlink(b.join("group"), b.join("link")).unwrap();

        let repos = scan(&[root(b)], &Discovery::default()).unwrap();
        assert_eq!(names(&repos, b), ["group/two", "one"]);
        assert!(repos.iter().all(|r| r.git_common_dir.is_some()));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_loops_are_not_followed() {
        let t = tempfile::tempdir().unwrap();
        let b = t.path();
        git_init(&b.join("one"));
        std::os::unix::fs::symlink(b, b.join("one/loop")).unwrap();

        let repos = scan(&[root(b)], &Discovery::default()).unwrap();
        assert_eq!(names(&repos, b), ["one"]);

        let mut following = root(b);
        let opts = Discovery {
            follow_symlinks: true,
            ..Discovery::default()
        };
        following.max_depth = 2;
        let repos = scan(&[following], &opts).unwrap();
        assert!(names(&repos, b).iter().all(|name| name == "one"));
    }

    #[test]
    fn missing_root_is_empty() {
        let repos = scan(
            &[root(Path::new("/nonexistent/doki"))],
            &Discovery::default(),
        )
        .unwrap();
        assert!(repos.is_empty());
    }
}
