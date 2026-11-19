use crate::write_quiet;
use anyhow::Result;
use doki_agent::adapter::on_path;
use doki_agent::resolve::resolve_id;
use doki_core::config::{Config, SecretScanner};
use doki_core::doctor::{self, Level, Probe};
use doki_core::proc;
use doki_core::timer::{self, TimerState};
use std::io;
use std::path::Path;
use std::time::Duration;

pub fn run(config: Result<Config>, cfg_path: &Path, state_dir: &Path) -> Result<bool> {
    let (cfg, config_probe) = match config {
        Ok(cfg) if cfg_path.exists() => {
            let probe = Probe::new("config", Level::Ok, cfg_path.display().to_string());
            (cfg, probe)
        }
        Ok(cfg) => (
            cfg,
            Probe::new("config", Level::Warn, "not found; defaults are in use"),
        ),
        Err(error) => (
            Config::default(),
            Probe::new("config", Level::Fail, format!("{error:#}")),
        ),
    };

    let mut probes = vec![config_probe];
    probes.push(git());
    probes.push(gitleaks(&cfg));
    probes.extend(agents(&cfg));
    probes.push(state_directory(state_dir));
    probes.push(inotify());
    probes.push(notifications());
    probes.push(timer());
    let text = doctor::render(&probes);
    write_quiet(io::stdout().lock(), &text)?;
    Ok(!doctor::failed(&probes))
}

fn git() -> Probe {
    match proc::version("git", "--version") {
        Some(version) => Probe::new("git", Level::Ok, version),
        None => Probe::new("git", Level::Fail, "git is not on PATH"),
    }
}

fn gitleaks(cfg: &Config) -> Probe {
    let wanted = cfg.guards.secret_scan && cfg.guards.secret_scanner == SecretScanner::Gitleaks;
    if let Some(version) = proc::version("gitleaks", "version") {
        return Probe::new("gitleaks", Level::Ok, version);
    }
    if wanted {
        Probe::new(
            "gitleaks",
            Level::Warn,
            "not found; the built-in scanner will be used",
        )
    } else {
        Probe::new("gitleaks", Level::Ok, "not required by the current guards")
    }
}

fn agents(cfg: &Config) -> Vec<Probe> {
    let mut probes = Vec::new();
    for id in cfg.agent_ids() {
        let check = format!("agent {id}");
        let Ok(agent) = resolve_id(cfg, &id) else {
            probes.push(Probe::new(
                check,
                Level::Warn,
                "configuration is incomplete",
            ));
            continue;
        };
        if !on_path(&agent.cli) {
            probes.push(Probe::new(
                check,
                Level::Warn,
                format!("{} not found on PATH", agent.cli),
            ));
            continue;
        }
        match proc::version(&agent.cli, "--version") {
            Some(version) => probes.push(Probe::new(check, Level::Ok, version)),
            None => probes.push(Probe::new(
                check,
                Level::Warn,
                format!("{} does not answer --version", agent.cli),
            )),
        }
    }
    probes
}

fn state_directory(dir: &Path) -> Probe {
    let result = std::fs::create_dir_all(dir).and_then(|()| {
        let probe = dir.join(format!(".doctor-{}", std::process::id()));
        std::fs::write(&probe, b"doki")?;
        std::fs::remove_file(probe)
    });
    match result {
        Ok(()) => Probe::new("state dir", Level::Ok, dir.display().to_string()),
        Err(error) => Probe::new(
            "state dir",
            Level::Fail,
            format!("{error}: {}", dir.display()),
        ),
    }
}

fn inotify() -> Probe {
    let path = "/proc/sys/fs/inotify/max_user_watches";
    match std::fs::read_to_string(path) {
        Ok(text) => match text.trim().parse::<u64>() {
            Ok(watches) if watches >= 8192 => {
                Probe::new("inotify", Level::Ok, format!("max_user_watches {watches}"))
            }
            Ok(watches) => Probe::new(
                "inotify",
                Level::Warn,
                format!("max_user_watches {watches} is low; raise it for large trees"),
            ),
            Err(_) => Probe::new("inotify", Level::Warn, "unreadable watch limit"),
        },
        Err(error) => Probe::new("inotify", Level::Warn, format!("{error}")),
    }
}

fn notifications() -> Probe {
    match notification_daemon() {
        Some(true) => Probe::new("notify", Level::Ok, "org.freedesktop.Notifications present"),
        Some(false) => Probe::new(
            "notify",
            Level::Warn,
            "no notification daemon; incidents stay in the journal",
        ),
        None => Probe::new("notify", Level::Warn, "cannot query the session bus"),
    }
}

fn notification_daemon() -> Option<bool> {
    if let Some((_, text)) = proc::capture(
        "busctl",
        &[
            "--user",
            "--no-pager",
            "call",
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            "s",
            "org.freedesktop.Notifications",
        ],
        Duration::from_secs(2),
    ) {
        return Some(text.contains("true"));
    }
    proc::capture(
        "gdbus",
        &[
            "call",
            "--session",
            "--dest",
            "org.freedesktop.DBus",
            "--object-path",
            "/org/freedesktop/DBus",
            "--method",
            "org.freedesktop.DBus.NameHasOwner",
            "org.freedesktop.Notifications",
        ],
        Duration::from_secs(2),
    )
    .map(|(_, text)| text.contains("true"))
}

fn timer() -> Probe {
    match timer::status() {
        TimerState::Active => Probe::new("systemd timer", Level::Ok, "doki.timer is active"),
        TimerState::Installed => Probe::new(
            "systemd timer",
            Level::Warn,
            "doki.timer is installed but inactive; enable it",
        ),
        TimerState::Absent => Probe::new(
            "systemd timer",
            Level::Warn,
            "not installed; run `doki install-timer` for syncs without the UI",
        ),
    }
}
