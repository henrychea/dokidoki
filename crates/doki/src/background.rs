use crate::sync::AgentMessenger;
use crate::tray::{self, Action, ActionQueue, TrayState};
use anyhow::Result;
use doki_core::config::Config;
use doki_core::discovery;
use doki_core::lock::LockAttempt;
use doki_core::notify::{Desktop, Notifications, Notifier, Off};
use doki_core::pool::{self, EventBus};
use doki_core::scheduler;
use doki_core::store::Store;
use doki_core::sync::{self, SyncOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static STOP: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub poll: Duration,
    pub tray: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            poll: Duration::from_secs(30),
            tray: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Report {
    pub ticks: usize,
    pub synced: usize,
    pub attention: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tick {
    pub synced: usize,
    pub attention: usize,
    pub due: usize,
}

struct Daemon<'a> {
    cfg: &'a Config,
    db: PathBuf,
    state_dir: PathBuf,
    notifications: Notifications<Box<dyn Notifier>>,
}

impl<'a> Daemon<'a> {
    fn new(cfg: &'a Config, db: &Path, state_dir: &Path) -> Self {
        let notifier: Box<dyn Notifier> = if cfg.notify.desktop {
            Box::new(Desktop)
        } else {
            Box::new(Off)
        };
        Self {
            cfg,
            db: db.to_path_buf(),
            state_dir: state_dir.to_path_buf(),
            notifications: Notifications::new(notifier, cfg.notify.coalesce_seconds),
        }
    }

    fn tick(&mut self, force: bool) -> Result<Tick> {
        let _lock = match scheduler::try_lock(&self.state_dir)? {
            LockAttempt::Acquired(lock) => lock,
            LockAttempt::Busy => {
                tracing::info!("scheduler busy; skipping this background tick");
                return Ok(Tick::default());
            }
        };
        let mut store = Store::open(&self.db)?;
        store.upsert_detected(&discovery::scan(&self.cfg.roots, &self.cfg.discovery)?)?;
        let now = now_unix();
        let paths: Vec<PathBuf> = if force {
            store
                .repos()?
                .into_iter()
                .filter(|row| row.enabled && !row.quarantined())
                .map(|row| row.path)
                .collect()
        } else {
            scheduler::due(self.cfg, &store, now)?
                .into_iter()
                .map(|row| row.path)
                .collect()
        };
        if paths.is_empty() {
            return Ok(Tick::default());
        }
        tracing::info!(repos = paths.len(), force, "background sync tick");
        let bus = EventBus::new();
        let results = pool::run(paths.clone(), self.cfg.sync.concurrency, &bus, |path| {
            let store = Store::open(&self.db)?;
            let messenger = AgentMessenger::new(self.cfg, path)?;
            sync::sync_with(
                self.cfg,
                &store,
                path,
                &messenger,
                SyncOptions {
                    dry_run: false,
                    state_dir: Some(self.state_dir.clone()),
                },
            )
        });
        let mut tick = Tick {
            due: paths.len(),
            ..Tick::default()
        };
        for (path, result) in results {
            match result {
                Ok(report) => {
                    if report.state == sync::SyncState::Committed {
                        tick.synced += 1;
                    }
                    if report.exit_code() != 0 {
                        tick.attention += 1;
                    }
                    tracing::info!(
                        repo = %path.display(),
                        state = report.state.label(),
                        "repo sync finished"
                    );
                    if let Some(row) = store.repo_by_path(&report.repo)? {
                        self.notifications
                            .notify_report(row.id, &row.name(), &report, now_unix());
                    }
                }
                Err(error) => {
                    tick.attention += 1;
                    tracing::warn!(repo = %path.display(), %error, "background sync failed");
                }
            }
        }
        Ok(tick)
    }

    fn incidents(&self) -> usize {
        Store::open(&self.db)
            .ok()
            .and_then(|store| store.journal_incidents(200).ok())
            .map(|incidents| incidents.iter().filter(|row| !row.acknowledged).count())
            .unwrap_or(0)
    }
}

pub fn run(cfg: &Config, db: &Path, state_dir: &Path, opts: Options) -> Result<Report> {
    install_signals();
    let mut daemon = Daemon::new(cfg, db, state_dir);
    let tray_state = Arc::new(TrayState::default());
    let tray = if opts.tray {
        tray::spawn(tray_state.clone())
    } else {
        None
    };
    let actions = tray.as_ref().map(|(_, actions)| actions.clone());
    let mut force = false;
    let mut report = Report::default();
    let step = Duration::from_millis(100);
    let steps = (opts.poll.as_millis() / step.as_millis()).max(1) as usize;
    tracing::info!(
        poll_seconds = opts.poll.as_secs(),
        tray = tray.is_some(),
        "background scheduler started"
    );
    while !stopped() {
        if !tray_state.paused.load(Ordering::Relaxed) {
            let tick = daemon.tick(force)?;
            force = false;
            report.ticks += 1;
            report.synced += tick.synced;
            report.attention += tick.attention;
            tray_state
                .incidents
                .store(daemon.incidents(), Ordering::Relaxed);
        }
        if let Some((handle, _)) = &tray {
            handle.update(|_| {});
        }
        let mut clicked_sync = false;
        for _ in 0..steps {
            if stopped() || drained(&actions, &tray_state, &mut force, &mut clicked_sync) {
                break;
            }
            std::thread::sleep(step);
        }
        if stopped() {
            break;
        }
        if clicked_sync {
            continue;
        }
        if let Some((handle, _)) = &tray {
            handle.update(|_| {});
        }
    }
    if let Some((handle, _)) = &tray {
        handle.shutdown().wait();
    }
    tracing::info!(
        ticks = report.ticks,
        synced = report.synced,
        attention = report.attention,
        "background scheduler stopped"
    );
    Ok(report)
}

/// Drains tray clicks; returns true when the outer loop should move on at once.
fn drained(
    actions: &Option<ActionQueue>,
    state: &Arc<TrayState>,
    force: &mut bool,
    clicked_sync: &mut bool,
) -> bool {
    let Some(queue) = actions else {
        return false;
    };
    let mut queue = queue.lock().expect("tray actions");
    let mut acted = false;
    while let Some(action) = queue.pop_front() {
        acted = true;
        match action {
            Action::Open => open_ui(),
            Action::SyncAll => {
                *force = true;
                *clicked_sync = true;
            }
            Action::TogglePause => {
                let paused = !state.paused.load(Ordering::Relaxed);
                state.paused.store(paused, Ordering::Relaxed);
                tracing::info!(paused, "background scheduler pause toggled");
            }
            Action::Quit => STOP.store(true, Ordering::SeqCst),
        }
    }
    acted
}

fn open_ui() {
    let result = std::env::current_exe().and_then(|exe| {
        std::process::Command::new(exe)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|_| ())
    });
    if let Err(error) = result {
        tracing::warn!(%error, "could not open the dokidoki window");
    }
}

pub fn stopped() -> bool {
    STOP.load(Ordering::SeqCst)
}

pub fn install_signals() {
    // A signal-safe flag keeps SIGTERM/SIGINT from leaving a lock behind.
    unsafe {
        libc::signal(
            libc::SIGINT,
            handle_signal as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            handle_signal as *const () as libc::sighandler_t,
        );
    }
}

extern "C" fn handle_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

pub fn print_report(report: Report) -> Result<()> {
    crate::write_quiet(
        std::io::stdout().lock(),
        &format!(
            "background stopped: {} tick(s), {} synced, {} need attention\n",
            report.ticks, report.synced, report.attention
        ),
    )
    .map_err(Into::into)
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}
