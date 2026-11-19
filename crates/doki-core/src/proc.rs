use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Version and status probes must never hang a CLI or the UI thread.
pub fn capture(program: &str, args: &[&str], timeout: Duration) -> Option<(bool, String)> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = child.wait_with_output().ok()?;
                let mut text = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if text.is_empty() {
                    text = String::from_utf8_lossy(&output.stderr).trim().to_string();
                }
                return Some((status.success(), first_line(&text)));
            }
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Err(_) => return None,
        }
    }
}

pub fn version(program: &str, flag: &str) -> Option<String> {
    capture(program, &[flag], Duration::from_secs(2)).map(|(_, text)| text)
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_versions_and_reports_failures() {
        let (ok, text) = capture(
            "/bin/sh",
            &["-c", "echo first; echo second"],
            Duration::from_secs(2),
        )
        .expect("sh runs");
        assert!(ok);
        assert_eq!(text, "first");

        let (ok, text) = capture(
            "/bin/sh",
            &["-c", "echo bad >&2; exit 3"],
            Duration::from_secs(2),
        )
        .expect("sh runs");
        assert!(!ok);
        assert_eq!(text, "bad");
        assert!(capture("doki-no-such-binary-xyz", &[], Duration::from_secs(1)).is_none());
    }

    #[test]
    fn a_hanging_child_is_killed() {
        let started = Instant::now();
        let result = capture("/bin/sh", &["-c", "sleep 5"], Duration::from_millis(80));
        assert!(result.is_none());
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
