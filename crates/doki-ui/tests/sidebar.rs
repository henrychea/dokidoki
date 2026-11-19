use doki_ui::{DokiApp, RepoVm, RowState, format_countdown, format_next_sync, schedule_interval};
use gpui_kit::AppContext as _;
use gpui_kit::base::Root;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    Bounds, ElementId, Entity, TestAppContext, WindowBounds, WindowHandle, WindowOptions, point,
    px, size,
};

fn open_app(
    cx: &mut TestAppContext,
    cfg: doki_core::config::Config,
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
            |window, cx| cx.new(|cx| DokiApp::fixture(cfg, repos, window, cx)),
        )
        .expect("open test window");
        (window.downcast::<Root>().expect("root window"), content)
    })
}

fn config() -> doki_core::config::Config {
    doki_core::config::Config::parse(
        "[[roots]]\nname = \"dev\"\npath = \"/tmp/doki/dev\"\n\
         [[roots]]\nname = \"work\"\npath = \"/tmp/doki/work\"\n",
    )
    .unwrap()
}

fn state_label<W: TestWindowExt>(window: &W, id: usize) -> String {
    window
        .find(ElementId::named_usize("state", id))
        .label()
        .unwrap_or_default()
        .to_string()
}

fn row_label<W: TestWindowExt>(window: &W, id: usize) -> String {
    window
        .find(ElementId::named_usize("repo", id))
        .label()
        .unwrap_or_default()
        .to_string()
}

fn repos() -> Vec<RepoVm> {
    vec![
        RepoVm::sample(1, "doki-core", "dev", RowState::Clean),
        RepoVm::sample(2, "billing-service", "dev", RowState::Dirty),
        RepoVm::sample(3, "analytics-pipeline", "dev", RowState::Running),
        RepoVm::sample(4, "legacy-auth", "work", RowState::Waiting),
        RepoVm::sample(5, "billing-legacy", "work", RowState::Incident),
        RepoVm::sample(6, "mobile-app", "work", RowState::Disabled),
    ]
}

#[gpui_kit::test]
fn sidebar_renders_every_row_state(cx: &mut TestAppContext) {
    let (handle, app) = open_app(cx, config(), repos());
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);

        assert!(row_label(window, 1).contains("doki-core"));
        assert_eq!(state_label(window, 1), "Synced");
        assert_eq!(state_label(window, 2), "3 dirty files");
        assert_eq!(state_label(window, 3), "omp: analyzing");
        assert_eq!(state_label(window, 4), "Diverged upstream");
        assert_eq!(state_label(window, 5), "Rebase in progress");
        assert!(row_label(window, 5).contains("incident"));

        assert_eq!(window.find("root-count-dev").label(), Some("3 repos"));
        assert_eq!(window.find("root-count-work").label(), Some("3 repos"));
        assert!(window.try_find(ElementId::named_usize("repo", 6)).is_none());
        window.click("disabled-toggle", cx);
        assert_eq!(state_label(window, 6), "Disabled");
        assert!(row_label(window, 6).contains("mobile-app"));

        window.click(ElementId::named_usize("repo", 2), cx);
    })
    .unwrap();
    cx.update(|cx| assert_eq!(app.read(cx).selected(), Some(2)));
}

#[gpui_kit::test]
fn root_tabs_filter_the_tree(cx: &mut TestAppContext) {
    let (handle, _) = open_app(cx, config(), repos());
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(ElementId::named_usize("repo", 4)).is_some());
        window.click("root-tab-work", cx);
        assert!(window.try_find(ElementId::named_usize("repo", 1)).is_none());
        assert!(window.try_find(ElementId::named_usize("repo", 4)).is_some());
        window.click("root-tab-work", cx);
        assert!(window.try_find(ElementId::named_usize("repo", 1)).is_some());
    })
    .unwrap();
}

#[test]
fn countdown_switches_to_hours_past_sixty_minutes() {
    assert_eq!(format_countdown(0), "00:00");
    assert_eq!(format_countdown(1), "00:01");
    assert_eq!(format_countdown(222), "03:42");
    assert_eq!(format_countdown(-9), "00:00");
    assert_eq!(format_countdown(3_599), "59:59");
    assert_eq!(format_countdown(3_600), "1h 00m");
    assert_eq!(format_countdown(6_903), "1h 55m");
    assert_eq!(format_next_sync(Some(1_000 + 6_903), 1_000), "1h 55m");
    assert_eq!(format_next_sync(Some(1_000 + 222), 1_000), "03:42");
    assert_eq!(format_next_sync(None, 1_000), "—");
}

#[test]
fn schedules_render_as_short_intervals() {
    assert_eq!(schedule_interval("*/20 * * * *"), "every 20m");
    assert_eq!(schedule_interval("0 */2 * * *"), "every 2h");
    assert_eq!(schedule_interval("0 7 * * *"), "daily at 07:00");
    assert_eq!(schedule_interval("nonsense"), "nonsense");
}

#[gpui_kit::test]
fn footer_shows_the_cached_systemd_state_and_all_twelve_rate_buckets(cx: &mut TestAppContext) {
    use doki_core::timer::TimerState;

    let (handle, app) = open_app(cx, config(), repos());
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("timer-mode").label(), Some("in-app scheduler"));
        for index in 0..12 {
            let id = ElementId::Name(format!("journal-spark-{index}").into());
            assert!(
                window.try_find(id).is_some(),
                "missing spark bucket {index}"
            );
        }

        app.update(cx, |app, cx| {
            app.set_timer_state(TimerState::Active, cx);
        });
        window.render_frame(cx);
        assert_eq!(
            window.find("timer-mode").label(),
            Some("systemd: doki.timer")
        );

        app.update(cx, |app, cx| {
            app.set_timer_state(TimerState::Installed, cx);
        });
        window.render_frame(cx);
        assert_eq!(
            window.find("timer-mode").label(),
            Some("systemd: doki.timer (inactive)")
        );
    })
    .unwrap();
}
