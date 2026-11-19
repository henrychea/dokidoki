use crate::adapter::Prompt;
use crate::resolve::Resolved;
use anyhow::{Context, Result, bail};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct AgentRun {
    pub argv: Vec<String>,
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
    pub last_message: Option<String>,
    pub latency: Duration,
    pub cwd: PathBuf,
}

impl AgentRun {
    pub fn output(&self) -> &str {
        self.last_message.as_deref().unwrap_or(&self.stdout)
    }

    pub fn stderr_tail(&self) -> String {
        self.stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .map(|l| l.trim().chars().take(200).collect())
            .unwrap_or_default()
    }
}

pub fn run(agent: &Resolved, prompt: &str) -> Result<AgentRun> {
    run_with_env(agent, prompt, &[])
}

/// The child runs in a fresh temp dir, never the repo, so an agent cannot edit
/// the working tree it is describing.
pub fn run_with_env(agent: &Resolved, prompt: &str, env: &[(&str, &str)]) -> Result<AgentRun> {
    let dir = tempfile::Builder::new()
        .prefix("doki-agent-")
        .tempdir()
        .context("create agent temp dir")?;
    let tmp = dir.path().to_path_buf();
    std::fs::write(tmp.join("prompt.txt"), prompt).context("write prompt.txt")?;
    let (argv, prompt_via, last_message_file) = agent.argv(&tmp, prompt)?;

    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(&tmp)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let prompt_on_stdin = prompt_via == Prompt::Stdin;
    command.stdin(if prompt_on_stdin {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    for (key, value) in env {
        command.env(key, value);
    }

    let start = Instant::now();
    let mut child = command
        .spawn()
        .with_context(|| format!("spawn {}", argv[0]))?;
    if prompt_on_stdin {
        let mut stdin = child.stdin.take().context("open agent stdin")?;
        let prompt = prompt.as_bytes().to_vec();
        // A thread keeps a prompt larger than the pipe buffer from blocking the child.
        std::thread::spawn(move || {
            let _ = stdin.write_all(&prompt);
        });
    }
    let stdout = child.stdout.take().map(read_thread);
    let stderr = child.stderr.take().map(read_thread);

    let deadline = start + agent.timeout;
    let status = loop {
        if let Some(status) = child.try_wait().context("wait for agent")? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "agent `{}` timed out after {}s",
                agent.id,
                agent.timeout.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let latency = start.elapsed();

    let last_message = last_message_file
        .and_then(|path| std::fs::read_to_string(path).ok())
        .filter(|text| !text.trim().is_empty());
    Ok(AgentRun {
        argv,
        status,
        stdout: String::from_utf8_lossy(&join(stdout)).into_owned(),
        stderr: String::from_utf8_lossy(&join(stderr)).into_owned(),
        last_message,
        latency,
        cwd: tmp,
    })
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
