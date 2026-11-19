use crate::config::{Guards, SecretScanner};
use chrono::{Local, TimeZone as _};
use std::path::PathBuf;

pub const SPARK_BUCKETS: usize = 12;
const HOUR: i64 = 3_600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    All,
    Runs,
    Alerts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    GuardOk,
    Synced,
    Skipped,
    Failed,
    Running,
    Blocked,
}

impl Kind {
    pub fn is_alert(self) -> bool {
        matches!(self, Self::Failed | Self::Blocked)
    }

    pub fn in_tab(self, tab: Tab) -> bool {
        match tab {
            Tab::All => true,
            Tab::Runs => !self.is_alert(),
            Tab::Alerts => self.is_alert(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub at: i64,
    pub repo_id: i64,
    pub repo: String,
    pub kind: Kind,
    pub body: String,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub latency_ms: Option<u64>,
    pub stderr: Option<String>,
    pub incident: Option<i64>,
    pub acknowledged: bool,
}

impl Entry {
    pub fn label(&self) -> &'static str {
        match self.kind {
            Kind::GuardOk => "GUARD OK",
            Kind::Synced => "SYNCED",
            Kind::Skipped => "SKIPPED",
            Kind::Failed => "FAILED",
            Kind::Running => "RUNNING",
            Kind::Blocked => "BLOCKED",
        }
    }

    pub fn in_tab(&self, tab: Tab) -> bool {
        self.kind.in_tab(tab)
    }

    pub fn clock(&self) -> String {
        Local
            .timestamp_opt(self.at, 0)
            .single()
            .map(|time| time.format("%H:%M:%S").to_string())
            .unwrap_or_else(|| "--:--:--".into())
    }

    pub fn latency_label(&self) -> Option<String> {
        self.latency_ms
            .map(|ms| format!("{:.1}s latency", ms as f64 / 1000.))
    }

    pub fn agent_label(&self) -> Option<String> {
        let agent = self.agent.as_ref()?;
        Some(format!(
            "{agent} ({})",
            variant_label(self.model.as_deref())
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRecord {
    pub id: i64,
    pub repo_id: i64,
    pub repo: PathBuf,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub state: String,
    pub message: Option<String>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub pushed: bool,
    pub warning: Option<String>,
    pub error_kind: Option<String>,
    pub upstream: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncidentRecord {
    pub id: i64,
    pub repo_id: i64,
    pub repo: PathBuf,
    pub run_id: Option<i64>,
    pub severity: String,
    pub kind: String,
    pub detail: String,
    pub created_at: i64,
    pub acknowledged: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardSummary {
    pub enabled: bool,
    pub body: String,
}

impl GuardSummary {
    pub fn from_guards(guards: &Guards) -> Self {
        let scanner = match guards.secret_scanner {
            SecretScanner::Gitleaks => "Gitleaks",
            SecretScanner::Trufflehog => "Trufflehog",
            SecretScanner::Off => "Secret",
        };
        Self {
            enabled: guards.secret_scan && guards.secret_scanner != SecretScanner::Off,
            body: format!("{scanner} scan complete. 0 detected credentials in worktree diff."),
        }
    }
}

fn repo_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Runs and incidents become one newest-first feed; an incident owns the run
/// that produced it so a failure never shows twice.
pub fn build(
    runs: &[RunRecord],
    incidents: &[IncidentRecord],
    guards: &GuardSummary,
) -> Vec<Entry> {
    let mut entries = Vec::new();
    let with_incident: Vec<i64> = incidents.iter().filter_map(|row| row.run_id).collect();
    for run in runs {
        if with_incident.contains(&run.id) {
            continue;
        }
        let repo = repo_name(&run.repo);
        let latency_ms = run
            .finished_at
            .map(|finished| (finished - run.started_at).max(0) as u64 * 1000);
        let finished = run.finished_at.unwrap_or(run.started_at);
        let entry = match run.state.as_str() {
            "committed" if run.pushed => Entry {
                id: format!("run-{}", run.id),
                at: finished,
                repo_id: run.repo_id,
                repo,
                kind: Kind::Synced,
                body: match &run.upstream {
                    Some(upstream) if upstream.contains('/') => {
                        format!("→ {upstream}")
                    }
                    _ => "→ origin/main".into(),
                },
                agent: run.agent.clone(),
                model: run.model.clone(),
                latency_ms,
                stderr: None,
                incident: None,
                acknowledged: false,
            },
            "committed" if guards.enabled => Entry {
                id: format!("guard-{}", run.id),
                at: finished,
                repo_id: run.repo_id,
                repo,
                kind: Kind::GuardOk,
                body: guards.body.clone(),
                agent: None,
                model: None,
                latency_ms: None,
                stderr: None,
                incident: None,
                acknowledged: false,
            },
            "committed" => Entry {
                id: format!("run-{}", run.id),
                at: finished,
                repo_id: run.repo_id,
                repo,
                kind: Kind::Synced,
                body: run
                    .message
                    .as_deref()
                    .map(|message| message.trim().to_string())
                    .or_else(|| run.warning.clone())
                    .unwrap_or_else(|| "committed".into()),
                agent: run.agent.clone(),
                model: run.model.clone(),
                latency_ms,
                stderr: None,
                incident: None,
                acknowledged: false,
            },
            "skipped" => Entry {
                id: format!("run-{}", run.id),
                at: finished,
                repo_id: run.repo_id,
                repo,
                kind: Kind::Skipped,
                body: run
                    .warning
                    .clone()
                    .unwrap_or_else(|| "Nothing staged for commit".into()),
                agent: run.agent.clone(),
                model: run.model.clone(),
                latency_ms,
                stderr: None,
                incident: None,
                acknowledged: false,
            },
            "failed" => Entry {
                id: format!("run-{}", run.id),
                at: finished,
                repo_id: run.repo_id,
                repo,
                kind: Kind::Failed,
                body: failure_body(run),
                agent: run.agent.clone(),
                model: run.model.clone(),
                latency_ms,
                stderr: None,
                incident: None,
                acknowledged: false,
            },
            "blocked" | "waiting" => Entry {
                id: format!("run-{}", run.id),
                at: finished,
                repo_id: run.repo_id,
                repo,
                kind: Kind::Blocked,
                body: failure_body(run),
                agent: run.agent.clone(),
                model: run.model.clone(),
                latency_ms,
                stderr: None,
                incident: None,
                acknowledged: false,
            },
            "running" => Entry {
                id: format!("run-{}", run.id),
                at: run.started_at,
                repo_id: run.repo_id,
                repo,
                kind: Kind::Running,
                body: format!("{} is running", run.agent.as_deref().unwrap_or("a sync")),
                agent: run.agent.clone(),
                model: run.model.clone(),
                latency_ms: None,
                stderr: None,
                incident: None,
                acknowledged: false,
            },
            _ => Entry {
                id: format!("run-{}", run.id),
                at: finished,
                repo_id: run.repo_id,
                repo,
                kind: Kind::Skipped,
                body: run
                    .warning
                    .clone()
                    .unwrap_or_else(|| format!("run {}", run.state)),
                agent: run.agent.clone(),
                model: run.model.clone(),
                latency_ms,
                stderr: None,
                incident: None,
                acknowledged: false,
            },
        };
        entries.push(entry);
    }
    for incident in incidents {
        let mut lines = incident.detail.trim().lines();
        let first = lines.next().unwrap_or_default().to_string();
        let tail: Vec<&str> = lines.collect();
        let stderr = (!tail.is_empty()).then(|| tail.join("\n"));
        entries.push(Entry {
            id: format!("incident-{}", incident.id),
            at: incident.created_at,
            repo_id: incident.repo_id,
            repo: repo_name(&incident.repo),
            kind: Kind::Blocked,
            body: first,
            agent: None,
            model: None,
            latency_ms: None,
            stderr,
            incident: Some(incident.id),
            acknowledged: incident.acknowledged || incident.severity == "warning",
        });
    }
    entries.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| b.id.cmp(&a.id)));
    entries
}

fn failure_body(run: &RunRecord) -> String {
    if let Some(warning) = &run.warning {
        return warning.clone();
    }
    match &run.error_kind {
        Some(kind) => format!("run {kind} failed"),
        None => "run failed".into(),
    }
}

pub fn counts(entries: &[Entry], tab: Tab) -> usize {
    entries.iter().filter(|entry| entry.in_tab(tab)).count()
}

fn succeeds(state: &str) -> bool {
    matches!(state, "committed" | "skipped")
}

fn fails(state: &str) -> bool {
    matches!(state, "failed" | "blocked" | "waiting")
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Bucket {
    pub runs: usize,
    pub failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rate {
    pub buckets: [Bucket; SPARK_BUCKETS],
    pub total: usize,
    pub succeeded: usize,
    pub avg_ms: Option<u64>,
}

impl Rate {
    pub fn ratio(&self) -> f64 {
        if self.total == 0 {
            1.
        } else {
            self.succeeded as f64 / self.total as f64
        }
    }

    pub fn percent_label(&self) -> String {
        format!("{:.1}% Deterministic", self.ratio() * 100.)
    }

    pub fn avg_label(&self) -> String {
        match self.avg_ms {
            Some(ms) => format!("Avg: {:.2}s", ms as f64 / 1000.),
            None => "Avg: —".into(),
        }
    }

    pub fn max(&self) -> usize {
        self.buckets
            .iter()
            .map(|bucket| bucket.runs)
            .max()
            .unwrap_or(0)
    }

    pub fn height(&self, index: usize) -> f32 {
        let max = self.max().max(1);
        let runs = self.buckets.get(index).map_or(0, |bucket| bucket.runs);
        if runs == 0 {
            0.
        } else {
            (runs as f32 / max as f32).max(0.12)
        }
    }
}

/// Twelve hourly buckets ending in the current hour; a bucket that held a
/// failed run turns vermilion in the footer.
pub fn rate(runs: &[RunRecord], now: i64) -> Rate {
    let current = now.div_euclid(HOUR);
    let mut rate = Rate {
        buckets: [Bucket::default(); SPARK_BUCKETS],
        total: 0,
        succeeded: 0,
        avg_ms: None,
    };
    let mut durations = Vec::new();
    for run in runs {
        let age = current - run.started_at.div_euclid(HOUR);
        if !(0..SPARK_BUCKETS as i64).contains(&age) {
            continue;
        }
        let index = SPARK_BUCKETS - 1 - age as usize;
        rate.buckets[index].runs += 1;
        rate.buckets[index].failed |= fails(&run.state);
        rate.total += 1;
        if succeeds(&run.state) {
            rate.succeeded += 1;
        }
        if let Some(finished) = run.finished_at {
            durations.push((finished - run.started_at).max(0) as u64 * 1000);
        }
    }
    if !durations.is_empty() {
        let total: u64 = durations.iter().sum();
        rate.avg_ms = Some(total / durations.len() as u64);
    }
    rate
}

fn variant_label(model: Option<&str>) -> String {
    let Some(model) = model.map(str::trim).filter(|model| !model.is_empty()) else {
        return "CLI default".into();
    };
    let lower = model.to_ascii_lowercase();
    for (needle, label) in [
        ("sonnet", "Sonnet"),
        ("opus", "Opus"),
        ("haiku", "Haiku"),
        ("gpt", "GPT"),
        ("gemini", "Gemini"),
        ("claude", "Claude"),
    ] {
        if lower.contains(needle) {
            return label.into();
        }
    }
    model.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: i64, repo: &str, at: i64, state: &str) -> RunRecord {
        RunRecord {
            id,
            repo_id: id,
            repo: PathBuf::from(format!("/tmp/{repo}")),
            started_at: at,
            finished_at: Some(at + 1),
            state: state.into(),
            message: Some("doki: a subject".into()),
            agent: Some("omp".into()),
            model: Some("claude-sonnet-4-5".into()),
            pushed: false,
            warning: None,
            error_kind: None,
            upstream: Some("origin/main".into()),
        }
    }

    fn incident(id: i64, repo: &str, at: i64, run_id: Option<i64>) -> IncidentRecord {
        IncidentRecord {
            id,
            repo_id: 4,
            repo: PathBuf::from(format!("/tmp/{repo}")),
            run_id,
            severity: "blocking".into(),
            kind: "diverged".into(),
            detail: "Diverged upstream\nfatal: not a fast-forward".into(),
            created_at: at,
            acknowledged: false,
        }
    }

    #[test]
    fn incidents_own_their_runs_and_the_feed_is_newest_first() {
        let mut pushed = run(1, "doki-core", 1_000, "committed");
        pushed.pushed = true;
        pushed.finished_at = Some(1_001);
        let mut blocked = run(2, "legacy-auth", 900, "blocked");
        blocked.warning = Some("Diverged upstream".into());
        let incidents = vec![incident(7, "legacy-auth", 1_100, Some(2))];
        let entries = build(
            &[pushed, blocked],
            &incidents,
            &GuardSummary::from_guards(&Guards::default()),
        );
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, "incident-7");
        assert_eq!(entries[0].kind, Kind::Blocked);
        assert_eq!(
            entries[0].stderr.as_deref(),
            Some("fatal: not a fast-forward")
        );
        assert_eq!(entries[1].kind, Kind::Synced);
        assert_eq!(entries[1].body, "→ origin/main");
        assert_eq!(entries[1].agent_label().as_deref(), Some("omp (Sonnet)"));
        assert_eq!(entries[1].latency_label().as_deref(), Some("1.0s latency"));
        assert_eq!(counts(&entries, Tab::All), 2);
        assert_eq!(counts(&entries, Tab::Runs), 1);
        assert_eq!(counts(&entries, Tab::Alerts), 1);
    }

    #[test]
    fn a_committed_unpushed_run_reports_the_guard_pass() {
        let entries = build(
            &[run(3, "billing-service", 1_000, "committed")],
            &[],
            &GuardSummary::from_guards(&Guards::default()),
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, Kind::GuardOk);
        assert_eq!(entries[0].label(), "GUARD OK");
        assert!(entries[0].body.starts_with("Gitleaks scan complete."));
        assert!(entries[0].agent_label().is_none());
    }

    #[test]
    fn skipped_and_failed_runs_keep_their_own_labels() {
        let mut failed = run(5, "analytics-pipeline", 1_100, "failed");
        failed.error_kind = Some("fetch".into());
        let entries = build(
            &[run(4, "doki-core", 1_000, "skipped"), failed],
            &[],
            &GuardSummary::from_guards(&Guards::default()),
        );
        assert_eq!(entries[0].kind, Kind::Failed);
        assert_eq!(entries[0].body, "run fetch failed");
        assert_eq!(entries[1].kind, Kind::Skipped);
        assert_eq!(entries[1].body, "Nothing staged for commit");
        assert_eq!(counts(&entries, Tab::Alerts), 1);
        assert_eq!(counts(&entries, Tab::Runs), 1);
    }

    #[test]
    fn sparkline_buckets_are_hourly_and_flag_failures() {
        let now = 100 * HOUR + 1_800;
        let healthy = run(1, "a", now - 10, "committed");
        let more = run(2, "b", now - 10, "skipped");
        let failed = run(3, "c", now - 2 * HOUR + 5, "failed");
        let old = run(4, "d", now - 20 * HOUR, "committed");
        let rate = rate(&[healthy, more, failed, old], now);
        assert_eq!(rate.buckets.len(), 12);
        assert_eq!(rate.buckets[11].runs, 2);
        assert!(!rate.buckets[11].failed);
        assert_eq!(rate.buckets[9].runs, 1);
        assert!(rate.buckets[9].failed);
        assert!(rate.buckets[0].runs == 0);
        assert_eq!(rate.total, 3);
        assert_eq!(rate.succeeded, 2);
        assert_eq!(rate.avg_ms, Some(1_000));
        assert!((rate.height(11) - 1.0).abs() < f32::EPSILON);
        assert!((rate.height(9) - 0.5).abs() < f32::EPSILON);
        assert_eq!(rate.height(0), 0.);
        assert_eq!(rate.percent_label(), "66.7% Deterministic");
        assert_eq!(rate.avg_label(), "Avg: 1.00s");
    }

    #[test]
    fn an_empty_window_is_a_full_ratio_with_no_latency() {
        let rate = rate(&[], 1_000);
        assert_eq!(rate.total, 0);
        assert_eq!(rate.ratio(), 1.);
        assert_eq!(rate.percent_label(), "100.0% Deterministic");
        assert_eq!(rate.avg_label(), "Avg: —");
        assert_eq!(rate.max(), 0);
    }

    #[test]
    fn guard_summary_names_the_scanner_and_respects_off() {
        let guards = Guards::default();
        let summary = GuardSummary::from_guards(&guards);
        assert!(summary.enabled);
        assert!(summary.body.contains("Gitleaks"));
        let off = Guards {
            secret_scan: false,
            ..Guards::default()
        };
        assert!(!GuardSummary::from_guards(&off).enabled);
    }

    #[test]
    fn tab_filtering_covers_all_three_tabs() {
        assert!(Kind::GuardOk.in_tab(Tab::All));
        assert!(Kind::GuardOk.in_tab(Tab::Runs));
        assert!(!Kind::GuardOk.in_tab(Tab::Alerts));
        assert!(Kind::Blocked.in_tab(Tab::Alerts));
        assert!(!Kind::Blocked.in_tab(Tab::Runs));
    }
}
