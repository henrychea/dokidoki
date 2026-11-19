use crate::sync::SyncReport;
use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    RunStarted {
        repo: PathBuf,
        at: i64,
    },
    RunFinished {
        repo: PathBuf,
        state: String,
        detail: Option<String>,
        pushed: bool,
        at: i64,
    },
    Incident {
        repo: PathBuf,
        kind: String,
        severity: String,
        detail: String,
        at: i64,
    },
}

/// The UI subscribes later; nothing here blocks on a consumer.
pub struct EventBus {
    sender: async_channel::Sender<Event>,
    receiver: async_channel::Receiver<Event>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    pub fn new() -> Self {
        let (sender, receiver) = async_channel::unbounded();
        Self { sender, receiver }
    }

    pub fn subscribe(&self) -> async_channel::Receiver<Event> {
        self.receiver.clone()
    }

    pub fn emit(&self, event: Event) {
        let _ = self.sender.send_blocking(event);
    }

    pub fn pending(&self) -> usize {
        self.receiver.len()
    }
}

/// Fixed pool of plain threads: git and agents are subprocess work.
pub fn run<F>(
    repos: Vec<PathBuf>,
    concurrency: u32,
    bus: &EventBus,
    job: F,
) -> Vec<(PathBuf, Result<SyncReport>)>
where
    F: Fn(&Path) -> Result<SyncReport> + Sync,
{
    let workers = (concurrency.max(1) as usize).min(repos.len().max(1));
    let queue = Mutex::new(repos.into_iter());
    let results = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let Some(repo) = queue.lock().expect("job queue").next() else {
                        break;
                    };
                    bus.emit(Event::RunStarted {
                        repo: repo.clone(),
                        at: now(),
                    });
                    let outcome =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&repo)))
                            .unwrap_or_else(|_| Err(anyhow!("sync worker panicked")));
                    match &outcome {
                        Ok(report) => emit_report(bus, &repo, report),
                        Err(error) => bus.emit(Event::RunFinished {
                            repo: repo.clone(),
                            state: "failed".into(),
                            detail: Some(format!("{error:#}")),
                            pushed: false,
                            at: now(),
                        }),
                    }
                    results.lock().expect("results").push((repo, outcome));
                }
            });
        }
    });
    let mut results = results.into_inner().expect("results");
    results.sort_by(|a, b| a.0.cmp(&b.0));
    results
}

/// The same fixed pool, for read-only work that must not appear in the journal.
pub fn run_each<T, F>(repos: Vec<PathBuf>, concurrency: u32, job: F) -> Vec<(PathBuf, Result<T>)>
where
    T: Send,
    F: Fn(&Path) -> Result<T> + Sync,
{
    let workers = (concurrency.max(1) as usize).min(repos.len().max(1));
    let queue = Mutex::new(repos.into_iter());
    let results = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let Some(repo) = queue.lock().expect("job queue").next() else {
                        break;
                    };
                    let outcome =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&repo)))
                            .unwrap_or_else(|_| Err(anyhow!("status worker panicked")));
                    results.lock().expect("results").push((repo, outcome));
                }
            });
        }
    });
    let mut results = results.into_inner().expect("results");
    results.sort_by(|a, b| a.0.cmp(&b.0));
    results
}

fn emit_report(bus: &EventBus, repo: &Path, report: &SyncReport) {
    tracing::info!(
        repo = %repo.display(),
        state = report.state.label(),
        "repo sync finished"
    );
    if let Some(kind) = &report.incident_kind {
        bus.emit(Event::Incident {
            repo: repo.to_path_buf(),
            kind: kind.clone(),
            severity: match report.state {
                crate::sync::SyncState::WaitingUser => "blocking",
                crate::sync::SyncState::Failed => "error",
                _ => "warning",
            }
            .into(),
            detail: report.detail.clone().unwrap_or_default(),
            at: now(),
        });
    }
    bus.emit(Event::RunFinished {
        repo: repo.to_path_buf(),
        state: report.state.label().into(),
        detail: report.detail.clone(),
        pushed: report.pushed,
        at: now(),
    });
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::SyncState;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn every_repo_runs_and_events_come_in_per_repo_order() {
        let bus = EventBus::new();
        let seen = bus.subscribe();
        let running = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let repos: Vec<PathBuf> = (0..6).map(|n| PathBuf::from(format!("/tmp/{n}"))).collect();
        let results = run(repos.clone(), 2, &bus, |path| {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(20));
            running.fetch_sub(1, Ordering::SeqCst);
            let mut report = SyncReport::new(path, false);
            report.state = SyncState::Committed;
            Ok(report)
        });
        assert_eq!(results.len(), 6);
        assert!(results.iter().all(|(_, r)| r.is_ok()));
        assert!(peak.load(Ordering::SeqCst) <= 2);

        let mut events = Vec::new();
        while let Ok(event) = seen.try_recv() {
            events.push(event);
        }
        assert_eq!(events.len(), 12);
        for repo in &repos {
            let started = events
                .iter()
                .position(|e| matches!(e, Event::RunStarted { repo: r, .. } if r == repo));
            let finished = events
                .iter()
                .position(|e| matches!(e, Event::RunFinished { repo: r, .. } if r == repo));
            let (started, finished) = (started.unwrap(), finished.unwrap());
            assert!(started < finished, "{repo:?}: {started} < {finished}");
        }
    }

    #[test]
    fn a_failing_job_is_reported_not_fatal() {
        let bus = EventBus::new();
        let results = run(vec![PathBuf::from("/tmp/a")], 2, &bus, |_| {
            Err(anyhow!("boom"))
        });
        assert_eq!(results.len(), 1);
        assert!(results[0].1.is_err());
        assert_eq!(bus.pending(), 2);
    }
}
