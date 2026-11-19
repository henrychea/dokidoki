use crate::config::{Config, home};
use crate::proc;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const MARKER: &str = "# doki-managed: doki install-timer wrote this file";
pub const FALLBACK_CALENDAR: &str = "*:0/20";
const SERVICE_FILE: &str = "doki.service";
const TIMER_FILE: &str = "doki.timer";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Units {
    pub service: String,
    pub timer: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimerState {
    #[default]
    Absent,
    Installed,
    Active,
}

impl TimerState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Absent => "not installed",
            Self::Installed => "installed, inactive",
            Self::Active => "active",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallOutcome {
    pub dir: PathBuf,
    pub service: PathBuf,
    pub timer: PathBuf,
    pub calendar: String,
    pub binary: PathBuf,
    pub backups: Vec<PathBuf>,
    pub unchanged: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallOutcome {
    pub removed: Vec<PathBuf>,
    pub kept: Vec<PathBuf>,
}

pub fn unit_dir() -> PathBuf {
    config_home().join("systemd/user")
}

fn config_home() -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => home().join(".config"),
    }
}

/// The most frequent enabled root schedule drives the shared timer.
pub fn calendar(cfg: &Config) -> String {
    let mut best: Option<(i64, &str)> = None;
    for root in cfg.roots.iter().filter(|root| root.enabled) {
        let Some(interval) = interval_seconds(&root.schedule) else {
            continue;
        };
        if best.is_none_or(|(current, _)| interval < current) {
            best = Some((interval, root.schedule.as_str()));
        }
    }
    best.and_then(|(_, schedule)| calendar_from_cron(schedule))
        .unwrap_or_else(|| FALLBACK_CALENDAR.to_string())
}

fn interval_seconds(expression: &str) -> Option<i64> {
    let first = crate::scheduler::next_occurrence(expression, 1_700_000_000).ok()?;
    let second = crate::scheduler::next_occurrence(expression, first).ok()?;
    Some(second - first)
}

pub fn calendar_from_cron(expression: &str) -> Option<String> {
    let fields: Vec<&str> = expression.split_whitespace().collect();
    if fields.len() != 5 {
        return None;
    }
    let (minute, hour, day, month, weekday) =
        (fields[0], fields[1], fields[2], fields[3], fields[4]);
    if month != "*" || weekday != "*" {
        return None;
    }
    if let Some(step) = minute.strip_prefix("*/").filter(|step| valid_step(step)) {
        if day == "*" && hour == "*" {
            return Some(format!("*:0/{step}"));
        }
        return None;
    }
    let minute = parse_number(minute, 59)?;
    if let Some(step) = hour.strip_prefix("*/").filter(|step| valid_step(step)) {
        if day == "*" {
            return Some(format!("*-*-* 0/{step}:{minute:02}:00"));
        }
        return None;
    }
    let hour = parse_number(hour, 23)?;
    if day == "*" {
        return Some(format!("*-*-* {hour}:{minute:02}:00"));
    }
    let day = parse_number(day, 31)?;
    Some(format!("*-*-{day} {hour}:{minute:02}:00"))
}

fn valid_step(step: &str) -> bool {
    step.parse::<u32>().is_ok_and(|value| value > 0)
}

fn parse_number(field: &str, max: u32) -> Option<u32> {
    field.parse::<u32>().ok().filter(|value| *value <= max)
}

pub fn units_for(binary: &Path, calendar: &str) -> Units {
    let binary = binary.display();
    Units {
        service: format!(
            "[Unit]\nDescription=dokidoki git sync\n{MARKER}\n\n\
             [Service]\nType=oneshot\nExecStart={binary} sync --due\n"
        ),
        timer: format!(
            "[Unit]\nDescription=dokidoki sync timer\n{MARKER}\n\n\
             [Timer]\nOnCalendar={calendar}\nPersistent=true\n\n\
             [Install]\nWantedBy=timers.target\n"
        ),
    }
}

pub fn install(cfg: &Config, binary: &Path, force: bool) -> Result<InstallOutcome> {
    let binary = binary
        .canonicalize()
        .unwrap_or_else(|_| binary.to_path_buf());
    let calendar = calendar(cfg);
    let units = units_for(&binary, &calendar);
    let dir = unit_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let service = dir.join(SERVICE_FILE);
    let timer = dir.join(TIMER_FILE);

    let planned = [(&service, &units.service), (&timer, &units.timer)];
    let mut writes = Vec::new();
    let mut backups = Vec::new();
    let mut unchanged = Vec::new();
    for (path, contents) in planned {
        match std::fs::read_to_string(path) {
            Ok(existing) if existing == *contents => unchanged.push(path.clone()),
            Ok(existing) => {
                if !force {
                    bail!(
                        "{} already exists with different content; rerun with --force-write to replace it",
                        path.display()
                    );
                }
                writes.push((path, contents, Some(existing)));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                writes.push((path, contents, None));
            }
            Err(error) => {
                return Err(error).with_context(|| format!("read {}", path.display()));
            }
        }
    }
    for (path, contents, existing) in writes {
        if let Some(existing) = existing {
            let backup = backup_path(path);
            std::fs::write(&backup, existing)
                .with_context(|| format!("write {}", backup.display()))?;
            backups.push(backup);
        }
        std::fs::write(path, contents).with_context(|| format!("write {}", path.display()))?;
    }
    Ok(InstallOutcome {
        dir,
        service,
        timer,
        calendar,
        binary,
        backups,
        unchanged,
    })
}

pub fn uninstall() -> Result<UninstallOutcome> {
    let dir = unit_dir();
    let mut removed = Vec::new();
    let mut kept = Vec::new();
    for name in [TIMER_FILE, SERVICE_FILE] {
        let path = dir.join(name);
        match std::fs::read_to_string(&path) {
            Ok(text) if text.contains(MARKER) => {
                std::fs::remove_file(&path)
                    .with_context(|| format!("remove {}", path.display()))?;
                removed.push(path);
            }
            Ok(_) => kept.push(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        }
    }
    Ok(UninstallOutcome { removed, kept })
}

fn backup_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| format!("{}.bak", name.to_string_lossy()))
        .unwrap_or_else(|| "doki.bak".into());
    path.with_file_name(name)
}

pub fn status() -> TimerState {
    if !unit_dir().join(TIMER_FILE).exists() {
        return TimerState::Absent;
    }
    match active() {
        Some(true) => TimerState::Active,
        _ => TimerState::Installed,
    }
}

/// Read-only, never enable or start anything from inside doki.
pub fn active() -> Option<bool> {
    let (_, text) = proc::capture(
        "systemctl",
        &["--user", "is-active", "doki.timer"],
        Duration::from_secs(2),
    )?;
    Some(text.trim() == "active")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_minute_and_hour_steps_to_systemd_calendars() {
        assert_eq!(
            calendar_from_cron("*/20 * * * *").as_deref(),
            Some("*:0/20")
        );
        assert_eq!(
            calendar_from_cron("0 */2 * * *").as_deref(),
            Some("*-*-* 0/2:00:00")
        );
        assert_eq!(
            calendar_from_cron("30 6 * * *").as_deref(),
            Some("*-*-* 6:30:00")
        );
        assert_eq!(
            calendar_from_cron("15 3 1 * *").as_deref(),
            Some("*-*-1 3:15:00")
        );
        assert_eq!(calendar_from_cron("5 4 * * 1"), None);
        assert_eq!(
            calendar_from_cron("0 0 * * *"),
            Some("*-*-* 0:00:00".into())
        );
        assert_eq!(calendar_from_cron("not a cron"), None);
        assert_eq!(calendar_from_cron("*/0 * * * *"), None);
        assert_eq!(calendar_from_cron("70 * * * *"), None);
    }

    #[test]
    fn the_smallest_root_schedule_wins_and_invalid_falls_back() {
        let cfg = Config::parse(
            "[[roots]]\nname=\"a\"\npath=\"/tmp/a\"\nschedule=\"0 */2 * * *\"\n\
             [[roots]]\nname=\"b\"\npath=\"/tmp/b\"\nschedule=\"*/20 * * * *\"\n",
        )
        .unwrap();
        assert_eq!(calendar(&cfg), "*:0/20");

        let cfg = Config::parse("[[roots]]\nname=\"a\"\npath=\"/tmp/a\"\nschedule=\"5 4 * * 1\"\n")
            .unwrap();
        assert_eq!(calendar(&cfg), FALLBACK_CALENDAR);

        let cfg = Config::parse(
            "[[roots]]\nname=\"a\"\npath=\"/tmp/a\"\nschedule=\"*/20 * * * *\"\nenabled=false\n",
        )
        .unwrap();
        assert_eq!(calendar(&cfg), FALLBACK_CALENDAR);
    }

    #[test]
    fn unit_content_is_stable_and_carries_the_marker() {
        let units = units_for(Path::new("/opt/bin/doki"), "*:0/20");
        let expected_service = "\
[Unit]
Description=dokidoki git sync
# doki-managed: doki install-timer wrote this file

[Service]
Type=oneshot
ExecStart=/opt/bin/doki sync --due
";
        let expected_timer = "\
[Unit]
Description=dokidoki sync timer
# doki-managed: doki install-timer wrote this file

[Timer]
OnCalendar=*:0/20
Persistent=true

[Install]
WantedBy=timers.target
";
        assert_eq!(units.service, expected_service);
        assert_eq!(units.timer, expected_timer);
    }
}
