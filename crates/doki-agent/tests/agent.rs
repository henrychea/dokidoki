use doki_agent::adapter::Prompt;
use doki_agent::generate::generate_with_env;
use doki_agent::prompt::{FileStat, PromptContext};
use doki_agent::resolve::{Effective, resolve_id};
use doki_agent::run::run_with_env;
use doki_core::config::Config;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tempfile::TempDir;

const SHIM: &str = r#"#!/bin/sh
{
  printf 'argv:'
  for a in "$@"; do printf ' %s' "$a"; done
  printf '\n'
  printf 'cwd:%s\n' "$PWD"
  printf 'prompt-file:%s\n' "$([ -f prompt.txt ] && echo yes || echo no)"
} >> "$DOKI_TEST_LOG"
prompt=$(cat)
printf 'stdin:%s\n' "$prompt" >> "$DOKI_TEST_LOG"
"#;

struct Shims {
    dir: TempDir,
    log: PathBuf,
}

impl Shims {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("calls.log");
        Self { dir, log }
    }

    fn add(&self, name: &str, output: &str) {
        let path = self.dir.path().join(name);
        fs::write(&path, format!("{SHIM}{output}\n")).unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
    }

    fn env(&self) -> Vec<(String, String)> {
        let path = format!(
            "{}:{}",
            self.dir.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        vec![
            ("PATH".to_string(), path),
            ("DOKI_TEST_LOG".to_string(), self.log.display().to_string()),
        ]
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn field(&self, key: &str) -> Option<String> {
        self.calls()
            .iter()
            .find_map(|line| line.strip_prefix(&format!("{key}:")).map(str::to_string))
    }
}

fn env_refs(env: &[(String, String)]) -> Vec<(&str, &str)> {
    env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

fn context() -> PromptContext {
    PromptContext {
        repo_name: "billing".into(),
        branch: "main".into(),
        recent_subjects: vec!["add invoice table".into()],
        files: vec![
            FileStat {
                path: "src/a.rs".into(),
                added: Some(3),
                deleted: Some(1),
            },
            FileStat {
                path: "docs/b.md".into(),
                added: Some(2),
                deleted: None,
            },
        ],
        diff: "diff --git a/src/a.rs b/src/a.rs\n+let x = 1;\n".into(),
    }
}

fn argv_of(id: &str, model: &str) -> Vec<String> {
    let cfg = Config::parse(&format!(
        "[agent]\norder = [\"{id}\"]\n\n[agents.{id}]\nmodel = \"{model}\"\n"
    ))
    .unwrap();
    resolve_id(&cfg, id)
        .unwrap()
        .argv(Path::new("/tmp/doki-agent-argv"), "PROMPT")
        .unwrap()
        .0
}

#[test]
fn argv_matches_each_cli_with_and_without_model() {
    const TMP: &str = "/tmp/doki-agent-argv";
    const MSG: &str = "/tmp/doki-agent-argv/last-message.txt";

    assert_eq!(
        argv_of("omp", ""),
        ["omp", "-p", "--mode=text", "--cwd", TMP, "PROMPT"]
    );
    assert_eq!(
        argv_of("omp", "anthropic/claude-sonnet-4.5"),
        [
            "omp",
            "-p",
            "--mode=text",
            "--cwd",
            TMP,
            "--model",
            "anthropic/claude-sonnet-4.5",
            "PROMPT"
        ]
    );

    assert_eq!(
        argv_of("codex", ""),
        [
            "codex",
            "exec",
            "--skip-git-repo-check",
            "-s",
            "read-only",
            "--ephemeral",
            "-o",
            MSG,
            "-"
        ]
    );
    assert_eq!(
        argv_of("codex", "gpt-5.2-codex"),
        [
            "codex",
            "exec",
            "--skip-git-repo-check",
            "-s",
            "read-only",
            "--ephemeral",
            "-m",
            "gpt-5.2-codex",
            "-o",
            MSG,
            "-"
        ]
    );

    assert_eq!(argv_of("claude", ""), ["claude", "-p"]);
    assert_eq!(
        argv_of("claude", "claude-sonnet-4-5"),
        ["claude", "-p", "--model", "claude-sonnet-4-5"]
    );

    assert_eq!(argv_of("opencode", ""), ["opencode", "run", "PROMPT"]);
    assert_eq!(
        argv_of("opencode", "openai/gpt-5.2"),
        ["opencode", "run", "-m", "openai/gpt-5.2", "PROMPT"]
    );

    assert_eq!(argv_of("pi", ""), ["pi", "-p", "PROMPT"]);
    assert_eq!(
        argv_of("pi", "anthropic/claude-sonnet-4.5"),
        [
            "pi",
            "-p",
            "--model",
            "anthropic/claude-sonnet-4.5",
            "PROMPT"
        ]
    );
}

#[test]
fn prompt_delivery_matches_each_cli() {
    for (id, expected) in [
        ("omp", Prompt::Argv),
        ("codex", Prompt::Stdin),
        ("claude", Prompt::Stdin),
        ("opencode", Prompt::Argv),
        ("pi", Prompt::Argv),
    ] {
        let cfg = Config::parse(&format!("[agent]\norder = [\"{id}\"]\n")).unwrap();
        let agent = resolve_id(&cfg, id).unwrap();
        let (_, via, last) = agent.argv(Path::new("/tmp/t"), "PROMPT").unwrap();
        assert_eq!(via, expected, "{id}");
        assert_eq!(last.is_some(), id == "codex", "{id}");
    }
}

#[test]
fn extra_args_follow_the_model_flag() {
    let cfg = Config::parse(
        "[agents.omp]\nmodel = \"m\"\nextra_args = [\"--no-session\", \"--no-tools\"]\n",
    )
    .unwrap();
    let (argv, _, _) = resolve_id(&cfg, "omp")
        .unwrap()
        .argv(Path::new("/tmp/t"), "PROMPT")
        .unwrap();
    assert_eq!(
        argv,
        [
            "omp",
            "-p",
            "--mode=text",
            "--cwd",
            "/tmp/t",
            "--model",
            "m",
            "--no-session",
            "--no-tools",
            "PROMPT"
        ]
    );

    let cfg = Config::parse("[agents.codex]\nextra_args = [\"--json\"]\n").unwrap();
    let (argv, _, _) = resolve_id(&cfg, "codex")
        .unwrap()
        .argv(Path::new("/tmp/t"), "PROMPT")
        .unwrap();
    assert_eq!(
        argv,
        [
            "codex",
            "exec",
            "--skip-git-repo-check",
            "-s",
            "read-only",
            "--ephemeral",
            "-o",
            "/tmp/t/last-message.txt",
            "--json",
            "-"
        ]
    );
}

#[test]
fn runs_in_a_temp_dir_never_the_repo() {
    let shims = Shims::new();
    shims.add("omp-shim", "printf '%s\\n' 'doki: add widget'");
    let repo = tempfile::tempdir().unwrap();
    fs::write(repo.path().join("a.txt"), "keep\n").unwrap();

    let cfg = Config::parse(
        "[agent]\norder = [\"omp\"]\n[agents.omp]\ncli = \"omp-shim\"\nmodel = \"m\"\n",
    )
    .unwrap();
    let agent = resolve_id(&cfg, "omp").unwrap();
    let env = shims.env();
    let run = run_with_env(&agent, "PROMPT", &env_refs(&env)).unwrap();

    assert!(run.status.success());
    let cwd = PathBuf::from(shims.field("cwd").unwrap());
    assert_ne!(cwd, repo.path());
    assert!(cwd.starts_with(std::env::temp_dir()), "{cwd:?}");
    assert_eq!(shims.field("prompt-file").as_deref(), Some("yes"));
    assert_eq!(
        shims.field("argv").unwrap(),
        format!(" -p --mode=text --cwd {} --model m PROMPT", cwd.display())
    );
    assert_eq!(
        fs::read_to_string(repo.path().join("a.txt")).unwrap(),
        "keep\n"
    );
    assert_eq!(fs::read_dir(repo.path()).unwrap().count(), 1);
}

#[test]
fn stdin_cli_receives_the_prompt() {
    let shims = Shims::new();
    shims.add("claude-shim", "printf '%s\\n' 'doki: add widget'");
    let cfg =
        Config::parse("[agent]\norder = [\"claude\"]\n[agents.claude]\ncli = \"claude-shim\"\n")
            .unwrap();
    let env = shims.env();
    let run = run_with_env(
        &resolve_id(&cfg, "claude").unwrap(),
        "PROMPT",
        &env_refs(&env),
    )
    .unwrap();
    assert!(run.status.success());
    assert_eq!(shims.field("stdin").as_deref(), Some("PROMPT"));
    assert_eq!(shims.field("argv").unwrap(), " -p".to_string());
    assert_eq!(run.output().trim(), "doki: add widget");
}

#[test]
fn repo_override_and_dot_doki_win_end_to_end() {
    let shims = Shims::new();
    shims.add("omp-shim", "printf '%s\\n' 'doki: from omp'");
    shims.add("codex-shim", "printf '%s\\n' 'doki: from codex'");
    let repo = tempfile::tempdir().unwrap();
    let cfg = Config::parse(&format!(
        "[agent]\norder = [\"omp\", \"codex\"]\nmax_attempts = 2\n\n\
         [agents.omp]\ncli = \"omp-shim\"\nmodel = \"omp-default\"\n\n\
         [agents.codex]\ncli = \"codex-shim\"\nmodel = \"codex-default\"\n\n\
         [[overrides]]\npath = \"{}\"\nagent = \"codex\"\nmodel = \"override-model\"\n",
        repo.path().display()
    ))
    .unwrap();
    let env = shims.env();

    let effective = Effective::resolve(&cfg, repo.path()).unwrap();
    assert_eq!(effective.agent.id, "codex");
    assert_eq!(effective.agent.model, "override-model");
    let generation = generate_with_env(&cfg, &effective, &context(), &env_refs(&env));
    assert_eq!(generation.agent.as_deref(), Some("codex"));
    assert_eq!(generation.message.subject, "doki: from codex");
    let log = fs::read_to_string(&shims.log).unwrap();
    assert!(log.contains("-m override-model"), "{log}");
    assert!(!log.contains("omp-shim"), "{log}");

    fs::write(
        repo.path().join(".doki.toml"),
        "agent = \"omp\"\nmodel = \"repo-model\"\n",
    )
    .unwrap();
    let effective = Effective::resolve(&cfg, repo.path()).unwrap();
    assert_eq!(effective.agent.id, "omp");
    assert_eq!(effective.agent.model, "repo-model");
    let generation = generate_with_env(&cfg, &effective, &context(), &env_refs(&env));
    assert_eq!(generation.agent.as_deref(), Some("omp"));
    assert_eq!(generation.message.subject, "doki: from omp");
    let log = fs::read_to_string(&shims.log).unwrap();
    assert!(log.contains("--model repo-model"), "{log}");
}

#[test]
fn missing_cli_falls_back_to_the_next_agent() {
    let shims = Shims::new();
    shims.add("codex-shim", "printf '%s\\n' 'doki: add widget'");
    let repo = tempfile::tempdir().unwrap();
    let cfg = Config::parse(
        "[agent]\norder = [\"omp\", \"codex\"]\nmax_attempts = 2\n\n\
         [agents.omp]\ncli = \"doki-missing-binary\"\n\n\
         [agents.codex]\ncli = \"codex-shim\"\n",
    )
    .unwrap();
    let effective = Effective::resolve(&cfg, repo.path()).unwrap();
    let env = shims.env();
    let generation = generate_with_env(&cfg, &effective, &context(), &env_refs(&env));

    assert_eq!(generation.agent.as_deref(), Some("codex"));
    assert_eq!(generation.message.subject, "doki: add widget");
    assert_eq!(generation.attempts.len(), 2);
    assert!(
        generation.attempts[0]
            .error
            .as_deref()
            .unwrap()
            .contains("spawn"),
        "{:?}",
        generation.attempts
    );
    assert!(generation.attempts[1].error.is_none());
}

#[test]
fn timeout_kills_the_child_and_falls_back() {
    let shims = Shims::new();
    shims.add("slow-shim", "sleep 5; printf '%s\\n' 'doki: too late'");
    shims.add("codex-shim", "printf '%s\\n' 'doki: add widget'");
    let repo = tempfile::tempdir().unwrap();
    let cfg = Config::parse(
        "[agent]\norder = [\"omp\", \"codex\"]\nmax_attempts = 2\n\n\
         [agents.omp]\ncli = \"slow-shim\"\ntimeout_seconds = 1\n\n\
         [agents.codex]\ncli = \"codex-shim\"\n",
    )
    .unwrap();
    let effective = Effective::resolve(&cfg, repo.path()).unwrap();
    let env = shims.env();
    let start = Instant::now();
    let generation = generate_with_env(&cfg, &effective, &context(), &env_refs(&env));
    let elapsed = start.elapsed();

    assert_eq!(generation.agent.as_deref(), Some("codex"));
    assert!(
        generation.attempts[0]
            .error
            .as_deref()
            .unwrap()
            .contains("timed out"),
        "{:?}",
        generation.attempts
    );
    assert!(elapsed.as_secs() < 4, "timeout did not kill the child");
}

#[test]
fn garbage_output_falls_back_to_the_deterministic_message() {
    let long = "a".repeat(500);
    let cases = [
        ("empty", String::new(), true),
        ("fences", "printf '```\\n```\\n'".to_string(), true),
        ("500-char line", format!("printf '%s\\n' '{long}'"), false),
    ];
    for (label, output, falls_back) in cases {
        let shims = Shims::new();
        shims.add("omp-shim", &output);
        let repo = tempfile::tempdir().unwrap();
        let cfg = Config::parse(
            "[agent]\norder = [\"omp\"]\nfallback_on_error = false\nmax_attempts = 1\n\n\
             [agents.omp]\ncli = \"omp-shim\"\n",
        )
        .unwrap();
        let effective = Effective::resolve(&cfg, repo.path()).unwrap();
        let env = shims.env();
        let generation = generate_with_env(&cfg, &effective, &context(), &env_refs(&env));

        if falls_back {
            assert!(generation.fallback(), "{label}");
            assert_eq!(
                generation.message.subject, "doki: update 2 files in docs, src",
                "{label}"
            );
        } else {
            assert!(!generation.fallback(), "{label}");
            assert_eq!(generation.message.subject.chars().count(), 72, "{label}");
            assert!(
                generation.message.subject.starts_with("doki: aaa"),
                "{label}"
            );
        }
    }
}

#[test]
fn generation_is_fallback_when_no_agent_can_answer() {
    let shims = Shims::new();
    shims.add(
        "omp-shim",
        "printf '%s\\n' 'I cannot help with that.'; exit 3",
    );
    let repo = tempfile::tempdir().unwrap();
    let cfg = Config::parse(
        "[agent]\norder = [\"omp\"]\nfallback_on_error = false\nmax_attempts = 1\n\n\
         [agents.omp]\ncli = \"omp-shim\"\n",
    )
    .unwrap();
    let effective = Effective::resolve(&cfg, repo.path()).unwrap();
    let env = shims.env();
    let generation = generate_with_env(&cfg, &effective, &context(), &env_refs(&env));

    assert!(generation.fallback());
    assert_eq!(
        generation.message.subject,
        "doki: update 2 files in docs, src"
    );
    assert!(
        generation.attempts[0]
            .error
            .as_deref()
            .unwrap()
            .contains("exit 3")
    );
}
