use doki_core::config::Config;
use doki_ui::DokiApp;
use gpui_kit::AppContext as _;
use gpui_kit::base::Root;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    Bounds, Entity, TestAppContext, WindowBounds, WindowHandle, WindowOptions, point, px, size,
};
use std::path::{Path, PathBuf};

fn open(
    cx: &mut TestAppContext,
    cfg: Config,
    cfg_path: PathBuf,
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
            |window, cx| cx.new(|cx| DokiApp::fixture_with_config_path(cfg, cfg_path, window, cx)),
        )
        .expect("open test window");
        (window.downcast::<Root>().expect("root window"), content)
    })
}

fn write_config(dir: &Path) -> PathBuf {
    let root = dir.join("dev");
    std::fs::create_dir_all(&root).unwrap();
    let path = dir.join("config.toml");
    let text = format!(
        "[ui]\ntheme = \"sumi\"\neditor = \"zed\"\n\n\
         [[roots]]\nname = \"dev\"\npath = \"{}\"\nmax_depth = 2\n",
        root.display()
    );
    std::fs::write(&path, text).unwrap();
    path
}

fn label(window: &impl TestWindowExt, id: &'static str) -> String {
    window.find(id).label().unwrap_or_default().to_string()
}

#[gpui_kit::test]
fn an_invalid_model_shows_an_inline_error_and_leaves_the_file_alone(cx: &mut TestAppContext) {
    let tmp = tempfile::tempdir().unwrap();
    let path = write_config(tmp.path());
    let before = std::fs::read_to_string(&path).unwrap();
    let cfg = Config::load(&path).unwrap();
    let (handle, app) = open(cx, cfg, path.clone());
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        app.update(cx, |app, cx| app.open_settings(window, cx));
        window.render_frame(cx);
        assert!(window.try_find("settings-view").is_some());

        app.update(cx, |app, cx| {
            app.focus_settings_field("agent.pi.model", window, cx)
        });
        window.input("bad model", cx);
        window.click("settings-save", cx);
        window.render_frame(cx);

        assert!(
            window.try_find("settings-error").is_some(),
            "no inline error"
        );
        assert!(
            label(window, "settings-error").contains("must match"),
            "error: {}",
            label(window, "settings-error")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        assert!(!tmp.path().join("config.toml.bak").exists());
    })
    .unwrap();
}

#[gpui_kit::test]
fn saving_a_valid_edit_keeps_a_backup_and_rewrites_the_file(cx: &mut TestAppContext) {
    let tmp = tempfile::tempdir().unwrap();
    let path = write_config(tmp.path());
    let before = std::fs::read_to_string(&path).unwrap();
    let cfg = Config::load(&path).unwrap();
    let (handle, app) = open(cx, cfg, path.clone());
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        app.update(cx, |app, cx| app.open_settings(window, cx));
        window.render_frame(cx);

        app.update(cx, |app, cx| {
            app.focus_settings_field("ui.editor", window, cx)
        });
        window.input(" code", cx);
        window.click("settings-save", cx);
        window.render_frame(cx);

        assert!(
            window.try_find("settings-saved").is_some(),
            "save did not confirm"
        );
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("editor = \"zed code\""), "{after}");
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("config.toml.bak")).unwrap(),
            before
        );
        assert_eq!(
            app.read(cx).palette().canvas,
            doki_ui::palette(doki_core::config::Theme::Sumi).canvas
        );
    })
    .unwrap();
}
