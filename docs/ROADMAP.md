# dokidoki — roadmap

The design and safety invariants live in [DESIGN.md](DESIGN.md). The internal
acceptance evidence lives in [ACCEPTANCE.md](ACCEPTANCE.md).

Each phase lands with one local commit `doki: <why>` after
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
and `cargo test --workspace` are green.

## Done

| Phase | Scope | Status |
|---|---|---|
| P0 | Workspace, config, depth-2 discovery, SQLite store, enable/disable, `doki status` | done |
| P1 | Git status parser, refuse in-progress states, per-repo lock, mass-change guard, dry run | done |
| P2 | Agent adapters (omp, codex, claude, opencode, pi), model resolution, fallback chain, prompt/parse | done |
| P3 | Pre-sync refs, commit scopes, secret scan, undo, run/journal schema, `doki sync` | done |
| P4 | Fetch/push, protected branches, divergence handling, cron scheduler, backoff, quarantine, incidents | done |
| P5 | UI frame: header, workspace dock, repo tree, live status from the worker pool | done |
| P6 | UI center: repo header, guard chips, AI Commit Studio, file tabs, diff view, empty/blocked/dry-run states | done |
| P7 | UI journal and themes: All/Runs/Alerts journal, sync-rate footer, Sumi/Washi/system themes, keys, settings | done |
| P8 | Ops: systemd user timer, `doki run --background`, tray, `doki doctor` | done |
| P9 | Final acceptance: full suite, design comparison, screenshots | done |

## Next

| Priority | Feature | Why |
|---|---|---|
| P0 | Bundle snapshots with optional upload | Real backup for repos without a remote |
| P1 | Re-read `.doki.toml` after a pull | A tightened policy should take effect without a rescan |
| P1 | Per-root model defaults | Fewer overrides on large trees |
| P2 | Multi-machine lease refs | Two hosts pushing the same repo without racing |
| P2 | Read-only AI conflict summary | Explain both sides; still never auto-resolve |
| P2 | Repo health report | Stale branches, large blobs, unpushed tags, LFS |
| P2 | `doki/*` backup branch push | Keeps `main` manual |
| P2 | Commit signing passthrough | Respect `commit.gpgsign` end to end |

## Non-goals

- Automatic conflict resolution, including AI resolution.
- Force push or history rewriting of published commits.
- Token storage or credential management.
- Editing the user's crontab.
- GitHub/GitLab API integration in v1.
- Watching every file in very large monorepos; poll instead.
- Cross-platform polish. Linux first; gpui-kit keeps macOS/Windows possible.
