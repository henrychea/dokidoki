use crate::config::{Config, config_path, expand_tilde};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// A config file is replaced only after the new text parses; the original stays
/// next to it as `.bak`.
pub fn write_config(path: &Path, next: &str) -> Result<()> {
    Config::parse(next).context("refusing to write a config that does not parse")?;
    let original = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    backup_and_write(path, &original, next)
}

pub fn add_root(raw: &str) -> Result<String> {
    add_root_at(&config_path(), raw)
}

pub fn add_root_at(cfg_path: &Path, raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("root path is empty");
    }
    let expanded = expand_tilde(Path::new(trimmed));
    if !expanded.is_dir() {
        bail!("{} is not a directory", expanded.display());
    }
    let existing = match std::fs::read_to_string(cfg_path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).with_context(|| format!("read {}", cfg_path.display())),
    };
    let cfg = Config::parse(&existing)
        .with_context(|| format!("{} must parse before a root is added", cfg_path.display()))?;
    if cfg.roots.iter().any(|root| same_dir(&root.path, &expanded)) {
        bail!("{} is already a root", expanded.display());
    }
    let base = expanded
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .context("root path needs a final component")?;
    let name = unique_name(&cfg, &base);
    let stored = if trimmed.starts_with('~') {
        trimmed.to_string()
    } else {
        expanded.display().to_string()
    };
    let mut next = existing.clone();
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(&format!(
        "\n[[roots]]\nname = {}\npath = {}\n",
        toml_string(&name),
        toml_string(&stored)
    ));
    let parsed = Config::parse(&next).context("the added root does not produce a valid config")?;
    if !parsed.roots.iter().any(|root| root.name == name) {
        bail!("the added root did not survive parsing");
    }
    backup_and_write(cfg_path, &existing, &next)?;
    Ok(name)
}

fn unique_name(cfg: &Config, base: &str) -> String {
    if !cfg.roots.iter().any(|root| root.name == base) {
        return base.to_string();
    }
    for suffix in 2.. {
        let candidate = format!("{base}-{suffix}");
        if !cfg.roots.iter().any(|root| root.name == candidate) {
            return candidate;
        }
    }
    unreachable!("an unused root name always exists")
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn toml_string(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn backup_and_write(path: &Path, original: &str, next: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .context("config path needs a file name")?;
    let backup = path.with_file_name(format!("{name}.bak"));
    std::fs::write(&backup, original).with_context(|| format!("write {}", backup.display()))?;
    let temp: PathBuf = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&temp, next).with_context(|| format!("write {}", temp.display()))?;
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&temp, meta.permissions());
    }
    std::fs::rename(&temp, path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_text(dir: &Path) -> String {
        format!(
            "[ui]\ntheme = \"sumi\"\n\n[[roots]]\nname = \"dev\"\npath = \"{}\"\n",
            dir.join("dev").display()
        )
    }

    #[test]
    fn appends_a_root_and_keeps_a_backup() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("dev")).unwrap();
        std::fs::create_dir(tmp.path().join("work")).unwrap();
        let path = tmp.path().join("config.toml");
        let original = config_text(tmp.path());
        std::fs::write(&path, &original).unwrap();

        let name = add_root_at(&path, tmp.path().join("work").to_str().unwrap()).unwrap();

        assert_eq!(name, "work");
        let text = std::fs::read_to_string(&path).unwrap();
        let cfg = Config::parse(&text).unwrap();
        assert_eq!(cfg.roots.len(), 2);
        assert_eq!(cfg.roots[0].name, "dev");
        assert_eq!(cfg.roots[1].name, "work");
        assert_eq!(cfg.ui.theme, crate::config::Theme::Sumi);
        let backup = std::fs::read_to_string(tmp.path().join("config.toml.bak")).unwrap();
        assert_eq!(backup, original);
        assert!(
            !tmp.path()
                .join(format!(".config.toml.{}.tmp", std::process::id()))
                .exists()
        );
    }

    #[test]
    fn rejects_an_unparseable_config_without_writing() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("work")).unwrap();
        let path = tmp.path().join("config.toml");
        let original = "bogus = true\n";
        std::fs::write(&path, original).unwrap();

        assert!(add_root_at(&path, tmp.path().join("work").to_str().unwrap()).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert!(!tmp.path().join("config.toml.bak").exists());
    }

    #[test]
    fn duplicate_paths_and_names_are_handled() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("a/dev")).unwrap();
        std::fs::create_dir_all(tmp.path().join("b/dev")).unwrap();
        let path = tmp.path().join("config.toml");
        let text = format!(
            "[[roots]]\nname = \"dev\"\npath = \"{}\"\n",
            tmp.path().join("a/dev").display()
        );
        std::fs::write(&path, &text).unwrap();

        assert!(add_root_at(&path, tmp.path().join("a/dev").to_str().unwrap()).is_err());
        let name = add_root_at(&path, tmp.path().join("b/dev").to_str().unwrap()).unwrap();
        assert_eq!(name, "dev-2");
        let cfg = Config::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(cfg.roots.len(), 2);
        assert_eq!(cfg.roots[1].name, "dev-2");
    }

    #[test]
    fn missing_config_is_created_and_bad_paths_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("work")).unwrap();
        let path = tmp.path().join("nested/config.toml");
        let name = add_root_at(&path, tmp.path().join("work").to_str().unwrap()).unwrap();
        assert_eq!(name, "work");
        let cfg = Config::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(cfg.roots.len(), 1);
        assert!(tmp.path().join("nested/config.toml.bak").exists());

        let missing = tmp.path().join("nope");
        assert!(add_root_at(&path, missing.to_str().unwrap()).is_err());
        assert_eq!(
            Config::parse(&std::fs::read_to_string(&path).unwrap())
                .unwrap()
                .roots
                .len(),
            1
        );
    }

    #[test]
    fn empty_and_relative_paths_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        assert!(add_root_at(&path, "  ").is_err());
        assert!(add_root_at(&path, "relative/does/not/exist").is_err());
        assert!(!path.exists());
    }

    #[test]
    fn write_config_validates_keeps_a_backup_and_writes_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("dev")).unwrap();
        let path = tmp.path().join("config.toml");
        let original = config_text(tmp.path());
        std::fs::write(&path, &original).unwrap();

        let next = format!("{original}\n[ui]\neditor = \"code\"\n");
        let good = format!("{original}\n[notify]\ndesktop = false\n");
        write_config(&path, &good).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), good);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("config.toml.bak")).unwrap(),
            original
        );

        assert!(write_config(&path, &next).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), good);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("config.toml.bak")).unwrap(),
            original
        );
        assert!(
            !tmp.path()
                .join(format!(".config.toml.{}.tmp", std::process::id()))
                .exists()
        );
    }
}
