use crate::generate;
use crate::prompt::{self, PromptContext};
use crate::resolve::Effective;
use anyhow::Result;
use doki_core::config::Config;
use doki_core::sync::{ChangedFile, CommitMessage, MessageContext, Messenger, Proposal};
use std::path::Path;

pub struct AgentMessenger {
    cfg: Config,
    resolved: Effective,
}

impl AgentMessenger {
    pub fn new(cfg: &Config, repo: &Path) -> Result<Self> {
        Ok(Self {
            cfg: cfg.clone(),
            resolved: Effective::resolve(cfg, repo)?,
        })
    }

    pub fn resolved(&self) -> &Effective {
        &self.resolved
    }
}

impl Messenger for AgentMessenger {
    fn agent(&self) -> &str {
        &self.resolved.agent.id
    }

    fn model(&self) -> &str {
        &self.resolved.agent.model
    }

    fn preview(&self, ctx: &MessageContext) -> Vec<String> {
        let prompt_ctx = prompt_context(ctx);
        let text = prompt::build(&prompt_ctx, self.cfg.guards.max_diff_bytes_for_model);
        let tmp = std::env::temp_dir().join("doki-agent-preview");
        self.resolved
            .agent
            .argv(&tmp, &text)
            .map(|(argv, _, _)| argv)
            .unwrap_or_default()
    }

    fn propose(&self, ctx: &MessageContext) -> Proposal {
        let prompt_ctx = prompt_context(ctx);
        let generation = generate::generate(&self.cfg, &self.resolved, &prompt_ctx);
        let warning = if generation.fallback() {
            generation
                .attempts
                .iter()
                .rev()
                .find_map(|attempt| attempt.error.clone())
                .map(|error| format!("agent fallback: {error}"))
        } else {
            None
        };
        Proposal {
            message: CommitMessage {
                subject: generation.message.subject,
                body: generation.message.body,
            },
            agent: generation.agent,
            model: generation.model,
            warning,
        }
    }
}

fn prompt_context(ctx: &MessageContext) -> PromptContext {
    PromptContext {
        repo_name: ctx.repo_name.clone(),
        branch: ctx.branch.clone(),
        recent_subjects: ctx.recent_subjects.clone(),
        files: ctx
            .files
            .iter()
            .map(|file: &ChangedFile| prompt::FileStat {
                path: file.path.clone(),
                added: file.added,
                deleted: file.deleted,
            })
            .collect(),
        diff: ctx.diff.clone(),
    }
}
