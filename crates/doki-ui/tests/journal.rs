use doki_core::config::{Config, Theme};
use doki_core::journal::Tab;
use doki_core::store::{NewIncident, RunFinish, Store};
use doki_ui::{DokiApp, RepoVm, RowState, palette};
use gpui_kit::AppContext as _;
use gpui_kit::base::Root;
use gpui_kit::component::ThemeMode;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    Bounds, Entity, TestAppContext, WindowBounds, WindowHandle, WindowOptions, point, px, size,
};

fn open(
    cx: &mut TestAppContext,
    cfg: Config,
    store: Store,
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
            |window, cx| cx.new(|cx| DokiApp::fixture_with_store(cfg, store, repos, window, cx)),
        )
        .expect("open test window");
        (window.downcast::<Root>().expect("root window"), content)
    })
}

fn config() -> Config {
    Config::parse(
        "[agent]\norder = [\"omp\"]\n\n[agents.omp]\nmodel = \"claude-sonnet-4-5\"\n\n\
         [[roots]]\nname = \"dev\"\npath = \"/tmp/doki/dev\"\n\
         [[roots]]\nname = \"work\"\npath = \"/tmp/doki/work\"\n",
    )
    .unwrap()
}

fn seeded() -> (Store, Vec<RepoVm>) {
    let mut store = Store::open_in_memory().unwrap();
    let detected: Vec<doki_core::discovery::DetectedRepo> = [
        ("dev", "/tmp/doki/dev/doki-core"),
        ("dev", "/tmp/doki/dev/billing-service"),
        ("work", "/tmp/doki/work/legacy-auth"),
    ]
    .into_iter()
    .map(|(root, path)| doki_core::discovery::DetectedRepo {
        root: root.into(),
        path: path.into(),
        git_common_dir: None,
    })
    .collect();
    store.upsert_detected(&detected).unwrap();
    let mut repos = Vec::new();
    for row in store.repos().unwrap() {
        let name = row.name();
        let state = if name == "legacy-auth" {
            RowState::Waiting
        } else {
            RowState::Clean
        };
        repos.push(RepoVm::sample(row.id, &name, &row.root, state));
    }
    (store, repos)
}

fn add_committed(store: &Store, path: &str, at: i64, pushed: bool, message: &str) -> i64 {
    let repo = store
        .repo_by_path(std::path::Path::new(path))
        .unwrap()
        .unwrap();
    let run = store.begin_run(repo.id, at - 1, None).unwrap();
    store
        .finish_run(RunFinish {
            run_id: run,
            finished_at: at,
            state: "committed",
            agent: Some("omp"),
            model: Some("claude-sonnet-4-5"),
            message: Some(message),
            exit_code: Some(0),
            ..RunFinish::default()
        })
        .unwrap();
    store.mark_run_pushed(run, pushed).unwrap();
    run
}

fn add_incident(store: &Store, path: &str, at: i64) -> i64 {
    let repo = store
        .repo_by_path(std::path::Path::new(path))
        .unwrap()
        .unwrap();
    let run = store.begin_run(repo.id, at - 1, None).unwrap();
    store
        .finish_run(RunFinish {
            run_id: run,
            finished_at: at,
            state: "blocked",
            error_kind: Some("diverged"),
            ..RunFinish::default()
        })
        .unwrap();
    store
        .add_incident(NewIncident {
            repo_id: repo.id,
            run_id: Some(run),
            severity: "blocking",
            kind: "diverged",
            detail: "Diverged upstream\nfatal: not a fast-forward",
            created_at: at,
        })
        .unwrap()
}

#[gpui_kit::test]
fn tabs_filter_all_runs_and_alerts_and_blocked_cards_keep_their_buttons(cx: &mut TestAppContext) {
    let now = 1_001;
    let (store, repos) = seeded();
    let guard = add_committed(
        &store,
        "/tmp/doki/dev/billing-service",
        now - 10,
        false,
        "doki: guard only",
    );
    let _ = guard;
    add_committed(
        &store,
        "/tmp/doki/dev/doki-core",
        now - 30,
        true,
        "doki: synced",
    );
    let incident = add_incident(&store, "/tmp/doki/work/legacy-auth", now - 60);

    let (handle, app) = open(cx, config(), store, repos);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(app.read(cx).journal_counts(), (3, 2, 1));
        assert_eq!(
            app.read(cx).journal_ids(),
            vec!["guard-1", "run-2", "incident-1"]
        );
        assert!(window.try_find("journal-card-guard-1").is_some());
        assert!(window.try_find("journal-card-guard-1").is_some());
        assert!(window.try_find("journal-card-run-2").is_some());
        assert!(window.try_find("journal-card-incident-1").is_some());
        assert!(
            window
                .try_find(format!("journal-review-incident-{incident}"))
                .is_some()
        );
        assert!(
            window
                .try_find(format!("journal-abort-incident-{incident}"))
                .is_some()
        );
        assert!(
            window
                .try_find(format!("journal-retry-incident-{incident}"))
                .is_some()
        );
        assert_eq!(window.find("journal-list").label(), Some("3 entries"));

        window.click("journal-tab-alerts", cx);
        window.render_frame(cx);
        assert_eq!(app.read(cx).journal_tab(), Tab::Alerts);
        assert_eq!(window.find("journal-list").label(), Some("1 entries"));
        assert!(window.try_find("journal-card-guard-1").is_none());

        window.click("journal-tab-runs", cx);
        window.render_frame(cx);
        assert_eq!(app.read(cx).journal_tab(), Tab::Runs);
        assert_eq!(window.find("journal-list").label(), Some("2 entries"));

        window.click("journal-tab-all", cx);
        window.render_frame(cx);
        assert_eq!(window.find("journal-list").label(), Some("3 entries"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn acknowledging_an_incident_replaces_the_abort_button(cx: &mut TestAppContext) {
    let (store, repos) = seeded();
    let incident = add_incident(&store, "/tmp/doki/work/legacy-auth", 940);
    let (handle, app) = open(cx, config(), store, repos);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window
                .try_find(format!("journal-abort-incident-{incident}"))
                .is_some()
        );
        app.update(cx, |app, cx| app.acknowledge_incident(incident, cx));
        window.render_frame(cx);
        assert!(
            window
                .try_find(format!("journal-abort-incident-{incident}"))
                .is_none()
        );
        assert_eq!(app.read(cx).journal_counts().0, 1);
    })
    .unwrap();
}

#[gpui_kit::test]
fn switching_to_washi_replaces_the_palette_tokens(cx: &mut TestAppContext) {
    let (store, repos) = seeded();
    let (handle, app) = open(cx, config(), store, repos);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(app.read(cx).palette().canvas, palette(Theme::Sumi).canvas);

        app.update(cx, |app, cx| app.set_theme(Theme::Washi, cx));
        window.render_frame(cx);
        assert_eq!(app.read(cx).palette().canvas, palette(Theme::Washi).canvas);
        assert_eq!(app.read(cx).palette().pane, palette(Theme::Washi).pane);
        assert_eq!(
            gpui_kit::component::Theme::global(cx).mode,
            ThemeMode::Light
        );

        app.update(cx, |app, cx| app.set_theme(Theme::Sumi, cx));
        window.render_frame(cx);
        assert_eq!(app.read(cx).palette().canvas, palette(Theme::Sumi).canvas);
        assert_eq!(gpui_kit::component::Theme::global(cx).mode, ThemeMode::Dark);
    })
    .unwrap();
}

#[gpui_kit::test]
fn j_and_k_move_the_selection_and_text_input_wins(cx: &mut TestAppContext) {
    let (store, repos) = seeded();
    let (handle, app) = open(cx, config(), store, repos);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let first = app.read(cx).selected();
        assert!(first.is_some());

        window.press("j", cx);
        let second = app.read(cx).selected();
        assert_ne!(first, second);

        window.press("k", cx);
        assert_eq!(app.read(cx).selected(), first);

        app.update(cx, |app, cx| app.focus_subject(window, cx));
        let before = app.read(cx).selected();
        window.press("j", cx);
        assert_eq!(app.read(cx).selected(), before);
    })
    .unwrap();
}

#[gpui_kit::test]
fn ctrl_k_opens_a_palette_that_runs_settings_and_escape_dismisses_it(cx: &mut TestAppContext) {
    let (store, repos) = seeded();
    let (handle, app) = open(cx, config(), store, repos);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("palette-overlay").is_none());

        window.press("ctrl-k", cx);
        window.render_frame(cx);
        assert!(window.try_find("palette-overlay").is_some());

        app.update(cx, |app, cx| app.set_palette_query("Settings", window, cx));
        window.render_frame(cx);
        assert_eq!(app.read(cx).palette_matches(cx), 1);
        window.dispatch_action(
            Box::new(gpui_kit::base::actions::Confirm { secondary: false }),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("settings-view").is_some());
        assert!(!app.read(cx).palette_is_open());

        window.press("escape", cx);
        window.render_frame(cx);
        assert!(window.try_find("settings-view").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn journal_cards_size_to_content_with_small_gaps(cx: &mut TestAppContext) {
    let now = 1_001;
    let (store, repos) = seeded();
    add_committed(
        &store,
        "/tmp/doki/dev/billing-service",
        now - 10,
        false,
        "doki: guard only",
    );
    add_committed(
        &store,
        "/tmp/doki/dev/doki-core",
        now - 30,
        true,
        "doki: synced",
    );
    add_incident(&store, "/tmp/doki/work/legacy-auth", now - 60);

    let (handle, _) = open(cx, config(), store, repos);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let guard = window.find("journal-card-guard-1").bounds();
        let synced = window.find("journal-card-run-2").bounds();
        let blocked = window.find("journal-card-incident-1").bounds();
        let first = window.find("journal-row-0").bounds();
        let second = window.find("journal-row-1").bounds();
        assert_eq!(first.size.height, guard.size.height + px(8.));
        assert_eq!(second.size.height, synced.size.height + px(8.));
        assert_eq!(guard.size.height, px(98.));
        assert_eq!(synced.size.height, px(85.));
        assert_eq!(blocked.size.height, px(114.));

        window.click("journal-stderr-toggle-incident-1", cx);
        window.render_frame(cx);
        let expanded = window.find("journal-card-incident-1").bounds();
        let third = window.find("journal-row-2").bounds();
        assert_eq!(third.size.height, expanded.size.height + px(8.));
    })
    .unwrap();
}

#[gpui_kit::test]
fn expanding_a_blocked_card_shows_the_log_ring(cx: &mut TestAppContext) {
    let logs = tempfile::tempdir().unwrap();
    doki_core::logging::init(logs.path());
    doki_core::logging::clear();
    tracing::info!(
        repo = "legacy-auth",
        "stopped before pushing a divergent branch"
    );

    let (store, repos) = seeded();
    add_incident(&store, "/tmp/doki/work/legacy-auth", 940);
    let (handle, _) = open(cx, config(), store, repos);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("journal-logs-incident-1").is_none());
        window.click("journal-stderr-toggle-incident-1", cx);
        window.render_frame(cx);
        assert!(window.try_find("journal-logs-incident-1").is_some());
    })
    .unwrap();
}
