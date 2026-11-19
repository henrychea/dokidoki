use crate::parse::{self, Message};
use crate::prompt::{self, PromptContext};
use crate::resolve::Effective;
use crate::run::{self, AgentRun};
use doki_core::config::Config;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub agent: String,
    pub model: String,
    pub latency: Duration,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generation {
    pub message: Message,
    pub agent: Option<String>,
    pub model: String,
    pub attempts: Vec<Attempt>,
}

impl Generation {
    pub fn fallback(&self) -> bool {
        self.agent.is_none()
    }
}

/// Tries the resolved agent, then the fallback chain, and always returns a
/// usable message: a total failure yields the deterministic fallback subject.
pub fn generate(cfg: &Config, effective: &Effective, ctx: &PromptContext) -> Generation {
    generate_with_env(cfg, effective, ctx, &[])
}

pub fn generate_with_env(
    cfg: &Config,
    effective: &Effective,
    ctx: &PromptContext,
    env: &[(&str, &str)],
) -> Generation {
    let prompt = prompt::build(ctx, cfg.guards.max_diff_bytes_for_model);
    let mut attempts = Vec::new();
    for agent in effective.plan(cfg) {
        let outcome = run::run_with_env(&agent, &prompt, env);
        match outcome {
            Ok(run) if run.status.success() => match parse::parse(run.output()) {
                Some(message) => {
                    attempts.push(attempt(&agent, &run, None));
                    return Generation {
                        message,
                        agent: Some(agent.id),
                        model: agent.model,
                        attempts,
                    };
                }
                None => attempts.push(attempt(
                    &agent,
                    &run,
                    Some("no usable message in output".into()),
                )),
            },
            Ok(run) => {
                let code = run
                    .status
                    .code()
                    .map_or_else(|| run.status.to_string(), |c| c.to_string());
                let error = format!("exit {code}: {}", run.stderr_tail());
                attempts.push(attempt(&agent, &run, Some(error.trim_end().into())));
            }
            Err(e) => attempts.push(Attempt {
                agent: agent.id,
                model: agent.model,
                latency: Duration::ZERO,
                error: Some(format!("{e:#}")),
            }),
        }
    }
    Generation {
        message: parse::fallback(&ctx.files),
        agent: None,
        model: String::new(),
        attempts,
    }
}

fn attempt(agent: &crate::resolve::Resolved, run: &AgentRun, error: Option<String>) -> Attempt {
    Attempt {
        agent: agent.id.clone(),
        model: agent.model.clone(),
        latency: run.latency,
        error,
    }
}
