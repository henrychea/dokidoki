#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

impl Level {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub check: String,
    pub level: Level,
    pub detail: String,
}

impl Probe {
    pub fn new(check: impl Into<String>, level: Level, detail: impl Into<String>) -> Self {
        Self {
            check: check.into(),
            level,
            detail: detail.into(),
        }
    }
}

pub fn failed(probes: &[Probe]) -> bool {
    probes.iter().any(|probe| probe.level == Level::Fail)
}

pub fn render(probes: &[Probe]) -> String {
    let check_width = probes
        .iter()
        .map(|probe| probe.check.chars().count())
        .chain(std::iter::once("CHECK".len()))
        .max()
        .unwrap_or(5);
    let mut out = format!("{:<6}  {:<check_width$}  DETAIL\n", "STATUS", "CHECK");
    for probe in probes {
        out.push_str(&format!(
            "{:<6}  {:<check_width$}  {}\n",
            probe.level.label(),
            probe.check,
            probe.detail
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probes() -> Vec<Probe> {
        vec![
            Probe::new("git", Level::Ok, "git version 2.51.0"),
            Probe::new("gitleaks", Level::Warn, "not found"),
            Probe::new("state dir", Level::Fail, "permission denied"),
        ]
    }

    #[test]
    fn renders_an_aligned_table_and_fails_on_a_fail_row() {
        let text = render(&probes());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "STATUS  CHECK      DETAIL");
        assert_eq!(lines[1], "OK      git        git version 2.51.0");
        assert_eq!(lines[2], "WARN    gitleaks   not found");
        assert_eq!(lines[3], "FAIL    state dir  permission denied");
        assert!(failed(&probes()));
        assert!(!failed(&probes()[..2]));
    }

    #[test]
    fn an_empty_report_still_has_a_header() {
        let text = render(&[]);
        assert_eq!(text, "STATUS  CHECK  DETAIL\n");
        assert!(!failed(&[]));
    }
}
