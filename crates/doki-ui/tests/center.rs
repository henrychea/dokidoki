use doki_core::config::{CommitScope, Config};
use doki_core::snapshot;
use doki_core::store::Store;
use doki_ui::studio::{self, Level, counter_label, counter_level};
use doki_ui::{DokiApp, RepoVm, RowState};
use gpui_kit::AppContext as _;
use gpui_kit::base::Root;
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    Bounds, Entity, TestAppContext, WindowBounds, WindowHandle, WindowOptions, point, px, size,
};
use std::path::Path;

fn open(
    cx: &mut TestAppContext,
    cfg: Config,
    store: Option<Store>,
    repos: Vec<RepoVm>,
) -> (WindowHandle<Root>, Entity<DokiApp>) {
    cx.update(gpui_kit::init);
    cx.update(|cx| {
        let bounds = WindowBounds::Windowed(Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(1280.), px(900.)),
        });
        let (window, content) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(bounds),
                ..Default::default()
            },
            cx,
            |window, cx| match store {
                Some(store) => {
                    cx.new(|cx| DokiApp::fixture_with_store(cfg, store, repos, window, cx))
                }
                None => cx.new(|cx| DokiApp::fixture(cfg, repos, window, cx)),
            },
        )
        .expect("open test window");
        (window.downcast::<Root>().expect("root window"), content)
    })
}

fn config() -> Config {
    Config::parse(
        "[[roots]]\nname = \"dev\"\npath = \"/tmp/doki/dev\"\n\
         [[roots]]\nname = \"work\"\npath = \"/tmp/doki/work\"\n",
    )
    .unwrap()
}

fn store_with(repos: &[RepoVm]) -> Store {
    let mut store = Store::open_in_memory().unwrap();
    let detected: Vec<doki_core::discovery::DetectedRepo> = repos
        .iter()
        .map(|repo| doki_core::discovery::DetectedRepo {
            root: repo.root.clone(),
            path: repo.path.clone(),
            git_common_dir: None,
        })
        .collect();
    store.upsert_detected(&detected).unwrap();
    store
}

fn label(window: &impl TestWindowExt, id: &'static str) -> String {
    window.find(id).label().unwrap_or_default().to_string()
}

#[gpui_kit::test]
fn typing_a_subject_updates_the_counter(cx: &mut TestAppContext) {
    let (handle, app) = open(
        cx,
        config(),
        None,
        vec![RepoVm::sample(1, "billing-service", "dev", RowState::Dirty)],
    );
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(label(window, "subject-counter"), "0 / 72");

        app.update(cx, |app, cx| app.focus_subject(window, cx));
        let subject = "doki: handle idempotent stripe webhook retries and grace periods";
        assert_eq!(subject.chars().count(), 64);
        window.input(subject, cx);
        window.render_frame(cx);
        assert_eq!(label(window, "subject-counter"), "64 / 72");
        assert_eq!(app.read_with(cx, |app, cx| app.subject_value(cx)), subject);
        assert_eq!(counter_level(64), Level::Normal);

        window.input(" now", cx);
        window.render_frame(cx);
        assert_eq!(label(window, "subject-counter"), "68 / 72");
        assert_eq!(counter_level(68), Level::Warn);

        window.input(" with grace", cx);
        window.render_frame(cx);
        assert_eq!(label(window, "subject-counter"), "79 / 72");
        assert_eq!(counter_level(79), Level::Danger);
    })
    .unwrap();
}

#[test]
fn counter_colors_switch_at_sixty_six_and_seventy_two() {
    assert_eq!(counter_label(60), "60 / 72");
    assert_eq!(counter_level(60), Level::Normal);
    assert_eq!(counter_level(65), Level::Normal);
    assert_eq!(counter_level(66), Level::Warn);
    assert_eq!(counter_level(71), Level::Warn);
    assert_eq!(counter_level(72), Level::Danger);
}

#[gpui_kit::test]
fn without_an_agent_the_primary_button_offers_configure_until_a_subject_exists(
    cx: &mut TestAppContext,
) {
    let cfg = Config::parse(
        "[[roots]]\nname = \"dev\"\npath = \"/tmp/doki/dev\"\n\n[agent]\norder = []\n",
    )
    .unwrap();
    let (handle, app) = open(
        cx,
        cfg,
        None,
        vec![RepoVm::sample(1, "billing-service", "dev", RowState::Dirty)],
    );
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(label(window, "commit-push"), "Configure");

        app.update(cx, |app, cx| app.focus_subject(window, cx));
        window.input("doki: commit what the user wrote", cx);
        window.render_frame(cx);
        assert_eq!(label(window, "commit-push"), "Commit & Push");
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_scope_menu_writes_a_repo_override(cx: &mut TestAppContext) {
    let repos = vec![RepoVm::sample(1, "billing-service", "dev", RowState::Dirty)];
    let store = store_with(&repos);
    let (handle, app) = open(cx, config(), Some(store), repos);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(label(window, "scope-chip"), "Tracked (0 files)");

        window.click("scope-chip", cx);
        assert!(window.try_find("scope-menu").is_some());
        window.click("scope-option-staged", cx);

        assert_eq!(app.read(cx).scope_override(1), Some(CommitScope::Staged));
        assert_eq!(label(window, "scope-chip"), "Staged (0 files)");
        assert!(window.try_find("scope-menu").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_waiting_repo_shows_the_blocked_panel_instead_of_the_commit_card(cx: &mut TestAppContext) {
    let (handle, _) = open(
        cx,
        config(),
        None,
        vec![RepoVm::sample(4, "legacy-auth", "work", RowState::Waiting)],
    );
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("blocked-panel").is_some());
        assert!(window.try_find("commit-card").is_none());
        assert!(window.try_find("blocked-review").is_some());
        assert!(window.try_find("blocked-retry").is_some());
        assert!(label(window, "blocked-panel").starts_with("Blocked:"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn undo_asks_for_confirmation_in_a_modal(cx: &mut TestAppContext) {
    let repos = vec![RepoVm::sample(1, "billing-service", "dev", RowState::Dirty)];
    let store = store_with(&repos);
    let (handle, _) = open(cx, config(), Some(store), repos);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(!window.has_active_dialog(cx));
        window.click("center-undo", cx);
        assert!(window.has_active_dialog(cx));
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_clean_repo_shows_the_empty_state(cx: &mut TestAppContext) {
    let (handle, _) = open(
        cx,
        config(),
        None,
        vec![RepoVm::sample(1, "doki-core", "dev", RowState::Clean)],
    );
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("clean-state").is_some());
        assert!(window.try_find("commit-card").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_dry_run_banner_appears_when_the_toggle_is_on(cx: &mut TestAppContext) {
    let (handle, _) = open(
        cx,
        config(),
        None,
        vec![RepoVm::sample(1, "billing-service", "dev", RowState::Dirty)],
    );
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("dry-run-banner").is_none());
        window.click("dry-run-toggle", cx);
        assert!(window.try_find("dry-run-banner").is_some());
        window.click("dry-run-toggle", cx);
        assert!(window.try_find("dry-run-banner").is_none());
    })
    .unwrap();
}

// ---------------------------------------------------------------- secret gate

fn git(repo: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

fn fixture_repo(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("app.rs"), "fn main() {}\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "init"]);
}

fn marker_shim(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let shim = dir.join("fake-agent");
    let marker = dir.join("called");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\ncat >/dev/null\ntouch '{}'\nprintf 'doki: fake agent message for the fixture\\n'\n",
            marker.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    let mut perms = std::fs::metadata(&shim).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&shim, perms).unwrap();
    (shim, marker)
}

fn shim_config(shim: &Path) -> Config {
    Config::parse(&format!(
        "[agent]\norder = [\"fake\"]\n\n[agents.fake]\ncommand = [\"{}\"]\n",
        shim.display()
    ))
    .unwrap()
}

#[test]
fn a_secret_diff_never_reaches_the_agent() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    fixture_repo(&repo);
    let (shim, marker) = marker_shim(tmp.path());
    let cfg = shim_config(&shim);

    std::fs::write(
        repo.join("app.rs"),
        "fn main() {}\nconst KEY: &str = \"AKIA0000000000000000\";\n",
    )
    .unwrap();
    let dirty = snapshot::read(&cfg, None, &repo).unwrap();
    assert!(dirty.blocked());
    let refused = studio::generate(&cfg, &repo, &dirty);
    assert!(refused.is_err(), "a blocked diff must not generate");
    assert!(!marker.exists(), "the agent was called for a blocked diff");

    std::fs::write(repo.join("app.rs"), "fn main() { println!(\"hi\"); }\n").unwrap();
    let clean = snapshot::read(&cfg, None, &repo).unwrap();
    assert!(!clean.blocked());
    let (draft, agent) = studio::generate(&cfg, &repo, &clean).expect("a message");
    assert_eq!(agent.id, "fake");
    assert!(draft.subject.starts_with("doki: "));
    assert!(
        marker.exists(),
        "the shim proves the clean path does call it"
    );
}

#[gpui_kit::test]
fn repo_header_omits_zero_ahead_and_a_clean_worktree(cx: &mut TestAppContext) {
    let (handle, _) = open(
        cx,
        config(),
        None,
        vec![RepoVm::sample(1, "doki-core", "dev", RowState::Clean)],
    );
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("repo-ahead").is_none());
        assert!(window.try_find("repo-behind").is_none());
        assert!(window.try_find("repo-worktree").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn repo_header_shows_dirty_ahead_and_behind_only_when_present(cx: &mut TestAppContext) {
    let (handle, _) = open(
        cx,
        config(),
        None,
        vec![RepoVm::sample(1, "billing-service", "dev", RowState::Dirty)],
    );
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("repo-ahead").is_none());
        assert!(window.try_find("repo-behind").is_none());
        assert_eq!(label(window, "repo-worktree"), "Worktree dirty");
    })
    .unwrap();

    let (handle, _) = open(
        cx,
        config(),
        None,
        vec![RepoVm::sample(4, "legacy-auth", "work", RowState::Waiting)],
    );
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(label(window, "repo-ahead"), "Ahead 1");
        assert_eq!(label(window, "repo-behind"), "Behind 2");
        assert_eq!(label(window, "repo-worktree"), "Action required");
    })
    .unwrap();
}
