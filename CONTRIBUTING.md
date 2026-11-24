# Contributing to dokidoki

Thanks for taking the time to improve `doki`. This project is a Rust workspace
with a GPUI desktop app and a CLI; the internals are described in
[docs/DESIGN.md](docs/DESIGN.md).

## Development setup

Linux and macOS are supported. On Linux, install the system libraries GPUI
needs:

```sh
sudo apt-get install -y \
  build-essential pkg-config \
  libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev \
  libfontconfig-dev libvulkan-dev \
  libx11-xcb-dev libxcb1-dev libxcb-render0-dev libxcb-shape0-dev \
  libxcb-xfixes0-dev libxcb-xkb-dev
```

On macOS, install the Xcode command line tools and Rust. The app links only
against system frameworks, so there is nothing else to install:

```sh
xcode-select --install
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

You also need Git on `PATH`. Any one of `omp`, `codex`, `claude`, `opencode`,
or `pi` is optional for development; the test suite uses fake CLIs, so no
account or network access is required.

```sh
cargo build
cargo run -p doki -- doctor
cargo test --workspace
```

The app reads `${XDG_CONFIG_HOME:-~/.config}/doki/config.toml` and writes state
under `${XDG_STATE_HOME:-~/.local/state}/doki`. For manual testing, point
`HOME`, `XDG_CONFIG_HOME`, and `XDG_STATE_HOME` at a scratch directory so your
real configuration and journals stay untouched.

### macOS app and disk image

`packaging/build-macos-app.sh` builds `dist/Doki.app`, signs it ad hoc, and
writes the `.dmg` and `.zip` beside it. The disk image needs
[uv](https://docs.astral.sh/uv/) on `PATH` for `dmgbuild`; the script never
opens a Finder window, so it is safe to run over SSH or in CI.

```sh
cargo build --release -p doki
bash packaging/build-macos-app.sh \
  --binary target/release/doki --version <version> --arch aarch64
```

Use `x86_64` on Intel. `dist/Doki.app` runs in place without an install step.

## Gates

Every change must pass all three before review:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs the same commands on `ubuntu-latest` and `macos-15`, and the GUI tests
run in both. A pull request that leaves a warning or a failing test is not
ready.

## Commit style

One commit per change, subject `doki: <why>`, lowercase after the prefix. The
subject explains the reason for the change, not the mechanical edit:

```text
doki: refuse repos with an index lock
doki: keep the journal when a run is aborted
```

No `--no-verify`, no force push, no amend of published commits. Keep the body
short and factual.

## Safety invariants

The invariants in [docs/DESIGN.md §3](docs/DESIGN.md#3-safety-invariants) are
not style preferences. A change must not weaken any of them, including:

- No `--force`, `--no-verify`, `reset --hard`, `clean -fd`, or amend of
  published commits.
- No push of `main`/`master` without a per-repo opt-in.
- Pre-sync ref and journal row before the first mutation.
- Refuse in-progress merge, rebase, cherry-pick, and locked repos.
- Guards and the secret scan run before any diff reaches a model.

If a change touches a guard, add or update a test that proves the refusal still
happens. A guard change without a test will be asked to add one.

## Pull requests

- Keep the change focused; separate refactors from behavior changes.
- Describe what breaks without the change and how you verified it.
- Add tests for new behavior and for every new refusal path.
- Update `README.md` when the CLI, configuration, or guarantees change.
- Do not include secrets, tokens, private repository names, or absolute paths
  from your own machine in tests, fixtures, screenshots, or logs.

By contributing you agree that your contribution is licensed under the MIT
license in [LICENSE](LICENSE), and that you will follow the
[Code of Conduct](CODE_OF_CONDUCT.md).
