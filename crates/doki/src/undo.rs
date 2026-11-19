use crate::write_quiet;
use anyhow::{Result, bail};
use doki_core::store::Store;
use doki_core::undo::{self, UndoReport, UndoState};
use std::io;
use std::path::Path;

pub fn run(store: &Store, repo: &Path) -> Result<()> {
    let report = undo::undo(store, repo)?;
    write_quiet(io::stdout().lock(), &report_text(&report))?;
    if report.state == UndoState::Refused {
        bail!("{}", report.detail);
    }
    Ok(())
}

fn report_text(report: &UndoReport) -> String {
    let mut out = format!(
        "repo:   {}\nstate:  {}\n",
        report.repo.display(),
        report.state.label()
    );
    if let Some(run_id) = report.run_id {
        out.push_str(&format!("run:    {run_id}\n"));
    }
    if let (Some(from), Some(to)) = (&report.from, &report.to) {
        out.push_str(&format!("head:   {} -> {}\n", short(from), short(to)));
    }
    out.push_str(&format!("detail: {}\n", report.detail));
    out
}

fn short(sha: &str) -> String {
    sha.chars().take(12).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_text_names_the_run_and_both_heads() {
        let report = UndoReport {
            repo: "/tmp/repo".into(),
            state: UndoState::Undone,
            run_id: Some(7),
            from: Some("abcdef1234567890".into()),
            to: Some("0123456789abcdef".into()),
            detail: "restored 0123456789ab".into(),
        };
        let text = report_text(&report);
        assert!(text.contains("state:  undone"), "{text}");
        assert!(text.contains("run:    7"), "{text}");
        assert!(
            text.contains("head:   abcdef123456 -> 0123456789ab"),
            "{text}"
        );
    }
}
