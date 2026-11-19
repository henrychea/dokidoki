# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

Nothing yet.

## [0.1.0] - 2026-11-19

First public release.

### Added

- Repository discovery under configured roots, depth 2 by default, with every
  detected repository disabled until the user enables it.
- Git status parsing for branch, upstream, ahead/behind, staged, unstaged,
  untracked, unmerged, detached, and unborn states.
- Refusal of repositories with `MERGE_HEAD`, `rebase-merge/`,
  `rebase-apply/`, `CHERRY_PICK_HEAD`, or `index.lock`.
- Pre-sync snapshot refs under `refs/doki/pre-sync/<ts>` and a journal row
  before the first mutation.
- Commit scopes (tracked, all, staged), commit message via stdin, and the
  `doki: ` subject prefix enforced by `doki`.
- AI commit messages through `omp`, `codex`, `claude`, `opencode`, and `pi`
  CLIs, with per-CLI model configuration, per-repository overrides, fallback
  chains, timeouts, and a deterministic fallback message.
- Per-repository `.doki.toml` that can only tighten policy.
- Secret scanning with gitleaks when installed, plus a built-in scanner, and a
  mass-change guard; both run before any diff can reach a model.
- Scheduled syncs per root, jitter, exponential backoff, quarantine after
  repeated failures, and a persisted `next_run_at`.
- Fetch and push with timeouts, protected-branch commit-only behavior, and
  divergence handling that never force-pushes.
- `doki undo` that restores the pre-sync snapshot only when it is still safe.
- GPUI desktop app: repository tree, guard chips, AI Commit Studio with an
  editable subject and 72-character counter, diff inspector, journal, settings,
  Sumi and Washi themes with bundled Inter and JetBrains Mono.
- Background mode and a StatusNotifierItem tray.
- systemd user timer generation through `doki install-timer`, without ever
  editing crontab or running `systemctl`.
- `doki doctor` environment checks and `doki agents list|test`.
- SQLite WAL journal for runs, incidents, snapshots, and cached messages.
