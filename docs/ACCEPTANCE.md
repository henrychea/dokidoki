# P9 acceptance evidence

Baseline: `main` at `ba0daa2`, P9 commit subject `doki: close acceptance gaps and record evidence`.
Toolchain: rustc/cargo 1.99.1. Test environment: KDE Plasma 6 on Wayland.

## Gates

| Gate | Command | Result |
|---|---|---|
| Format | `cargo fmt --check` | exit 0 |
| Lints | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| Tests | `cargo test --workspace` | exit 0, **217 passed**, 0 failed |
| Doctor | `doki doctor` with isolated `XDG_CONFIG_HOME`/`XDG_STATE_HOME` | exit 0, warnings only (below) |
| Adapters | `doki agents list` with the same isolated dirs | exit 0 |

217 is the number of unit, integration, and UI headless tests reported by `cargo test --workspace` (`test result: ok` rows).

## DESIGN §14 required cases

| Required case | Proof (file::test) |
|---|---|
| tracked dirty | `crates/doki-core/tests/git_core.rs::tracked_dirty` |
| untracked only | `crates/doki-core/tests/git_core.rs::untracked_only` |
| staged only | `crates/doki-core/tests/git_core.rs::staged_only` |
| hook failure | `crates/doki/tests/sync.rs::a_failing_pre_commit_hook_fails_the_run_without_bypass` |
| diverged + rebase conflict | `crates/doki/tests/push.rs::a_diverged_branch_waits_for_a_human_and_never_forces` plus `crates/doki/tests/sync.rs::an_in_progress_rebase_is_refused_without_touching_the_tree` |
| protected branch | `crates/doki/tests/push.rs::a_protected_branch_commits_but_does_not_push_without_opt_in` |
| missing agent | `crates/doki-agent/tests/agent.rs::missing_cli_falls_back_to_the_next_agent`; `crates/doki-agent/tests/agent.rs::generation_is_fallback_when_no_agent_can_answer`; `crates/doki/src/agents.rs::agents::tests::test_reports_a_missing_binary` |
| agent timeout | `crates/doki-agent/tests/agent.rs::timeout_kills_the_child_and_falls_back` |
| agent garbage output | `crates/doki-agent/tests/agent.rs::garbage_output_falls_back_to_the_deterministic_message` |
| model flag omitted when unset | `crates/doki-agent/src/resolve.rs::resolve::tests::empty_model_omits_the_flag`; `crates/doki-agent/tests/agent.rs::argv_matches_each_cli_with_and_without_model` |
| per-repo model override wins | `crates/doki-agent/tests/agent.rs::repo_override_and_dot_doki_win_end_to_end` |
| invalid model string rejected | `crates/doki-core/src/config.rs::config::tests::rejects_invalid_models_and_agent_commands` |
| secret finding blocks the model call | `crates/doki/tests/sync.rs::a_secret_never_reaches_the_agent_and_raises_an_incident`; `crates/doki-ui/tests/center.rs::a_secret_diff_never_reaches_the_agent`; `crates/doki-core/src/secret.rs::secret::tests::built_in_scan_finds_the_common_shapes` |
| mass-delete guard | `crates/doki/tests/sync.rs::mass_delete_guard_blocks_before_the_agent`; `crates/doki-core/tests/git_core.rs::mass_change_guard_trips_and_passes` |
| lock contention | `crates/doki-core/tests/git_core.rs::lock_is_exclusive_and_released_on_drop`; `crates/doki-core/tests/git_core.rs::worktrees_of_one_repo_share_a_single_common_dir_lock`; `crates/doki/tests/cli.rs::sync_due_exits_busy_when_the_scheduler_lock_is_held` |
| nested repo | `crates/doki-core/src/discovery.rs::discovery::tests::finds_depth_two_not_three_and_respects_skips` (repo at `one/nested` is skipped once `one` is found) |
| symlink loop | `crates/doki-core/src/discovery.rs::discovery::tests::symlink_loops_are_not_followed` |
| depth 2 vs 3 discovery | `crates/doki-core/src/discovery.rs::discovery::tests::finds_depth_two_not_three_and_respects_skips` |
| inotify overflow fallback | `crates/doki-core/src/watch.rs::watch::tests::overflow_or_a_broken_target_falls_back_to_polling`; event delivery covered by `watch::tests::a_repo_change_sets_the_flag` |
| quarantine after N failures | `crates/doki/tests/push.rs::repeated_failures_quarantine_until_a_manual_reset`; `crates/doki-core/tests/scheduler.rs::failures_back_off_then_quarantine_and_reset_clears_everything` |
| undo refusal after push | `crates/doki/tests/sync.rs::undo_refuses_after_the_run_pushed` |
| dry run runs no mutating command | `crates/doki/tests/sync.rs::dry_run_reports_argv_and_mutates_nothing`; `crates/doki/tests/cli.rs::sync_all_dry_run_leaves_remote_head_and_refs_alone`; `crates/doki/tests/push.rs::dry_run_against_a_remote_executes_no_fetch_or_push`; `crates/doki-core/tests/git_core.rs::dry_run_records_argv_and_leaves_repo_unchanged` |
| UI render/interaction headless | `crates/doki-ui/tests/sidebar.rs::sidebar_renders_every_row_state`; `crates/doki-ui/tests/sidebar.rs::root_tabs_filter_the_tree`; `crates/doki-ui/tests/center.rs::typing_a_subject_updates_the_counter`; `crates/doki-ui/tests/center.rs::undo_asks_for_confirmation_in_a_modal`; `crates/doki-ui/tests/journal.rs::tabs_filter_all_runs_and_alerts_and_blocked_cards_keep_their_buttons`; `crates/doki-ui/tests/settings.rs::saving_a_valid_edit_keeps_a_backup_and_rewrites_the_file` |
| manual visual check against the design PNGs | `docs/screenshots/sumi.png`, `docs/screenshots/washi.png`; method and comparison below |
| adapter acceptance | `doki agents list` output below; `crates/doki/src/agents.rs::agents::tests::test_runs_a_shim_and_parses_the_message` |

## DESIGN §3 safety invariants

| Invariant | Proof |
|---|---|
| 1. Discovery never enables a repo | `crates/doki-core/src/store.rs::store::tests::detected_repos_start_disabled_and_keep_flag_on_rescan`; UI enable path in `crates/doki-ui/tests/sidebar.rs::sidebar_renders_every_row_state` |
| 2. Never force, bypass hooks, hard reset, clean, or amend | `crates/doki-core/src/git.rs::git::tests::only_read_only_subcommands_are_allowed`; `crates/doki/tests/push.rs::a_diverged_branch_waits_for_a_human_and_never_forces`; `crates/doki/tests/sync.rs::a_failing_pre_commit_hook_fails_the_run_without_bypass`; safety grep below |
| 3. No protected-branch push without opt-in | `crates/doki/tests/push.rs::a_protected_branch_commits_but_does_not_push_without_opt_in` (asserts refusal, then the explicit `allow_protected_push` opt-in) |
| 4. Pre-sync ref and journal row before the first mutation | `crates/doki/tests/sync.rs::dirty_repo_commits_the_shim_subject_and_snapshots_first` (pre-commit hook observes `refs/doki/pre-sync/*`; run row and snapshot are asserted) |
| 5. Refuse MERGE_HEAD, rebase, CHERRY_PICK_HEAD, index.lock | `crates/doki-core/tests/git_core.rs::refuses_every_in_progress_state`; `crates/doki/tests/sync.rs::an_in_progress_rebase_is_refused_without_touching_the_tree` |
| 6. One sync per repo and one scheduler at a time | `crates/doki-core/tests/git_core.rs::lock_is_exclusive_and_released_on_drop`; `crates/doki-core/tests/git_core.rs::worktrees_of_one_repo_share_a_single_common_dir_lock`; `crates/doki/tests/cli.rs::sync_due_exits_busy_when_the_scheduler_lock_is_held` |
| 7. Guards and secret scan before any diff leaves the machine | `crates/doki/tests/sync.rs::a_secret_never_reaches_the_agent_and_raises_an_incident`; `crates/doki/tests/sync.rs::mass_delete_guard_blocks_before_the_agent`; `crates/doki-ui/tests/center.rs::a_secret_diff_never_reaches_the_agent` |
| 8. Every state change is journaled | `crates/doki/tests/sync.rs::dirty_repo_commits_the_shim_subject_and_snapshots_first`; `crates/doki/tests/sync.rs::a_failing_pre_commit_hook_fails_the_run_without_bypass`; `crates/doki-core/src/store.rs::store::tests::runs_snapshots_incidents_and_cache_round_trip`; `crates/doki-core/src/journal.rs::journal::tests::incidents_own_their_runs_and_the_feed_is_newest_first` |

## DESIGN §15 milestone acceptance

| Milestone row | Proof |
|---|---|
| M0: fixture repos found at depth 2, not 3; nothing enabled by default | `crates/doki-core/src/discovery.rs::discovery::tests::finds_depth_two_not_three_and_respects_skips`; `crates/doki-core/src/store.rs::store::tests::detected_repos_start_disabled_and_keep_flag_on_rescan` |
| M1: dirty fixture → `doki:` commit | `crates/doki/tests/sync.rs::dirty_repo_commits_the_shim_subject_and_snapshots_first`; `crates/doki/tests/push.rs::a_dirty_repo_commits_and_the_push_lands_in_the_bare_remote` |
| M1: commit studio editable subject and 72-char counter | `crates/doki-ui/tests/center.rs::typing_a_subject_updates_the_counter`; `crates/doki-ui/src/studio.rs::studio::tests::counter_turns_sand_past_65_and_vermilion_at_72` |
| M1: dry run mutates nothing | `crates/doki/tests/sync.rs::dry_run_reports_argv_and_mutates_nothing`; `crates/doki/tests/push.rs::dry_run_against_a_remote_executes_no_fetch_or_push` |
| M1: undo restores HEAD | `crates/doki/tests/sync.rs::undo_restores_head_and_keeps_every_change`; `crates/doki/tests/sync.rs::undo_refuses_after_the_run_pushed` |
| M2: bare remote + diverged → WaitingUser, no force push | `crates/doki/tests/push.rs::a_diverged_branch_waits_for_a_human_and_never_forces`; `crates/doki/tests/push.rs::a_behind_only_branch_waits_for_a_human` |
| M3: fake CLI records correct argv and model per CLI | `crates/doki-agent/tests/agent.rs::argv_matches_each_cli_with_and_without_model`; `crates/doki-agent/tests/agent.rs::prompt_delivery_matches_each_cli`; `crates/doki-agent/tests/agent.rs::runs_in_a_temp_dir_never_the_repo` |
| M3: per-repo override wins | `crates/doki-agent/tests/agent.rs::repo_override_and_dot_doki_win_end_to_end` |
| M3: secret diff never reaches a model | `crates/doki/tests/sync.rs::a_secret_never_reaches_the_agent_and_raises_an_incident`; `crates/doki-ui/tests/center.rs::a_secret_diff_never_reaches_the_agent` |
| M4: `doki sync --due` with the UI closed | `crates/doki/tests/cli.rs::sync_due_commits_and_pushes_once_then_waits_for_the_next_slot`; `crates/doki/tests/cli.rs::background_runs_a_due_repo_and_stops_on_sigterm`; `crates/doki/tests/cli.rs::install_timer_refuses_foreign_units_then_backs_up_with_force_write` |
| M4: Sumi and Washi render with bundled fonts | `crates/doki-ui/tests/journal.rs::switching_to_washi_replaces_the_palette_tokens`; `crates/doki-ui/src/theme.rs::theme::tests::system_theme_follows_the_window_appearance`; visual captures below (fonts registered from `assets/fonts/` in `crates/doki-ui/src/fonts.rs`) |
| M4: doctor and docs | `crates/doki/tests/cli.rs::doctor_prints_every_probe_and_names_the_timer_state`; `crates/doki/src/doctor.rs::doctor::tests::renders_an_aligned_table_and_fails_on_a_fail_row`; `README.md`; this file |

## P9 gaps found and closed

| Gap | Fix | Proof |
|---|---|---|
| Countdown showed `115:03` beyond 99 minutes and `--:--` with no next run | `format_countdown` now returns `MM:SS` below one hour and `h MMm` at/above one hour; `format_next_sync` returns the em dash when no run is scheduled | `crates/doki-ui/tests/sidebar.rs::countdown_switches_to_hours_past_sixty_minutes` |
| Repo header always printed `↑ Ahead 0` and `Worktree clean` | Ahead is emitted only when non-zero, behind is `↓ Behind N`, and the worktree phrase is omitted for a clean tree | `crates/doki-ui/tests/center.rs::repo_header_omits_zero_ahead_and_a_clean_worktree`; `crates/doki-ui/tests/center.rs::repo_header_shows_dirty_ahead_and_behind_only_when_present` |
| Root group count excluded disabled repos (a `~/Work` with one disabled repo said `1 repo`) | Root count now counts every row under the root, including the Disabled group | `crates/doki-ui/tests/sidebar.rs::sidebar_renders_every_row_state` |
| Mass-delete guard had only a unit test | Added an end-to-end blocked-run test that also proves the agent is never called | `crates/doki/tests/sync.rs::mass_delete_guard_blocks_before_the_agent` |
| Rebase-conflict refusal had only the guard-level test | Added an end-to-end sync refusal that leaves the tree and HEAD untouched | `crates/doki/tests/sync.rs::an_in_progress_rebase_is_refused_without_touching_the_tree` |
| Symlink loops were only implicitly skipped | Added discovery coverage with and without `follow_symlinks` | `crates/doki-core/src/discovery.rs::discovery::tests::symlink_loops_are_not_followed` |
| No inotify implementation or overflow fallback | Added `doki-core::watch`: native watches for repo root, `.git/HEAD`, `.git/index`, `.git/refs`; over budget or any watch failure returns Polling. The UI rebuilds the watcher from the enabled repo set, refreshes on native events with a 3 s debounce, and polls every 30 s in Polling mode | `crates/doki-core/src/watch.rs::watch::tests::overflow_or_a_broken_target_falls_back_to_polling`; `crates/doki-core/src/watch.rs::watch::tests::a_repo_change_sets_the_flag` |
| Studio card, repo header, journal cards, Washi buttons and blocked cards drifted from the PNGs | Matched the design values: 16/500 repo name, 8/13 px header rhythm, taller subject-input padding, 4 px journal radius with flat `sumi-850`/white fill, Washi white buttons, and a separate blocked-card token (`vermilion-soft/30` Sumi, `shuiro-soft` Washi) | visual comparison below |

## Safety grep

Commands run from the repository root. Product code means `crates/*/src`; integration tests under `crates/*/tests` are excluded unless stated.

```sh
rg -n -F -e '--force-with-lease' -e 'clean -fd' -e 'clean -f' -e '--no-verify' -e '--amend' crates/*/src
rg -n -F -- '--force' crates/*/src
rg -n 'Command::new\("(sh|bash|zsh)"\)|\.args\(\["-c"' crates/*/src
rg -n 'systemctl|crontab' crates/*/src
rg -n 'reset.*hard' crates
```

Results:

| Pattern | Result |
|---|---|
| `--force-with-lease` | no hits |
| `clean -fd`, `clean -f` | no hits |
| `--no-verify` | no hits |
| `--amend` | no hits |
| `--force` | one hit, `crates/doki-core/src/timer.rs:160`: the message for doki's own `--force-write` timer-install flag. It is not a git flag. |
| `reset --hard` | no contiguous string. `crates/doki-core/src/git.rs:314` contains the argv `["reset", "--hard"]` only inside the `#[cfg(test)]` list of commands that must be rejected. Production undo uses `reset --soft` (`crates/doki-core/src/undo.rs:109`). |
| `sh -c` / `bash -c` | no production hit. `crates/doki-core/src/git.rs:326` uses `Command::new("sh")` inside `#[cfg(test)]` to exercise the timeout killer. Agents and git are spawned as direct argv vectors. |
| `systemctl` | production hits: `crates/doki-core/src/timer.rs:235` runs only `systemctl --user is-active doki.timer` (read-only). `crates/doki/src/main.rs:160-170` prints `systemctl --user … enable/disable` as instructions for the user, and `doki` never executes them. No `systemctl start`. |
| `crontab` | only a source comment and a test name (`crates/doki-core/src/scheduler.rs:14,102`). No code opens or writes a crontab. |

## Doctor and adapters (isolated dirs)

Environment: `HOME=/tmp/doki-accept/home`, `XDG_CONFIG_HOME=/tmp/doki-accept/home/.config`, `XDG_STATE_HOME=/tmp/doki-accept/home/.local/state`, plus fixture agent shims on `PATH`. The timer unit exists in the isolated config; the real user manager has not enabled it, so the warning is honest.

```text
$ doki doctor
STATUS  CHECK           DETAIL
OK      config          /tmp/doki-accept/home/.config/doki/config.toml
OK      git             git version 2.56.0
WARN    gitleaks        not found; the built-in scanner will be used
OK      agent codex     doki: handle idempotent stripe webhook retries and grace periods
OK      agent omp       doki: handle idempotent stripe webhook retries and grace periods
WARN    agent claude    claude not found on PATH
OK      agent opencode  opencode v2.3.0
WARN    agent pi        pi not found on PATH
OK      state dir       /tmp/doki-accept/home/.local/state/doki
OK      inotify         max_user_watches 524288
OK      notify          org.freedesktop.Notifications present
WARN    systemd timer   doki.timer is installed but inactive; enable it
```

`doctor` exited 0; all rows are warnings or OK, no blocking failure.

```text
$ doki agents list
id         cli          path     model
codex      codex        found    gpt-5.2
omp        omp          found    anthropic/claude-sonnet-4.5
claude     claude       missing  (cli default)
opencode   opencode     found    (cli default)
pi         pi           missing  (cli default)
```

## Visual acceptance

Captured in a KDE Plasma 6 Wayland session with no other application windows on screen. No desktop switch, no headless compositor, no external browser.

Fixture (isolated `HOME` and XDG dirs):

- `~/Developer`: `doki-core` clean and synced; `billing-service` on `feat/stripe-v2`, ahead 1, three staged dirty files (`Cargo.lock`, `src/stripe.rs`, `src/subscriptions/checkout.rs`, totals `+180 -18`); `analytics-pipeline` running with one tracked modification.
- `~/Work`: `legacy-auth` blocked/diverged; `mobile-app` detected and disabled.
- Fake `omp`/`codex` shims return `doki: handle idempotent stripe webhook retries and grace periods`; a fake `gitleaks` shim returns an empty report so the chip names Gitleaks; a `systemctl` shim in the screenshot environment reports the fixture timer active (the doctor run above used the real manager and did not use that shim).
- SQLite journal seeded with three runs inside the 12-hour window (guard OK, synced, blocked diverged incident) and the message cache seeded for the exact billing diff. `next_run_at` is 55 minutes out, so the countdown stays in `MM:SS`.
- `[ui] window = [1280, 1024]`, `theme = "sumi"`/`"washi"`.

Capture path:

1. `./target/debug/doki` launched with `HOME`, `XDG_CONFIG_HOME`, and `XDG_STATE_HOME` pointed at an isolated fixture directory and fixture agent shims first on `PATH`.
2. The compositor's screenshot tool captured the `dokidoki` window at 1280×1024, without decorations, shadow, or cursor.
3. The raw RGBA stream was encoded to PNG with ffmpeg and saved as `docs/screenshots/sumi.png` and `docs/screenshots/washi.png`.

### Region comparison

| Region | Result against the PNG |
|---|---|
| Header | 44 px, wordmark + version pill, root tabs, pine next-sync dot, Dry Run, Filter, Sync All and avatar all match. Left origin is 16 px instead of the mock's traffic-light block; native KDE decorations replace the traffic lights. Version reads `v0.1.0`; the mock's `v2.4-gpui` is not a real package version. |
| Left dock | 268 px, Workspaces header, All Trees/Diff Inspector/AI Commit, root groups, two-line repo rows and disabled group match; root counts now include disabled repos (`~/Work 2 repos`). Row order is live path order (`analytics-pipeline`, `billing-service`, `doki-core`); the mock orders `doki-core`, `billing-service`, `analytics-pipeline`. |
| Center header | 16/500 repo name, branch chip, `↑ Ahead 1 · Worktree dirty`, action row and guard chips match; the section is 3 px shorter than the mock because of font line boxes. Action label is `Open in Zed` per DESIGN §11.4; the mock prints only `Zed`. |
| Commit studio | Agent `codex [gpt-5.2]`, scope `Staged (3 files)`, editable subject, refresh, `Switch to omp (Claude)`, `Edit Body`, `Commit & Push` and guard/counter colors match. Counter reads `64 / 72` (the real length of the design's subject); the mock's `58 / 72` is arithmetically wrong. Studio card top is 2 px above the mock and the subject input is 4 px shorter; the bottom edge and action row align within 1 px. |
| File tabs + diff | Three tabs with per-file counts and `+180 -18` totals match. Tab order is live path order (`Cargo.lock`, `stripe.rs`, `checkout.rs`), so the selected `checkout.rs` tab is third rather than first. The mock shows one hunk; the real fixture has a second separated hunk (needed for the same `+42 -6` file totals), so the diff card continues below the mock's bottom edge. Hunk header line numbers are fixture data (`-127,7 +127,11` vs `-128,6 +128,14`). The tab strip is 3 px taller, so the diff body starts 2 px lower. |
| Journal | Header, All/Runs/Alerts tabs, card radii, 4 px card gaps, GUARD OK/SYNCED/BLOCKED colors and the Review/Abort/Retry row match. Clock values and the 1.0 s latency are fixture data. Rate is `66.7%` from three seeded runs (one blocked) instead of the mock's `99.8%` from a larger synthetic history; the sparkline has 12 hourly buckets (DESIGN §11.5) and is therefore sparser than the mock's eight sample bars. Card heights are 2–6 px shorter because of the bundled font line boxes. |
| Footer | `systemd: doki.timer`, `every 20m`, `Safe Guards v2`, `Armed`, `Daemon Sync Rate`, `Past 12h` and `Avg` match; the footer sits 2–4 px from the mock. |
| Sumi palette | Sampled fills match at the header, left pane, studio card, diff wash, journal card and blocked card (`journal_card=#101216`, blocked card `vermilion-soft/30`). |
| Washi palette | Sampled fills match at the header, pane, white cards, selected/active rows, diff wash, and blocked card (`shuiro-soft`, 0.08 over the washi pane). Buttons and active nav rows now use white as in `washi-light.png`. |

No other layout, spacing, type, or color difference remains. The differences above are the native-decoration/version limits, fixture data (repo/tab order, timestamps, counts, history density, hunk line numbers, real subject length), or documented plan/UI choices (`Open in Zed`, 12 hourly buckets).
