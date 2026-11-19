use crate::sync::{SyncReport, SyncState};
use std::collections::HashMap;
use std::sync::Mutex;

pub trait Notifier: Send + Sync {
    /// A missing notification daemon is a normal false, never a sync failure.
    fn notify(&self, summary: &str, body: &str) -> bool;
}

impl Notifier for Box<dyn Notifier> {
    fn notify(&self, summary: &str, body: &str) -> bool {
        (**self).notify(summary, body)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Desktop;

impl Notifier for Desktop {
    fn notify(&self, summary: &str, body: &str) -> bool {
        notify_rust::Notification::new()
            .appname("doki")
            .summary(summary)
            .body(body)
            .show()
            .is_ok()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Off;

impl Notifier for Off {
    fn notify(&self, _summary: &str, _body: &str) -> bool {
        false
    }
}

#[derive(Debug, Default)]
pub struct Memory {
    sent: Mutex<Vec<(String, String)>>,
}

impl Memory {
    pub fn sent(&self) -> Vec<(String, String)> {
        self.sent.lock().expect("notify memory").clone()
    }
}

impl Notifier for Memory {
    fn notify(&self, summary: &str, body: &str) -> bool {
        self.sent
            .lock()
            .expect("notify memory")
            .push((summary.to_string(), body.to_string()));
        true
    }
}

pub struct Notifications<N: Notifier> {
    notifier: N,
    coalesce_seconds: i64,
    last: HashMap<i64, i64>,
}

impl<N: Notifier> Notifications<N> {
    pub fn new(notifier: N, coalesce_seconds: i64) -> Self {
        Self {
            notifier,
            coalesce_seconds,
            last: HashMap::new(),
        }
    }

    pub fn notifier(&self) -> &N {
        &self.notifier
    }

    pub fn notify_report(
        &mut self,
        repo_id: i64,
        name: &str,
        report: &SyncReport,
        now: i64,
    ) -> Option<(String, String)> {
        let (summary, body) = summary(report, name)?;
        if let Some(last) = self.last.get(&repo_id)
            && now - last < self.coalesce_seconds
        {
            return None;
        }
        self.last.insert(repo_id, now);
        self.notifier.notify(&summary, &body);
        Some((summary, body))
    }
}

/// Only incidents are worth a toast; a clean or skipped run stays silent.
pub fn summary(report: &SyncReport, name: &str) -> Option<(String, String)> {
    if report.dry_run {
        return None;
    }
    let detail = report
        .detail
        .clone()
        .or_else(|| report.warning.clone())
        .unwrap_or_else(|| report.state.label().to_string());
    let push_failed = report
        .incident_kind
        .as_deref()
        .is_some_and(|kind| kind.starts_with("push"));
    let (title, summary) = if report.quarantined {
        ("quarantined", format!("doki: {name} quarantined"))
    } else {
        match report.state {
            SyncState::WaitingUser => ("diverged", format!("doki: {name} needs a decision")),
            SyncState::Blocked => ("blocked", format!("doki: {name} blocked")),
            SyncState::Failed if push_failed => {
                ("push failed", format!("doki: {name} push failed"))
            }
            SyncState::Failed => ("sync failed", format!("doki: {name} sync failed")),
            _ => return None,
        }
    };
    Some((summary, format!("{title}\n{detail}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CommitScope;

    fn report(state: SyncState) -> SyncReport {
        let mut report = SyncReport::new("/tmp/repo", false);
        report.state = state;
        report.scope = Some(CommitScope::Tracked);
        report.detail = Some("detail".into());
        report
    }

    #[test]
    fn clean_runs_and_dry_runs_stay_silent() {
        assert!(summary(&report(SyncState::Committed), "repo").is_none());
        assert!(summary(&report(SyncState::Skipped), "repo").is_none());
        assert!(summary(&report(SyncState::Busy), "repo").is_none());
        let mut dry = report(SyncState::Failed);
        dry.dry_run = true;
        assert!(summary(&dry, "repo").is_none());
    }

    #[test]
    fn incidents_carry_the_repo_and_detail() {
        let (headline, body) = summary(&report(SyncState::WaitingUser), "repo").unwrap();
        assert!(headline.contains("repo"), "{headline}");
        assert!(body.contains("diverged"), "{body}");
        assert!(body.contains("detail"), "{body}");
        assert!(
            summary(&report(SyncState::Blocked), "repo")
                .unwrap()
                .1
                .contains("blocked")
        );
        let mut failed = report(SyncState::Failed);
        failed.incident_kind = Some("push".into());
        assert!(summary(&failed, "repo").unwrap().1.contains("push failed"));
        failed.incident_kind = Some("fetch".into());
        assert!(summary(&failed, "repo").unwrap().1.contains("sync failed"));
        let mut quarantined = report(SyncState::Failed);
        quarantined.quarantined = true;
        assert!(
            summary(&quarantined, "repo")
                .unwrap()
                .1
                .contains("quarantined")
        );
    }

    #[test]
    fn coalesces_per_repo_within_the_window() {
        let mut notifications = Notifications::new(Memory::default(), 30);
        let waiting = report(SyncState::WaitingUser);
        assert!(
            notifications
                .notify_report(1, "one", &waiting, 100)
                .is_some()
        );
        assert!(
            notifications
                .notify_report(1, "one", &waiting, 129)
                .is_none()
        );
        assert!(
            notifications
                .notify_report(2, "two", &waiting, 129)
                .is_some()
        );
        assert!(
            notifications
                .notify_report(1, "one", &waiting, 130)
                .is_some()
        );
        assert_eq!(notifications.notifier().sent().len(), 3);
        assert!(
            notifications
                .notify_report(3, "three", &report(SyncState::Committed), 200)
                .is_none()
        );
    }
}
