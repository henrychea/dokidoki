## What and why

<!-- What changes, and what breaks without it. -->

## How it was verified

<!-- Commands, tests, and the environment used. -->

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`

## Safety

- [ ] No safety invariant is weakened (see `docs/DESIGN.md` §3).
- [ ] Any changed guard still has a test proving its refusal.
- [ ] No `--force`, `--no-verify`, `reset --hard`, `clean -fd`, or publish-history amend is introduced.
- [ ] No secrets, tokens, private repository names, or machine-specific absolute paths in code, tests, docs, or screenshots.

## Checklist

- [ ] The change is focused and does not mix a refactor with a behavior change.
- [ ] Documentation and `CHANGELOG.md` are updated when behavior, the CLI, or configuration changes.
- [ ] New tests cover the new behavior and every new refusal path.
