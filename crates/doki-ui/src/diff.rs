use std::path::PathBuf;

/// Lines kept per file; a diff that big is unreadable and expensive to lay out.
pub const MAX_LINES_PER_FILE: usize = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
    Meta,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub header: String,
    pub section: String,
    pub old_start: u32,
    pub new_start: u32,
    pub old_cursor: u32,
    pub new_cursor: u32,
    pub lines: Vec<DiffLine>,
}

impl Hunk {
    pub fn label(&self) -> String {
        if self.section.is_empty() {
            self.header.clone()
        } else {
            format!("{} {}", self.header, self.section)
        }
    }

    /// Appends one body line and advances the gutters it uses.
    fn push(&mut self, kind: LineKind, text: &str) -> DiffLine {
        let line = DiffLine {
            kind,
            old: matches!(kind, LineKind::Context | LineKind::Removed).then_some(self.old_cursor),
            new: matches!(kind, LineKind::Context | LineKind::Added).then_some(self.new_cursor),
            text: text.to_string(),
        };
        if line.old.is_some() {
            self.old_cursor += 1;
        }
        if line.new.is_some() {
            self.new_cursor += 1;
        }
        self.lines.push(line.clone());
        line
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilePatch {
    pub path: PathBuf,
    pub old_path: Option<PathBuf>,
    pub binary: bool,
    pub rename: bool,
    pub added: u64,
    pub removed: u64,
    pub hunks: Vec<Hunk>,
    pub truncated: bool,
}

impl FilePatch {
    pub fn header_label(&self) -> String {
        self.hunks.first().map(Hunk::label).unwrap_or_else(|| {
            if self.binary {
                "binary file".into()
            } else if self.rename {
                "renamed".into()
            } else {
                String::new()
            }
        })
    }

    pub fn line_count(&self) -> usize {
        self.hunks.iter().map(|hunk| hunk.lines.len()).sum()
    }

    pub fn renamed_from(&self) -> Option<String> {
        self.old_path
            .as_ref()
            .filter(|_| self.rename)
            .map(|path| path.display().to_string())
    }

    pub fn is_lockfile(&self) -> bool {
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        name.ends_with(".lock")
            || name == "package-lock.json"
            || name == "yarn.lock"
            || name == "pnpm-lock.yaml"
            || name == "cargo.lock"
            || name == "go.sum"
            || name == "flake.lock"
    }
}

/// Parses the unified output of `git diff` into one patch per file.
pub fn parse(text: &str) -> Vec<FilePatch> {
    let mut files: Vec<FilePatch> = Vec::new();
    for line in text.split('\n') {
        if let Some(path) = header_path(line) {
            files.push(FilePatch {
                path,
                ..FilePatch::default()
            });
            continue;
        }
        let Some(file) = files.last_mut() else {
            continue;
        };
        if file.binary {
            continue;
        }
        if let Some(from) = line.strip_prefix("rename from ") {
            file.rename = true;
            file.old_path = Some(PathBuf::from(unquote(from)));
        } else if let Some(to) = line.strip_prefix("rename to ") {
            file.rename = true;
            file.path = PathBuf::from(unquote(to));
        } else if line.starts_with("deleted file mode ") {
            if let Some(old) = file.old_path.clone() {
                file.path = old;
            }
        } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
            file.binary = true;
        } else if let Some((old_start, new_start)) = hunk_start(line) {
            file.hunks.push(Hunk {
                header: hunk_header(line),
                section: hunk_section(line),
                old_start,
                new_start,
                old_cursor: old_start,
                new_cursor: new_start,
                lines: Vec::new(),
            });
        } else if let Some((kind, body)) = body_line(line)
            && file.hunks.last().is_some()
        {
            match kind {
                LineKind::Added => file.added += 1,
                LineKind::Removed => file.removed += 1,
                LineKind::Context | LineKind::Meta => {}
            }
            if file.line_count() >= MAX_LINES_PER_FILE {
                file.truncated = true;
            } else if let Some(hunk) = file.hunks.last_mut() {
                hunk.push(kind, body);
            }
        }
    }
    files
}

fn body_line(line: &str) -> Option<(LineKind, &str)> {
    if let Some(rest) = line.strip_prefix('+') {
        Some((LineKind::Added, rest))
    } else if let Some(rest) = line.strip_prefix('-') {
        Some((LineKind::Removed, rest))
    } else if let Some(rest) = line.strip_prefix(' ') {
        Some((LineKind::Context, rest))
    } else if line.starts_with('\\') {
        Some((LineKind::Meta, line.trim_start_matches('\\')))
    } else {
        None
    }
}

fn hunk_start(line: &str) -> Option<(u32, u32)> {
    if !line.starts_with("@@ ") {
        return None;
    }
    let mut fields = line.split_whitespace();
    fields.next()?;
    let old = range_start(fields.next()?.strip_prefix('-')?)?;
    let new = range_start(fields.next()?.strip_prefix('+')?)?;
    Some((old, new))
}

fn range_start(range: &str) -> Option<u32> {
    let start = range.split_once(',').map_or(range, |(start, _)| start);
    start.parse().ok()
}

fn hunk_header(line: &str) -> String {
    let rest = line.strip_prefix("@@").unwrap_or(line);
    match rest.find("@@") {
        Some(at) => format!("@@{}@@", &rest[..at]),
        None => line.to_string(),
    }
}

fn hunk_section(line: &str) -> String {
    let mut rest = line;
    for _ in 0..2 {
        match rest.find("@@") {
            Some(at) => rest = &rest[at + 2..],
            None => return String::new(),
        }
    }
    rest.trim_start().to_string()
}

fn header_path(line: &str) -> Option<PathBuf> {
    let rest = line.strip_prefix("diff --git ")?;
    let (_, rest) = take_path(rest)?;
    let (b, _) = take_path(rest.trim_start()).or_else(|| take_path(rest))?;
    Some(PathBuf::from(strip_prefix(&unquote(&b), "b/")))
}

/// Splits one diff path off the front, aware that git quotes names with spaces.
fn take_path(text: &str) -> Option<(String, &str)> {
    if !text.starts_with('"') {
        let at = text.find(' ').unwrap_or(text.len());
        return Some((text[..at].to_string(), &text[at..]));
    }
    let mut escaped = false;
    for (index, ch) in text.char_indices().skip(1) {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => return Some((text[..=index].to_string(), &text[index + 1..])),
            _ => {}
        }
    }
    None
}

fn strip_prefix(path: &str, prefix: &str) -> String {
    path.strip_prefix(prefix).unwrap_or(path).to_string()
}

fn unquote(text: &str) -> String {
    let text = text.trim();
    let Some(inner) = text.strip_prefix('"').and_then(|t| t.strip_suffix('"')) else {
        return text.to_string();
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIMPLE: &str = "\
diff --git a/src/checkout.rs b/src/checkout.rs
index 111..222 100644
--- a/src/checkout.rs
+++ b/src/checkout.rs
@@ -128,3 +128,5 @@ impl SubscriptionHandler {
     pub async fn process(&self) {
-        let sub_id = payload.get(\"id\");
+        let key = event.key();
+        if self.journal.has(key) {
     }
";

    #[test]
    fn parses_context_removed_and_added_with_line_numbers() {
        let files = parse(SIMPLE);
        assert_eq!(files.len(), 1);
        let file = &files[0];
        assert_eq!(file.path, PathBuf::from("src/checkout.rs"));
        assert_eq!((file.added, file.removed), (2, 1));
        assert!(!file.binary && !file.rename);

        let lines = &file.hunks[0].lines;
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0].kind, LineKind::Context);
        assert_eq!((lines[0].old, lines[0].new), (Some(128), Some(128)));
        assert_eq!(lines[1].kind, LineKind::Removed);
        assert_eq!((lines[1].old, lines[1].new), (Some(129), None));
        assert_eq!(lines[2].kind, LineKind::Added);
        assert_eq!((lines[2].old, lines[2].new), (None, Some(129)));
        assert_eq!(lines[3].kind, LineKind::Added);
        assert_eq!((lines[3].old, lines[3].new), (None, Some(130)));
        assert_eq!(lines[1].text, "        let sub_id = payload.get(\"id\");");
        assert_eq!(lines[2].text, "        let key = event.key();");
        assert_eq!(
            (file.hunks[0].old_start, file.hunks[0].new_start),
            (128, 128)
        );
    }

    #[test]
    fn keeps_the_hunk_header_and_section() {
        let files = parse(SIMPLE);
        let hunk = &files[0].hunks[0];
        assert_eq!(hunk.header, "@@ -128,3 +128,5 @@");
        assert_eq!(hunk.section, "impl SubscriptionHandler {");
        assert_eq!(
            hunk.label(),
            "@@ -128,3 +128,5 @@ impl SubscriptionHandler {"
        );
        assert_eq!(files[0].header_label(), hunk.label());
    }

    #[test]
    fn multiple_hunks_restart_the_gutter() {
        let text = "\
diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1,2 +1,2 @@
 one
-two
+three
@@ -40,2 +40,3 @@ fn tail
 forty
+forty one
 forty two
";
        let file = &parse(text)[0];
        assert_eq!(file.hunks.len(), 2);
        let second = &file.hunks[1].lines;
        assert_eq!((second[0].old, second[0].new), (Some(40), Some(40)));
        assert_eq!((second[1].old, second[1].new), (None, Some(41)));
        assert_eq!((second[2].old, second[2].new), (Some(41), Some(42)));
        assert_eq!((file.added, file.removed), (2, 1));
    }

    #[test]
    fn binary_files_and_renames_are_recognized() {
        let text = "\
diff --git a/assets/logo.png b/assets/logo.png
new file mode 100644
index 000..111
Binary files /dev/null and b/assets/logo.png differ
diff --git a/src/old.rs b/src/new.rs
similarity index 92%
rename from src/old.rs
rename to src/new.rs
";
        let files = parse(text);
        assert_eq!(files.len(), 2);
        assert!(files[0].binary);
        assert_eq!(files[0].path, PathBuf::from("assets/logo.png"));
        assert!(files[0].hunks.is_empty());

        assert!(files[1].rename);
        assert_eq!(files[1].path, PathBuf::from("src/new.rs"));
        assert_eq!(files[1].old_path, Some(PathBuf::from("src/old.rs")));
        assert_eq!(files[1].renamed_from().as_deref(), Some("src/old.rs"));
        assert_eq!(files[1].header_label(), "renamed");
    }

    #[test]
    fn a_long_file_stops_at_the_cap_and_flags_truncation() {
        let mut text = String::from(
            "diff --git a/big.txt b/big.txt\n--- a/big.txt\n+++ b/big.txt\n@@ -1,1 +1,2500 @@\n",
        );
        for n in 0..2500 {
            text.push_str(&format!("+line {n}\n"));
        }
        let file = &parse(&text)[0];
        assert!(file.truncated);
        assert_eq!(file.line_count(), MAX_LINES_PER_FILE);
        assert_eq!(file.added, 2500);
    }

    #[test]
    fn lockfiles_and_quoted_paths_are_handled() {
        let text = "diff --git \"a/sp ace\\\"x.txt\" \"b/sp ace\\\"x.txt\"\n";
        let file = &parse(text)[0];
        assert_eq!(file.path, PathBuf::from("sp ace\"x.txt"));
        assert!(!file.is_lockfile());

        let lock = "diff --git a/Cargo.lock b/Cargo.lock\n";
        assert!(parse(lock)[0].is_lockfile());
    }
}
