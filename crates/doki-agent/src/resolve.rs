use crate::adapter::{Prompt, adapter, custom_argv};
use anyhow::{Context, Result, bail};
use doki_core::config::{CommitScope, Config, Override};
use doki_core::repo_config::RepoConfig;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub id: String,
    pub cli: String,
    pub model: String,
    pub extra_args: Vec<String>,
    pub timeout: Duration,
    pub command: Option<Vec<String>>,
}

impl Resolved {
    pub fn label(&self) -> String {
        if self.model.is_empty() {
            format!("{} (cli default)", self.id)
        } else {
            format!("{} [{}]", self.id, self.model)
        }
    }

    pub fn argv(&self, tmp: &Path, prompt: &str) -> Result<(Vec<String>, Prompt, Option<PathBuf>)> {
        match &self.command {
            Some(command) => {
                let (argv, prompt_via) = custom_argv(command, &self.model, prompt)?;
                Ok((argv, prompt_via, None))
            }
            None => {
                let adapter = adapter(&self.id).with_context(|| {
                    format!(
                        "unknown agent `{}`; add [agents.{}] with command = [\"...\"]",
                        self.id, self.id
                    )
                })?;
                Ok(adapter.argv(&self.cli, &self.model, &self.extra_args, tmp, prompt))
            }
        }
    }
}

pub fn resolve_id(cfg: &Config, id: &str) -> Result<Resolved> {
    let entry = cfg
        .agent_entry(id)
        .with_context(|| format!("unknown agent `{id}`"))?;
    if entry.command.is_none() && adapter(id).is_none() {
        bail!("unknown agent `{id}`; add [agents.{id}] with command = [\"...\"]");
    }
    let cli = match entry.command.as_deref() {
        Some([first, ..]) => first.clone(),
        Some([]) => bail!("unknown agent `{id}` has an empty command"),
        None => entry.cli.unwrap_or_else(|| id.to_string()),
    };
    Ok(Resolved {
        id: id.to_string(),
        cli,
        model: entry.model,
        extra_args: entry.extra_args,
        timeout: Duration::from_secs(entry.timeout_seconds.unwrap_or(cfg.agent.timeout_seconds)),
        command: entry.command,
    })
}

/// Per-repo configuration after applying the tighten-only rules of `.doki.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effective {
    pub repo: PathBuf,
    pub disabled: bool,
    pub agent: Resolved,
    pub ignore: Vec<String>,
    pub commit_scope: CommitScope,
}

impl Effective {
    /// `.doki.toml` > `[[overrides]]` > `[agents.*]` > CLI default.
    pub fn resolve(cfg: &Config, repo: &Path) -> Result<Self> {
        let repo_cfg = RepoConfig::load(repo)?;
        let ov = override_for(cfg, repo);
        let id = repo_cfg
            .agent
            .clone()
            .or_else(|| ov.map(|o| o.agent.clone()))
            .or_else(|| cfg.agent.order.first().cloned())
            .context("no agent configured: [agent].order is empty")?;
        let mut agent = resolve_id(cfg, &id)?;
        let model = repo_cfg.model.clone().or_else(|| {
            ov.filter(|o| o.agent == id)
                .map(|o| o.model.clone())
                .filter(|m| !m.is_empty())
        });
        if let Some(model) = model {
            agent.model = model;
        }
        Ok(Self {
            repo: repo.to_path_buf(),
            disabled: repo_cfg.disabled(),
            commit_scope: repo_cfg.narrowed_scope(cfg.sync.commit_scope)?,
            ignore: repo_cfg.ignore,
            agent,
        })
    }

    /// The first choice plus the fallback chain from `[agent].order`.
    pub fn plan(&self, cfg: &Config) -> Vec<Resolved> {
        let mut plan = vec![self.agent.clone()];
        let max = cfg.agent.max_attempts.max(1) as usize;
        if cfg.agent.fallback_on_error {
            for id in &cfg.agent.order {
                if plan.len() >= max {
                    break;
                }
                if plan.iter().any(|r| &r.id == id) {
                    continue;
                }
                if let Ok(next) = resolve_id(cfg, id) {
                    plan.push(next);
                }
            }
        }
        plan.truncate(max);
        plan
    }
}

pub fn override_for<'a>(cfg: &'a Config, repo: &Path) -> Option<&'a Override> {
    cfg.overrides.iter().find(|o| same_path(&o.path, repo))
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn repo_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn resolution_order_is_dot_doki_then_override_then_entry() {
        let tmp = repo_dir();
        let repo = tmp.path();
        let cfg = Config::parse(&format!(
            "[agent]\norder = [\"omp\"]\n\n[agents.codex]\nmodel = \"gpt-5.2\"\n\n\
             [agents.claude]\nmodel = \"claude-sonnet-4-5\"\n\n\
             [[overrides]]\npath = \"{}\"\nagent = \"codex\"\nmodel = \"o3\"\n",
            repo.display()
        ))
        .unwrap();

        let eff = Effective::resolve(&cfg, repo).unwrap();
        assert_eq!(eff.agent.id, "codex");
        assert_eq!(eff.agent.model, "o3");

        fs::write(repo.join(".doki.toml"), "agent = \"claude\"\n").unwrap();
        let eff = Effective::resolve(&cfg, repo).unwrap();
        assert_eq!(eff.agent.id, "claude");
        assert_eq!(eff.agent.model, "claude-sonnet-4-5");

        fs::write(repo.join(".doki.toml"), "agent = \"pi\"\nmodel = \"x/y\"\n").unwrap();
        let eff = Effective::resolve(&cfg, repo).unwrap();
        assert_eq!(eff.agent.id, "pi");
        assert_eq!(eff.agent.model, "x/y");
    }

    #[test]
    fn empty_model_omits_the_flag() {
        let tmp = repo_dir();
        let cfg = Config::parse("[agent]\norder = [\"claude\"]\n").unwrap();
        let eff = Effective::resolve(&cfg, tmp.path()).unwrap();
        assert_eq!(eff.agent.model, "");
        let (argv, _, _) = eff.agent.argv(Path::new("/tmp/t"), "prompt").unwrap();
        assert_eq!(argv, ["claude", "-p"]);
    }

    #[test]
    fn repo_file_cannot_widen_scope_or_choose_a_binary() {
        let tmp = repo_dir();
        let cfg = Config::parse("").unwrap();
        fs::write(tmp.path().join(".doki.toml"), "commit_scope = \"all\"\n").unwrap();
        assert!(Effective::resolve(&cfg, tmp.path()).is_err());

        fs::write(
            tmp.path().join(".doki.toml"),
            "command = [\"curl\", \"evil\"]\n",
        )
        .unwrap();
        let err = format!("{:#}", Effective::resolve(&cfg, tmp.path()).unwrap_err());
        assert!(err.contains("command"), "{err}");

        fs::write(tmp.path().join(".doki.toml"), "commit_scope = \"staged\"\n").unwrap();
        let eff = Effective::resolve(&cfg, tmp.path()).unwrap();
        assert_eq!(eff.commit_scope, CommitScope::Staged);
        assert!(!eff.disabled);
    }

    #[test]
    fn fallback_plan_respects_max_attempts_and_switch() {
        let tmp = repo_dir();
        let cfg =
            Config::parse("[agent]\norder = [\"omp\", \"codex\", \"claude\"]\nmax_attempts = 2\n")
                .unwrap();
        let eff = Effective::resolve(&cfg, tmp.path()).unwrap();
        let plan = eff.plan(&cfg);
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].id, "omp");
        assert_eq!(plan[1].id, "codex");

        let off = Config::parse(
            "[agent]\norder = [\"omp\", \"codex\"]\nfallback_on_error = false\nmax_attempts = 5\n",
        )
        .unwrap();
        let plan = Effective::resolve(&off, tmp.path()).unwrap().plan(&off);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].id, "omp");
    }

    #[test]
    fn unknown_agent_ids_need_a_command() {
        assert!(Config::parse("[agents.mine]\nmodel = \"m\"\n").is_err());

        let cfg = Config::parse(
            "[agents.mine]\ncommand = [\"mine-cli\", \"--model\", \"{model}\", \"{prompt}\"]\n",
        )
        .unwrap();
        let agent = resolve_id(&cfg, "mine").unwrap();
        assert_eq!(agent.cli, "mine-cli");
        let (argv, via, last) = agent.argv(Path::new("/tmp/t"), "hello").unwrap();
        assert_eq!(argv, ["mine-cli", "--model", "hello"]);
        assert_eq!(via, Prompt::Argv);
        assert!(last.is_none());
    }
}
