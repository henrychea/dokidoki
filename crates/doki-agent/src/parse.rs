use crate::prompt::FileStat;

pub const PREFIX: &str = "doki: ";
pub const MAX_SUBJECT: usize = 72;
pub const MAX_BODY: usize = 2000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub subject: String,
    pub body: String,
}

impl Message {
    pub fn full(&self) -> String {
        if self.body.is_empty() {
            format!("{}\n", self.subject)
        } else {
            format!("{}\n\n{}\n", self.subject, self.body)
        }
    }
}

/// Agent output is untrusted text: keep one subject line, drop markdown and
/// control characters, and enforce the prefix and caps doki owns.
pub fn parse(raw: &str) -> Option<Message> {
    let mut lines = raw
        .lines()
        .filter(|line| !line.trim().starts_with("```"))
        .map(clean);
    let first = lines.find(|line| !line.is_empty())?;
    Some(Message {
        subject: subject(&first)?,
        body: cap(&lines.collect::<Vec<_>>().join("\n"), MAX_BODY)
            .trim_end()
            .to_string(),
    })
}

pub fn fallback(files: &[FileStat]) -> Message {
    let mut areas: Vec<String> = files.iter().map(|f| area(&f.path)).collect();
    areas.sort();
    areas.dedup();
    areas.truncate(3);
    let text = if areas.is_empty() {
        format!("update {} files", files.len())
    } else {
        format!("update {} files in {}", files.len(), areas.join(", "))
    };
    Message {
        subject: format!("{PREFIX}{}", cap(&text, MAX_SUBJECT - PREFIX.len())),
        body: String::new(),
    }
}

fn area(path: &std::path::Path) -> String {
    path.components()
        .next()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn subject(line: &str) -> Option<String> {
    let text = without_prefix(line);
    let text = cap(text, MAX_SUBJECT - PREFIX.len());
    (!text.is_empty()).then(|| format!("{PREFIX}{text}"))
}

fn without_prefix(line: &str) -> &str {
    let rest = line.trim_start();
    match rest.get(..5) {
        Some(head) if head.eq_ignore_ascii_case("doki:") => {
            rest[5..].trim_start_matches([':', ' '])
        }
        _ => rest,
    }
}

fn cap(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let hard: String = text.chars().take(max).collect();
    let Some(cut) = hard.rfind(' ').filter(|i| *i > 0) else {
        return hard.trim_end().to_string();
    };
    let word = hard[..cut]
        .trim_end_matches([',', ';', ':', '-', '.'])
        .trim_end();
    if word.is_empty() {
        hard.trim_end().to_string()
    } else {
        word.to_string()
    }
}

fn clean(line: &str) -> String {
    let stripped: String = line.chars().filter(|c| !c.is_control()).collect();
    let mut text = stripped.trim();
    for marker in ["# ", "## ", "### ", "- ", "* ", "> "] {
        if let Some(rest) = text.strip_prefix(marker) {
            text = rest.trim_start();
            break;
        }
    }
    text.trim_matches(|c: char| matches!(c, '`' | '"' | '\'' | '*' | '“' | '”' | '‘' | '’' | '_'))
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn stat(path: &str) -> FileStat {
        FileStat {
            path: PathBuf::from(path),
            added: Some(1),
            deleted: None,
        }
    }

    #[test]
    fn parses_plain_subject_and_body() {
        let msg = parse("add widget\n\nIt now renders.\n").unwrap();
        assert_eq!(msg.subject, "doki: add widget");
        assert_eq!(msg.body, "It now renders.");
        assert_eq!(msg.full(), "doki: add widget\n\nIt now renders.\n");
    }

    #[test]
    fn prefix_is_applied_exactly_once() {
        for raw in [
            "doki: add widget",
            "doki:add widget",
            "DOKI: add widget",
            "doki: : add widget",
        ] {
            assert_eq!(parse(raw).unwrap().subject, "doki: add widget", "{raw}");
        }
    }

    #[test]
    fn strips_markdown_quotes_and_fences() {
        for raw in [
            "```\nadd widget\n```\n",
            "```text\nadd widget\n```",
            "\"add widget\"",
            "'add widget'",
            "# add widget",
            "## add widget",
            "- add widget",
            "> add widget",
            "`add widget`",
        ] {
            assert_eq!(parse(raw).unwrap().subject, "doki: add widget", "{raw}");
        }
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(parse("").is_none());
        assert!(parse("\n\n  \n").is_none());
        assert!(parse("```\n```\n").is_none());
        assert!(parse("\u{7}\u{1b}").is_none());
        assert!(parse("doki:").is_none());
    }

    #[test]
    fn control_characters_are_dropped() {
        let msg = parse("add \u{7}widget\u{1b}[0m\n\nbo\u{0}dy\n").unwrap();
        assert_eq!(msg.subject, "doki: add widget[0m");
        assert_eq!(msg.body, "body");
    }

    #[test]
    fn body_keeps_paragraph_breaks() {
        let msg = parse("add widget\n\nFirst paragraph.\n\nSecond paragraph.\n").unwrap();
        assert_eq!(msg.subject, "doki: add widget");
        assert_eq!(msg.body, "First paragraph.\n\nSecond paragraph.");
    }

    #[test]
    fn subject_is_capped_on_a_word_boundary() {
        let long = "a".repeat(300);
        let msg = parse(&long).unwrap();
        assert_eq!(msg.subject.chars().count(), MAX_SUBJECT);
        assert!(msg.subject.starts_with(PREFIX));

        let words = "word ".repeat(60);
        let msg = parse(&words).unwrap();
        assert!(msg.subject.chars().count() <= MAX_SUBJECT);
        assert!(msg.subject.starts_with("doki: word word"));
        assert!(!msg.subject.ends_with(' '));
    }

    #[test]
    fn body_is_capped() {
        let raw = format!("subject\n\n{}", "x".repeat(5000));
        let msg = parse(&raw).unwrap();
        assert_eq!(msg.body.chars().count(), MAX_BODY);
    }

    #[test]
    fn fallback_names_top_areas() {
        let files = [stat("src/a.rs"), stat("src/b.rs"), stat("docs/c.md")];
        assert_eq!(
            fallback(&files).subject,
            "doki: update 3 files in docs, src"
        );
        assert_eq!(fallback(&[]).subject, "doki: update 0 files");

        let many: Vec<FileStat> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|d| stat(&format!("{d}/f")))
            .collect();
        assert_eq!(fallback(&many).subject, "doki: update 5 files in a, b, c");
    }
}
