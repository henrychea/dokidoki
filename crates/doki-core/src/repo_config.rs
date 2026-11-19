use crate::config::{CommitScope, valid_model};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::Path;

pub const FILE: &str = ".doki.toml";

/// A cloned repo must never be able to choose a binary or widen policy, so this
/// file is parsed into a type that only carries tightening knobs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoConfig {
    pub enabled: Option<bool>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub ignore: Vec<String>,
    pub commit_scope: Option<CommitScope>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    enabled: Option<bool>,
    agent: Option<String>,
    model: Option<String>,
    ignore: Option<Vec<String>>,
    commit_scope: Option<CommitScope>,
    command: Option<toml::Value>,
    cli: Option<toml::Value>,
    extra_args: Option<toml::Value>,
    push: Option<toml::Value>,
    push_mode: Option<toml::Value>,
    guards: Option<toml::Value>,
}

impl RepoConfig {
    pub fn parse(text: &str) -> Result<Self> {
        let raw: Raw = toml::from_str(text)?;
        for (key, present) in [
            ("command", raw.command.is_some()),
            ("cli", raw.cli.is_some()),
            ("extra_args", raw.extra_args.is_some()),
            ("push", raw.push.is_some()),
            ("push_mode", raw.push_mode.is_some()),
            ("guards", raw.guards.is_some()),
        ] {
            if present {
                bail!("{FILE} may not set `{key}`: a repo file can only tighten policy");
            }
        }
        if raw.enabled == Some(true) {
            bail!("{FILE} may only disable sync with `enabled = false`");
        }
        if raw.agent.as_ref().is_some_and(|a| a.trim().is_empty()) {
            bail!("{FILE} agent must not be empty");
        }
        if let Some(model) = &raw.model
            && !valid_model(model)
        {
            bail!("{FILE} model `{model}` must match ^[A-Za-z0-9._:/@+-]+$");
        }
        let ignore = raw.ignore.unwrap_or_default();
        for glob in &ignore {
            globset::Glob::new(glob).with_context(|| format!("{FILE} ignore glob `{glob}`"))?;
        }
        Ok(Self {
            enabled: raw.enabled,
            agent: raw.agent,
            model: raw.model,
            ignore,
            commit_scope: raw.commit_scope,
        })
    }

    pub fn load(repo: &Path) -> Result<Self> {
        let path = repo.join(FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).with_context(|| format!("invalid {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }

    pub fn disabled(&self) -> bool {
        self.enabled == Some(false)
    }

    pub fn narrowed_scope(&self, base: CommitScope) -> Result<CommitScope> {
        match self.commit_scope {
            Some(scope) if !scope.narrows(base) => {
                bail!("{FILE} commit_scope may only narrow `{base:?}`, not widen it")
            }
            Some(scope) => Ok(scope),
            None => Ok(base),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_tightening_knobs() {
        let cfg = RepoConfig::parse(
            "enabled = false\nagent = \"codex\"\nmodel = \"gpt-5.2\"\n\
             ignore = [\"fixtures/**\"]\ncommit_scope = \"staged\"\n",
        )
        .unwrap();
        assert!(cfg.disabled());
        assert_eq!(cfg.agent.as_deref(), Some("codex"));
        assert_eq!(cfg.model.as_deref(), Some("gpt-5.2"));
        assert_eq!(cfg.ignore, ["fixtures/**"]);
        assert_eq!(cfg.commit_scope, Some(CommitScope::Staged));
    }

    #[test]
    fn rejects_executable_or_widening_keys() {
        for text in [
            "command = [\"curl\", \"evil\"]\n",
            "cli = \"evil\"\n",
            "extra_args = [\"--yolo\"]\n",
            "push = true\n",
            "push_mode = \"push\"\n",
            "[guards]\nmax_changed_files = 100000\n",
            "allow_protected_push = true\n",
            "push_protected = true\n",
        ] {
            let err = format!("{:#}", RepoConfig::parse(text).unwrap_err());
            assert!(
                err.contains("may not set") || err.contains("unknown field"),
                "unexpected error for {text:?}: {err}"
            );
        }
        assert!(RepoConfig::parse("enabled = true\n").is_err());
        assert!(RepoConfig::parse("model = \"bad model\"\n").is_err());
        assert!(RepoConfig::parse("ignore = [\"[\"]\n").is_err());
        assert!(RepoConfig::parse("nonsense = 1\n").is_err());
    }

    #[test]
    fn commit_scope_only_narrows() {
        let tracked = CommitScope::Tracked;
        let staged = RepoConfig::parse("commit_scope = \"staged\"\n").unwrap();
        assert_eq!(staged.narrowed_scope(tracked).unwrap(), CommitScope::Staged);
        let all = RepoConfig::parse("commit_scope = \"all\"\n").unwrap();
        assert!(all.narrowed_scope(tracked).is_err());
        assert_eq!(
            RepoConfig::default().narrowed_scope(tracked).unwrap(),
            tracked
        );
    }

    #[test]
    fn missing_file_is_default() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(RepoConfig::load(tmp.path()).unwrap(), RepoConfig::default());
        std::fs::write(tmp.path().join(FILE), "agent = \"pi\"\n").unwrap();
        assert_eq!(
            RepoConfig::load(tmp.path()).unwrap().agent.as_deref(),
            Some("pi")
        );
    }
}
