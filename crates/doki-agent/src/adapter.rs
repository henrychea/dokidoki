use anyhow::{Result, bail};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
    Stdin,
    Argv,
}

/// One row per supported CLI. Flags are the CLI's documented headless form.
#[derive(Debug)]
pub struct Adapter {
    pub id: &'static str,
    pub head: &'static [&'static str],
    pub cwd_flag: Option<&'static str>,
    pub model_flag: &'static str,
    pub last_message_flag: Option<&'static str>,
    pub trailer: &'static [&'static str],
    pub prompt: Prompt,
}

pub const ADAPTERS: &[Adapter] = &[
    Adapter {
        id: "omp",
        head: &["-p", "--mode=text"],
        cwd_flag: Some("--cwd"),
        model_flag: "--model",
        last_message_flag: None,
        trailer: &[],
        prompt: Prompt::Argv,
    },
    Adapter {
        id: "codex",
        head: &[
            "exec",
            "--skip-git-repo-check",
            "-s",
            "read-only",
            "--ephemeral",
        ],
        cwd_flag: None,
        model_flag: "-m",
        last_message_flag: Some("-o"),
        trailer: &["-"],
        prompt: Prompt::Stdin,
    },
    Adapter {
        id: "claude",
        head: &["-p"],
        cwd_flag: None,
        model_flag: "--model",
        last_message_flag: None,
        trailer: &[],
        prompt: Prompt::Stdin,
    },
    Adapter {
        id: "opencode",
        head: &["run"],
        cwd_flag: None,
        model_flag: "-m",
        last_message_flag: None,
        trailer: &[],
        prompt: Prompt::Argv,
    },
    Adapter {
        id: "pi",
        head: &["-p"],
        cwd_flag: None,
        model_flag: "--model",
        last_message_flag: None,
        trailer: &[],
        prompt: Prompt::Argv,
    },
];

pub fn adapter(id: &str) -> Option<&'static Adapter> {
    ADAPTERS.iter().find(|a| a.id == id)
}

pub fn on_path(cli: &str) -> bool {
    let executable = |p: &Path| {
        p.is_file()
            && p.metadata()
                .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    };
    if cli.contains('/') {
        return executable(Path::new(cli));
    }
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(cli))
            .any(|p| executable(&p))
    })
}

impl Adapter {
    pub fn argv(
        &self,
        cli: &str,
        model: &str,
        extra_args: &[String],
        tmp: &Path,
        prompt: &str,
    ) -> (Vec<String>, Prompt, Option<PathBuf>) {
        let mut argv = vec![cli.to_string()];
        argv.extend(self.head.iter().map(|s| s.to_string()));
        if let Some(flag) = self.cwd_flag {
            argv.push(flag.to_string());
            argv.push(tmp.display().to_string());
        }
        if !model.is_empty() {
            argv.push(self.model_flag.to_string());
            argv.push(model.to_string());
        }
        let mut last_message = None;
        if let Some(flag) = self.last_message_flag {
            let file = tmp.join("last-message.txt");
            argv.push(flag.to_string());
            argv.push(file.display().to_string());
            last_message = Some(file);
        }
        argv.extend(extra_args.iter().cloned());
        argv.extend(self.trailer.iter().map(|s| s.to_string()));
        if self.prompt == Prompt::Argv {
            argv.push(prompt.to_string());
        }
        (argv, self.prompt, last_message)
    }
}

/// `command` is machine-config only, for CLIs doki has no adapter for.
pub fn custom_argv(command: &[String], model: &str, prompt: &str) -> Result<(Vec<String>, Prompt)> {
    let mut argv = Vec::with_capacity(command.len());
    let mut prompt_in_argv = false;
    for token in command {
        match token.as_str() {
            "{model}" if model.is_empty() => {}
            "{model}" => argv.push(model.to_string()),
            "{prompt}" => {
                argv.push(prompt.to_string());
                prompt_in_argv = true;
            }
            _ => argv.push(token.clone()),
        }
    }
    if argv.first().is_none_or(|bin| bin.trim().is_empty()) {
        bail!("custom agent command must start with a binary");
    }
    Ok((
        argv,
        if prompt_in_argv {
            Prompt::Argv
        } else {
            Prompt::Stdin
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_command_substitutes_placeholders() {
        let command = ["my-cli", "--model", "{model}", "{prompt}"]
            .map(String::from)
            .to_vec();
        let (argv, via) = custom_argv(&command, "m", "hello").unwrap();
        assert_eq!(argv, ["my-cli", "--model", "m", "hello"]);
        assert_eq!(via, Prompt::Argv);

        let (argv, via) = custom_argv(&command, "", "hello").unwrap();
        assert_eq!(argv, ["my-cli", "--model", "hello"]);
        assert_eq!(via, Prompt::Argv);

        let command = ["my-cli", "{model}"].map(String::from).to_vec();
        let (argv, via) = custom_argv(&command, "", "hello").unwrap();
        assert_eq!(argv, ["my-cli"]);
        assert_eq!(via, Prompt::Stdin);
    }

    #[test]
    fn path_lookup_works_for_known_binaries() {
        assert!(on_path("/bin/sh"));
        assert!(on_path("sh"));
        assert!(!on_path("doki-no-such-binary-xyz"));
    }
}
