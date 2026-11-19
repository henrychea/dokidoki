use crate::diff::{self, DiffLine, FilePatch, LineKind};
use crate::fonts;
use crate::studio::{self, Agents, Draft, GuardChips, Level};
use crate::{DokiApp, Nav, branch_chip, dot, fade, icon};
use doki_core::config::CommitScope;
use doki_core::snapshot::{self, Snapshot};
use doki_core::status::RepoState as LiveState;
use doki_core::store::Store;
use doki_core::sync::{self, SyncOptions};
use gpui_kit::assets::IconName;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, Textarea};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::v_virtual_list;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::path::PathBuf;

const CARD_RADIUS: f32 = 8.;
const LINE_HEIGHT: f32 = 20.;
const GUTTER: f32 = 40.;
const DIFF_CAP: f32 = 520.;
const PLAIN_ROWS: usize = 120;

#[derive(Clone, Debug, Default)]
pub struct RepoView {
    pub snapshot: Option<Snapshot>,
    pub patches: Vec<FilePatch>,
    pub agents: Agents,
    pub draft: Option<Draft>,
    pub selected_file: Option<PathBuf>,
}

impl RepoView {
    pub fn files(&self) -> Vec<doki_core::sync::ChangedFile> {
        self.snapshot
            .as_ref()
            .map(|snapshot| snapshot.files.clone())
            .unwrap_or_default()
    }

    pub fn scope(&self) -> Option<CommitScope> {
        self.snapshot.as_ref().map(|snapshot| snapshot.scope)
    }

    pub fn patch(&self, path: &std::path::Path) -> Option<&FilePatch> {
        self.patches.iter().find(|patch| patch.path == path)
    }

    pub fn totals(&self) -> (u64, u64) {
        studio::totals(&self.files())
    }

    pub fn chips(&self, cfg: &doki_core::config::Config) -> Option<GuardChips> {
        self.snapshot
            .as_ref()
            .map(|snapshot| studio::guard_chips(snapshot, &cfg.guards))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeKind {
    Done,
    Refused,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub kind: NoticeKind,
    pub title: String,
    pub detail: String,
    pub at: i64,
}

impl Notice {
    pub fn failed(title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            kind: NoticeKind::Failed,
            title: title.into(),
            detail: detail.into(),
            at: crate::now_unix(),
        }
    }

    pub fn label(&self) -> &'static str {
        match self.kind {
            NoticeKind::Done => "DONE",
            NoticeKind::Refused => "REFUSED",
            NoticeKind::Failed => "FAILED",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct DryPreview {
    pub state: String,
    pub argv: Vec<Vec<String>>,
    pub agent_argv: Vec<String>,
    pub message: Option<String>,
    pub detail: Option<String>,
    pub warning: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct StudioState {
    pub repo: Option<i64>,
    pub subject: String,
    pub body: String,
    pub body_open: bool,
    pub generating: bool,
    pub committing: bool,
    pub dirty: bool,
    pub scope_open: bool,
    pub error: Option<String>,
    pub notice: Option<Notice>,
    pub dry: Option<DryPreview>,
    pub abortable: bool,
}

struct Loaded {
    snapshot: Snapshot,
    patches: Vec<FilePatch>,
    agents: Agents,
    draft: Option<Draft>,
}

impl DokiApp {
    /// Runs `work` on its own thread and applies `done` back on the UI thread.
    pub(crate) fn off_thread<T, F, D>(&self, cx: &mut Context<Self>, work: F, done: D)
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
        D: FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
    {
        let job = cx.background_spawn(async move { work() });
        cx.spawn(async move |this, cx| {
            let value = job.await;
            let _ = this.update(cx, |app, cx| {
                done(app, value, cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn ensure_view(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.selected else {
            return;
        };
        if self.views.contains_key(&id) || self.view_jobs.contains(&id) {
            return;
        }
        self.load_view(id, cx);
    }

    pub(crate) fn set_nav(&mut self, nav: Nav, cx: &mut Context<Self>) {
        self.nav = nav;
        if nav != Nav::All {
            let ids: Vec<i64> = self
                .repos
                .iter()
                .filter(|repo| repo.enabled && repo.changed > 0)
                .map(|repo| repo.id)
                .collect();
            for id in ids {
                self.load_view(id, cx);
            }
        }
        cx.notify();
    }

    pub(crate) fn select_repo(&mut self, id: i64, cx: &mut Context<Self>) {
        self.selected = Some(id);
        self.auto_selected = None;
        self.studio = StudioState {
            body_open: self.studio.body_open,
            ..StudioState::default()
        };
        self.load_view(id, cx);
        cx.notify();
    }

    pub(crate) fn select_file(&mut self, repo_id: i64, path: PathBuf, cx: &mut Context<Self>) {
        self.selected = Some(repo_id);
        self.auto_selected = None;
        self.nav = Nav::All;
        if let Some(view) = self.views.get_mut(&repo_id) {
            view.selected_file = Some(path);
        }
        self.load_view(repo_id, cx);
        cx.notify();
    }

    pub(crate) fn load_view(&mut self, id: i64, cx: &mut Context<Self>) {
        if !self.view_jobs.insert(id) {
            return;
        }
        let Some(repo) = self.repos.iter().find(|repo| repo.id == id) else {
            self.view_jobs.remove(&id);
            return;
        };
        let path = repo.path.clone();
        let cfg = self.cfg.clone();
        let agent_pref = repo.agent_pref.clone();
        let db = self.db_path.clone();
        self.off_thread(
            cx,
            move || {
                let store = db.as_deref().and_then(|path| Store::open(path).ok());
                let cfg = studio::override_config(&cfg, &path, agent_pref.as_ref());
                let snapshot = snapshot::read(&cfg, store.as_ref(), &path)
                    .map_err(|error| format!("{error:#}"))?;
                let patches = diff::parse(&snapshot.diff);
                let agents = studio::agents(&cfg, &path);
                let draft = studio::cached(&cfg, store.as_ref(), &path, &snapshot);
                Ok::<Loaded, String>(Loaded {
                    snapshot,
                    patches,
                    agents,
                    draft,
                })
            },
            move |this, loaded, cx| {
                this.view_jobs.remove(&id);
                this.apply_view(id, loaded, cx);
            },
        );
    }

    fn apply_view(&mut self, id: i64, loaded: Result<Loaded, String>, cx: &mut Context<Self>) {
        match loaded {
            Ok(loaded) => {
                let previous = self
                    .views
                    .get(&id)
                    .and_then(|view| view.selected_file.clone());
                let keep = previous.filter(|path| loaded.patches.iter().any(|p| &p.path == path));
                let first = loaded
                    .patches
                    .iter()
                    .find(|patch| !patch.is_lockfile())
                    .or_else(|| loaded.patches.first())
                    .map(|patch| patch.path.clone());
                let wanted = std::env::var("DOKI_START_FILE").ok().and_then(|suffix| {
                    loaded
                        .patches
                        .iter()
                        .find(|patch| patch.path.ends_with(&suffix))
                        .map(|patch| patch.path.clone())
                });
                let view = RepoView {
                    selected_file: keep.or(wanted).or(first),
                    snapshot: Some(loaded.snapshot),
                    patches: loaded.patches,
                    agents: loaded.agents,
                    draft: loaded.draft,
                };
                let draft = view.draft.clone();
                let agents = view.agents.clone();
                let generate = self.selected == Some(id)
                    && draft.is_none()
                    && agents.current.is_some()
                    && view
                        .snapshot
                        .as_ref()
                        .is_some_and(|snapshot| !snapshot.blocked() && !snapshot.files.is_empty());
                self.views.insert(id, view);
                if self.selected == Some(id) {
                    self.adopt_draft(id, draft, agents, cx);
                }
                if generate {
                    self.regenerate_with(id, false, cx);
                }
            }
            Err(error) => {
                self.views.remove(&id);
                if self.selected == Some(id) {
                    if self.studio.repo != Some(id) {
                        self.studio = StudioState {
                            repo: Some(id),
                            ..StudioState::default()
                        };
                    }
                    self.studio.error = Some(error);
                }
            }
        }
    }

    fn adopt_draft(
        &mut self,
        id: i64,
        draft: Option<Draft>,
        agents: Agents,
        cx: &mut Context<Self>,
    ) {
        if self.studio.repo != Some(id) {
            self.studio = StudioState {
                repo: Some(id),
                ..StudioState::default()
            };
            self.studio.dirty = false;
        }
        self.studio.error = agents.error.clone().or_else(|| self.studio.error.clone());
        if self.studio.dirty {
            return;
        }
        if let Some(draft) = draft {
            self.studio.subject = draft.subject.clone();
            self.studio.body = draft.body.clone();
            self.pending_subject = Some(draft.subject);
            self.pending_body = Some(draft.body);
        } else {
            self.studio.subject.clear();
            self.studio.body.clear();
            self.pending_subject = Some(String::new());
        }
        cx.notify();
    }

    pub(crate) fn regenerate(&mut self, id: i64, cx: &mut Context<Self>) {
        self.regenerate_with(id, true, cx);
    }

    fn regenerate_with(&mut self, id: i64, announce: bool, cx: &mut Context<Self>) {
        let Some(view) = self.views.get(&id) else {
            return;
        };
        let Some(snapshot) = view.snapshot.clone() else {
            return;
        };
        if snapshot.blocked() {
            let detail = snapshot
                .scan
                .scan
                .finding()
                .unwrap_or("the secret scan blocked this diff")
                .to_string();
            self.studio.notice = Some(Notice::failed(
                "Message generation refused",
                format!("{detail}; no diff left this machine"),
            ));
            self.studio.generating = false;
            cx.notify();
            return;
        }
        let cfg = self.cfg.clone();
        let path = view
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.repo.clone())
            .unwrap_or_default();
        self.studio.generating = true;
        self.studio.error = None;
        self.off_thread(
            cx,
            move || studio::generate(&cfg, &path, &snapshot),
            move |this, result, cx| {
                this.studio.generating = false;
                match result {
                    Ok((draft, agent)) => {
                        this.studio.subject = draft.subject.clone();
                        this.studio.body = draft.body.clone();
                        this.pending_subject = Some(draft.subject.clone());
                        this.pending_body = Some(draft.body.clone());
                        this.studio.dirty = false;
                        if announce {
                            this.studio.notice = Some(Notice {
                                kind: NoticeKind::Done,
                                title: "Message generated".into(),
                                detail: format!("{} ({})", agent.id, agent.model_label()),
                                at: crate::now_unix(),
                            });
                        }
                        if let Some(view) = this.views.get_mut(&id) {
                            view.draft = Some(draft);
                        }
                    }
                    Err(error) => {
                        this.studio.notice = Some(Notice::failed("No message", error));
                    }
                }
                cx.notify();
            },
        );
        cx.notify();
    }

    pub(crate) fn set_scope(&mut self, id: i64, scope: CommitScope, cx: &mut Context<Self>) {
        self.studio.scope_open = false;
        if let Some(store) = self.store.as_ref()
            && let Err(error) = store.set_scope_pref(id, Some(scope))
        {
            self.studio.notice = Some(Notice::failed("Scope not saved", format!("{error:#}")));
            cx.notify();
            return;
        }
        if let Some(repo) = self.repos.iter_mut().find(|repo| repo.id == id) {
            repo.scope_pref = Some(scope);
        }
        self.views.remove(&id);
        self.load_view(id, cx);
        cx.notify();
    }

    pub(crate) fn switch_agent(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(next) = self
            .views
            .get(&id)
            .and_then(|view| view.agents.next.clone())
        else {
            return;
        };
        if let Some(store) = self.store.as_ref()
            && let Err(error) = store.set_agent_pref(id, &next.id, &next.model)
        {
            self.studio.notice = Some(Notice::failed("Agent not saved", format!("{error:#}")));
            cx.notify();
            return;
        }
        if let Some(repo) = self.repos.iter_mut().find(|repo| repo.id == id) {
            repo.agent_pref = Some(next.clone());
            repo.agent = next.id.clone();
        }
        self.studio.notice = Some(Notice {
            kind: NoticeKind::Done,
            title: "Agent switched".into(),
            detail: next.label(),
            at: crate::now_unix(),
        });
        self.views.remove(&id);
        self.load_view(id, cx);
        cx.notify();
    }

    fn effective_agent(&self, id: i64) -> Option<studio::AgentInfo> {
        self.views
            .get(&id)
            .and_then(|view| view.agents.current.clone())
    }

    pub(crate) fn run_dry(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(repo) = self.repos.iter().find(|repo| repo.id == id) else {
            return;
        };
        let Some(db) = self.db_path.clone() else {
            self.studio.notice = Some(Notice::failed(
                "Dry run unavailable",
                "this window has no state database",
            ));
            cx.notify();
            return;
        };
        let path = repo.path.clone();
        let cfg = studio::override_config(&self.cfg, &path, repo.agent_pref.as_ref());
        let state_dir = self.state_dir.clone();
        self.studio.notice = None;
        self.off_thread(
            cx,
            move || {
                let store = Store::open(&db).map_err(|error| format!("{error:#}"))?;
                let messenger = doki_agent::messenger::AgentMessenger::new(&cfg, &path)
                    .map_err(|error| format!("{error:#}"))?;
                let report = sync::sync_with(
                    &cfg,
                    &store,
                    &path,
                    &messenger,
                    SyncOptions {
                        dry_run: true,
                        state_dir: Some(state_dir),
                    },
                )
                .map_err(|error| format!("{error:#}"))?;
                let snapshot = snapshot::read(&cfg, Some(&store), &path).ok();
                let message = match &snapshot {
                    Some(snapshot) if !snapshot.blocked() => {
                        studio::generate(&cfg, &path, snapshot)
                            .ok()
                            .map(|(draft, _)| draft)
                    }
                    _ => None,
                };
                Ok::<DryPreview, String>(DryPreview {
                    state: report.state.label().into(),
                    argv: report.argv.clone(),
                    agent_argv: report.agent_argv.clone(),
                    message: message.map(|draft| draft.full()),
                    detail: report.detail.clone(),
                    warning: report.warning.clone(),
                })
            },
            move |this, result, cx| {
                match result {
                    Ok(preview) => {
                        this.studio.dry = Some(preview);
                        this.studio.notice = None;
                    }
                    Err(error) => {
                        this.studio.notice = Some(Notice::failed("Dry run failed", error));
                    }
                }
                cx.notify();
            },
        );
        cx.notify();
    }

    fn draft(&self, cx: &Context<Self>) -> Draft {
        Draft {
            subject: self.subject_input.read(cx).value().to_string(),
            body: self.body_input.read(cx).value().to_string(),
        }
    }

    pub(crate) fn commit_and_push(&mut self, id: i64, cx: &mut Context<Self>) {
        if self.syncing {
            return;
        }
        let Some(db) = self.db_path.clone() else {
            self.studio.notice = Some(Notice::failed(
                "Commit unavailable",
                "this window has no state database",
            ));
            cx.notify();
            return;
        };
        let draft = self.draft(cx);
        let agent = self.effective_agent(id);
        let Some(repo) = self.repos.iter().find(|repo| repo.id == id) else {
            return;
        };
        let path = repo.path.clone();
        let cfg = studio::override_config(&self.cfg, &path, repo.agent_pref.as_ref());
        if draft.ready() {
            if self.dry_run {
                self.run_dry(id, cx);
                return;
            }
            self.studio.committing = true;
            let state_dir = self.state_dir.clone();
            self.off_thread(
                cx,
                move || {
                    let store = Store::open(&db).map_err(|error| format!("{error:#}"))?;
                    let messenger = match &agent {
                        Some(agent) => studio::DraftMessenger::new(&cfg, agent, draft)?,
                        None => studio::DraftMessenger::unresolved(draft),
                    };
                    let report = sync::sync_with(
                        &cfg,
                        &store,
                        &path,
                        &messenger,
                        SyncOptions {
                            dry_run: false,
                            state_dir: Some(state_dir),
                        },
                    )
                    .map_err(|error| format!("{error:#}"))?;
                    Ok::<_, String>((report.state.label().to_string(), report.detail.clone()))
                },
                move |this, result, cx| {
                    this.studio.committing = false;
                    match result {
                        Ok((state, detail)) => {
                            this.studio.notice = Some(Notice {
                                kind: if state == "committed" || state == "skipped" {
                                    NoticeKind::Done
                                } else {
                                    NoticeKind::Refused
                                },
                                title: format!("Commit {state}"),
                                detail: detail.unwrap_or_default(),
                                at: crate::now_unix(),
                            });
                            this.studio.dirty = false;
                            this.finish_center_refresh(cx);
                        }
                        Err(error) => {
                            this.studio.notice = Some(Notice::failed("Commit failed", error));
                        }
                    }
                    cx.notify();
                },
            );
            cx.notify();
            return;
        }
        self.run_pipeline(id, cx);
    }

    fn run_pipeline(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(db) = self.db_path.clone() else {
            return;
        };
        let Some(repo) = self.repos.iter().find(|repo| repo.id == id) else {
            return;
        };
        let path = repo.path.clone();
        let cfg = studio::override_config(&self.cfg, &path, repo.agent_pref.as_ref());
        let dry = self.dry_run;
        let state_dir = self.state_dir.clone();
        self.studio.committing = true;
        self.off_thread(
            cx,
            move || {
                let store = Store::open(&db).map_err(|error| format!("{error:#}"))?;
                let messenger = doki_agent::messenger::AgentMessenger::new(&cfg, &path)
                    .map_err(|error| format!("{error:#}"))?;
                let report = sync::sync_with(
                    &cfg,
                    &store,
                    &path,
                    &messenger,
                    SyncOptions {
                        dry_run: dry,
                        state_dir: Some(state_dir),
                    },
                )
                .map_err(|error| format!("{error:#}"))?;
                Ok::<_, String>((report.state.label().to_string(), report.detail.clone()))
            },
            move |this, result, cx| {
                this.studio.committing = false;
                match result {
                    Ok((state, detail)) => {
                        this.studio.notice = Some(Notice {
                            kind: if state == "committed" || state == "skipped" {
                                NoticeKind::Done
                            } else {
                                NoticeKind::Refused
                            },
                            title: format!("Sync {state}"),
                            detail: detail.unwrap_or_default(),
                            at: crate::now_unix(),
                        });
                        this.finish_center_refresh(cx);
                    }
                    Err(error) => {
                        this.studio.notice = Some(Notice::failed("Sync failed", error));
                    }
                }
                cx.notify();
            },
        );
        cx.notify();
    }

    pub(crate) fn finish_center_refresh(&mut self, cx: &mut Context<Self>) {
        self.reload();
        self.refresh_statuses(cx);
        if let Some(id) = self.selected {
            self.views.remove(&id);
            self.load_view(id, cx);
        }
    }

    pub(crate) fn confirm_undo(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let entity = cx.entity();
        let name = self
            .repos
            .iter()
            .find(|repo| repo.id == id)
            .map(|repo| repo.name.clone())
            .unwrap_or_default();
        let palette = self.p;
        window.open_dialog(cx, move |dialog, _, _| {
            let entity = entity.clone();
            dialog
                .title("Undo the last doki run")
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(palette.text_muted)
                        .child(format!(
                            "{name} moves back to its pre-sync snapshot with `git reset --soft`, so every change stays in the index. This is refused once a run pushed."
                        )),
                )
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().trigger(|button| button.label("Cancel")))
                        .child(
                            DialogAction::new()
                                .child(Button::new("undo-confirm").label("Undo").primary()),
                        ),
                )
                .on_ok(move |_, _, cx| {
                    entity.update(cx, |this, cx| this.run_undo(id, cx));
                    true
                })
        });
    }

    fn run_undo(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(db) = self.db_path.clone() else {
            self.studio.notice = Some(Notice::failed(
                "Undo unavailable",
                "this window has no state database",
            ));
            cx.notify();
            return;
        };
        let Some(repo) = self.repos.iter().find(|repo| repo.id == id) else {
            return;
        };
        let path = repo.path.clone();
        self.off_thread(
            cx,
            move || {
                let store = Store::open(&db).map_err(|error| format!("{error:#}"))?;
                doki_core::undo::undo(&store, &path).map_err(|error| format!("{error:#}"))
            },
            move |this, result, cx| {
                match result {
                    Ok(report) => {
                        this.studio.notice = Some(Notice {
                            kind: match report.state {
                                doki_core::undo::UndoState::Undone => NoticeKind::Done,
                                doki_core::undo::UndoState::Refused => NoticeKind::Refused,
                            },
                            title: format!("Undo {}", report.state.label()),
                            detail: report.detail,
                            at: crate::now_unix(),
                        });
                        this.finish_center_refresh(cx);
                    }
                    Err(error) => {
                        this.studio.notice = Some(Notice::failed("Undo failed", error));
                    }
                }
                cx.notify();
            },
        );
        cx.notify();
    }

    pub(crate) fn confirm_abort(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let entity = cx.entity();
        let Some(repo) = self.repos.iter().find(|repo| repo.id == id) else {
            return;
        };
        let path = repo.path.clone();
        let operation = abort_operation(repo).unwrap_or("rebase");
        let palette = self.p;
        window.open_dialog(cx, move |dialog, _, _| {
            let entity = entity.clone();
            let path = path.clone();
            dialog
                .title(format!("Abort the {operation}"))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(palette.text_muted)
                        .child(format!(
                            "`git {operation} --abort` restores the branch to the state before the {operation} started. Uncommitted work stays on disk."
                        )),
                )
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().trigger(|button| button.label("Cancel")))
                        .child(
                            DialogAction::new().child(
                                Button::new("abort-confirm")
                                    .label("Abort")
                                    .danger(),
                            ),
                        ),
                )
                .on_ok(move |_, _, cx| {
                    let path = path.clone();
                    entity.update(cx, |this, cx| this.run_abort(id, path, operation, cx));
                    true
                })
        });
    }

    fn run_abort(
        &mut self,
        id: i64,
        path: PathBuf,
        operation: &'static str,
        cx: &mut Context<Self>,
    ) {
        self.off_thread(
            cx,
            move || {
                let git = doki_core::git::Git::new(&path);
                let out = git
                    .run_unchecked(&[operation, "--abort"])
                    .map_err(|error| format!("{error:#}"))?;
                if out.success() {
                    Ok(format!("git {operation} --abort"))
                } else {
                    Err(out.stderr_lossy().trim().to_string())
                }
            },
            move |this, result, cx| {
                match result {
                    Ok(detail) => {
                        this.studio.notice = Some(Notice {
                            kind: NoticeKind::Done,
                            title: "Aborted".into(),
                            detail,
                            at: crate::now_unix(),
                        });
                        this.finish_center_refresh(cx);
                    }
                    Err(error) => {
                        this.studio.notice = Some(Notice::failed("Abort failed", error));
                    }
                }
                let _ = id;
                cx.notify();
            },
        );
        cx.notify();
    }

    pub(crate) fn open_editor(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repo) = self.repos.iter().find(|repo| repo.id == id) else {
            return;
        };
        let editor = editor_program(&self.cfg.ui.editor);
        let path = repo.path.clone();
        match std::process::Command::new(&editor).arg(&path).spawn() {
            Ok(_) => {
                self.studio.notice = Some(Notice {
                    kind: NoticeKind::Done,
                    title: "Opened editor".into(),
                    detail: format!("{editor} {}", path.display()),
                    at: crate::now_unix(),
                });
                cx.notify();
            }
            Err(error) => {
                self.studio.notice = Some(Notice::failed(
                    "Editor not started",
                    format!("{editor}: {error}"),
                ));
                cx.notify();
            }
        }
        let _ = window;
    }

    pub(crate) fn center(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p;
        if self.settings.is_some() {
            return self.settings_center(window, cx);
        }
        let body = match self.nav {
            Nav::Diff => self.diff_inspector(cx),
            Nav::Commit => self.commit_queue(cx),
            Nav::All => match self
                .selected
                .and_then(|id| self.repos.iter().find(|repo| repo.id == id))
                .cloned()
            {
                Some(repo) => self.repo_center(&repo, cx),
                None => self.select_hint(),
            },
        };
        v_flex()
            .size_full()
            .bg(p.canvas)
            .when(self.dry_run, |column| column.child(self.dry_banner()))
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }

    fn select_hint(&self) -> AnyElement {
        let p = self.p;
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .text_size(px(12.))
                    .font_family(fonts::MONO)
                    .text_color(p.text_dim)
                    .child("Select a repo"),
            )
            .into_any_element()
    }

    fn dry_banner(&self) -> AnyElement {
        let p = self.p;
        h_flex()
            .id("dry-run-banner")
            .test_support()
            .aria_label("Dry run — no git command that changes a repo will run")
            .flex_shrink_0()
            .gap(px(8.))
            .px(px(24.))
            .py(px(7.))
            .bg(p.warning_soft)
            .border_b_1()
            .border_color(p.warning_border)
            .child(dot(px(6.), p.warning))
            .child(
                div()
                    .text_size(px(11.))
                    .font_family(fonts::MONO)
                    .text_color(p.warning)
                    .child("Dry run — no git command that changes a repo will run"),
            )
            .into_any_element()
    }

    fn repo_center(&mut self, repo: &crate::RepoVm, cx: &mut Context<Self>) -> AnyElement {
        let id = repo.id;
        let state = repo.row_state();
        let blocked = matches!(state, crate::RowState::Incident | crate::RowState::Waiting)
            || matches!(repo.live, Some(LiveState::Refused(_)));
        let clean = !blocked && repo.changed == 0 && state == crate::RowState::Clean;
        let has_files = self
            .views
            .get(&id)
            .is_some_and(|view| !view.files().is_empty());
        let chips = self.views.get(&id).and_then(|view| view.chips(&self.cfg));
        let _guards_block = chips.as_ref().is_some_and(GuardChips::blocking);

        // The 24 px design gap includes the section border, which is outside this flex gap.
        let mut content = v_flex().p(px(24.)).gap(px(19.)).w_full();
        if let Some(notice) = self.studio.notice.clone() {
            content = content.child(self.notice_card(&notice));
        } else if let Some(error) = self.studio.error.clone() {
            content = content.child(self.notice_card(&Notice {
                kind: NoticeKind::Failed,
                title: "Center unavailable".into(),
                detail: error,
                at: self.now,
            }));
        }
        if let Some(preview) = self.studio.dry.clone() {
            content = content.child(self.dry_preview_card(&preview));
        }
        if blocked {
            content = content.child(self.blocked_panel(repo, cx));
            if has_files {
                content = content.child(self.diff_section(repo, cx));
            }
        } else if clean {
            content = content.child(self.clean_state(repo));
        } else {
            content = content.child(self.studio_card(repo, cx));
            content = content.child(self.diff_section(repo, cx));
        }

        v_flex()
            .id("center-scroll")
            .size_full()
            .min_w_0()
            .overflow_y_scroll()
            .child(self.repo_header(repo, chips.as_ref(), cx))
            .child(content)
            .into_any_element()
    }

    fn repo_header(
        &self,
        repo: &crate::RepoVm,
        chips: Option<&GuardChips>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = self.p;
        let id = repo.id;
        let dirty_text = match repo.row_state() {
            crate::RowState::Incident => Some("Blocked".to_string()),
            crate::RowState::Waiting => Some("Action required".to_string()),
            crate::RowState::Running => Some(format!("{}: analyzing", repo.agent)),
            _ if repo.changed > 0 => Some("Worktree dirty".to_string()),
            _ => None,
        };
        let mut parts: Vec<(&'static str, &'static str, String, Option<IconName>)> = Vec::new();
        if repo.ahead > 0 {
            parts.push((
                "repo-ahead",
                "Ahead",
                repo.ahead.to_string(),
                Some(IconName::ArrowUp),
            ));
        }
        if repo.behind > 0 {
            parts.push((
                "repo-behind",
                "Behind",
                repo.behind.to_string(),
                Some(IconName::ArrowDown),
            ));
        }
        if let Some(text) = dirty_text {
            parts.push(("repo-worktree", "", text, None));
        }
        let mut summary = h_flex().gap(px(4.));
        for (index, (id, label, value, icon_name)) in parts.into_iter().enumerate() {
            if index > 0 {
                summary = summary.child(
                    div()
                        .text_size(px(11.))
                        .font_family(fonts::MONO)
                        .text_color(p.text_faint)
                        .child("·"),
                );
            }
            let text = if label.is_empty() {
                value
            } else {
                format!("{label} {value}")
            };
            if let Some(icon_name) = icon_name {
                summary = summary.child(icon(icon_name, 13., p.text_dim));
            }
            summary = summary.child(
                div()
                    .id(id)
                    .test_support()
                    .aria_label(text.clone())
                    .text_size(px(11.))
                    .font_family(fonts::MONO)
                    .text_color(p.text_dim)
                    .child(text),
            );
        }
        let ahead = summary;

        let editor = editor_label(&self.cfg.ui.editor);
        v_flex()
            .flex_shrink_0()
            .px(px(20.))
            .pt(px(16.))
            .pb(px(13.))
            .gap(px(8.))
            .bg(fade(p.card, 0.4))
            .border_b_1()
            .border_color(p.hairline)
            .child(
                h_flex()
                    .flex_wrap()
                    .gap(px(10.))
                    .child(
                        div()
                            .id("center-repo-name")
                            .test_support()
                            .aria_label(repo.name.clone())
                            .flex_shrink_0()
                            .text_size(px(16.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.text)
                            .child(repo.name.clone()),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .px(px(8.))
                            .py(px(2.))
                            .rounded(px(4.))
                            .bg(p.accent_surface)
                            .border_1()
                            .border_color(fade(p.accent_border, 0.3))
                            .text_size(px(11.))
                            .font_family(fonts::MONO)
                            .text_color(p.accent_subtle)
                            .child(repo.branch_label()),
                    )
                    .child(ahead),
            )
            .child(
                h_flex()
                    .mt(px(4.))
                    .gap(px(8.))
                    .child(self.action_button(
                        "center-dry-run",
                        IconName::Eye,
                        "Dry Run",
                        false,
                        cx.listener(move |this, _, _, cx| this.run_dry(id, cx)),
                    ))
                    .child(self.action_button(
                        "center-undo",
                        IconName::Undo2,
                        "Undo",
                        false,
                        cx.listener(move |this, _, window, cx| this.confirm_undo(id, window, cx)),
                    ))
                    .child(self.action_button(
                        "center-editor",
                        IconName::SquareArrowOutUpRight,
                        &format!("Open in {editor}"),
                        false,
                        cx.listener(move |this, _, window, cx| this.open_editor(id, window, cx)),
                    )),
            )
            .child(self.guard_row(chips))
            .into_any_element()
    }

    fn guard_row(&self, chips: Option<&GuardChips>) -> AnyElement {
        let p = self.p;
        let mut row = h_flex()
            .flex_wrap()
            .gap(px(24.))
            .pt(px(4.))
            .border_t_1()
            .border_color(p.hairline_soft);
        match chips {
            Some(chips) => {
                for chip in [&chips.secret, &chips.mass, &chips.context] {
                    row = row.child(
                        h_flex()
                            .id(ElementId::Name(
                                format!("guard-{}", chip.label.replace(' ', "-")).into(),
                            ))
                            .test_support()
                            .aria_label(format!(
                                "{}: {} {}",
                                chip.label,
                                chip.value,
                                if chip.ok { "ok" } else { "blocking" }
                            ))
                            .gap(px(6.))
                            .child(dot(px(6.), if chip.ok { p.success } else { p.danger }))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .font_family(fonts::MONO)
                                    .text_color(p.text_dim)
                                    .child(format!("{}:", chip.label)),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .font_family(fonts::MONO)
                                    .text_color(if chip.ok { p.text_muted } else { p.danger })
                                    .child(chip.value.clone()),
                            ),
                    );
                }
            }
            None => {
                row = row.child(
                    div()
                        .text_size(px(11.))
                        .font_family(fonts::MONO)
                        .text_color(p.text_dim)
                        .child("Guards idle — no diff loaded"),
                );
            }
        }
        row.into_any_element()
    }

    fn action_button(
        &self,
        id: &'static str,
        icon_name: IconName,
        label: &str,
        disabled: bool,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        let p = self.p;
        let label = label.to_string();
        h_flex()
            .id(id)
            .test_support()
            .aria_label(label.clone())
            .gap(px(6.))
            .px(px(8.))
            .py(px(2.))
            .rounded(px(4.))
            .bg(p.button)
            .border_1()
            .border_color(p.hairline)
            .when(disabled, |button| button.opacity(0.55))
            .when(!disabled, |button| {
                button
                    .cursor_pointer()
                    .hover(|style| style.bg(p.button_hover))
            })
            .on_click(move |event, window, cx| {
                if !disabled {
                    handler(event, window, cx);
                }
            })
            .child(icon(icon_name, 13., p.text_dim))
            .child(
                div()
                    .text_size(px(11.))
                    .font_family(fonts::MONO)
                    .text_color(p.text_muted)
                    .child(label),
            )
            .into_any_element()
    }

    fn notice_card(&self, notice: &Notice) -> AnyElement {
        let p = self.p;
        let (fg, bg, border) = match notice.kind {
            NoticeKind::Done => (p.success, p.success_soft, p.success_border),
            NoticeKind::Refused => (p.warning, p.warning_soft, p.warning_border),
            NoticeKind::Failed => (p.danger, p.danger_soft, p.danger_border),
        };
        h_flex()
            .id("center-notice")
            .test_support()
            .aria_label(format!("{}: {}", notice.label(), notice.detail))
            .justify_between()
            .gap(px(12.))
            .px(px(12.))
            .py(px(8.))
            .rounded(px(4.))
            .bg(bg)
            .border_1()
            .border_color(border)
            .child(
                h_flex()
                    .gap(px(8.))
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(10.))
                            .font_family(fonts::MONO)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(fg)
                            .child(notice.label()),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_family(fonts::MONO)
                            .text_color(p.text_muted)
                            .child(notice.title.clone()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.))
                            .text_color(p.text_dim)
                            .child(notice.detail.clone()),
                    ),
            )
            .into_any_element()
    }

    fn clean_state(&self, repo: &crate::RepoVm) -> AnyElement {
        let p = self.p;
        let last = repo
            .last_sync_at
            .map(|at| format!("{} ago", crate::format_span(self.now - at)))
            .unwrap_or_else(|| "never".into());
        div()
            .id("clean-state")
            .test_support()
            .aria_label("Working tree clean")
            .w_full()
            .py(px(56.))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(8.))
            .rounded(px(CARD_RADIUS))
            .border_1()
            .border_color(p.hairline)
            .child(icon(IconName::CircleCheck, 22., p.success))
            .child(
                div()
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.text)
                    .child("Working tree clean"),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .font_family(fonts::MONO)
                    .text_color(p.text_dim)
                    .child(format!("last sync {last}")),
            )
            .into_any_element()
    }

    fn blocked_panel(&mut self, repo: &crate::RepoVm, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p;
        let id = repo.id;
        let reason = match &repo.live {
            Some(LiveState::Refused(reason)) => reason.clone(),
            _ => repo.last_error.clone().unwrap_or_else(|| repo.state_text()),
        };
        let abortable = matches!(repo.live, Some(LiveState::Refused(_)))
            && matches!(
                abort_operation(repo),
                Some("rebase") | Some("merge") | Some("cherry-pick")
            );
        self.studio.abortable = abortable;
        v_flex()
            .id("blocked-panel")
            .test_support()
            .aria_label(format!("Blocked: {reason}"))
            .w_full()
            .gap(px(12.))
            .p(px(16.))
            .rounded(px(CARD_RADIUS))
            .bg(p.danger_surface)
            .border_1()
            .border_color(p.danger_border)
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(icon(IconName::TriangleAlert, 16., p.danger))
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.text)
                            .child("Blocked — the commit studio is paused"),
                    ),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .font_family(fonts::MONO)
                    .text_color(p.danger)
                    .child(reason.clone()),
            )
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(self.secondary_button("blocked-review", "Review", {
                        let entity = cx.entity();
                        let reason = reason.clone();
                        move |_, _, cx: &mut App| {
                            entity.update(cx, |this, cx| {
                                this.set_nav(Nav::Diff, cx);
                                this.studio.notice = Some(Notice {
                                    kind: NoticeKind::Refused,
                                    title: "Review".into(),
                                    detail: reason.clone(),
                                    at: crate::now_unix(),
                                });
                            });
                        }
                    }))
                    .child(self.secondary_button(
                        "blocked-abort",
                        "Abort",
                        cx.listener(move |this, _, window, cx| this.confirm_abort(id, window, cx)),
                    ))
                    .child(self.secondary_button(
                        "blocked-retry",
                        "Retry",
                        cx.listener(move |this, _, _, cx| this.run_pipeline(id, cx)),
                    )),
            )
            .into_any_element()
    }

    fn chip_button(
        &self,
        id: &'static str,
        icon_name: Option<IconName>,
        label: &str,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        let p = self.p;
        h_flex()
            .id(id)
            .test_support()
            .aria_label(label.to_string())
            .flex_shrink_0()
            .gap(px(6.))
            .px(px(10.))
            .py(px(5.))
            .rounded(px(4.))
            .bg(p.button)
            .border_1()
            .border_color(p.hairline)
            .cursor_pointer()
            .hover(|style| style.bg(p.button_hover))
            .on_click(handler)
            .when_some(icon_name, |button, name| {
                button.child(icon(name, 13., p.text_dim))
            })
            .child(
                div()
                    .text_size(px(11.))
                    .font_family(fonts::MONO)
                    .text_color(p.text_muted)
                    .child(label.to_string()),
            )
            .into_any_element()
    }

    fn secondary_button(
        &self,
        id: &'static str,
        label: &str,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        let p = self.p;
        h_flex()
            .id(id)
            .test_support()
            .aria_label(label.to_string())
            .justify_center()
            .flex_1()
            .py(px(5.))
            .rounded(px(4.))
            .bg(p.button)
            .border_1()
            .border_color(p.hairline)
            .cursor_pointer()
            .hover(|style| style.bg(p.button_hover))
            .on_click(handler)
            .child(
                div()
                    .text_size(px(11.))
                    .font_family(fonts::MONO)
                    .text_color(p.text_muted)
                    .child(label.to_string()),
            )
            .into_any_element()
    }

    fn studio_card(&mut self, repo: &crate::RepoVm, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p;
        let id = repo.id;
        let view = self.views.get(&id);
        let suffix = view
            .and_then(|view| view.agents.current.as_ref())
            .map(studio::AgentInfo::label)
            .unwrap_or_else(|| "no agent".into());
        let next = view
            .and_then(|view| view.agents.next.clone())
            .map(|next| next.switch_label())
            .unwrap_or_else(|| "Switch agent".into());
        let scope = view
            .and_then(|view| view.scope())
            .or(repo.scope_pref)
            .unwrap_or(self.cfg.sync.commit_scope);
        let files = view.map(RepoView::files).unwrap_or_default();
        let scope_chip = studio::scope_label(scope, files.len());
        let subject = self.subject_input.read(cx).value().to_string();
        let chars = subject.chars().count();
        let counter = studio::counter_label(chars);
        let counter_color = match studio::counter_level(chars) {
            Level::Normal => p.text_muted,
            Level::Warn => p.warning,
            Level::Danger => p.danger,
        };
        let blocked = view
            .and_then(|view| view.snapshot.as_ref())
            .is_some_and(Snapshot::blocked);
        let ready = Draft {
            subject: subject.clone(),
            body: String::new(),
        }
        .ready();
        let has_agent = view.and_then(|view| view.agents.current.as_ref()).is_some();
        let can_commit = ready && !blocked && !self.studio.committing;
        let configure = !has_agent && !ready;
        let generating = self.studio.generating;
        let body_open = self.studio.body_open;
        let scope_open = self.studio.scope_open;
        let mut card = v_flex()
            .id("commit-card")
            .test_support()
            .relative()
            .w_full()
            .gap(px(7.))
            .p(px(16.))
            .pb(px(18.))
            .rounded(px(CARD_RADIUS))
            .bg(p.card)
            .border_1()
            .border_color(p.hairline)
            .child(
                h_flex()
                    .justify_between()
                    .gap(px(12.))
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .items_center()
                            .child(icon(IconName::Sparkles, 16., p.text))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.text)
                                    .child("AI Commit Studio"),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .font_family(fonts::MONO)
                                    .text_color(p.text_faint)
                                    .child("·"),
                            )
                            .child(
                                div()
                                    .id("studio-agent")
                                    .test_support()
                                    .aria_label(suffix.clone())
                                    .text_size(px(11.))
                                    .font_family(fonts::MONO)
                                    .text_color(p.text_dim)
                                    .child(suffix),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .items_center()
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .font_family(fonts::MONO)
                                    .text_color(p.text_dim)
                                    .child("Scope:"),
                            )
                            .child(
                                div()
                                    .id("scope-chip")
                                    .test_support()
                                    .aria_label(scope_chip.clone())
                                    .px(px(6.))
                                    .py(px(2.))
                                    .rounded(px(4.))
                                    .bg(p.button)
                                    .border_1()
                                    .border_color(p.hairline)
                                    .cursor_pointer()
                                    .hover(|style| style.bg(p.button_hover))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.studio.scope_open = !this.studio.scope_open;
                                        cx.notify();
                                    }))
                                    .text_size(px(11.))
                                    .font_family(fonts::MONO)
                                    .text_color(p.text_muted)
                                    .child(scope_chip),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .gap(px(2.))
                    .child(
                        h_flex()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .font_family(fonts::MONO)
                                    .text_color(p.text_dim)
                                    .child("PROPOSED SUBJECT"),
                            )
                            .child(
                                div()
                                    .id("subject-counter")
                                    .test_support()
                                    .aria_label(counter.clone())
                                    .text_size(px(11.))
                                    .font_family(fonts::MONO)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(counter_color)
                                    .child(counter),
                            ),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .gap(px(4.))
                            .px(px(8.))
                            .py(px(10.))
                            .rounded(px(4.))
                            .bg(p.canvas)
                            .border_1()
                            .border_color(p.hairline)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .font_family(fonts::MONO)
                                    .text_size(px(12.))
                                    .child(Input::new(&self.subject_input)),
                            )
                            .child(
                                div()
                                    .id("subject-refresh")
                                    .flex_shrink_0()
                                    .p(px(4.))
                                    .rounded(px(4.))
                                    .cursor_pointer()
                                    .hover(|style| style.bg(p.button))
                                    .on_click(
                                        cx.listener(move |this, _, _, cx| this.regenerate(id, cx)),
                                    )
                                    .child(if generating {
                                        Spinner::new().color(p.text_muted.into()).into_any_element()
                                    } else {
                                        icon(IconName::RefreshCw, 15., p.text_dim)
                                            .into_any_element()
                                    }),
                            ),
                    )
                    .when(body_open, |block| {
                        block.child(
                            div()
                                .h(px(84.))
                                .px(px(8.))
                                .py(px(6.))
                                .rounded(px(4.))
                                .bg(p.canvas)
                                .border_1()
                                .border_color(p.hairline)
                                .font_family(fonts::MONO)
                                .text_size(px(12.))
                                .child(Textarea::new(&self.body_input).h_full()),
                        )
                    }),
            )
            .child(
                h_flex()
                    .justify_between()
                    .gap(px(8.))
                    .mt(px(4.))
                    .pt(px(4.))
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .child(self.chip_button(
                                "switch-agent",
                                Some(IconName::ArrowLeftRight),
                                &next,
                                cx.listener(move |this, _, _, cx| this.switch_agent(id, cx)),
                            ))
                            .child(self.chip_button(
                                "edit-body",
                                None,
                                "Edit Body",
                                cx.listener(|this, _, _, cx| {
                                    this.studio.body_open = !this.studio.body_open;
                                    cx.notify();
                                }),
                            )),
                    )
                    .child(
                        div()
                            .id("commit-push")
                            .test_support()
                            .aria_label(if configure {
                                "Configure".to_string()
                            } else if can_commit {
                                "Commit & Push".to_string()
                            } else {
                                "Commit & Push (unavailable)".to_string()
                            })
                            .flex_shrink_0()
                            .px(px(16.))
                            .py(px(5.))
                            .rounded(px(4.))
                            .bg(if configure || can_commit {
                                p.text
                            } else {
                                p.button
                            })
                            .when(configure || can_commit, |button| {
                                button.cursor_pointer().hover(|style| style.opacity(0.9))
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let has_agent = this
                                    .views
                                    .get(&id)
                                    .is_some_and(|view| view.agents.current.is_some());
                                if !has_agent && !this.draft(cx).ready() {
                                    this.studio.notice = Some(Notice::failed(
                                        "No agent",
                                        "configure an agent before generating a message",
                                    ));
                                    cx.notify();
                                    return;
                                }
                                this.commit_and_push(id, cx);
                            }))
                            .child(
                                h_flex()
                                    .gap(px(6.))
                                    .items_center()
                                    .child(icon(
                                        IconName::Send,
                                        14.,
                                        if can_commit || configure {
                                            p.canvas
                                        } else {
                                            p.text_dim
                                        },
                                    ))
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(if can_commit || configure {
                                                p.canvas
                                            } else {
                                                p.text_dim
                                            })
                                            .child(if configure {
                                                "Configure".to_string()
                                            } else {
                                                "Commit & Push".to_string()
                                            }),
                                    ),
                            ),
                    ),
            );

        if scope_open {
            card = card.child(
                div()
                    .id("scope-scrim")
                    .absolute()
                    .top(px(0.))
                    .right(px(0.))
                    .bottom(px(0.))
                    .left(px(0.))
                    .rounded(px(CARD_RADIUS))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.studio.scope_open = false;
                        cx.notify();
                    }))
                    .child(
                        v_flex()
                            .id("scope-menu")
                            .test_support()
                            .absolute()
                            .top(px(34.))
                            .right(px(12.))
                            .w(px(150.))
                            .p(px(4.))
                            .gap(px(2.))
                            .rounded(px(4.))
                            .bg(p.surface)
                            .border_1()
                            .border_color(p.hairline)
                            .children(scope_options(scope).into_iter().map(|option| {
                                let selected = option == scope;
                                div()
                                    .id(ElementId::Name(
                                        format!("scope-option-{}", option.label()).into(),
                                    ))
                                    .test_support()
                                    .aria_label(format!(
                                        "{}{}",
                                        option.label(),
                                        if selected { " (selected)" } else { "" }
                                    ))
                                    .px(px(6.))
                                    .py(px(3.))
                                    .rounded(px(2.))
                                    .cursor_pointer()
                                    .hover(|style| style.bg(p.button))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.set_scope(id, option, cx)
                                    }))
                                    .child(
                                        h_flex()
                                            .justify_between()
                                            .child(
                                                div()
                                                    .text_size(px(11.))
                                                    .font_family(fonts::MONO)
                                                    .text_color(if selected {
                                                        p.text
                                                    } else {
                                                        p.text_muted
                                                    })
                                                    .child(title_case(option.label())),
                                            )
                                            .when(selected, |row| {
                                                row.child(icon(IconName::Check, 12., p.accent))
                                            }),
                                    )
                            })),
                    ),
            );
        }
        card.into_any_element()
    }

    fn diff_section(&mut self, repo: &crate::RepoVm, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p;
        v_flex()
            .id("diff-card")
            .test_support()
            .w_full()
            .rounded(px(CARD_RADIUS))
            .border_1()
            .border_color(p.hairline)
            .bg(p.card)
            .overflow_hidden()
            .child(self.file_tabs(repo, cx))
            .child(self.diff_body(repo, cx))
            .into_any_element()
    }

    fn file_tabs(&mut self, repo: &crate::RepoVm, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p;
        let id = repo.id;
        let files = self.views.get(&id).map(RepoView::files).unwrap_or_default();
        let totals = self.views.get(&id).map(RepoView::totals).unwrap_or((0, 0));
        let selected_file = self
            .views
            .get(&id)
            .and_then(|view| view.selected_file.clone())
            .or_else(|| files.first().map(|file| file.path.clone()));
        let counts: std::collections::HashMap<PathBuf, (Option<u64>, Option<u64>)> = files
            .iter()
            .map(|file| (file.path.clone(), (file.added, file.deleted)))
            .collect();
        let mut tabs = h_flex()
            .id("file-tabs")
            .test_support()
            .gap(px(4.))
            .py(px(6.))
            .min_w_0()
            .overflow_x_scroll();
        for file in &files {
            let selected = selected_file.as_ref() == Some(&file.path);
            let name = file
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| file.path.display().to_string());
            let lock = file.path.extension().is_some_and(|ext| ext == "lock")
                || name == "package-lock.json";
            let (added, deleted) = counts.get(&file.path).copied().unwrap_or((None, None));
            let path = file.path.clone();
            tabs = tabs.child(
                h_flex()
                    .id(ElementId::Name(format!("file-tab-{}", name).into()))
                    .test_support()
                    .aria_label(format!(
                        "{name} +{} -{}{}",
                        added.map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
                        deleted.map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
                        if selected { " (selected)" } else { "" }
                    ))
                    .flex_shrink_0()
                    .gap(px(6.))
                    .px(px(10.))
                    .py(px(4.))
                    .rounded(px(4.))
                    .when(selected, |tab| {
                        tab.bg(p.surface).border_1().border_color(p.hairline)
                    })
                    .when(!selected, |tab| {
                        tab.cursor_pointer()
                            .hover(|style| style.bg(fade(p.button, 0.5)))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(view) = this.views.get_mut(&id) {
                            view.selected_file = Some(path.clone());
                        }
                        cx.notify();
                    }))
                    .child(icon(
                        if lock {
                            IconName::Lock
                        } else {
                            IconName::FileText
                        },
                        13.,
                        if selected {
                            p.accent_subtle
                        } else {
                            p.text_dim
                        },
                    ))
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_family(fonts::MONO)
                            .text_color(if selected { p.text } else { p.text_muted })
                            .child(name),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .font_family(fonts::MONO)
                            .text_color(if selected { p.text_muted } else { p.text_dim })
                            .child(format!(
                                "+{} -{}",
                                added.map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
                                deleted.map(|n| n.to_string()).unwrap_or_else(|| "?".into())
                            )),
                    ),
            );
        }
        if files.is_empty() {
            tabs = tabs.child(
                div()
                    .px(px(6.))
                    .text_size(px(11.))
                    .font_family(fonts::MONO)
                    .text_color(p.text_dim)
                    .child("no changed files"),
            );
        }
        h_flex()
            .w_full()
            .justify_between()
            .gap(px(12.))
            .px(px(12.))
            .bg(fade(p.canvas, 0.6))
            .border_b_1()
            .border_color(p.hairline)
            .child(tabs)
            .child(
                h_flex()
                    .id("file-totals")
                    .test_support()
                    .aria_label(format!("+{} -{}", totals.0, totals.1))
                    .flex_shrink_0()
                    .gap(px(12.))
                    .pl(px(8.))
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_family(fonts::MONO)
                            .text_color(p.success)
                            .child(format!("+{}", totals.0)),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_family(fonts::MONO)
                            .text_color(p.danger)
                            .child(format!("-{}", totals.1)),
                    ),
            )
            .into_any_element()
    }

    fn diff_body(&mut self, repo: &crate::RepoVm, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p;
        let id = repo.id;
        let Some(view) = self.views.get(&id) else {
            return div().into_any_element();
        };
        let Some(path) = view.selected_file.clone() else {
            return div().into_any_element();
        };
        let Some(patch) = view.patch(&path).cloned() else {
            return div().into_any_element();
        };
        let rows = diff_rows(&patch);
        let height = (rows.len() as f32 * LINE_HEIGHT + 8.).min(DIFF_CAP);
        let entity = cx.entity();
        let rows_for_list: Vec<Row> = rows.clone();
        let mut body = div().id("diff-body").test_support().bg(p.patch);
        if patch.binary {
            body = body.px(px(12.)).py(px(12.)).child(
                div()
                    .text_size(px(12.))
                    .font_family(fonts::MONO)
                    .text_color(p.text_dim)
                    .child("Binary file — no textual diff"),
            );
        } else if rows.is_empty() {
            body = body.px(px(12.)).py(px(12.)).child(
                div()
                    .text_size(px(12.))
                    .font_family(fonts::MONO)
                    .text_color(p.text_dim)
                    .child("No textual changes in this scope"),
            );
        } else if rows.len() <= PLAIN_ROWS {
            body = body.p(px(8.)).child(
                v_flex().children(
                    rows.iter()
                        .enumerate()
                        .map(|(index, row)| diff_row(row, index, self.p)),
                ),
            );
        } else {
            let sizes = std::rc::Rc::new(vec![size(px(0.), px(LINE_HEIGHT)); rows_for_list.len()]);
            let palette = self.p;
            let scroll = self.diff_scroll.clone();
            body = body.h(px(height)).child(
                v_virtual_list(entity, "diff-lines", sizes, move |_, range, _, _| {
                    range
                        .clone()
                        .map(|index| diff_row(&rows_for_list[index], index, palette))
                        .collect::<Vec<_>>()
                })
                .track_scroll(&scroll),
            );
        }

        let mut header = h_flex()
            .justify_between()
            .gap(px(12.))
            .px(px(12.))
            .pt(px(8.))
            .pb(px(8.))
            .border_b_1()
            .border_color(p.hairline_soft)
            .child(
                h_flex().gap(px(6.)).min_w_0().child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(11.))
                        .font_family(fonts::MONO)
                        .text_color(p.text_dim)
                        .child(path.display().to_string()),
                ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(11.))
                    .font_family(fonts::MONO)
                    .text_color(p.text_dim)
                    .child(patch.header_label()),
            );
        if let Some(from) = patch.renamed_from() {
            header = header.child(
                div()
                    .text_size(px(11.))
                    .font_family(fonts::MONO)
                    .text_color(p.text_faint)
                    .child(format!("from {from}")),
            );
        }
        let mut card = v_flex()
            .id("diff-pane")
            .test_support()
            .w_full()
            .child(header)
            .child(body);
        if patch.truncated {
            card = card.child(
                div()
                    .px(px(12.))
                    .py(px(6.))
                    .border_t_1()
                    .border_color(p.hairline)
                    .text_size(px(11.))
                    .font_family(fonts::MONO)
                    .text_color(p.warning)
                    .child(format!(
                        "diff truncated at {} lines",
                        diff::MAX_LINES_PER_FILE
                    )),
            );
        }
        card.into_any_element()
    }

    fn dry_preview_card(&self, preview: &DryPreview) -> AnyElement {
        let p = self.p;
        let mut card = v_flex()
            .id("dry-preview")
            .test_support()
            .aria_label(format!("Dry run {}", preview.state))
            .w_full()
            .gap(px(8.))
            .p(px(12.))
            .rounded(px(CARD_RADIUS))
            .bg(p.warning_soft)
            .border_1()
            .border_color(p.warning_border)
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(10.))
                            .font_family(fonts::MONO)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.warning)
                            .child(format!("DRY RUN · {}", preview.state.to_uppercase())),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_family(fonts::MONO)
                            .text_color(p.text_dim)
                            .child(format!("{} commands", preview.argv.len())),
                    ),
            );
        if let Some(detail) = &preview.detail {
            card = card.child(
                div()
                    .text_size(px(11.))
                    .text_color(p.text_muted)
                    .child(detail.clone()),
            );
        }
        let mut lines = v_flex().gap(px(1.));
        for argv in &preview.argv {
            lines = lines.child(
                div()
                    .text_size(px(11.))
                    .font_family(fonts::MONO)
                    .text_color(p.text_dim)
                    .child(format!("$ {}", argv.join(" "))),
            );
        }
        if !preview.agent_argv.is_empty() {
            lines = lines.child(
                div()
                    .text_size(px(11.))
                    .font_family(fonts::MONO)
                    .text_color(p.accent_subtle)
                    .child(format!("$ {}", preview.agent_argv.join(" "))),
            );
        }
        card = card.child(
            div()
                .p(px(8.))
                .rounded(px(4.))
                .bg(p.patch)
                .border_1()
                .border_color(p.hairline)
                .child(lines),
        );
        if let Some(message) = &preview.message {
            card = card.child(
                v_flex()
                    .gap(px(4.))
                    .child(
                        div()
                            .text_size(px(10.))
                            .font_family(fonts::MONO)
                            .text_color(p.text_dim)
                            .child("GENERATED MESSAGE"),
                    )
                    .child(
                        div()
                            .p(px(8.))
                            .rounded(px(4.))
                            .bg(p.patch)
                            .border_1()
                            .border_color(p.hairline)
                            .text_size(px(12.))
                            .font_family(fonts::MONO)
                            .text_color(p.text)
                            .child(message.clone()),
                    ),
            );
        }
        if let Some(warning) = &preview.warning {
            card = card.child(
                div()
                    .text_size(px(11.))
                    .font_family(fonts::MONO)
                    .text_color(p.warning)
                    .child(warning.clone()),
            );
        }
        card.into_any_element()
    }

    fn diff_inspector(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p;
        let mut rows = v_flex().gap(px(4.)).p(px(24.));
        let mut total = 0;
        for repo in self.repos.iter().filter(|repo| repo.enabled) {
            let Some(view) = self.views.get(&repo.id) else {
                continue;
            };
            for file in view.files() {
                total += 1;
                let repo_id = repo.id;
                let path = file.path.clone();
                let name = file
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| file.path.display().to_string());
                let dir = file
                    .path
                    .parent()
                    .map(|parent| parent.display().to_string())
                    .unwrap_or_default();
                rows = rows.child(
                    h_flex()
                        .id(ElementId::Name(
                            format!("inspector-{}-{}", repo_id, name).into(),
                        ))
                        .test_support()
                        .aria_label(format!("{}/{}", repo.name, file.path.display()))
                        .justify_between()
                        .gap(px(12.))
                        .px(px(10.))
                        .py(px(6.))
                        .rounded(px(4.))
                        .border_1()
                        .border_color(fade(p.hairline, 0.))
                        .cursor_pointer()
                        .hover(|style| style.bg(p.pane_raised).border_color(p.hairline))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.select_file(repo_id, path.clone(), cx)
                        }))
                        .child(
                            h_flex()
                                .gap(px(8.))
                                .min_w_0()
                                .child(icon(IconName::FileText, 13., p.text_dim))
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .font_family(fonts::MONO)
                                        .text_color(p.text_dim)
                                        .child(format!("{} {}", repo.name, dir)),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.))
                                        .font_family(fonts::MONO)
                                        .text_color(p.text)
                                        .child(name),
                                ),
                        )
                        .child(
                            h_flex()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .font_family(fonts::MONO)
                                        .text_color(p.success)
                                        .child(format!(
                                            "+{}",
                                            file.added
                                                .map(|n| n.to_string())
                                                .unwrap_or_else(|| "?".into())
                                        )),
                                )
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .font_family(fonts::MONO)
                                        .text_color(p.danger)
                                        .child(format!(
                                            "-{}",
                                            file.deleted
                                                .map(|n| n.to_string())
                                                .unwrap_or_else(|| "?".into())
                                        )),
                                ),
                        ),
                );
            }
        }
        let subtitle = if total == 0 {
            "No dirty files across enabled repos".to_string()
        } else {
            format!("{total} dirty files across enabled repos")
        };
        v_flex()
            .id("diff-inspector")
            .test_support()
            .aria_label(subtitle.clone())
            .size_full()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .flex_shrink_0()
                    .px(px(24.))
                    .py(px(16.))
                    .gap(px(4.))
                    .border_b_1()
                    .border_color(p.hairline)
                    .bg(fade(p.card, 0.4))
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.text)
                            .child("Diff Inspector"),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_family(fonts::MONO)
                            .text_color(p.text_dim)
                            .child(subtitle),
                    ),
            )
            .child(rows)
            .into_any_element()
    }

    fn commit_queue(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p;
        let mut rows = v_flex().gap(px(4.)).p(px(24.));
        let mut pending = 0;
        for repo in self.repos.iter().filter(|repo| repo.enabled) {
            let has_diff = repo.changed > 0;
            if !has_diff {
                continue;
            }
            let has_message = self
                .views
                .get(&repo.id)
                .and_then(|view| view.draft.as_ref())
                .is_some();
            if has_message {
                continue;
            }
            pending += 1;
            let id = repo.id;
            let name = repo.name.clone();
            let branch = repo.branch_label();
            let files = repo.changed;
            rows = rows.child(
                h_flex()
                    .id(ElementId::Name(format!("commit-queue-{id}").into()))
                    .test_support()
                    .aria_label(format!("{name} · {files} files · no message"))
                    .justify_between()
                    .px(px(10.))
                    .py(px(8.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(fade(p.hairline, 0.))
                    .cursor_pointer()
                    .hover(|style| style.bg(p.pane_raised).border_color(p.hairline))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_nav(Nav::All, cx);
                        this.select_repo(id, cx);
                    }))
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .child(dot(px(6.), p.warning))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .font_family(fonts::MONO)
                                    .text_color(p.text)
                                    .child(name),
                            )
                            .child(branch_chip(
                                &branch,
                                p.accent_subtle,
                                p.accent_surface,
                                p.accent_border,
                            )),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_family(fonts::MONO)
                            .text_color(p.text_dim)
                            .child(format!("{files} files · no message")),
                    ),
            );
        }
        let subtitle = if pending == 0 {
            "Every diff already has a message".to_string()
        } else {
            format!("{pending} repos waiting for a message")
        };
        v_flex()
            .id("ai-commit-queue")
            .test_support()
            .aria_label(subtitle.clone())
            .size_full()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .flex_shrink_0()
                    .px(px(24.))
                    .py(px(16.))
                    .gap(px(4.))
                    .border_b_1()
                    .border_color(p.hairline)
                    .bg(fade(p.card, 0.4))
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.text)
                            .child("AI Commit"),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_family(fonts::MONO)
                            .text_color(p.text_dim)
                            .child(subtitle),
                    ),
            )
            .child(rows)
            .into_any_element()
    }
}

#[derive(Clone, Debug)]
enum Row {
    Hunk(String),
    Line(DiffLine),
}

fn diff_rows(patch: &FilePatch) -> Vec<Row> {
    let mut rows = Vec::new();
    for (index, hunk) in patch.hunks.iter().enumerate() {
        if index > 0 {
            rows.push(Row::Hunk(hunk.label()));
        }
        for line in &hunk.lines {
            rows.push(Row::Line(line.clone()));
        }
    }
    rows
}

fn diff_row(row: &Row, index: usize, p: crate::Palette) -> AnyElement {
    match row {
        Row::Hunk(label) => h_flex()
            .id(ElementId::Name(format!("diff-hunk-{index}").into()))
            .w_full()
            .h(px(LINE_HEIGHT))
            .px(px(8.))
            .text_size(px(11.))
            .font_family(fonts::MONO)
            .text_color(p.text_faint)
            .child(label.clone())
            .into_any_element(),
        Row::Line(line) => {
            let (wash, accent) = match line.kind {
                LineKind::Added => (p.diff_add, p.success),
                LineKind::Removed => (p.diff_del, p.danger),
                LineKind::Context => (p.patch, p.patch),
                LineKind::Meta => (p.patch, p.patch),
            };
            let number = line
                .new
                .or(line.old)
                .map(|n| n.to_string())
                .unwrap_or_default();
            let (sign, text_color) = match line.kind {
                LineKind::Added => ("+", p.text),
                LineKind::Removed => ("-", p.text),
                LineKind::Context => (" ", p.text_muted),
                LineKind::Meta => ("", p.text_faint),
            };
            h_flex()
                .id(ElementId::Name(format!("diff-line-{index}").into()))
                .w_full()
                .h(px(LINE_HEIGHT))
                .items_center()
                .bg(wash)
                .border_l_2()
                .border_color(
                    if matches!(line.kind, LineKind::Added | LineKind::Removed) {
                        accent
                    } else {
                        wash
                    },
                )
                .child(
                    div()
                        .w(px(GUTTER))
                        .flex_shrink_0()
                        .pr(px(12.))
                        .text_right()
                        .text_size(px(11.))
                        .font_family(fonts::MONO)
                        .text_color(number_color(line, p))
                        .child(number),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .whitespace_nowrap()
                        .text_size(px(12.))
                        .font_family(fonts::MONO)
                        .text_color(text_color)
                        .child(format!("{sign}{}", line.text)),
                )
                .into_any_element()
        }
    }
}

fn number_color(line: &DiffLine, p: crate::Palette) -> Rgba {
    match line.kind {
        LineKind::Added => fade(p.success, 0.7),
        LineKind::Removed => fade(p.danger, 0.7),
        LineKind::Context => p.text_dim,
        LineKind::Meta => p.text_faint,
    }
}

fn scope_options(current: CommitScope) -> Vec<CommitScope> {
    let mut options = vec![CommitScope::Staged, CommitScope::Tracked, CommitScope::All];
    if !options.contains(&current) {
        options.push(current);
    }
    options
}

fn title_case(label: &str) -> String {
    let mut chars = label.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

pub(crate) fn abort_operation(repo: &crate::RepoVm) -> Option<&'static str> {
    let Some(LiveState::Refused(reason)) = &repo.live else {
        return None;
    };
    if reason.contains("rebase-merge") || reason.contains("rebase-apply") {
        Some("rebase")
    } else if reason.contains("MERGE_HEAD") {
        Some("merge")
    } else if reason.contains("CHERRY_PICK_HEAD") {
        Some("cherry-pick")
    } else {
        None
    }
}

fn editor_program(editor: &str) -> String {
    match editor.trim() {
        "" => "zed".into(),
        value => value.to_string(),
    }
}

fn editor_label(editor: &str) -> String {
    let program = editor_program(editor);
    let base = program.rsplit('/').next().unwrap_or(&program);
    match base {
        "zed" => "Zed".into(),
        "code" | "codium" => "Code".into(),
        "nvim" | "vim" => "Neovim".into(),
        "idea" => "IntelliJ".into(),
        "subl" => "Sublime".into(),
        other => title_case(other),
    }
}
