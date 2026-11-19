use anyhow::{Context, Result, bail};
use doki_agent::adapter::on_path;
use doki_agent::parse;
use doki_agent::resolve::resolve_id;
use doki_agent::run::run;
use doki_core::config::Config;
use std::io::{self, Write};

const TEST_PROMPT: &str = "Reply with exactly one line and nothing else: doki: agent test";

pub fn list(cfg: &Config) -> Result<()> {
    write_out(&list_text(cfg))
}

fn list_text(cfg: &Config) -> String {
    let mut out = format!("{:<10} {:<12} {:<8} {}\n", "id", "cli", "path", "model");
    for id in cfg.agent_ids() {
        let Ok(agent) = resolve_id(cfg, &id) else {
            continue;
        };
        let found = if on_path(&agent.cli) {
            "found"
        } else {
            "missing"
        };
        let model = if agent.model.is_empty() {
            "(cli default)".to_string()
        } else {
            agent.model.clone()
        };
        out.push_str(&format!(
            "{:<10} {:<12} {:<8} {}\n",
            agent.id, agent.cli, found, model
        ));
    }
    out
}

pub fn test(cfg: &Config, id: Option<String>) -> Result<()> {
    let id = id
        .or_else(|| cfg.agent.order.first().cloned())
        .context("no agent configured: [agent].order is empty")?;
    let agent = resolve_id(cfg, &id)?;
    if !on_path(&agent.cli) {
        bail!("`{}` is not on PATH", agent.cli);
    }
    let result = run(&agent, TEST_PROMPT)?;
    let mut out = String::new();
    out.push_str(&format!("agent:   {}\n", agent.label()));
    out.push_str(&format!("argv:    {}\n", result.argv.join(" ")));
    out.push_str(&format!("latency: {:.2}s\n", result.latency.as_secs_f64()));
    out.push_str(&format!("status:  {}\n", result.status));
    if !result.status.success() {
        write_out(&out)?;
        bail!(
            "`{}` exited {}: {}",
            agent.cli,
            result.status,
            result.stderr_tail()
        );
    }
    match parse::parse(result.output()) {
        Some(message) => out.push_str(&format!("message: {}\n", message.subject)),
        None => {
            write_out(&out)?;
            bail!(
                "no usable message in output: {}",
                result.output().chars().take(200).collect::<String>()
            );
        }
    }
    write_out(&out)
}

fn write_out(text: &str) -> Result<()> {
    let mut stdout = io::stdout().lock();
    match stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        other => Ok(other?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn list_shows_cli_path_and_model() {
        let cfg = Config::parse(
            "[agent]\norder = [\"omp\"]\n\n[agents.omp]\ncli = \"/bin/sh\"\nmodel = \"m/n\"\n\n\
             [agents.mine]\ncommand = [\"/bin/sh\", \"{prompt}\"]\n",
        )
        .unwrap();
        let text = list_text(&cfg);
        let row = |id: &str| {
            text.lines()
                .find(|line| line.starts_with(&format!("{id} ")))
                .unwrap_or_else(|| panic!("no row for {id}: {text}"))
                .to_string()
        };
        assert!(row("omp").contains("/bin/sh"), "{text}");
        assert!(row("omp").contains("found"), "{text}");
        assert!(row("omp").contains("m/n"), "{text}");
        assert!(row("mine").contains("/bin/sh"), "{text}");
        assert!(row("mine").contains("(cli default)"), "{text}");
        assert!(row("pi").contains("(cli default)"), "{text}");
    }

    #[test]
    fn test_reports_a_missing_binary() {
        let cfg = Config::parse("[agents.omp]\ncli = \"doki-no-such-binary-xyz\"\n").unwrap();
        assert!(test(&cfg, Some("omp".into())).is_err());
    }

    #[test]
    fn test_runs_a_shim_and_parses_the_message() {
        let tmp = tempfile::tempdir().unwrap();
        let shim = tmp.path().join("claude-shim");
        fs::write(
            &shim,
            "#!/bin/sh\ncat >/dev/null\nprintf 'doki: agent test\\n'\n",
        )
        .unwrap();
        let mut perms = fs::metadata(&shim).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&shim, perms).unwrap();

        let cfg =
            Config::parse(&format!("[agents.claude]\ncli = \"{}\"\n", shim.display())).unwrap();
        assert!(test(&cfg, Some("claude".into())).is_ok());
    }
}
