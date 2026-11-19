use crate::studio::AgentInfo;
use doki_core::config::{CommitScope, Config};
use doki_core::status::{RepoState as LiveState, RepoStatus};
use doki_core::store::{RepoRow, RepoState as StoredState};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowState {
    Clean,
    Dirty,
    Running,
    Waiting,
    Incident,
    Disabled,
}

impl RowState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Dirty => "dirty",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Incident => "incident",
            Self::Disabled => "disabled",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoVm {
    pub id: i64,
    pub name: String,
    pub path: PathBuf,
    pub root: String,
    pub root_path: PathBuf,
    pub enabled: bool,
    pub stored: Option<StoredState>,
    pub live: Option<LiveState>,
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub changed: usize,
    pub error: Option<String>,
    pub last_error: Option<String>,
    pub agent: String,
    pub agent_pref: Option<AgentInfo>,
    pub scope_pref: Option<CommitScope>,
    pub running_since: Option<i64>,
    pub next_run_at: Option<i64>,
    pub last_sync_at: Option<i64>,
}

impl RepoVm {
    pub fn from_row(row: &RepoRow, cfg: &Config, now: i64) -> Self {
        let root_path = cfg
            .roots
            .iter()
            .find(|root| root.name == row.root)
            .map(|root| root.path.clone())
            .unwrap_or_default();
        let agent_pref = row.agent.clone().map(|id| AgentInfo {
            id,
            model: row.model.clone().unwrap_or_default(),
        });
        let agent = agent_pref
            .as_ref()
            .map(|pref| pref.id.clone())
            .or_else(|| {
                cfg.overrides
                    .iter()
                    .find(|o| same_path(&o.path, &row.path))
                    .map(|o| o.agent.clone())
            })
            .or_else(|| cfg.agent.order.first().cloned())
            .unwrap_or_else(|| "agent".into());
        Self {
            id: row.id,
            name: row.name(),
            path: row.path.clone(),
            root: row.root.clone(),
            root_path,
            enabled: row.enabled,
            stored: row.state,
            live: None,
            branch: None,
            upstream: None,
            ahead: 0,
            behind: 0,
            changed: 0,
            error: None,
            last_error: row.last_error.clone(),
            agent,
            agent_pref,
            scope_pref: row.commit_scope,
            running_since: (row.state == Some(StoredState::Running))
                .then_some(now.saturating_sub(1)),
            next_run_at: row.next_run_at,
            last_sync_at: row.last_sync_at,
        }
    }

    pub fn sample(id: i64, name: &str, root: &str, state: RowState) -> Self {
        let mut vm = Self {
            id,
            name: name.into(),
            path: PathBuf::from(format!("/tmp/doki/{root}/{name}")),
            root: root.into(),
            root_path: PathBuf::from(format!("/tmp/doki/{root}")),
            enabled: true,
            stored: None,
            live: None,
            branch: Some("main".into()),
            upstream: Some("origin/main".into()),
            ahead: 0,
            behind: 0,
            changed: 0,
            error: None,
            last_error: None,
            agent: "codex".into(),
            agent_pref: None,
            scope_pref: None,
            running_since: None,
            next_run_at: None,
            last_sync_at: None,
        };
        match state {
            RowState::Clean => {}
            RowState::Dirty => {
                vm.live = Some(LiveState::Dirty);
                vm.changed = 3;
                vm.branch = Some("feat/stripe-v2".into());
            }
            RowState::Running => {
                vm.stored = Some(StoredState::Running);
                vm.running_since = Some(1_000);
                vm.agent = "omp".into();
            }
            RowState::Waiting => {
                vm.stored = Some(StoredState::WaitingUser);
                vm.ahead = 1;
                vm.behind = 2;
            }
            RowState::Incident => {
                vm.stored = Some(StoredState::Blocked);
                vm.live = Some(LiveState::Refused("rebase-merge/ present".into()));
                vm.last_error = Some("rebase-merge/ present".into());
                vm.ahead = 1;
                vm.behind = 2;
            }
            RowState::Disabled => {
                vm.enabled = false;
                vm.stored = Some(StoredState::Disabled);
                vm.live = None;
            }
        }
        vm
    }

    pub fn apply_status(&mut self, status: Result<RepoStatus, String>) {
        match status {
            Ok(status) => {
                self.branch = status.branch.clone();
                self.upstream = status.upstream.clone();
                self.ahead = status.ahead;
                self.behind = status.behind;
                self.changed = status.dirty;
                self.live = Some(status.state);
                self.error = None;
            }
            Err(error) => {
                self.error = Some(error);
                self.live = None;
            }
        }
    }

    pub fn row_state(&self) -> RowState {
        if !self.enabled {
            return RowState::Disabled;
        }
        if self.running_since.is_some() || self.stored == Some(StoredState::Running) {
            return RowState::Running;
        }
        if self
            .stored
            .is_some_and(|state| matches!(state, StoredState::Blocked | StoredState::Quarantined))
            || self.error.is_some()
            || matches!(self.live, Some(LiveState::Refused(_)))
        {
            return RowState::Incident;
        }
        if self.stored == Some(StoredState::WaitingUser) || (self.ahead > 0 && self.behind > 0) {
            return RowState::Waiting;
        }
        if self.changed > 0
            || matches!(
                self.live,
                Some(LiveState::Dirty | LiveState::Detached | LiveState::Unborn)
            )
        {
            return RowState::Dirty;
        }
        RowState::Clean
    }

    pub fn state_text(&self) -> String {
        match self.row_state() {
            RowState::Clean => "Synced".into(),
            RowState::Dirty => match self.changed {
                1 => "1 dirty file".into(),
                n => format!("{n} dirty files"),
            },
            RowState::Running => format!("{}: analyzing", self.agent),
            RowState::Waiting => {
                if self.ahead > 0 && self.behind > 0 {
                    "Diverged upstream".into()
                } else {
                    "Waiting for action".into()
                }
            }
            RowState::Incident => self
                .live
                .as_ref()
                .and_then(|state| match state {
                    LiveState::Refused(reason) => Some(short_refusal(reason)),
                    _ => None,
                })
                .or_else(|| self.last_error.clone())
                .unwrap_or_else(|| "Blocked".into()),
            RowState::Disabled => "Disabled".into(),
        }
    }

    pub fn state_detail(&self, now: i64) -> String {
        match self.row_state() {
            RowState::Clean => format!("{}↑ {}↓", self.ahead, self.behind),
            RowState::Dirty => self.agent.clone(),
            RowState::Running => {
                format_elapsed(now.saturating_sub(self.running_since.unwrap_or(now)))
            }
            RowState::Waiting | RowState::Incident => "action required".into(),
            RowState::Disabled => self
                .last_error
                .clone()
                .unwrap_or_else(|| "not managed".into()),
        }
    }

    pub fn branch_label(&self) -> String {
        self.branch.clone().unwrap_or_else(|| match self.live {
            Some(LiveState::Detached) => "detached".into(),
            Some(LiveState::Unborn) => "unborn".into(),
            _ => "—".into(),
        })
    }

    pub fn line2(&self, now: i64) -> String {
        format!("{} · {}", self.state_text(), self.state_detail(now))
    }
}

fn short_refusal(reason: &str) -> String {
    if reason.contains("MERGE_HEAD") {
        "Merge in progress".into()
    } else if reason.contains("rebase-merge") || reason.contains("rebase-apply") {
        "Rebase in progress".into()
    } else if reason.contains("CHERRY_PICK_HEAD") {
        "Cherry-pick in progress".into()
    } else if reason.contains("index.lock") {
        "Index locked".into()
    } else {
        reason.into()
    }
}

pub fn format_elapsed(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds < 60 {
        format!("{seconds}.0s")
    } else {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}
