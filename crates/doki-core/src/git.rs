use anyhow::{Context, Result, bail};
use std::cell::RefCell;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Dry run executes only these subcommands. Everything else, including `fetch`,
/// is recorded and skipped, so an unlisted mutating command cannot slip through.
const READ_ONLY: &[&str] = &[
    "status",
    "diff",
    "rev-parse",
    "log",
    "show",
    "ls-files",
    "merge-base",
    "for-each-ref",
    "cat-file",
    "rev-list",
    "check-ignore",
    "var",
    "config",
];

const CONFIG_READS: &[&str] = &["--get", "--get-all", "--list", "-l"];

fn is_read_only(args: &[&str]) -> bool {
    let mut rest = args;
    loop {
        match rest {
            // Leading global options hide the subcommand: `-c k=v commit` must not read as `commit`.
            ["-c" | "-C", _, tail @ ..] => rest = tail,
            [first, tail @ ..] if first.starts_with('-') => rest = tail,
            [sub, tail @ ..] => return read_only_subcommand(sub, tail),
            [] => return false,
        }
    }
}

fn read_only_subcommand(sub: &str, args: &[&str]) -> bool {
    // `symbolic-ref HEAD <ref>` and `-d` both write; only a plain query is safe.
    if sub == "symbolic-ref" {
        let mut operands = 0;
        for arg in args {
            match *arg {
                "-q" | "--quiet" => {}
                option if option.starts_with('-') => return false,
                _ => operands += 1,
            }
        }
        return operands == 1;
    }
    READ_ONLY.contains(&sub) && (sub != "config" || args.iter().any(|a| CONFIG_READS.contains(a)))
}

pub struct Git {
    repo: PathBuf,
    dry_run: bool,
    recorded: RefCell<Vec<Vec<String>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    pub argv: Vec<String>,
    pub executed: bool,
    pub status: Option<ExitStatus>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
}

impl GitOutput {
    pub fn stdout_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }

    pub fn success(&self) -> bool {
        self.status.is_some_and(|s| s.success())
    }
}

impl Git {
    pub fn new(repo: impl Into<PathBuf>) -> Self {
        Self {
            repo: repo.into(),
            dry_run: false,
            recorded: RefCell::new(Vec::new()),
        }
    }

    pub fn dry_run(repo: impl Into<PathBuf>) -> Self {
        Self {
            dry_run: true,
            ..Self::new(repo)
        }
    }

    pub fn recorded(&self) -> Vec<Vec<String>> {
        self.recorded.borrow().clone()
    }

    pub fn run(&self, args: &[&str]) -> Result<GitOutput> {
        let out = self.execute(args, None)?;
        if out.executed && !out.success() {
            bail!(
                "git {} failed: {}",
                out.argv.join(" "),
                out.stderr_lossy().trim()
            );
        }
        Ok(out)
    }

    pub fn run_unchecked(&self, args: &[&str]) -> Result<GitOutput> {
        self.execute(args, None)
    }

    pub fn run_stdin(&self, args: &[&str], stdin: &[u8]) -> Result<GitOutput> {
        let out = self.execute(args, Some(stdin))?;
        if out.executed && !out.success() {
            bail!(
                "git {} failed: {}",
                out.argv.join(" "),
                out.stderr_lossy().trim()
            );
        }
        Ok(out)
    }

    pub fn run_stdin_unchecked(&self, args: &[&str], stdin: &[u8]) -> Result<GitOutput> {
        self.execute(args, Some(stdin))
    }

    /// A hung fetch or push must not wedge a worker thread, so the deadline kills it.
    pub fn run_timeout(&self, args: &[&str], timeout: Duration) -> Result<GitOutput> {
        self.execute_with(args, None, Some(timeout))
    }

    fn execute(&self, args: &[&str], stdin: Option<&[u8]>) -> Result<GitOutput> {
        self.execute_with(args, stdin, None)
    }

    fn execute_with(
        &self,
        args: &[&str],
        stdin: Option<&[u8]>,
        timeout: Option<Duration>,
    ) -> Result<GitOutput> {
        let argv: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
        self.recorded.borrow_mut().push(argv.clone());
        if self.dry_run && !is_read_only(args) {
            return Ok(GitOutput {
                argv,
                executed: false,
                status: None,
                stdout: Vec::new(),
                stderr: Vec::new(),
                timed_out: false,
            });
        }
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("LC_ALL", "C")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_EDITOR", "true")
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let mut child = command
            .spawn()
            .with_context(|| format!("run git {}", args.join(" ")))?;
        if let Some(bytes) = stdin {
            let mut pipe = child.stdin.take().context("open git stdin")?;
            pipe.write_all(bytes)
                .with_context(|| format!("write git {}", args.join(" ")))?;
        }
        let stdout = child.stdout.take().map(read_thread);
        let stderr = child.stderr.take().map(read_thread);
        let (status, timed_out) = wait_deadline(&mut child, timeout)
            .with_context(|| format!("run git {}", args.join(" ")))?;
        if timed_out {
            // A killed transport can leave a hook holding the pipe.
            return Ok(GitOutput {
                argv,
                executed: true,
                status,
                stdout: Vec::new(),
                stderr: Vec::new(),
                timed_out,
            });
        }
        Ok(GitOutput {
            argv,
            executed: true,
            status,
            stdout: join(stdout),
            stderr: join(stderr),
            timed_out,
        })
    }

    pub fn git_dir(&self) -> Result<PathBuf> {
        let out = self.run(&["rev-parse", "--path-format=absolute", "--git-dir"])?;
        Ok(PathBuf::from(out.stdout_lossy().trim()))
    }

    /// Destructive work locks the common dir, which every worktree of a repo shares.
    pub fn common_dir(&self) -> Result<PathBuf> {
        let out = self.run(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
        Ok(PathBuf::from(out.stdout_lossy().trim()))
    }

    pub fn repo_path(&self) -> &std::path::Path {
        &self.repo
    }
}

fn read_thread(mut reader: impl Read + Send + 'static) -> JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = reader.read_to_end(&mut buffer);
        buffer
    })
}

fn join(handle: Option<JoinHandle<Vec<u8>>>) -> Vec<u8> {
    handle.and_then(|h| h.join().ok()).unwrap_or_default()
}

#[cfg(unix)]
fn kill_tree(child: &mut Child) {
    // The child leads its own group, so a transport hook dies with it.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
}

#[cfg(not(unix))]
fn kill_tree(child: &mut Child) {
    let _ = child.kill();
}

fn wait_deadline(
    child: &mut Child,
    timeout: Option<Duration>,
) -> std::io::Result<(Option<ExitStatus>, bool)> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok((Some(status), false));
        }
        if timeout.is_some_and(|t| !t.is_zero() && start.elapsed() >= t) {
            kill_tree(child);
            let status = child.wait()?;
            return Ok((Some(status), true));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_read_only_subcommands_are_allowed() {
        for args in [
            &["status", "--porcelain=v2", "-z", "--branch"][..],
            &["diff", "--cached"],
            &["rev-parse", "HEAD"],
            &["log", "-5"],
            &["show", "HEAD"],
            &["ls-files"],
            &["merge-base", "main", "HEAD"],
            &["for-each-ref"],
            &["cat-file", "-t", "HEAD"],
            &["rev-list", "--count", "HEAD"],
            &["symbolic-ref", "HEAD"],
            &["symbolic-ref", "-q", "HEAD"],
            &["check-ignore", "a.txt"],
            &["var", "GIT_AUTHOR_IDENT"],
            &["config", "--get", "user.name"],
            &["config", "--get-all", "remote.origin.url"],
            &["config", "--list"],
            &["-c", "x=y", "status"],
            &["-C", "/tmp", "status"],
        ] {
            assert!(is_read_only(args), "rejected read-only {args:?}");
        }

        for args in [
            &["-c", "x=y", "commit", "-m", "x"][..],
            &["-C", "/tmp", "commit", "-m", "x"],
            &["fetch"],
            &["fetch", "--prune", "origin"],
            &["config", "user.name", "x"],
            &["push"],
            &["reset", "--hard"],
            &["gc", "--aggressive"],
            &["symbolic-ref", "HEAD", "refs/heads/x"],
            &["symbolic-ref", "-d", "HEAD"],
            &["symbolic-ref", "--delete", "HEAD"],
        ] {
            assert!(!is_read_only(args), "allowed mutating {args:?}");
        }
    }

    #[test]
    fn a_timeout_kills_the_child() {
        let mut child = Command::new("sh")
            .args(["-c", "sleep 30"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let start = std::time::Instant::now();
        let (status, timed_out) =
            wait_deadline(&mut child, Some(Duration::from_millis(100))).unwrap();
        assert!(timed_out);
        assert!(status.is_some());
        assert!(start.elapsed() < Duration::from_secs(5));
    }
}
