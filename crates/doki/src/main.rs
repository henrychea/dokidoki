use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use doki::sync::Selection;
use doki_core::config::{Config, config_path, state_dir};
use doki_core::discovery;
use doki_core::git::Git;
use doki_core::scheduler;
use doki_core::status::{self, RepoState, RepoStatus};
use doki_core::store::{RepoRow, Store};
use doki_core::timer;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Parser)]
#[command(
    name = "doki",
    version,
    about = "Watch git repos and keep them committed and pushed"
)]
struct Cli {
    /// Open the UI with the dry-run switch already on
    #[arg(long)]
    dry_run: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Rescan roots and list known repos
    Status {
        #[arg(long)]
        json: bool,
    },
    /// List agent CLIs and test one with a tiny prompt
    Agents {
        #[command(subcommand)]
        cmd: AgentsCmd,
    },
    /// Commit and push enabled repos
    Sync {
        /// Sync one repo
        #[arg(long, value_name = "PATH", conflicts_with_all = ["all", "due"])]
        repo: Option<PathBuf>,
        /// Sync every enabled repo
        #[arg(long)]
        all: bool,
        /// Sync only the repos the scheduler says are due
        #[arg(long)]
        due: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Move HEAD back to the last run's pre-sync snapshot
    Undo { repo: PathBuf },
    /// Clear a repo's quarantine so the scheduler retries it
    ResetQuarantine { repo: PathBuf },
    /// Schedule syncs without the window
    Run {
        #[arg(long)]
        background: bool,
        /// Skip the StatusNotifierItem tray icon
        #[arg(long)]
        no_tray: bool,
    },
    /// Check git, agents, guards, inotify, notifications and the timer
    Doctor,
    /// Write systemd user units for `doki sync --due`
    InstallTimer {
        /// Replace differing units, keeping a .bak of each
        #[arg(long)]
        force_write: bool,
    },
    /// Print the disable command and remove doki-managed units
    UninstallTimer,
}

#[derive(Subcommand)]
enum AgentsCmd {
    /// Show each CLI, whether it is on PATH, and its resolved model
    List,
    /// Run a fixed prompt through one CLI and print the parsed message
    Test { id: Option<String> },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let state = state_dir();
    doki_core::logging::init(&state);
    let db = state.join("doki.db");
    if matches!(cli.cmd, Some(Cmd::Doctor)) {
        let cfg = Config::load(&config_path());
        return match doki::doctor::run(cfg, &config_path(), &state)? {
            true => Ok(()),
            false => bail!("doctor found blocking failures"),
        };
    }
    let cfg = Config::load(&config_path())?;
    match cli.cmd {
        None => {
            doki_ui::run(cfg, Store::open(&db)?, cli.dry_run);
            Ok(())
        }
        Some(Cmd::Status { json }) => status(&cfg, &db, json),
        Some(Cmd::Agents { cmd }) => match cmd {
            AgentsCmd::List => doki::agents::list(&cfg),
            AgentsCmd::Test { id } => doki::agents::test(&cfg, id),
        },
        Some(Cmd::Sync {
            repo,
            all,
            due,
            dry_run,
        }) => {
            let selection = match (repo, all, due) {
                (Some(path), false, false) => Selection::Repo(path),
                (None, true, false) => Selection::All,
                (None, false, true) => Selection::Due,
                _ => bail!("choose one of --repo <path>, --all or --due"),
            };
            doki::sync::run(&cfg, &db, &state, selection, dry_run)
        }
        Some(Cmd::Undo { repo }) => {
            let store = Store::open(&db)?;
            doki::undo::run(&store, &repo)
        }
        Some(Cmd::ResetQuarantine { repo }) => doki::sync::reset_quarantine(&cfg, &db, &repo),
        Some(Cmd::Run {
            background,
            no_tray,
        }) => {
            if !background {
                bail!("run requires --background; start the window with `doki`");
            }
            let options = doki::background::Options {
                poll: Duration::from_secs(30),
                tray: !no_tray,
            };
            let report = doki::background::run(&cfg, &db, &state, options)?;
            doki::background::print_report(report)
        }
        Some(Cmd::Doctor) => unreachable!("handled before config load"),
        Some(Cmd::InstallTimer { force_write }) => {
            let binary = std::env::current_exe()?;
            let outcome = timer::install(&cfg, &binary, force_write)?;
            let mut out = format!(
                "calendar: {}\nservice: {}\ntimer:   {}\n",
                outcome.calendar,
                outcome.service.display(),
                outcome.timer.display()
            );
            for path in &outcome.backups {
                out.push_str(&format!("backup:  {}\n", path.display()));
            }
            for path in &outcome.unchanged {
                out.push_str(&format!("kept:    {}\n", path.display()));
            }
            out.push_str(
                "\nrun these yourself (doki never runs systemctl):\n  \
                 systemctl --user daemon-reload && systemctl --user enable --now doki.timer\n",
            );
            doki::write_quiet(io::stdout().lock(), &out)?;
            Ok(())
        }
        Some(Cmd::UninstallTimer) => {
            doki::write_quiet(
                io::stdout().lock(),
                "run this yourself (doki never runs systemctl):\n  \
                 systemctl --user disable --now doki.timer\n\n",
            )?;
            let outcome = timer::uninstall()?;
            let mut out = String::new();
            for path in &outcome.removed {
                out.push_str(&format!("removed: {}\n", path.display()));
            }
            for path in &outcome.kept {
                out.push_str(&format!(
                    "kept:    {} (not written by doki)\n",
                    path.display()
                ));
            }
            if outcome.removed.is_empty() {
                out.push_str("no doki-managed units found\n");
            }
            doki::write_quiet(io::stdout().lock(), &out)?;
            Ok(())
        }
    }
}

fn status(cfg: &Config, db: &std::path::Path, json: bool) -> Result<()> {
    let mut store = Store::open(db)?;
    store.upsert_detected(&discovery::scan(&cfg.roots, &cfg.discovery)?)?;
    let now = now_unix();
    scheduler::plan_missing(cfg, &store, now)?;
    let mut rows = Vec::new();
    for repo in store.repos()? {
        let status = repo.enabled.then(|| status::read(&Git::new(&repo.path)));
        let (status, error) = match status {
            None => (None, None),
            Some(Ok(s)) => (Some(s), None),
            Some(Err(e)) => (None, Some(format!("{e:#}"))),
        };
        rows.push((repo, status, error));
    }
    let mut out = String::new();
    if json {
        let rows: Vec<_> = rows
            .iter()
            .map(|(r, s, error)| {
                serde_json::json!({
                    "path": r.path, "root": r.root,
                    "git_common_dir": r.git_common_dir, "enabled": r.enabled,
                    "branch": s.as_ref().and_then(|s| s.branch.clone()),
                    "upstream": s.as_ref().and_then(|s| s.upstream.clone()),
                    "ahead": s.as_ref().map(|s| s.ahead),
                    "behind": s.as_ref().map(|s| s.behind),
                    "dirty": s.as_ref().map(|s| s.dirty),
                    "state": s.as_ref().map(|s| s.state.label()),
                    "repo_state": r.state.map(|state| state.label()),
                    "next_run_at": r.next_run_at,
                    "fail_count": r.fail_count,
                    "quarantined": r.quarantined(),
                    "error": error,
                })
            })
            .collect();
        out = format!("{}\n", serde_json::to_string_pretty(&rows)?);
    } else {
        for (r, s, error) in &rows {
            let state = if r.enabled { "enabled " } else { "disabled" };
            out.push_str(&format!(
                "{state}  {:<10}  {}  {}  {:<12} {}\n",
                r.root,
                r.path.display(),
                detail(s.as_ref(), error.as_deref()),
                state_text(r, s.as_ref()),
                next_text(r, now)
            ));
        }
    }
    // `| head` closes stdout early; a broken pipe is a normal pipeline exit.
    doki::write_quiet(io::stdout().lock(), &out)?;
    Ok(())
}

/// Before the first run there is no persisted state, so show the live git one.
fn state_text(row: &RepoRow, status: Option<&RepoStatus>) -> &'static str {
    if let Some(state) = row.state {
        return state.label();
    }
    if !row.enabled {
        return "disabled";
    }
    match status.map(|status| &status.state) {
        Some(RepoState::Clean) => "clean",
        Some(RepoState::Dirty) => "dirty",
        Some(RepoState::Refused(_) | RepoState::Detached | RepoState::Unborn) => "blocked",
        None => "-",
    }
}

fn next_text(row: &RepoRow, now: i64) -> String {
    if row.quarantined() {
        return "quarantined".into();
    }
    match row.next_run_at {
        Some(at) if at <= now => "due now".into(),
        Some(at) => format!("next {}", span(at - now)),
        None => "-".into(),
    }
}

fn span(seconds: i64) -> String {
    if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}h{:02}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

fn detail(status: Option<&RepoStatus>, error: Option<&str>) -> String {
    let Some(status) = status else {
        return error.map_or_else(|| "-".into(), |e| format!("error: {e}"));
    };
    let branch = status.branch.clone().unwrap_or_else(|| "(detached)".into());
    let mut out = format!(
        "{branch} ↑{} ↓{}  {} dirty",
        status.ahead, status.behind, status.dirty
    );
    match &status.state {
        RepoState::Refused(reason) => out.push_str(&format!("  refused: {reason}")),
        RepoState::Unborn => out.push_str("  unborn"),
        RepoState::Detached => out.push_str("  detached"),
        RepoState::Clean | RepoState::Dirty => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_shows_branch_and_state() {
        let dirty = RepoStatus {
            branch: Some("main".into()),
            upstream: Some("origin/main".into()),
            ahead: 2,
            behind: 1,
            dirty: 3,
            state: RepoState::Dirty,
        };
        assert_eq!(detail(Some(&dirty), None), "main ↑2 ↓1  3 dirty");

        let detached = RepoStatus {
            branch: None,
            ..dirty.clone()
        };
        assert_eq!(detail(Some(&detached), None), "(detached) ↑2 ↓1  3 dirty");

        let refused = RepoStatus {
            state: RepoState::Refused("MERGE_HEAD present".into()),
            dirty: 0,
            ..dirty
        };
        assert_eq!(
            detail(Some(&refused), None),
            "main ↑2 ↓1  0 dirty  refused: MERGE_HEAD present"
        );
        assert_eq!(detail(None, None), "-");
        assert_eq!(detail(None, Some("boom")), "error: boom");
    }

    #[test]
    fn next_run_text_is_relative_and_flags_due() {
        let mut row = RepoRow {
            id: 1,
            path: PathBuf::from("/tmp/repo"),
            root: "dev".into(),
            git_common_dir: None,
            enabled: true,
            state: None,
            last_sync_at: None,
            next_run_at: Some(1_000 + 90),
            fail_count: 0,
            quarantine_until: None,
            last_error: None,
            allow_protected_push: false,
            agent: None,
            model: None,
            commit_scope: None,
        };
        assert_eq!(next_text(&row, 1_000), "next 1m");
        assert_eq!(next_text(&row, 5_000), "due now");
        row.next_run_at = Some(1_000 + 2 * 3600 + 5 * 60);
        assert_eq!(next_text(&row, 1_000), "next 2h05m");
        row.state = Some(doki_core::store::RepoState::Quarantined);
        assert_eq!(next_text(&row, 1_000), "quarantined");
        row.state = None;
        row.next_run_at = None;
        assert_eq!(next_text(&row, 1_000), "-");
    }

    #[test]
    fn state_falls_back_to_live_git_before_the_first_run() {
        let mut row = RepoRow {
            id: 1,
            path: PathBuf::from("/tmp/repo"),
            root: "dev".into(),
            git_common_dir: None,
            enabled: true,
            state: None,
            last_sync_at: None,
            next_run_at: None,
            fail_count: 0,
            quarantine_until: None,
            last_error: None,
            allow_protected_push: false,
            agent: None,
            model: None,
            commit_scope: None,
        };
        let clean = RepoStatus {
            branch: Some("main".into()),
            upstream: None,
            ahead: 0,
            behind: 0,
            dirty: 0,
            state: RepoState::Clean,
        };
        assert_eq!(state_text(&row, Some(&clean)), "clean");
        assert_eq!(state_text(&row, None), "-");
        row.enabled = false;
        assert_eq!(state_text(&row, None), "disabled");
        row.enabled = true;
        row.state = Some(doki_core::store::RepoState::WaitingUser);
        assert_eq!(state_text(&row, Some(&clean)), "waiting");
        let dirty = RepoStatus {
            dirty: 2,
            state: RepoState::Dirty,
            ..clean
        };
        row.state = None;
        assert_eq!(state_text(&row, Some(&dirty)), "dirty");
    }
}
