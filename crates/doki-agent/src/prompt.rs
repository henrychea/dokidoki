use anyhow::Result;
use doki_core::git::Git;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStat {
    pub path: PathBuf,
    pub added: Option<u64>,
    pub deleted: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptContext {
    pub repo_name: String,
    pub branch: String,
    pub recent_subjects: Vec<String>,
    pub files: Vec<FileStat>,
    pub diff: String,
}

/// The staged state is what gets committed, so the prompt describes `--cached`.
pub fn gather(git: &Git, repo: &Path, max_diff_bytes: usize) -> Result<PromptContext> {
    let branch = git.run(&["rev-parse", "--abbrev-ref", "HEAD"])?;
    let subjects = git.run_unchecked(&["log", "-5", "--format=%s"])?;
    let numstat = git.run(&["diff", "--cached", "--numstat"])?;
    let diff = git.run(&["diff", "--cached"])?;
    Ok(PromptContext {
        repo_name: repo
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| repo.display().to_string()),
        branch: branch.stdout_lossy().trim().to_string(),
        recent_subjects: subjects
            .stdout_lossy()
            .lines()
            .map(str::to_string)
            .filter(|l| !l.is_empty())
            .collect(),
        files: parse_numstat(&numstat.stdout),
        diff: cap_diff(&diff.stdout_lossy(), max_diff_bytes),
    })
}

pub fn build(ctx: &PromptContext, max_diff_bytes: usize) -> String {
    let mut out = String::new();
    out.push_str("Write one git commit message for the changes below.\n\n");
    out.push_str(
        "Rules:\n\
         - First line: the subject, at most 65 characters.\n\
         - Then optionally a blank line and a short body.\n\
         - Plain text only: no markdown, no code fences, no quotes, no `doki:` prefix.\n\n",
    );
    out.push_str(&format!(
        "Repository: {}\nBranch: {}\n\n",
        ctx.repo_name, ctx.branch
    ));
    if !ctx.recent_subjects.is_empty() {
        out.push_str("Recent commits:\n");
        for subject in &ctx.recent_subjects {
            out.push_str(&format!("- {subject}\n"));
        }
        out.push('\n');
    }
    out.push_str(&format!("Files changed ({}):\n", ctx.files.len()));
    for file in &ctx.files {
        let stats = match (file.added, file.deleted) {
            (Some(added), Some(deleted)) => format!("+{added} -{deleted}"),
            _ => "+- -".to_string(),
        };
        out.push_str(&format!("{} {stats}\n", file.path.display()));
    }
    out.push_str("\nDiff:\n");
    out.push_str(&cap_diff(&ctx.diff, max_diff_bytes));
    out
}

fn cap_diff(diff: &str, max: usize) -> String {
    if diff.len() <= max {
        return diff.to_string();
    }
    let mut end = max;
    while end > 0 && !diff.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[diff truncated at {max} bytes]\n", &diff[..end])
}

fn parse_numstat(numstat: &[u8]) -> Vec<FileStat> {
    String::from_utf8_lossy(numstat)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let added = fields.next()?;
            let deleted = fields.next()?;
            let path = fields.next()?;
            Some(FileStat {
                path: PathBuf::from(path),
                added: added.parse().ok(),
                deleted: deleted.parse().ok(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn git(root: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn context() -> PromptContext {
        PromptContext {
            repo_name: "billing".into(),
            branch: "main".into(),
            recent_subjects: vec!["add invoice table".into(), "fix rounding".into()],
            files: vec![
                FileStat {
                    path: "src/a.rs".into(),
                    added: Some(12),
                    deleted: Some(3),
                },
                FileStat {
                    path: "logo.png".into(),
                    added: None,
                    deleted: None,
                },
            ],
            diff: "diff --git a/src/a.rs b/src/a.rs\n+let x = 1;\n".into(),
        }
    }

    #[test]
    fn prompt_has_repo_branch_files_and_diff() {
        let prompt = build(&context(), 60_000);
        assert!(prompt.contains("Repository: billing\nBranch: main\n"));
        assert!(prompt.contains("- add invoice table\n"));
        assert!(prompt.contains("Files changed (2):\n"));
        assert!(prompt.contains("src/a.rs +12 -3\n"));
        assert!(prompt.contains("logo.png +- -\n"));
        assert!(prompt.contains("+let x = 1;"));
        assert!(prompt.contains("no `doki:` prefix"));
    }

    #[test]
    fn gather_reads_branch_files_diff_and_recent_subjects() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        git(root, &["init", "-q", "-b", "main"]);
        fs::write(root.join("a.txt"), "one\n").unwrap();
        git(root, &["add", "a.txt"]);
        git(
            root,
            &[
                "-c",
                "user.name=doki",
                "-c",
                "user.email=doki@example.com",
                "commit",
                "-qm",
                "initial commit",
            ],
        );
        fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
        git(root, &["add", "a.txt"]);

        let ctx = gather(&Git::new(root), root, 60_000).unwrap();
        assert!(!ctx.repo_name.is_empty());
        assert_eq!(ctx.branch, "main");
        assert_eq!(ctx.recent_subjects, ["initial commit"]);
        assert_eq!(ctx.files.len(), 1);
        assert_eq!(ctx.files[0].path, PathBuf::from("a.txt"));
        assert_eq!(ctx.files[0].added, Some(1));
        assert_eq!(ctx.files[0].deleted, Some(0));
        assert!(ctx.diff.contains("+two"));

        let capped = gather(&Git::new(root), root, 10).unwrap();
        assert!(capped.diff.contains("[diff truncated at 10 bytes]"));
    }

    #[test]
    fn diff_is_capped() {
        let mut ctx = context();
        ctx.diff = "y".repeat(100);
        let prompt = build(&ctx, 10);
        assert!(prompt.contains(&"y".repeat(10)));
        assert!(!prompt.contains(&"y".repeat(11)));
        assert!(prompt.contains("[diff truncated at 10 bytes]"));

        let mut ctx = context();
        ctx.diff = "é".repeat(20);
        let prompt = build(&ctx, 7);
        assert!(prompt.contains("[diff truncated at 7 bytes]"));
        assert!(std::str::from_utf8(prompt.as_bytes()).is_ok());
    }

    #[test]
    fn numstat_parses_binary_and_text() {
        let files = parse_numstat(b"12\t3\tsrc/a.rs\n-\t-\tlogo.png\n");
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].added, Some(12));
        assert_eq!(files[1].path, PathBuf::from("logo.png"));
        assert_eq!(files[1].added, None);
    }
}
