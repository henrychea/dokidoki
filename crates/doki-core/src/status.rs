use crate::git::Git;
use crate::guards::{self, Refusal};
use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PorcelainStatus {
    pub head: Option<String>,
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub staged: Vec<PathBuf>,
    pub unstaged: Vec<PathBuf>,
    pub untracked: Vec<PathBuf>,
    pub unmerged: Vec<PathBuf>,
}

impl PorcelainStatus {
    pub fn detached(&self) -> bool {
        self.branch.is_none()
    }

    pub fn unborn(&self) -> bool {
        self.head.is_none()
    }

    pub fn dirty(&self) -> usize {
        let paths: BTreeSet<&Path> = self
            .staged
            .iter()
            .chain(&self.unstaged)
            .chain(&self.untracked)
            .chain(&self.unmerged)
            .map(PathBuf::as_path)
            .collect();
        paths.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoState {
    Clean,
    Dirty,
    Refused(String),
    Detached,
    Unborn,
}

impl RepoState {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Dirty => "dirty",
            Self::Refused(_) => "refused",
            Self::Detached => "detached",
            Self::Unborn => "unborn",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoStatus {
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub dirty: usize,
    pub state: RepoState,
}

pub fn read(git: &Git) -> Result<RepoStatus> {
    let out = git.run(&["status", "--porcelain=v2", "-z", "--branch"])?;
    let parsed = parse(&out.stdout)?;
    let refusal = refusal(git)?;
    let dirty = parsed.dirty();
    let state = match refusal {
        Some(r) => RepoState::Refused(r.to_string()),
        None if parsed.unborn() => RepoState::Unborn,
        None if parsed.detached() => RepoState::Detached,
        None if dirty > 0 => RepoState::Dirty,
        None => RepoState::Clean,
    };
    Ok(RepoStatus {
        branch: parsed.branch,
        upstream: parsed.upstream,
        ahead: parsed.ahead,
        behind: parsed.behind,
        dirty,
        state,
    })
}

fn refusal(git: &Git) -> Result<Option<Refusal>> {
    let dir = git.git_dir().context("resolve git dir")?;
    Ok(guards::refusal(&dir))
}

pub fn parse(raw: &[u8]) -> Result<PorcelainStatus> {
    let mut status = PorcelainStatus::default();
    let mut fields = raw.split(|b| *b == 0).filter(|f| !f.is_empty());
    while let Some(field) = fields.next() {
        let text = String::from_utf8_lossy(field);
        let (tag, rest) = text.split_once(' ').unwrap_or((text.as_ref(), ""));
        match tag {
            "#" => header(&mut status, rest)?,
            "1" => ordinary(&mut status, rest)?,
            "2" => rename(&mut status, rest, fields.next())?,
            "u" => unmerged(&mut status, rest)?,
            "?" => status.untracked.push(PathBuf::from(rest)),
            "!" => {}
            _ => bail!("unexpected git status record `{tag}`"),
        }
    }
    Ok(status)
}

fn header(status: &mut PorcelainStatus, rest: &str) -> Result<()> {
    let (key, value) = rest.split_once(' ').unwrap_or((rest, ""));
    match key {
        "branch.oid" if value != "(initial)" => status.head = Some(value.into()),
        "branch.head" if value != "(detached)" => status.branch = Some(value.into()),
        "branch.upstream" => status.upstream = Some(value.into()),
        "branch.ab" => {
            for token in value.split(' ') {
                if let Some(n) = token.strip_prefix('+') {
                    status.ahead = n.parse().context("branch.ab ahead")?;
                } else if let Some(n) = token.strip_prefix('-') {
                    status.behind = n.parse().context("branch.ab behind")?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn ordinary(status: &mut PorcelainStatus, rest: &str) -> Result<()> {
    let parts: Vec<&str> = rest.splitn(8, ' ').collect();
    if parts.len() < 8 {
        bail!("malformed status entry `{rest}`");
    }
    mark(status, parts[0], PathBuf::from(parts[7]))?;
    Ok(())
}

fn rename(status: &mut PorcelainStatus, rest: &str, original: Option<&[u8]>) -> Result<()> {
    let parts: Vec<&str> = rest.splitn(9, ' ').collect();
    if parts.len() < 9 || original.is_none() {
        bail!("malformed rename entry `{rest}`");
    }
    mark(status, parts[0], PathBuf::from(parts[8]))
}

fn unmerged(status: &mut PorcelainStatus, rest: &str) -> Result<()> {
    let parts: Vec<&str> = rest.splitn(10, ' ').collect();
    if parts.len() < 10 {
        bail!("malformed unmerged entry `{rest}`");
    }
    status.unmerged.push(PathBuf::from(parts[9]));
    Ok(())
}

fn mark(status: &mut PorcelainStatus, xy: &str, path: PathBuf) -> Result<()> {
    if xy.len() != 2 {
        bail!("malformed XY field `{xy}`");
    }
    if !xy.starts_with('.') {
        status.staged.push(path.clone());
    }
    if !xy.ends_with('.') {
        status.unstaged.push(path);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nul(parts: &[&str]) -> Vec<u8> {
        let mut raw = Vec::new();
        for p in parts {
            raw.extend_from_slice(p.as_bytes());
            raw.push(0);
        }
        raw
    }

    #[test]
    fn parses_clean_branch_with_upstream() {
        let raw = nul(&[
            "# branch.oid abc123",
            "# branch.head main",
            "# branch.upstream origin/main",
            "# branch.ab +1 -2",
        ]);
        let st = parse(&raw).unwrap();
        assert_eq!(st.head.as_deref(), Some("abc123"));
        assert_eq!(st.branch.as_deref(), Some("main"));
        assert_eq!(st.upstream.as_deref(), Some("origin/main"));
        assert_eq!((st.ahead, st.behind), (1, 2));
        assert_eq!(st.dirty(), 0);
        assert!(!st.detached() && !st.unborn());
    }

    #[test]
    fn parses_rename_and_untracked() {
        let raw = nul(&[
            "# branch.oid abc",
            "# branch.head main",
            "1 .M N... 100644 100644 100644 aaa aaa b.txt",
            "2 R. N... 100644 100644 100644 bbb bbb R100 renamed file.txt",
            "old name.txt",
            "? untracked.txt",
        ]);
        let st = parse(&raw).unwrap();
        assert_eq!(st.staged, [PathBuf::from("renamed file.txt")]);
        assert_eq!(st.unstaged, [PathBuf::from("b.txt")]);
        assert_eq!(st.untracked, [PathBuf::from("untracked.txt")]);
        assert_eq!(st.dirty(), 3);
    }

    #[test]
    fn parses_unmerged_entry() {
        let raw = nul(&[
            "# branch.oid abc",
            "# branch.head main",
            "u UU N... 100644 100644 100644 100644 aaa bbb ccc conflict.txt",
        ]);
        let st = parse(&raw).unwrap();
        assert_eq!(st.unmerged, [PathBuf::from("conflict.txt")]);
        assert_eq!(st.dirty(), 1);
        assert!(st.staged.is_empty() && st.unstaged.is_empty());
    }

    #[test]
    fn counts_a_both_staged_and_unstaged_file_once() {
        let raw = nul(&[
            "# branch.head main",
            "1 MM N... 100644 100644 100644 a a both.txt",
        ]);
        let st = parse(&raw).unwrap();
        assert_eq!(st.staged.len(), 1);
        assert_eq!(st.unstaged.len(), 1);
        assert_eq!(st.dirty(), 1);
    }

    #[test]
    fn parses_detached_and_unborn_headers() {
        let detached = parse(&nul(&["# branch.oid abc", "# branch.head (detached)"])).unwrap();
        assert!(detached.detached() && !detached.unborn());

        let unborn = parse(&nul(&["# branch.oid (initial)", "# branch.head main"])).unwrap();
        assert!(unborn.unborn());
        assert_eq!(unborn.branch.as_deref(), Some("main"));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse(&nul(&["nonsense"])).is_err());
        assert!(parse(&nul(&["1 .M"])).is_err());
        assert!(parse(&nul(&["2 R. N... 100644 100644 100644 a a R100 x"])).is_err());
    }
}
