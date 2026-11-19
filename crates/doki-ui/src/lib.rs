mod center;
pub mod diff;
mod fonts;
mod journal_view;
mod model;
mod settings;
pub mod studio;
mod theme;

pub use model::{RepoVm, RowState, format_elapsed};
pub use theme::{Palette, palette};

use center::{RepoView, StudioState};
use doki_agent::messenger::AgentMessenger;
use doki_core::config::{Config, Theme as ConfigTheme, config_path, state_dir};
use doki_core::discovery;
use doki_core::git::Git;
use doki_core::lock::{LockAttempt, RepoLock};
use doki_core::pool::{self, Event, EventBus};
use doki_core::scheduler;
use doki_core::status;
use doki_core::store::{RepoState as StoredState, Store};
use doki_core::sync::{self, SyncOptions};
use doki_core::timer::{self, TimerState};
use doki_core::watch::{self, Watch, WatchMode};
use gpui_kit::assets::IconName;
use gpui_kit::component::IndexPath;
use gpui_kit::component::VirtualListScrollHandle;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::input::{Input, InputEvent, InputState, TextareaState};
use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Icon, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const LEFT_DOCK: f32 = 268.;
const RIGHT_DOCK: f32 = 300.;
const HEADER: f32 = 44.;
const TICK_SECONDS: i64 = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PaletteTarget {
    Repo(i64),
    SyncAll,
    DryRun,
    Settings,
    Rescan,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Nav {
    All,
    Diff,
    Commit,
}

impl Nav {
    fn label(self) -> &'static str {
        match self {
            Self::All => "All Trees",
            Self::Diff => "Diff Inspector",
            Self::Commit => "AI Commit",
        }
    }
}

pub struct DokiApp {
    cfg: Config,
    cfg_path: PathBuf,
    store: Option<Store>,
    db_path: Option<PathBuf>,
    state_dir: PathBuf,
    home: PathBuf,
    repos: Vec<RepoVm>,
    selected: Option<i64>,
    auto_selected: Option<i64>,
    root_filter: Option<String>,
    nav: Nav,
    filter_open: bool,
    filter: String,
    filter_input: Entity<InputState>,
    add_root_open: bool,
    add_root_input: Entity<InputState>,
    add_root_error: Option<String>,
    collapsed_disabled: bool,
    dry_run: bool,
    syncing: bool,
    scanning: bool,
    refreshing: bool,
    error: Option<String>,
    now: i64,
    last_tick: i64,
    next_sync_at: Option<i64>,
    timer: TimerState,
    last_timer_check: i64,
    last_status_refresh: i64,
    watch: Watch,
    p: Palette,
    theme: ConfigTheme,
    appearance: WindowAppearance,
    focus_handle: FocusHandle,
    avatar_open: bool,
    palette_open: bool,
    palette_targets: Vec<PaletteTarget>,
    palette_repos: Vec<i64>,
    command: Entity<CommandState>,
    journal: journal_view::JournalFeed,
    journal_scroll: VirtualListScrollHandle,
    settings: Option<settings::SettingsView>,
    scheduler_lock: Option<RepoLock>,
    views: HashMap<i64, RepoView>,
    view_jobs: std::collections::HashSet<i64>,
    studio: StudioState,
    subject_input: Entity<InputState>,
    body_input: Entity<TextareaState>,
    pending_subject: Option<String>,
    pending_body: Option<String>,
    diff_scroll: VirtualListScrollHandle,
    _filter_sub: Subscription,
    _add_root_sub: Subscription,
    _subject_sub: Subscription,
    _body_sub: Subscription,
    _appearance_sub: Subscription,
}

impl DokiApp {
    fn new(
        cfg: Config,
        store: Store,
        dry_run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let db_path = state_dir().join("doki.db");
        let mut app = Self::base(
            cfg,
            Some(store),
            Some(db_path),
            Vec::new(),
            now_unix(),
            window,
            cx,
        );
        app.dry_run = dry_run;
        app.reload();
        app.reload_journal();
        app.plan_next();
        app.start_tick(cx);
        app.refresh_timer(cx);
        app.rescan(cx);
        app.refresh_statuses(cx);
        // Lets the visual check open a view without synthetic input.
        match std::env::var("DOKI_START_VIEW").as_deref() {
            Ok("settings") => app.open_settings(window, cx),
            Ok("palette") => app.open_palette(window, cx),
            _ => {}
        }
        app
    }

    fn base(
        cfg: Config,
        store: Option<Store>,
        db_path: Option<PathBuf>,
        repos: Vec<RepoVm>,
        now: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let filter_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Filter repos")
                .submit_on_enter(false)
        });
        let add_root_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("~/Projects")
                .submit_on_enter(true)
        });
        let subject_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("doki: describe the change")
                .submit_on_enter(false)
        });
        let body_input =
            cx.new(|cx| TextareaState::new(window, cx).placeholder("Optional commit body"));
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let filter_sub = cx.subscribe(
            &filter_input,
            |this: &mut Self, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.filter = this.filter_input.read(cx).value().to_string();
                    cx.notify();
                }
            },
        );
        let add_root_sub = cx.subscribe(
            &add_root_input,
            |this: &mut Self, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.commit_add_root(cx);
                }
            },
        );
        let subject_sub = cx.subscribe(
            &subject_input,
            |this: &mut Self, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = this.subject_input.read(cx).value().to_string();
                    if value != this.studio.subject {
                        this.studio.dirty = true;
                    }
                    this.studio.subject = value;
                    cx.notify();
                }
            },
        );
        let body_sub = cx.subscribe(&body_input, |this: &mut Self, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let value = this.body_input.read(cx).value().to_string();
                if value != this.studio.body {
                    this.studio.dirty = true;
                }
                this.studio.body = value;
                cx.notify();
            }
        });
        let command = cx.new(|cx| CommandState::new(window, cx));
        let focus_handle = cx.focus_handle();
        let appearance = window.appearance();
        let resolved = theme::resolve(cfg.ui.theme, appearance);
        let appearance_sub = cx.observe_window_appearance(window, |app, window, cx| {
            app.apply_appearance(window.appearance(), cx);
        });
        let mut app = Self {
            cfg,
            cfg_path: config_path(),
            store,
            db_path,
            state_dir: state_dir(),
            home,
            repos,
            selected: None,
            auto_selected: None,
            root_filter: None,
            nav: Nav::All,
            filter_open: false,
            filter: String::new(),
            filter_input,
            add_root_open: false,
            add_root_input,
            add_root_error: None,
            collapsed_disabled: true,
            dry_run: false,
            syncing: false,
            scanning: false,
            refreshing: false,
            error: None,
            now,
            last_tick: now,
            next_sync_at: None,
            timer: TimerState::Absent,
            last_timer_check: now,
            last_status_refresh: now,
            watch: Watch::polling(),
            p: palette(resolved),
            theme: resolved,
            appearance,
            focus_handle,
            avatar_open: false,
            palette_open: false,
            palette_targets: Vec::new(),
            palette_repos: Vec::new(),
            command,
            journal: journal_view::JournalFeed::default(),
            journal_scroll: VirtualListScrollHandle::new(),
            settings: None,
            scheduler_lock: None,
            views: HashMap::new(),
            view_jobs: std::collections::HashSet::new(),
            studio: StudioState::default(),
            subject_input,
            body_input,
            pending_subject: None,
            pending_body: None,
            diff_scroll: VirtualListScrollHandle::new(),
            _filter_sub: filter_sub,
            _add_root_sub: add_root_sub,
            _subject_sub: subject_sub,
            _body_sub: body_sub,
            _appearance_sub: appearance_sub,
        };
        app.p = palette(app.theme);
        gpui_kit::component::Theme::change(theme::mode(app.theme), None, cx);
        app.focus_handle.focus(window, cx);
        app.recompute_next_sync();
        app.ensure_selection();
        app.reload_journal();
        app
    }

    #[doc(hidden)]
    pub fn fixture(
        cfg: Config,
        repos: Vec<RepoVm>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::base(cfg, None, None, repos, 1_001, window, cx)
    }

    #[doc(hidden)]
    pub fn fixture_with_store(
        cfg: Config,
        store: Store,
        repos: Vec<RepoVm>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::base(cfg, Some(store), None, repos, 1_001, window, cx)
    }

    #[doc(hidden)]
    pub fn fixture_with_config_path(
        cfg: Config,
        cfg_path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut app = Self::base(cfg, None, None, Vec::new(), 1_001, window, cx);
        app.cfg_path = cfg_path;
        app
    }

    #[doc(hidden)]
    pub fn focus_subject(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.subject_input
            .update(cx, |state, cx| state.focus(window, cx));
    }

    #[doc(hidden)]
    pub fn subject_value(&self, cx: &gpui_kit::App) -> String {
        self.subject_input.read(cx).value().to_string()
    }

    #[doc(hidden)]
    pub fn scope_override(&self, id: i64) -> Option<doki_core::config::CommitScope> {
        self.store
            .as_ref()
            .and_then(|store| store.repo(id).ok().flatten())
            .and_then(|row| row.commit_scope)
    }

    #[doc(hidden)]
    pub fn set_timer_state(&mut self, state: TimerState, cx: &mut Context<Self>) {
        self.timer = state;
        cx.notify();
    }

    #[doc(hidden)]
    pub fn set_dry_run(&mut self, on: bool) {
        self.dry_run = on;
    }

    #[doc(hidden)]
    pub fn set_view(&mut self, id: i64, view: center::RepoView) {
        self.views.insert(id, view);
    }

    pub fn selected(&self) -> Option<i64> {
        self.selected
    }

    pub fn row_state(&self, id: i64) -> Option<RowState> {
        self.repos
            .iter()
            .find(|repo| repo.id == id)
            .map(RepoVm::row_state)
    }

    pub fn palette(&self) -> Palette {
        self.p
    }

    #[doc(hidden)]
    pub fn palette_query(&self, cx: &App) -> String {
        self.command.read(cx).query(cx).to_string()
    }

    #[doc(hidden)]
    pub fn palette_selected(&self, cx: &App) -> Option<IndexPath> {
        self.command.read(cx).selected_index()
    }

    #[doc(hidden)]
    pub fn palette_matches(&self, cx: &App) -> usize {
        self.command.read(cx).matched_count()
    }

    #[doc(hidden)]
    pub fn palette_is_open(&self) -> bool {
        self.palette_open
    }

    #[doc(hidden)]
    pub fn root_focus(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    #[doc(hidden)]
    pub fn set_palette_query(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        let query = query.to_string();
        self.command
            .update(cx, |state, cx| state.set_query(query, window, cx));
        cx.notify();
    }

    #[doc(hidden)]
    pub fn set_theme(&mut self, theme: ConfigTheme, cx: &mut Context<Self>) {
        self.cfg.ui.theme = theme;
        self.apply_theme(theme, cx);
    }

    pub(crate) fn apply_theme(&mut self, theme: ConfigTheme, cx: &mut Context<Self>) {
        let resolved = theme::resolve(theme, self.appearance);
        self.theme = resolved;
        self.p = palette(resolved);
        gpui_kit::component::Theme::change(theme::mode(resolved), None, cx);
        cx.notify();
    }

    fn apply_appearance(&mut self, appearance: WindowAppearance, cx: &mut Context<Self>) {
        self.appearance = appearance;
        if self.cfg.ui.theme == ConfigTheme::System {
            self.apply_theme(ConfigTheme::System, cx);
        }
    }

    fn handle_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette_open {
            return;
        }
        let key = event.keystroke.key.to_ascii_lowercase();
        let modifiers = event.keystroke.modifiers;
        if key == "escape" {
            if self.settings.take().is_some() {
                self.apply_theme(self.cfg.ui.theme, cx);
            }
            self.scope_close();
            self.avatar_open = false;
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if modifiers.control || modifiers.platform {
            if key == "k" {
                self.open_palette(window, cx);
                cx.stop_propagation();
            }
            return;
        }
        if modifiers.alt || modifiers.function {
            return;
        }
        match key.as_str() {
            "j" => {
                self.move_selection(1, cx);
                cx.stop_propagation();
            }
            "k" => {
                self.move_selection(-1, cx);
                cx.stop_propagation();
            }
            "enter" => {
                if let Some(id) = self.selected {
                    self.nav = Nav::All;
                    self.select_repo(id, cx);
                }
                cx.stop_propagation();
            }
            "s" => {
                self.sync_selected(cx);
                cx.stop_propagation();
            }
            "d" => {
                self.dry_run = !self.dry_run;
                cx.notify();
                cx.stop_propagation();
            }
            "u" => {
                if let Some(id) = self.selected {
                    self.confirm_undo(id, window, cx);
                }
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    fn scope_close(&mut self) {
        self.studio.scope_open = false;
        self.studio.abortable = false;
    }

    pub(crate) fn move_selection(&mut self, delta: i32, cx: &mut Context<Self>) {
        let ids: Vec<i64> = self
            .visible_repos()
            .into_iter()
            .filter(|repo| repo.enabled)
            .map(|repo| repo.id)
            .collect();
        if ids.is_empty() {
            return;
        }
        let current = self
            .selected
            .and_then(|id| ids.iter().position(|candidate| *candidate == id));
        let next = match current {
            Some(index) => (index as i32 + delta).rem_euclid(ids.len() as i32) as usize,
            None if delta < 0 => ids.len() - 1,
            None => 0,
        };
        self.select_repo(ids[next], cx);
    }

    fn sync_selected(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.selected else {
            return;
        };
        let Some(repo) = self.repos.iter().find(|repo| repo.id == id) else {
            return;
        };
        if !repo.enabled {
            self.error = Some("the selected repo is disabled".into());
            cx.notify();
            return;
        }
        if self.syncing {
            return;
        }
        let path = repo.path.clone();
        if !self.acquire_scheduler() {
            self.error = Some("scheduler busy: another doki run is active".into());
            cx.notify();
            return;
        }
        self.begin_sync(vec![path], cx);
    }

    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette_open {
            return;
        }
        self.settings = None;
        let repos: Vec<i64> = self
            .visible_repos()
            .into_iter()
            .filter(|repo| repo.enabled)
            .map(|repo| repo.id)
            .collect();
        let mut targets: Vec<PaletteTarget> =
            repos.iter().map(|id| PaletteTarget::Repo(*id)).collect();
        targets.extend([
            PaletteTarget::SyncAll,
            PaletteTarget::DryRun,
            PaletteTarget::Settings,
            PaletteTarget::Rescan,
        ]);
        self.palette_repos = repos;
        self.palette_targets = targets;
        self.palette_open = true;
        self.command.update(cx, |state, cx| {
            state.set_query("", window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    #[doc(hidden)]
    pub fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.palette_open {
            return;
        }
        self.palette_open = false;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn palette_pick(&mut self, index: IndexPath, window: &mut Window, cx: &mut Context<Self>) {
        let target = if index.section == 0 {
            self.palette_repos
                .get(index.row)
                .copied()
                .map(PaletteTarget::Repo)
        } else {
            [
                PaletteTarget::SyncAll,
                PaletteTarget::DryRun,
                PaletteTarget::Settings,
                PaletteTarget::Rescan,
            ]
            .get(index.row)
            .copied()
        };
        self.close_palette(window, cx);
        match target {
            Some(PaletteTarget::Repo(id)) => {
                self.nav = Nav::All;
                self.select_repo(id, cx);
            }
            Some(PaletteTarget::SyncAll) => self.sync_all(cx),
            Some(PaletteTarget::DryRun) => {
                self.dry_run = !self.dry_run;
                cx.notify();
            }
            Some(PaletteTarget::Settings) => self.open_settings(window, cx),
            Some(PaletteTarget::Rescan) => self.rescan(cx),
            None => {}
        }
    }

    fn avatar_menu(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p;
        div()
            .id("avatar-menu")
            .test_support()
            .absolute()
            .top(px(HEADER + 4.))
            .right(px(12.))
            .w(px(180.))
            .p(px(4.))
            .gap(px(2.))
            .rounded(px(4.))
            .bg(p.surface)
            .border_1()
            .border_color(p.hairline)
            .child(
                div()
                    .id("settings-open")
                    .test_support()
                    .aria_label("Settings")
                    .px(px(8.))
                    .py(px(5.))
                    .rounded(px(2.))
                    .cursor_pointer()
                    .hover(|style| style.bg(p.button))
                    .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx)))
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .child(icon(IconName::Settings, 13., p.text_dim))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(p.text_muted)
                                    .child("Settings"),
                            ),
                    ),
            )
            .child(
                div()
                    .id("rescan-now")
                    .test_support()
                    .aria_label("Rescan roots")
                    .px(px(8.))
                    .py(px(5.))
                    .rounded(px(2.))
                    .cursor_pointer()
                    .hover(|style| style.bg(p.button))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.avatar_open = false;
                        this.rescan(cx);
                    }))
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .child(icon(IconName::RefreshCw, 13., p.text_dim))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(p.text_muted)
                                    .child("Rescan roots"),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn palette_overlay(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p;
        let mut repos = Vec::new();
        for id in self.palette_repos.clone() {
            if let Some(repo) = self.repos.iter().find(|repo| repo.id == id) {
                let keywords = vec![
                    repo.name.clone(),
                    repo.branch_label(),
                    repo.path.display().to_string(),
                ];
                repos.push(
                    CommandItem::new()
                        .label(repo.name.clone())
                        .icon(IconName::FileText)
                        .keywords(keywords),
                );
            }
        }
        let actions = vec![
            CommandItem::new()
                .label("Sync All")
                .icon(IconName::RefreshCw)
                .keywords(["sync", "run"]),
            CommandItem::new()
                .label("Toggle Dry Run")
                .icon(IconName::Eye)
                .keywords(["dry", "run", "toggle"]),
            CommandItem::new()
                .label("Settings")
                .icon(IconName::Settings)
                .keywords(["config", "preferences"]),
            CommandItem::new()
                .label("Rescan")
                .icon(IconName::RefreshCw)
                .keywords(["roots", "discover"]),
        ];
        let entity = cx.entity();
        let confirm = move |index: IndexPath, window: &mut Window, cx: &mut App| {
            entity.update(cx, |this, cx| this.palette_pick(index, window, cx));
        };
        let entity = cx.entity();
        let cancel = move |window: &mut Window, cx: &mut App| {
            entity.update(cx, |this, cx| this.close_palette(window, cx));
        };
        let command = Command::new(&self.command)
            .placeholder("Jump to a repo or run a command")
            .bordered(false)
            .max_h(px(420.))
            .group(CommandGroup::new().label("Repos").items(repos))
            .group(CommandGroup::new().label("Actions").items(actions))
            .on_confirm(confirm)
            .on_cancel(cancel);
        div()
            .id("palette-overlay")
            .test_support()
            .absolute()
            .top(px(0.))
            .right(px(0.))
            .bottom(px(0.))
            .left(px(0.))
            .bg(fade(p.canvas, 0.72))
            .flex()
            .justify_center()
            .items_start()
            .pt(px(96.))
            .on_click(cx.listener(|this, _, window, cx| this.close_palette(window, cx)))
            .child(
                div()
                    .id("palette-card")
                    .test_support()
                    .w(px(560.))
                    .h(px(480.))
                    .p(px(4.))
                    .rounded(px(8.))
                    .bg(p.pane_raised)
                    .border_1()
                    .border_color(p.hairline)
                    .on_click(|_, _, cx: &mut App| cx.stop_propagation())
                    .child(
                        div()
                            .font_family(fonts::SANS)
                            .text_size(px(12.))
                            .child(command),
                    ),
            )
            .into_any_element()
    }

    fn start_tick(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let alive = this.update(cx, |app, cx| {
                    app.now = now_unix();
                    if app.now - app.last_tick >= TICK_SECONDS {
                        app.last_tick = app.now;
                        app.reload_journal();
                        app.scheduler_tick(cx);
                        if app.now - app.last_timer_check >= 60 {
                            app.last_timer_check = app.now;
                            app.refresh_timer(cx);
                        }
                    }
                    let interval = match app.watch.mode() {
                        WatchMode::Native => 3,
                        WatchMode::Polling => 30,
                    };
                    if app.now - app.last_status_refresh >= interval
                        && (app.watch.mode() == WatchMode::Polling || app.watch.changed())
                    {
                        app.last_status_refresh = app.now;
                        app.refresh_statuses(cx);
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn scheduler_tick(&mut self, cx: &mut Context<Self>) {
        if self.syncing || self.scanning || self.store.is_none() {
            return;
        }
        if !self.acquire_scheduler() {
            return;
        }
        let due = {
            let store = self.store.as_ref().expect("checked above");
            scheduler::due(&self.cfg, store, self.now)
        };
        match due {
            Ok(rows) if rows.is_empty() => {
                self.scheduler_lock = None;
                self.recompute_next_sync();
            }
            Ok(rows) => {
                let paths = rows.into_iter().map(|row| row.path).collect();
                self.begin_sync(paths, cx);
            }
            Err(error) => {
                self.scheduler_lock = None;
                self.error = Some(format!("scheduler: {error:#}"));
            }
        }
        cx.notify();
    }

    fn acquire_scheduler(&mut self) -> bool {
        if self.scheduler_lock.is_some() {
            return true;
        }
        match scheduler::try_lock(&self.state_dir) {
            Ok(LockAttempt::Acquired(lock)) => {
                self.scheduler_lock = Some(lock);
                true
            }
            Ok(LockAttempt::Busy) => false,
            Err(error) => {
                self.error = Some(format!("scheduler lock: {error:#}"));
                false
            }
        }
    }

    fn begin_sync(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if self.syncing || paths.is_empty() {
            return;
        }
        let Some(db) = self.db_path.clone() else {
            return;
        };
        let cfg = self.cfg.clone();
        let state_dir = self.state_dir.clone();
        let dry_run = self.dry_run;
        let bus = EventBus::new();
        let live = bus.subscribe();
        let (done_tx, done_rx) = async_channel::unbounded::<usize>();
        self.syncing = true;
        self.error = None;
        std::thread::spawn(move || {
            let results = pool::run(paths, cfg.sync.concurrency.max(1), &bus, |path| {
                let store = Store::open(&db)?;
                let messenger = AgentMessenger::new(&cfg, path)?;
                sync::sync_with(
                    &cfg,
                    &store,
                    path,
                    &messenger,
                    SyncOptions {
                        dry_run,
                        state_dir: Some(state_dir.clone()),
                    },
                )
            });
            let _ = done_tx.send_blocking(results.len());
        });
        cx.spawn(async move |this, cx| {
            while let Ok(event) = live.recv().await {
                let _ = this.update(cx, |app, cx| {
                    app.apply_pool_event(event);
                    cx.notify();
                });
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            if done_rx.recv().await.is_ok() {
                let _ = this.update(cx, |app, cx| app.finish_sync(cx));
            }
        })
        .detach();
        cx.notify();
    }

    fn finish_sync(&mut self, cx: &mut Context<Self>) {
        self.syncing = false;
        self.scheduler_lock = None;
        self.reload();
        self.reload_journal();
        self.refresh_statuses(cx);
        self.recompute_next_sync();
        cx.notify();
    }

    fn apply_pool_event(&mut self, event: Event) {
        match event {
            Event::RunStarted { repo, at } => {
                if let Some(row) = self.repos.iter_mut().find(|row| row.path == repo) {
                    row.running_since = Some(at);
                    row.stored = Some(StoredState::Running);
                }
            }
            Event::RunFinished {
                repo,
                state,
                detail,
                ..
            } => {
                if let Some(row) = self.repos.iter_mut().find(|row| row.path == repo) {
                    row.running_since = None;
                    row.stored = StoredState::parse(&state);
                    if state == "failed" {
                        row.last_error = detail;
                    }
                }
            }
            Event::Incident { repo, detail, .. } => {
                if let Some(row) = self.repos.iter_mut().find(|row| row.path == repo) {
                    row.last_error = Some(detail);
                }
            }
        }
        self.reload_journal();
    }

    fn sync_all(&mut self, cx: &mut Context<Self>) {
        if self.syncing {
            return;
        }
        if self.store.is_none() {
            return;
        }
        let paths: Vec<PathBuf> = self
            .repos
            .iter()
            .filter(|repo| repo.enabled)
            .map(|repo| repo.path.clone())
            .collect();
        if paths.is_empty() {
            self.error = Some("no enabled repos".into());
            cx.notify();
            return;
        }
        if !self.acquire_scheduler() {
            if self.scheduler_lock.is_none() && self.error.is_none() {
                self.error = Some("scheduler busy: another doki run is active".into());
            }
            cx.notify();
            return;
        }
        self.begin_sync(paths, cx);
    }

    fn reload(&mut self) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let rows = match store.repos() {
            Ok(rows) => rows,
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                return;
            }
        };
        let previous: HashMap<i64, RepoVm> =
            self.repos.iter().cloned().map(|vm| (vm.id, vm)).collect();
        let now = self.now;
        let mut repos = Vec::with_capacity(rows.len());
        for row in rows {
            let mut vm = RepoVm::from_row(&row, &self.cfg, now);
            if let Some(prev) = previous.get(&row.id) {
                vm.live = prev.live.clone();
                vm.branch = prev.branch.clone();
                vm.upstream = prev.upstream.clone();
                vm.ahead = prev.ahead;
                vm.behind = prev.behind;
                vm.changed = prev.changed;
                vm.error = prev.error.clone();
                vm.running_since = prev.running_since;
            }
            repos.push(vm);
        }
        self.repos = repos;
        if self
            .selected
            .is_some_and(|id| !self.repos.iter().any(|repo| repo.id == id))
        {
            self.selected = None;
        }
        self.rebuild_watch();
    }

    fn rebuild_watch(&mut self) {
        let paths: Vec<PathBuf> = self
            .repos
            .iter()
            .filter(|repo| repo.enabled)
            .map(|repo| repo.path.clone())
            .collect();
        self.watch = watch::watch_repos(&paths, watch::DEFAULT_WATCH_CAP);
    }

    fn plan_next(&mut self) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        if let Err(error) = scheduler::plan_missing(&self.cfg, store, self.now) {
            self.error = Some(format!("scheduler: {error:#}"));
        }
        self.recompute_next_sync();
    }

    fn recompute_next_sync(&mut self) {
        let from_rows = self
            .repos
            .iter()
            .filter(|repo| repo.enabled)
            .filter_map(|repo| repo.next_run_at)
            .min();
        self.next_sync_at = from_rows.or_else(|| {
            let schedule = self
                .cfg
                .roots
                .iter()
                .find(|root| root.enabled)
                .map_or(scheduler::DEFAULT_SCHEDULE, |root| root.schedule.as_str());
            scheduler::next_occurrence(schedule, self.now).ok()
        });
    }

    fn rescan(&mut self, cx: &mut Context<Self>) {
        if self.scanning || self.store.is_none() {
            return;
        }
        self.scanning = true;
        self.error = None;
        let (roots, opts) = (self.cfg.roots.clone(), self.cfg.discovery.clone());
        let job = cx.background_spawn(async move { discovery::scan(&roots, &opts) });
        cx.spawn(async move |this, cx| {
            let found = job.await;
            let _ = this.update(cx, |app, cx| {
                app.scanning = false;
                match found
                    .and_then(|found| app.store.as_mut().expect("store").upsert_detected(&found))
                {
                    Ok(()) => {
                        app.reload();
                        app.plan_next();
                        app.ensure_view(cx);
                        app.refresh_statuses(cx);
                    }
                    Err(error) => app.error = Some(format!("scan failed: {error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn refresh_statuses(&mut self, cx: &mut Context<Self>) {
        if self.refreshing {
            return;
        }
        let paths: Vec<PathBuf> = self
            .repos
            .iter()
            .filter(|repo| repo.enabled)
            .map(|repo| repo.path.clone())
            .collect();
        if paths.is_empty() {
            return;
        }
        self.refreshing = true;
        let (tx, rx) = async_channel::unbounded::<(PathBuf, anyhow::Result<status::RepoStatus>)>();
        std::thread::spawn(move || {
            let results = pool::run_each(paths, 2, |path| status::read(&Git::new(path)));
            for (path, result) in results {
                let _ = tx.send_blocking((path, result));
            }
        });
        cx.spawn(async move |this, cx| {
            while let Ok((path, result)) = rx.recv().await {
                let _ = this.update(cx, |app, cx| {
                    if let Some(row) = app.repos.iter_mut().find(|row| row.path == path) {
                        row.apply_status(result.map_err(|error| format!("{error:#}")));
                    }
                    cx.notify();
                });
            }
            let _ = this.update(cx, |app, cx| {
                app.refreshing = false;
                app.recompute_next_sync();
                app.reload_journal();
                app.ensure_selection();
                app.ensure_view(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn set_enabled(&mut self, id: i64, enabled: bool, cx: &mut Context<Self>) {
        self.error = None;
        if let Some(store) = self.store.as_ref() {
            if let Err(error) = store.set_enabled(id, enabled) {
                self.error = Some(format!("{error:#}"));
            }
        } else if let Some(repo) = self.repos.iter_mut().find(|repo| repo.id == id) {
            repo.enabled = enabled;
            repo.stored = (!enabled).then_some(StoredState::Disabled);
        }
        self.reload();
        self.refresh_statuses(cx);
        self.recompute_next_sync();
        cx.notify();
    }

    fn toggle_root(&mut self, name: &str) {
        if self.root_filter.as_deref() == Some(name) {
            self.root_filter = None;
        } else {
            self.root_filter = Some(name.to_string());
        }
    }

    fn toggle_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter_open = !self.filter_open;
        if self.filter_open {
            self.filter_input.update(cx, |state, cx| {
                state.set_value("", window, cx);
                state.focus(window, cx);
            });
            self.filter.clear();
        } else {
            self.filter.clear();
            self.filter_input
                .update(cx, |state, cx| state.set_value("", window, cx));
        }
        cx.notify();
    }

    fn open_add_root(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.add_root_open = true;
        self.add_root_error = None;
        self.add_root_input.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    fn commit_add_root(&mut self, cx: &mut Context<Self>) {
        let raw = self.add_root_input.read(cx).value().to_string();
        match doki_core::config_edit::add_root_at(&self.cfg_path, &raw) {
            Ok(_) => {
                self.add_root_open = false;
                self.add_root_error = None;
                self.root_filter = None;
                match Config::load(&self.cfg_path) {
                    Ok(cfg) => {
                        self.p = palette(cfg.ui.theme);
                        self.cfg = cfg;
                    }
                    Err(error) => self.error = Some(format!("{error:#}")),
                }
                self.rescan(cx);
            }
            Err(error) => self.add_root_error = Some(format!("{error:#}")),
        }
        cx.notify();
    }

    fn ensure_selection(&mut self) {
        if self.auto_selected.is_some() || self.selected.is_none() {
            let picked = [
                RowState::Dirty,
                RowState::Incident,
                RowState::Waiting,
                RowState::Running,
            ]
            .iter()
            .find_map(|state| {
                self.repos
                    .iter()
                    .find(|repo| repo.enabled && repo.row_state() == *state)
                    .map(|repo| repo.id)
            })
            .or_else(|| {
                self.repos
                    .iter()
                    .find(|repo| repo.enabled)
                    .map(|repo| repo.id)
            });
            self.selected = picked;
            self.auto_selected = picked;
        }
    }

    fn visible_repos(&self) -> Vec<&RepoVm> {
        self.repos
            .iter()
            .filter(|repo| self.matches_root(repo))
            .filter(|repo| self.matches_text(repo))
            .filter(|repo| match self.nav {
                Nav::All => true,
                Nav::Diff => !matches!(repo.row_state(), RowState::Clean | RowState::Disabled),
                Nav::Commit => repo.row_state() == RowState::Dirty,
            })
            .collect()
    }

    fn visible_disabled(&self) -> Vec<&RepoVm> {
        self.repos
            .iter()
            .filter(|repo| !repo.enabled)
            .filter(|repo| self.matches_root(repo))
            .filter(|repo| self.matches_text(repo))
            .collect()
    }

    fn matches_root(&self, repo: &RepoVm) -> bool {
        self.root_filter
            .as_deref()
            .is_none_or(|root| repo.root == root)
    }

    fn matches_text(&self, repo: &RepoVm) -> bool {
        if self.filter.trim().is_empty() {
            return true;
        }
        let needle = self.filter.to_lowercase();
        repo.name.to_lowercase().contains(&needle)
            || repo.branch_label().to_lowercase().contains(&needle)
            || repo
                .path
                .display()
                .to_string()
                .to_lowercase()
                .contains(&needle)
    }

    fn dirty_repos(&self) -> usize {
        self.repos
            .iter()
            .filter(|repo| repo.enabled && repo.changed > 0)
            .count()
    }

    fn managed_repos(&self) -> usize {
        self.repos.iter().filter(|repo| repo.enabled).count()
    }

    fn shorten(&self, path: &Path) -> String {
        shorten_home(&self.home, path)
    }

    fn timer_mode(&self) -> &'static str {
        match self.timer {
            TimerState::Active => "systemd: doki.timer",
            TimerState::Installed => "systemd: doki.timer (inactive)",
            TimerState::Absent => "in-app scheduler",
        }
    }

    fn refresh_timer(&mut self, cx: &mut Context<Self>) {
        let job = cx.background_spawn(async { timer::status() });
        cx.spawn(async move |this, cx| {
            let state = job.await;
            let _ = this.update(cx, |app, cx| {
                if app.timer != state {
                    app.timer = state;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn schedule_label(&self) -> String {
        let schedule = self
            .cfg
            .roots
            .iter()
            .find(|root| root.enabled)
            .map_or(scheduler::DEFAULT_SCHEDULE, |root| root.schedule.as_str());
        schedule_interval(schedule)
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.p;
        let countdown = format_next_sync(self.next_sync_at, self.now);
        let version = concat!("v", env!("CARGO_PKG_VERSION"));
        let mut tabs = h_flex().gap(px(6.)).items_center();
        for (index, root) in self.cfg.roots.iter().enumerate() {
            if index > 0 {
                tabs = tabs.child(
                    div()
                        .text_size(px(11.))
                        .font_family(fonts::MONO)
                        .text_color(p.text_faint)
                        .child("/"),
                );
            }
            let active = self.root_filter.as_deref() == Some(root.name.as_str());
            let name = root.name.clone();
            let label = self.shorten(&root.path);
            tabs = tabs.child(
                div()
                    .id(ElementId::Name(format!("root-tab-{}", root.name).into()))
                    .test_support()
                    .text_size(px(11.))
                    .font_family(fonts::MONO)
                    .text_color(if active { p.text } else { p.text_muted })
                    .cursor_pointer()
                    .hover(|style| style.text_color(p.text))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_root(&name);
                        cx.notify();
                    }))
                    .child(label),
            );
        }
        h_flex()
            .h(px(HEADER))
            .w_full()
            .flex_shrink_0()
            .px(px(16.))
            .bg(p.chrome)
            .border_b_1()
            .border_color(p.hairline)
            .justify_between()
            .child(
                h_flex()
                    .gap(px(16.))
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.text)
                                    .child("dokidoki"),
                            )
                            .child(
                                div()
                                    .px(px(6.))
                                    .py(px(2.))
                                    .rounded(px(4.))
                                    .bg(p.button)
                                    .border_1()
                                    .border_color(p.hairline)
                                    .text_size(px(10.))
                                    .font_family(fonts::MONO)
                                    .text_color(p.text_dim)
                                    .child(version),
                            ),
                    )
                    .child(tabs),
            )
            .child(
                h_flex()
                    .gap(px(12.))
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .pr(px(8.))
                            .border_r_1()
                            .border_color(p.hairline)
                            .child(dot(px(6.), p.success))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .font_family(fonts::MONO)
                                    .text_color(p.text_dim)
                                    .child("next sync"),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .font_family(fonts::MONO)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.text_muted)
                                    .child(countdown),
                            ),
                    )
                    .child(
                        h_flex()
                            .id("dry-run-toggle")
                            .test_support()
                            .aria_label("Dry Run")
                            .gap(px(6.))
                            .px(px(8.))
                            .py(px(2.))
                            .rounded(px(4.))
                            .border_1()
                            .border_color(if self.dry_run {
                                p.warning_border
                            } else {
                                p.hairline
                            })
                            .bg(p.pane_raised)
                            .cursor_pointer()
                            .hover(|style| style.bg(p.button_hover))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.dry_run = !this.dry_run;
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .font_family(fonts::MONO)
                                    .text_color(if self.dry_run { p.text } else { p.text_dim })
                                    .child("Dry Run"),
                            )
                            .child(dot(
                                px(6.),
                                if self.dry_run {
                                    p.warning
                                } else {
                                    p.text_faint
                                },
                            )),
                    )
                    .child(
                        h_flex()
                            .id("filter-toggle")
                            .gap(px(6.))
                            .px(px(10.))
                            .py(px(4.))
                            .rounded(px(4.))
                            .bg(if self.filter_open {
                                p.accent_surface
                            } else {
                                p.button
                            })
                            .border_1()
                            .border_color(if self.filter_open {
                                p.accent_border
                            } else {
                                p.hairline
                            })
                            .cursor_pointer()
                            .hover(|style| style.bg(p.button_hover))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_filter(window, cx);
                            }))
                            .child(icon(
                                IconName::SlidersHorizontal,
                                14.,
                                if self.filter_open {
                                    p.accent
                                } else {
                                    p.text_dim
                                },
                            ))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(p.text_muted)
                                    .child("Filter"),
                            ),
                    )
                    .child(
                        h_flex()
                            .id("sync-all")
                            .gap(px(6.))
                            .px(px(12.))
                            .py(px(4.))
                            .rounded(px(4.))
                            .bg(p.text)
                            .cursor_pointer()
                            .opacity(if self.syncing { 0.65 } else { 1. })
                            .on_click(cx.listener(|this, _, _, cx| this.sync_all(cx)))
                            .child(icon(IconName::RefreshCw, 13., p.canvas))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.canvas)
                                    .child(if self.syncing {
                                        "Syncing…"
                                    } else {
                                        "Sync All"
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .id("avatar")
                            .test_support()
                            .aria_label("Account")
                            .size(px(24.))
                            .rounded_full()
                            .bg(p.button)
                            .border_1()
                            .border_color(p.hairline)
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|style| style.bg(p.button_hover))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.avatar_open = !this.avatar_open;
                                cx.notify();
                            }))
                            .child(icon(IconName::User, 13., p.text_muted)),
                    ),
            )
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.p;
        let dirty = self.dirty_repos();
        let mut nav = v_flex()
            .gap(px(2.))
            .child(self.workspaces_header(cx))
            .child(self.nav_row(
                Nav::All,
                IconName::LayoutDashboard,
                format_count(self.managed_repos(), p.text_dim),
                cx,
            ))
            .child(self.nav_row(
                Nav::Diff,
                IconName::FileDiff,
                if dirty > 0 {
                    dot(px(6.), p.warning).into_any_element()
                } else {
                    div().into_any_element()
                },
                cx,
            ))
            .child(self.nav_row(
                Nav::Commit,
                IconName::Sparkles,
                format_count(dirty, p.text_dim),
                cx,
            ));
        if self.add_root_open {
            nav = nav.child(self.add_root_panel(cx));
        }
        let mut body = v_flex()
            .gap(px(16.))
            .p(px(12.))
            .child(nav)
            .when(self.filter_open, |body| {
                body.child(
                    div()
                        .rounded(px(4.))
                        .bg(p.pane_raised)
                        .border_1()
                        .border_color(p.hairline)
                        .px(px(6.))
                        .py(px(2.))
                        .child(
                            Input::new(&self.filter_input)
                                .id("filter-input")
                                .appearance(false)
                                .bordered(false)
                                .focus_bordered(false)
                                .text_size(px(11.))
                                .font_family(fonts::MONO)
                                .w_full(),
                        ),
                )
            })
            .child(div().h(px(1.)).bg(p.hairline_soft));
        if self.cfg.roots.is_empty() {
            body = body.child(
                div()
                    .px(px(8.))
                    .py(px(12.))
                    .text_size(px(11.))
                    .font_family(fonts::MONO)
                    .text_color(p.text_dim)
                    .child("No roots. Add one with +"),
            );
        } else {
            let mut groups = v_flex().gap(px(16.));
            for root in &self.cfg.roots {
                if self
                    .root_filter
                    .as_deref()
                    .is_some_and(|filter| filter != root.name)
                {
                    continue;
                }
                let rows: Vec<&RepoVm> = self
                    .visible_repos()
                    .into_iter()
                    .filter(|repo| repo.root == root.name && repo.enabled)
                    .collect();
                if rows.is_empty()
                    && (self.filter_open && !self.filter.is_empty()
                        || self.nav != Nav::All
                        || self.visible_repos().is_empty())
                {
                    continue;
                }
                let root_total = self
                    .repos
                    .iter()
                    .filter(|repo| repo.root == root.name)
                    .count();
                let mut group = v_flex()
                    .gap(px(4.))
                    .child(self.root_header(root, root_total));
                for repo in rows {
                    group = group.child(self.repo_row(repo, cx));
                }
                groups = groups.child(group);
            }
            body = body.child(groups);
        }
        let disabled = self.visible_disabled();
        if !disabled.is_empty() && self.nav == Nav::All {
            body = body.child(self.disabled_group(&disabled, cx));
        }
        if !self.cfg.roots.is_empty() && self.visible_repos().is_empty() && disabled.is_empty() {
            body = body.child(
                div()
                    .px(px(8.))
                    .py(px(4.))
                    .text_size(px(11.))
                    .font_family(fonts::MONO)
                    .text_color(p.text_dim)
                    .child("No repos match"),
            );
        }
        v_flex()
            .size_full()
            .bg(p.pane)
            .border_r_1()
            .border_color(p.hairline)
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(body),
            )
            .child(self.sidebar_footer())
    }

    fn workspaces_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.p;
        h_flex()
            .justify_between()
            .px(px(8.))
            .py(px(4.))
            .child(
                div()
                    .text_size(px(10.))
                    .font_family(fonts::MONO)
                    .text_color(p.text_dim)
                    .child("WORKSPACES"),
            )
            .child(
                div()
                    .id("add-root")
                    .cursor_pointer()
                    .text_color(p.text_dim)
                    .hover(|style| style.text_color(p.text))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_add_root(window, cx);
                    }))
                    .child(icon(IconName::Plus, 13., p.text_dim)),
            )
    }

    fn add_root_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.p;
        v_flex()
            .gap(px(4.))
            .p(px(6.))
            .rounded(px(4.))
            .bg(p.pane_raised)
            .border_1()
            .border_color(p.hairline)
            .child(
                h_flex()
                    .gap(px(4.))
                    .child(
                        Input::new(&self.add_root_input)
                            .id("add-root-input")
                            .appearance(false)
                            .bordered(false)
                            .focus_bordered(false)
                            .text_size(px(11.))
                            .font_family(fonts::MONO)
                            .flex_1(),
                    )
                    .child(
                        div()
                            .id("add-root-confirm")
                            .px(px(6.))
                            .py(px(2.))
                            .rounded(px(4.))
                            .bg(p.text)
                            .text_color(p.canvas)
                            .text_size(px(10.))
                            .font_family(fonts::MONO)
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| this.commit_add_root(cx)))
                            .child("Add"),
                    ),
            )
            .when_some(self.add_root_error.clone(), |panel, error| {
                panel.child(div().text_size(px(10.)).text_color(p.danger).child(error))
            })
    }

    fn nav_row(
        &self,
        nav: Nav,
        icon_name: IconName,
        trailing: AnyElement,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = self.p;
        let active = self.nav == nav;
        h_flex()
            .id(ElementId::Name(
                format!("nav-{}", nav.label().replace(' ', "-")).into(),
            ))
            .justify_between()
            .px(px(8.))
            .py(px(6.))
            .rounded(px(4.))
            .border_1()
            .border_color(if active {
                p.hairline_soft
            } else {
                fade(p.hairline, 0.)
            })
            .when(active, |row| row.bg(p.surface))
            .cursor_pointer()
            .when(!active, |row| row.hover(|style| style.bg(p.pane_raised)))
            .on_click(cx.listener(move |this, _, _, cx| this.set_nav(nav, cx)))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(icon(
                        icon_name,
                        15.,
                        if active { p.text_muted } else { p.text_dim },
                    ))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(if active { p.text } else { p.text_muted })
                            .child(nav.label()),
                    ),
            )
            .child(trailing)
    }

    fn root_header(&self, root: &doki_core::config::Root, count: usize) -> impl IntoElement {
        let p = self.p;
        h_flex()
            .justify_between()
            .px(px(8.))
            .py(px(4.))
            .child(
                h_flex()
                    .gap(px(6.))
                    .child(icon(root_icon(root), 14., p.text_dim))
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_family(fonts::MONO)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.text_muted)
                            .truncate()
                            .child(self.shorten(&root.path)),
                    ),
            )
            .child(
                div()
                    .id(ElementId::Name(format!("root-count-{}", root.name).into()))
                    .test_support()
                    .aria_label(format!("{count} repo{}", if count == 1 { "" } else { "s" }))
                    .text_size(px(10.))
                    .font_family(fonts::MONO)
                    .text_color(p.text_dim)
                    .child(format!("{count} repo{}", if count == 1 { "" } else { "s" })),
            )
    }

    fn repo_row(&self, repo: &RepoVm, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.p;
        let id = repo.id;
        let state = repo.row_state();
        let selected = self.selected == Some(id);
        let branch = repo.branch_label();
        let now = self.now;
        let row_label = format!("{} · {} · {}", repo.name, state.label(), repo.line2(now));
        let state_text = repo.state_text();
        let state_detail = repo.state_detail(now);
        let state_color = match state {
            RowState::Dirty => p.warning,
            RowState::Waiting => fade(p.danger, 0.8),
            RowState::Incident => p.danger,
            RowState::Running | RowState::Clean | RowState::Disabled => p.text_dim,
        };
        let mut row = v_flex()
            .id(ElementId::named_usize("repo", id as usize))
            .relative()
            .px(px(9.))
            .py(px(6.))
            .rounded(px(4.))
            .border_1()
            .border_color(if selected {
                p.hairline
            } else if state == RowState::Incident {
                p.danger_border
            } else {
                fade(p.hairline, 0.)
            })
            .when(selected, |row| row.bg(p.surface))
            .when(state == RowState::Incident, |row| row.bg(p.danger_surface))
            .when(!selected, |row| {
                row.hover(|style| style.bg(p.pane_raised).border_color(p.hairline))
            })
            .cursor_pointer()
            .test_support()
            .aria_label(row_label)
            .on_click(cx.listener(move |this, _, _, cx| this.select_repo(id, cx)))
            .child(
                h_flex()
                    .justify_between()
                    .gap(px(6.))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(6.))
                            .child(self.state_dot(state, id))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(12.))
                                    .line_height(px(15.))
                                    .font_family(fonts::MONO)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(if repo.enabled { p.text } else { p.text_muted })
                                    .truncate()
                                    .child(repo.name.clone()),
                            ),
                    )
                    .when(state == RowState::Incident, |line| {
                        line.child(danger_pill(
                            p.danger,
                            p.danger_soft,
                            p.danger_border,
                            "BLOCKED",
                        ))
                    })
                    .when(state != RowState::Incident && selected, |line| {
                        line.child(branch_chip(
                            &branch,
                            p.accent_subtle,
                            p.accent_surface,
                            p.accent_border,
                        ))
                    })
                    .when(state != RowState::Incident && !selected, |line| {
                        line.child(
                            div()
                                .max_w(px(64.))
                                .truncate()
                                .text_size(px(10.))
                                .font_family(fonts::MONO)
                                .text_color(p.text_dim)
                                .child(branch),
                        )
                    }),
            )
            .child(
                h_flex()
                    .justify_between()
                    .pl(px(10.))
                    .child(
                        div()
                            .id(ElementId::named_usize("state", id as usize))
                            .test_support()
                            .aria_label(state_text.clone())
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.))
                            .line_height(px(14.))
                            .font_family(fonts::MONO)
                            .font_weight(if state == RowState::Dirty {
                                FontWeight::MEDIUM
                            } else {
                                FontWeight::NORMAL
                            })
                            .text_color(state_color)
                            .child(state_text),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(10.))
                            .font_family(fonts::MONO)
                            .text_color(p.text_dim)
                            .child(state_detail),
                    ),
            );
        if selected {
            row = row.child(
                div()
                    .absolute()
                    .left(px(0.))
                    .top(px(8.))
                    .bottom(px(8.))
                    .w(px(2.))
                    .rounded(px(1.))
                    .bg(p.accent),
            );
        }
        row
    }

    fn state_dot(&self, state: RowState, id: i64) -> AnyElement {
        let p = self.p;
        let core = match state {
            RowState::Clean => dot(px(6.), p.success).into_any_element(),
            RowState::Dirty => div()
                .size(px(10.))
                .rounded_full()
                .bg(p.warning_aura)
                .flex()
                .items_center()
                .justify_center()
                .child(dot(px(6.), p.warning))
                .into_any_element(),
            RowState::Running => pulse(px(6.), p.accent, id, 1600).into_any_element(),
            RowState::Waiting => div()
                .size(px(6.))
                .rounded_full()
                .border_1()
                .border_color(p.warning)
                .into_any_element(),
            RowState::Incident => pulse(px(6.), p.danger, id, 1600).into_any_element(),
            RowState::Disabled => dot(px(6.), p.text_dim).into_any_element(),
        };
        div()
            .size(px(10.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .child(core)
            .into_any_element()
    }

    fn disabled_group(&self, rows: &[&RepoVm], cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.p;
        let mut group = v_flex().gap(px(2.)).child(
            h_flex()
                .id("disabled-toggle")
                .test_support()
                .justify_between()
                .px(px(8.))
                .py(px(4.))
                .cursor_pointer()
                .hover(|style| style.bg(p.pane_raised))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.collapsed_disabled = !this.collapsed_disabled;
                    cx.notify();
                }))
                .child(
                    h_flex()
                        .gap(px(4.))
                        .child(icon(
                            if self.collapsed_disabled {
                                IconName::ChevronRight
                            } else {
                                IconName::ChevronDown
                            },
                            12.,
                            p.text_dim,
                        ))
                        .child(
                            div()
                                .text_size(px(11.))
                                .font_family(fonts::MONO)
                                .text_color(p.text_dim)
                                .child(format!("Disabled workspace ({})", rows.len())),
                        ),
                )
                .when(rows.len() == 1, |header| {
                    header.child(
                        div()
                            .text_size(px(10.))
                            .font_family(fonts::MONO)
                            .text_color(p.text_faint)
                            .child(rows[0].name.clone()),
                    )
                }),
        );
        if !self.collapsed_disabled {
            for repo in rows {
                group = group.child(self.disabled_row(repo, cx));
            }
        }
        group
    }

    fn disabled_row(&self, repo: &RepoVm, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.p;
        let id = repo.id;
        h_flex()
            .id(ElementId::named_usize("repo", id as usize))
            .justify_between()
            .gap(px(6.))
            .px(px(10.))
            .py(px(4.))
            .rounded(px(4.))
            .hover(|style| style.bg(p.pane_raised))
            .test_support()
            .aria_label(format!("{} · disabled · Disabled", repo.name))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(8.))
                    .child(
                        div()
                            .size(px(12.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(dot(px(8.), p.text_dim)),
                    )
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .font_family(fonts::MONO)
                                    .text_color(p.text_muted)
                                    .truncate()
                                    .child(repo.name.clone()),
                            )
                            .child(
                                div()
                                    .id(ElementId::named_usize("state", id as usize))
                                    .test_support()
                                    .aria_label("Disabled")
                                    .text_size(px(10.))
                                    .font_family(fonts::MONO)
                                    .text_color(p.text_faint)
                                    .truncate()
                                    .child("Disabled"),
                            ),
                    ),
            )
            .child(
                Switch::new(("enable", id as u64))
                    .checked(repo.enabled)
                    .small()
                    .accessibility_label(format!("Enable {}", repo.name))
                    .color(p.success)
                    .on_change(cx.listener(move |this, on: &bool, _, cx| {
                        this.set_enabled(id, *on, cx);
                    })),
            )
    }

    fn sidebar_footer(&self) -> impl IntoElement {
        let p = self.p;
        v_flex()
            .flex_shrink_0()
            .gap(px(6.))
            .p(px(12.))
            .border_t_1()
            .border_color(p.hairline)
            .bg(p.chrome)
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        h_flex().gap(px(6.)).child(dot(px(6.), p.success)).child(
                            div()
                                .id("timer-mode")
                                .test_support()
                                .aria_label(self.timer_mode())
                                .text_size(px(11.))
                                .font_family(fonts::MONO)
                                .text_color(p.text_muted)
                                .child(self.timer_mode()),
                        ),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_family(fonts::MONO)
                            .text_color(p.text_dim)
                            .child(self.schedule_label()),
                    ),
            )
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_family(fonts::MONO)
                            .text_color(p.text_dim)
                            .child("Safe Guards v2"),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_family(fonts::MONO)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.success)
                            .child("Armed"),
                    ),
            )
    }

    fn flush_pending_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.pending_subject.take() {
            self.subject_input
                .update(cx, |state, cx| state.set_value(text, window, cx));
        }
        if let Some(text) = self.pending_body.take() {
            self.body_input
                .update(cx, |state, cx| state.set_value(text, window, cx));
        }
    }
}

impl Render for DokiApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.flush_pending_inputs(window, cx);
        let p = self.p;
        let error = self.error.clone();
        let header = self.header(cx).into_any_element();
        let sidebar = self.sidebar(cx).into_any_element();
        let center = self.center(window, cx);
        let right = self.right(cx);
        let avatar_menu = self.avatar_menu(cx);
        let palette = self.palette_overlay(cx);
        let avatar_open = self.avatar_open;
        let palette_open = self.palette_open;
        v_flex()
            .relative()
            .size_full()
            .bg(p.canvas)
            .text_color(p.text)
            .font_family(fonts::SANS)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if this.focus_handle.is_focused(window) {
                    this.handle_key(event, window, cx);
                }
            }))
            .child(header)
            .when_some(error, |frame, error| {
                frame.child(
                    div()
                        .px(px(16.))
                        .py(px(4.))
                        .bg(p.danger_soft)
                        .border_b_1()
                        .border_color(p.danger_border)
                        .text_size(px(11.))
                        .font_family(fonts::MONO)
                        .text_color(p.danger)
                        .child(error),
                )
            })
            .child(
                h_flex().flex_1().min_h_0().child(
                    h_resizable("doki-frame")
                        .child(
                            resizable_panel()
                                .size(px(LEFT_DOCK))
                                .size_range(px(200.)..px(380.))
                                .fixed()
                                .child(sidebar),
                        )
                        .child(resizable_panel().min_w(px(420.)).child(center))
                        .child(
                            resizable_panel()
                                .size(px(RIGHT_DOCK))
                                .size_range(px(240.)..px(420.))
                                .fixed()
                                .child(right),
                        ),
                ),
            )
            .when(avatar_open && !palette_open, |frame| {
                frame.child(avatar_menu)
            })
            .when(palette_open, |frame| frame.child(palette))
    }
}

pub fn format_next_sync(next: Option<i64>, now: i64) -> String {
    match next {
        Some(at) => format_countdown(at - now),
        None => "—".into(),
    }
}

pub fn format_countdown(remaining: i64) -> String {
    let seconds = remaining.max(0);
    if seconds >= 3600 {
        format!("{}h {:02}m", seconds / 3600, (seconds % 3600) / 60)
    } else {
        format!("{:02}:{:02}", seconds / 60, seconds % 60)
    }
}

pub fn schedule_interval(schedule: &str) -> String {
    let parts: Vec<&str> = schedule.split_whitespace().collect();
    if parts.len() == 5 {
        if let Some(step) = parts[0].strip_prefix("*/") {
            return format!("every {step}m");
        }
        if parts[0] == "0" {
            if let Some(step) = parts[1].strip_prefix("*/") {
                return format!("every {step}h");
            }
            if let Ok(hour) = parts[1].parse::<u32>() {
                return format!("daily at {hour:02}:00");
            }
        }
    }
    schedule.to_string()
}

pub fn shorten_home(home: &Path, path: &Path) -> String {
    if !home.as_os_str().is_empty() {
        if path == home {
            return "~".into();
        }
        if let Ok(rest) = path.strip_prefix(home) {
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

fn fade(color: Rgba, alpha: f32) -> Rgba {
    Rgba { a: alpha, ..color }
}

fn format_span(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds >= 86_400 {
        format!("{}d {}h", seconds / 86_400, (seconds % 86_400) / 3_600)
    } else if seconds >= 3_600 {
        format!("{}h {:02}m", seconds / 3_600, (seconds % 3_600) / 60)
    } else if seconds >= 60 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

fn icon(name: IconName, size: f32, color: Rgba) -> Icon {
    Icon::new(name).with_size(px(size)).text_color(color)
}

fn dot(size: Pixels, color: Rgba) -> impl IntoElement {
    div().size(size).flex_shrink_0().rounded_full().bg(color)
}

fn pulse(size: Pixels, color: Rgba, id: i64, millis: u64) -> impl IntoElement {
    div().size(size).rounded_full().bg(color).with_animation(
        ("pulse", id as u64),
        Animation::new(Duration::from_millis(millis)).repeat(),
        |this, delta| this.opacity(0.35 + 0.65 * (delta * std::f32::consts::PI).sin()),
    )
}

fn format_count(count: usize, color: Rgba) -> AnyElement {
    div()
        .text_size(px(10.))
        .font_family(fonts::MONO)
        .text_color(color)
        .child(count.to_string())
        .into_any_element()
}

fn branch_chip(text: &str, fg: Rgba, bg: Rgba, border: Rgba) -> impl IntoElement {
    div()
        .max_w(px(110.))
        .truncate()
        .px(px(3.))
        .py(px(1.))
        .rounded(px(2.))
        .bg(bg)
        .border_1()
        .border_color(border)
        .text_size(px(10.))
        .font_family(fonts::MONO)
        .text_color(fg)
        .child(text.to_string())
}

fn danger_pill(fg: Rgba, bg: Rgba, border: Rgba, text: &str) -> impl IntoElement {
    div()
        .px(px(3.))
        .py(px(1.))
        .rounded(px(2.))
        .bg(bg)
        .border_1()
        .border_color(border)
        .text_size(px(10.))
        .font_family(fonts::MONO)
        .text_color(fg)
        .child(text.to_string())
}

fn root_icon(root: &doki_core::config::Root) -> IconName {
    let text = format!("{} {}", root.name, root.path.display()).to_lowercase();
    if text.contains("work") {
        IconName::Briefcase
    } else {
        IconName::FolderOpen
    }
}

pub fn run(cfg: Config, store: Store, dry_run: bool) {
    application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            let resolved = theme::resolve(cfg.ui.theme, cx.window_appearance());
            gpui_kit::component::Theme::change(theme::mode(resolved), None, cx);
            fonts::register(cx);
            let [w, h] = cfg.ui.window;
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(w as f32), px(h as f32)),
                    cx,
                ))),
                window_min_size: Some(size(px(1100.), px(720.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("dokidoki".into()),
                    ..Default::default()
                }),
                app_id: Some("dokidoki".into()),
                ..Default::default()
            };
            open_window(options, cx, |window, cx| {
                cx.new(|cx| DokiApp::new(cfg, store, dry_run, window, cx))
            })
            .expect("failed to open window");
        });
}
