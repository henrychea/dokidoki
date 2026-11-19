use crate::config::{Guards, SecretScanner};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const GITLEAKS: &str = "gitleaks";
const GITLEAKS_ARGS: &[&str] = &[
    "stdin",
    "--no-banner",
    "--redact",
    "--report-format",
    "json",
    "--report-path",
    "-",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scan {
    Off,
    Clean {
        warning: Option<String>,
    },
    Finding {
        detail: String,
        warning: Option<String>,
    },
}

/// Which scanner produced the verdict, so the guard chip can name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanReport {
    pub scan: Scan,
    pub scanner: &'static str,
}

impl Scan {
    pub fn blocks(&self) -> bool {
        matches!(self, Self::Finding { .. })
    }

    pub fn finding(&self) -> Option<&str> {
        match self {
            Self::Finding { detail, .. } => Some(detail),
            Self::Off | Self::Clean { .. } => None,
        }
    }

    pub fn warning(&self) -> Option<&str> {
        match self {
            Self::Clean { warning } | Self::Finding { warning, .. } => warning.as_deref(),
            Self::Off => None,
        }
    }
}

impl ScanReport {
    pub fn blocks(&self) -> bool {
        self.scan.blocks()
    }

    pub fn summary(&self) -> String {
        match &self.scan {
            Scan::Off => "off".into(),
            Scan::Clean { .. } => "0 secrets detected".into(),
            Scan::Finding { detail, .. } => format!("blocked: {detail}"),
        }
    }
}

/// The scanner name travels with the result because a fallback to the built-in
/// rules changes what the UI may claim.
pub fn scan_report(diff: &[u8], guards: &Guards) -> ScanReport {
    scan_report_with(diff, guards, GITLEAKS)
}

fn scan_report_with(diff: &[u8], guards: &Guards, binary: &str) -> ScanReport {
    if !guards.secret_scan || guards.secret_scanner == SecretScanner::Off {
        return ScanReport {
            scan: Scan::Off,
            scanner: "off",
        };
    }
    match guards.secret_scanner {
        SecretScanner::Off => ScanReport {
            scan: Scan::Off,
            scanner: "off",
        },
        SecretScanner::Gitleaks => convey(gitleaks(diff, binary), diff),
        SecretScanner::Trufflehog => ScanReport {
            scan: builtin(diff, "trufflehog scanning is not implemented yet"),
            scanner: "built-in",
        },
    }
}

fn convey(scanned: Result<Scan, String>, diff: &[u8]) -> ScanReport {
    match scanned {
        Ok(scan) => ScanReport {
            scan,
            scanner: "gitleaks",
        },
        Err(reason) => ScanReport {
            scan: builtin(diff, &reason),
            scanner: "built-in",
        },
    }
}

pub fn scan(diff: &[u8], guards: &Guards) -> Scan {
    scan_report(diff, guards).scan
}

/// A finding blocks the commit, so this reports rule and line and never the secret.
const KEY_NAMES: &[&str] = &[
    "api_key",
    "apikey",
    "secret",
    "token",
    "password",
    "passwd",
    "access_key",
];

fn gitleaks(diff: &[u8], binary: &str) -> Result<Scan, String> {
    let path = locate(binary).ok_or_else(|| format!("{binary} is not on PATH"))?;
    let mut child = Command::new(&path)
        .args(GITLEAKS_ARGS)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run {}: {e}", path.display()))?;
    let mut stdin = child.stdin.take().ok_or("gitleaks stdin unavailable")?;
    stdin
        .write_all(diff)
        .map_err(|e| format!("could not write the diff to gitleaks: {e}"))?;
    drop(stdin);
    let out = child
        .wait_with_output()
        .map_err(|e| format!("gitleaks did not finish: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let findings = gitleaks_findings(&stdout);
    if !findings.is_empty() {
        return Ok(Scan::Finding {
            detail: format!("gitleaks: {}", findings.join("; ")),
            warning: None,
        });
    }
    if out.status.success() {
        Ok(Scan::Clean { warning: None })
    } else {
        Err(format!(
            "gitleaks exited {} without a report",
            out.status.code().unwrap_or(-1)
        ))
    }
}

fn gitleaks_findings(stdout: &str) -> Vec<String> {
    let Ok(serde_json::Value::Array(items)) = serde_json::from_str(stdout) else {
        return Vec::new();
    };
    items
        .iter()
        .map(|item| {
            let rule = item
                .get("RuleID")
                .and_then(|v| v.as_str())
                .unwrap_or("secret");
            let file = item.get("File").and_then(|v| v.as_str()).unwrap_or("diff");
            let line = item
                .get("StartLine")
                .and_then(|v| v.as_u64())
                .map(|n| format!(":{n}"))
                .unwrap_or_default();
            format!("{rule} in {file}{line}")
        })
        .collect()
}

fn builtin(diff: &[u8], reason: &str) -> Scan {
    let warning = Some(format!("{reason}; used doki's built-in secret scan"));
    let text = String::from_utf8_lossy(diff);
    let findings = builtin_findings(&text);
    if findings.is_empty() {
        Scan::Clean { warning }
    } else {
        Scan::Finding {
            detail: findings.join("; "),
            warning,
        }
    }
}

fn builtin_findings(text: &str) -> Vec<String> {
    let mut findings = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        let lower = line.to_ascii_lowercase();
        if lower.contains("-----begin ") && lower.contains("private key-----") {
            findings.push(format!("line {number}: private key header"));
            continue;
        }
        let mut rules = Vec::new();
        if let Some(rest) = after(line, "AKIA")
            && prefixed_ascii(rest, 16, |c| c.is_ascii_uppercase() || c.is_ascii_digit())
        {
            rules.push("aws access key id");
        }
        if let Some(rest) = after(line, "ghp_")
            && prefixed_ascii(rest, 36, |c| c.is_ascii_alphanumeric())
        {
            rules.push("github token");
        }
        if let Some(rest) = after(line, "github_pat_")
            && prefixed_ascii(rest, 22, |c| c.is_ascii_alphanumeric() || c == '_')
        {
            rules.push("github fine-grained token");
        }
        if has_slack_token(line) {
            rules.push("slack token");
        }
        if has_key_value_secret(line) {
            rules.push("key/value credential");
        }
        if !rules.is_empty() {
            findings.push(format!("line {number}: {}", rules.join(", ")));
        }
    }
    findings
}

fn after<'a>(text: &'a str, marker: &str) -> Option<&'a str> {
    text.find(marker).map(|at| &text[at + marker.len()..])
}

fn prefixed_ascii(rest: &str, len: usize, accept: impl Fn(char) -> bool) -> bool {
    rest.chars().take(len).count() == len && rest.chars().take(len).all(accept)
}

fn has_slack_token(line: &str) -> bool {
    let mut rest = line;
    while let Some(at) = rest.find("xox") {
        let tail = &rest[at + 3..];
        let mut chars = tail.chars();
        if chars.next().is_some_and(|c| "baprs".contains(c))
            && chars.next() == Some('-')
            && chars
                .take(10)
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                .count()
                == 10
        {
            return true;
        }
        rest = &rest[at + 3..];
    }
    false
}

fn has_key_value_secret(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    for key in KEY_NAMES {
        let mut from = 0;
        while let Some(found) = lower[from..].find(key) {
            let at = from + found;
            let rest = line[at + key.len()..].trim_start();
            let Some(rest) = rest.strip_prefix(':').or_else(|| rest.strip_prefix('=')) else {
                from = at + key.len();
                continue;
            };
            let rest = rest.trim_start_matches([' ', '\t']);
            let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'');
            if let Some(quote) = quote {
                let value = &rest[quote.len_utf8()..];
                if value.chars().take_while(|c| *c != quote).count() >= 12 {
                    return true;
                }
            }
            from = at + key.len();
        }
    }
    false
}

fn locate(binary: &str) -> Option<PathBuf> {
    if binary.contains('/') {
        let path = PathBuf::from(binary);
        return is_executable(&path).then_some(path);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(binary))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    path.is_file()
        && path
            .metadata()
            .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guards(scanner: SecretScanner) -> Guards {
        Guards {
            secret_scan: true,
            secret_scanner: scanner,
            ..Guards::default()
        }
    }

    #[test]
    fn built_in_scan_finds_the_common_shapes() {
        let cases = [
            "+aws = \"AKIA0000000000000000\"\n",
            "+-----BEGIN OPENSSH PRIVATE KEY-----\n",
            "+token = \"ghp_000000000000000000000000000000000000\"\n",
            "+github_pat_0000000000000000000000\n",
            "+slack = xoxb-0000000000\n",
            "+api_key = \"0123456789abcdef\"\n",
            "+password: 'correct horse battery'\n",
        ];
        for diff in cases {
            let scan = builtin(diff.as_bytes(), "gitleaks is not on PATH");
            assert!(
                matches!(scan, Scan::Finding { .. }),
                "missed {diff:?}: {scan:?}"
            );
        }

        let clean = "+let total = 3;\n+fn password_policy() {}\n+password = \"short\"\n";
        assert!(matches!(
            builtin(clean.as_bytes(), "no gitleaks"),
            Scan::Clean { .. }
        ));
    }

    #[test]
    fn findings_mask_the_secret() {
        let scan = builtin(b"+key = \"AKIA0000000000000000\"\n", "no gitleaks");
        let Scan::Finding { detail, .. } = scan else {
            panic!("expected a finding");
        };
        assert!(!detail.contains("0000000000000000"));
        assert!(detail.contains("aws access key id"));
    }

    #[test]
    fn scanner_settings_short_circuit() {
        assert_eq!(
            scan(b"AKIA0000000000000000", &guards(SecretScanner::Off)),
            Scan::Off
        );
        let off = Guards {
            secret_scan: false,
            ..guards(SecretScanner::Gitleaks)
        };
        assert_eq!(scan(b"AKIA0000000000000000", &off), Scan::Off);
    }

    #[test]
    fn gitleaks_json_report_blocks_and_empty_report_passes() {
        let tmp = tempfile::tempdir().unwrap();
        let report = |name: &str, body: &str| {
            let path = tmp.path().join(name);
            std::fs::write(&path, body).unwrap();
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
            path
        };
        let blocked = report(
            "blocked",
            "#!/bin/sh\ncat >/dev/null\nprintf '[{\"RuleID\":\"aws-access-token\",\"File\":\"a.env\",\"StartLine\":2}]'\nexit 1\n",
        );
        let clean = report("clean", "#!/bin/sh\ncat >/dev/null\nprintf '[]'\n");

        let found = scan_report_with(
            b"AKIA0000000000000000",
            &guards(SecretScanner::Gitleaks),
            blocked.to_str().unwrap(),
        )
        .scan;
        let Scan::Finding { detail, warning } = found else {
            panic!("expected gitleaks finding, got {found:?}");
        };
        assert!(detail.contains("aws-access-token in a.env:2"), "{detail}");
        assert!(warning.is_none());

        assert_eq!(
            scan_report_with(
                b"+nothing here\n",
                &guards(SecretScanner::Gitleaks),
                clean.to_str().unwrap()
            )
            .scan,
            Scan::Clean { warning: None }
        );
    }

    #[test]
    fn a_broken_gitleaks_falls_back_to_the_built_in_scan() {
        let tmp = tempfile::tempdir().unwrap();
        let broken = tmp.path().join("gitleaks");
        std::fs::write(&broken, "#!/bin/sh\ncat >/dev/null\nexit 3\n").unwrap();
        let mut perms = std::fs::metadata(&broken).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&broken, perms).unwrap();

        let scan = scan_report_with(
            b"+AKIA0000000000000000\n",
            &guards(SecretScanner::Gitleaks),
            broken.to_str().unwrap(),
        )
        .scan;
        let Scan::Finding { detail, warning } = scan else {
            panic!("expected a built-in finding, got {scan:?}");
        };
        assert!(detail.contains("aws access key id"));
        assert!(warning.unwrap_or_default().contains("built-in secret scan"));
    }
}
