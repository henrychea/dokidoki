use crate::center::abort_operation;
use crate::fonts;
use crate::{DokiApp, dot, fade, icon};
use doki_core::git::Git;
use doki_core::journal::{self, Entry, GuardSummary, Kind, Rate, Tab};
use doki_core::lock::{self, LockAttempt};
use doki_core::logging;
use doki_core::status::{self, RepoState as LiveState};
use gpui_kit::assets::IconName;
use gpui_kit::component::{h_flex, v_flex, v_virtual_list};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::collections::HashSet;
use std::path::Path;
use std::rc::Rc;

const ROW_GAP: f32 = 8.;
const CARD_PAD: f32 = 20.;
const CARD_BORDER: f32 = 2.;
const HEADER_AND_REPO: f32 = 34.;
const BODY_LINE: f32 = 17.;
const FOOTER_HEIGHT: f32 = 21.;
const BLOCKED_BASE: f32 = 98.;
const BLOCKED_LINE: f32 = 16.;
const GAP: f32 = 4.;
const BLOCK_GAP: f32 = 6.;
const STDERR_LINE: f32 = 14.;
const LOG_TAIL: usize = 6;
const BODY_CHARS: usize = 46;

pub struct JournalFeed {
    pub entries: Vec<Entry>,
    pub rate: Rate,
    pub tab: Tab,
    pub expanded: HashSet<String>,
    logs: Vec<String>,
}

impl Default for JournalFeed {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            rate: journal::rate(&[], 0),
            tab: Tab::All,
            expanded: HashSet::new(),
            logs: Vec::new(),
        }
    }
}

impl JournalFeed {
    pub fn rebuild(
        &mut self,
        runs: &[journal::RunRecord],
        incidents: &[journal::IncidentRecord],
        guards: &GuardSummary,
        now: i64,
    ) {
        self.entries = journal::build(runs, incidents, guards);
        self.rate = journal::rate(runs, now);
        self.logs = logging::recent(200);
        self.expanded
            .retain(|id| self.entries.iter().any(|entry| &entry.id == id));
    }

    pub fn visible(&self) -> Vec<&Entry> {
        self.entries
            .iter()
            .filter(|entry| entry.in_tab(self.tab))
            .collect()
    }

    pub fn counts(&self) -> (usize, usize, usize) {
        (
            self.entries.len(),
            journal::counts(&self.entries, Tab::Runs),
            journal::counts(&self.entries, Tab::Alerts),
        )
    }

    fn height(&self, entry: &Entry) -> f32 {
        let body = body_lines(&entry.body);
        let card = if entry.kind == Kind::Blocked {
            let mut card = BLOCKED_BASE + BLOCKED_LINE * body;
            if self.expanded.contains(&entry.id) {
                if entry.stderr.is_some() {
                    card += BLOCK_GAP + 12. + STDERR_LINE * stderr_lines(entry.stderr.as_deref());
                }
                card += self.log_extra(entry);
            }
            card
        } else if entry.kind == Kind::Synced && entry.agent_label().is_some() {
            CARD_PAD + CARD_BORDER + HEADER_AND_REPO + FOOTER_HEIGHT + GAP * 2.
        } else {
            CARD_PAD + CARD_BORDER + HEADER_AND_REPO + BODY_LINE * body + GAP * 2.
        };
        card + ROW_GAP
    }

    fn log_extra(&self, entry: &Entry) -> f32 {
        let lines = self.logs_for(&entry.repo).len();
        if lines == 0 {
            0.
        } else {
            BLOCK_GAP + 12. + STDERR_LINE * lines as f32 + (lines as f32 - 1.)
        }
    }

    fn logs_for(&self, repo: &str) -> Vec<&str> {
        let mut lines: Vec<&str> = self
            .logs
            .iter()
            .filter(|line| line.contains(repo))
            .map(String::as_str)
            .collect();
        if lines.len() > LOG_TAIL {
            lines.drain(..lines.len() - LOG_TAIL);
        }
        lines
    }
}

impl DokiApp {
    pub(crate) fn reload_journal(&mut self) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let mut runs = store.journal_runs(200).unwrap_or_default();
        let incidents = store.journal_incidents(200).unwrap_or_default();
        for run in &mut runs {
            run.upstream = self
                .repos
                .iter()
                .find(|repo| repo.id == run.repo_id)
                .and_then(|repo| repo.upstream.clone());
        }
        let guards = GuardSummary::from_guards(&self.cfg.guards);
        self.journal.rebuild(&runs, &incidents, &guards, self.now);
    }

    #[doc(hidden)]
    pub fn journal_counts(&self) -> (usize, usize, usize) {
        self.journal.counts()
    }

    #[doc(hidden)]
    pub fn journal_tab(&self) -> Tab {
        self.journal.tab
    }

    #[doc(hidden)]
    pub fn journal_ids(&self) -> Vec<String> {
        self.journal
            .entries
            .iter()
            .map(|entry| entry.id.clone())
            .collect()
    }

    pub(crate) fn set_journal_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.journal.tab = tab;
        cx.notify();
    }

    fn toggle_stderr(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.journal.expanded.remove(id) {
            self.journal.expanded.insert(id.to_string());
        }
        cx.notify();
    }

    #[doc(hidden)]
    pub fn acknowledge_incident(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        match store.acknowledge_incident(id, crate::now_unix()) {
            Ok(()) => self.reload_journal(),
            Err(error) => self.error = Some(format!("acknowledge: {error:#}")),
        }
        cx.notify();
    }

    pub(crate) fn retry_repo(&mut self, repo_id: i64, cx: &mut Context<Self>) {
        let Some(repo) = self.repos.iter().find(|repo| repo.id == repo_id) else {
            return;
        };
        if !repo.enabled {
            self.error = Some("Retry refused: the repo is disabled".into());
            cx.notify();
            return;
        }
        if self.syncing {
            self.error = Some("Retry refused: a sync run is already active".into());
            cx.notify();
            return;
        }
        let path = repo.path.clone();
        let retry_path = path.clone();
        self.off_thread(
            cx,
            move || validate_retry(&retry_path),
            move |this, result, cx| {
                match result {
                    Ok(()) => {
                        if this.acquire_scheduler() {
                            this.begin_sync(vec![path], cx);
                        } else {
                            this.error =
                                Some("Retry refused: another doki run holds the scheduler".into());
                        }
                    }
                    Err(reason) => this.error = Some(format!("Retry refused: {reason}")),
                }
                cx.notify();
            },
        );
        cx.notify();
    }

    pub(crate) fn right(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p;
        let active_tab = self.journal.tab;
        let mut tabs = h_flex()
            .gap(px(2.))
            .p(px(2.))
            .rounded(px(4.))
            .bg(p.chrome)
            .border_1()
            .border_color(p.hairline);
        for (tab, label) in [
            (Tab::All, "All"),
            (Tab::Runs, "Runs"),
            (Tab::Alerts, "Alerts"),
        ] {
            let active = active_tab == tab;
            tabs = tabs.child(
                div()
                    .id(ElementId::Name(
                        format!("journal-tab-{}", label.to_lowercase()).into(),
                    ))
                    .test_support()
                    .aria_label(label.to_string())
                    .px(px(8.))
                    .py(px(2.))
                    .rounded(px(4.))
                    .when(active, |chip| chip.bg(p.surface))
                    .when(!active, |chip| {
                        chip.cursor_pointer()
                            .hover(|style| style.bg(p.button_hover))
                    })
                    .text_size(px(10.))
                    .font_family(fonts::MONO)
                    .text_color(if active { p.text } else { p.text_dim })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_journal_tab(tab, cx);
                    }))
                    .child(label),
            );
        }

        let visible = self.journal.visible();
        let count = visible.len();
        let sizes: Rc<Vec<Size<Pixels>>> = Rc::new(
            visible
                .iter()
                .map(|entry| size(px(0.), px(self.journal.height(entry))))
                .collect(),
        );
        let rows = Rc::new(visible.into_iter().cloned().collect::<Vec<_>>());
        let entity = cx.entity();
        let scroll = self.journal_scroll.clone();
        let list = if rows.is_empty() {
            v_flex()
                .flex_1()
                .p(px(12.))
                .child(
                    div()
                        .text_size(px(11.))
                        .font_family(fonts::MONO)
                        .text_color(p.text_dim)
                        .child("No journal entries yet"),
                )
                .into_any_element()
        } else {
            v_flex()
                .flex_1()
                .min_h_0()
                .pt(px(12.))
                .child(
                    v_virtual_list(entity, "journal-rows", sizes, move |this, range, _, cx| {
                        range
                            .clone()
                            .map(|index| this.journal_row(&rows[index], index, cx))
                            .collect::<Vec<_>>()
                    })
                    .flex_1()
                    .min_h_0()
                    .track_scroll(&scroll),
                )
                .into_any_element()
        };

        v_flex()
            .size_full()
            .bg(p.pane)
            .border_l_1()
            .border_color(p.hairline)
            .child(
                h_flex()
                    .justify_between()
                    .p(px(12.))
                    .border_b_1()
                    .border_color(p.hairline)
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .child(icon(IconName::ReceiptText, 15., p.text_muted))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.text)
                                    .child("Journal & Logs"),
                            ),
                    )
                    .child(tabs),
            )
            .child(
                v_flex()
                    .id("journal-list")
                    .test_support()
                    .aria_label(format!("{count} entries"))
                    .flex_1()
                    .min_h_0()
                    .child(list),
            )
            .child(self.journal_footer())
            .into_any_element()
    }

    fn journal_row(&self, entry: &Entry, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let height = self.journal.height(entry);
        let expanded = self.journal.expanded.contains(&entry.id);
        let card = if entry.kind == Kind::Blocked {
            self.blocked_row(entry, expanded, cx)
        } else {
            self.feed_row(entry)
        };
        div()
            .id(ElementId::Name(format!("journal-row-{index}").into()))
            .test_support()
            .aria_label(format!("{} {}", entry.clock(), entry.label()))
            .h(px(height))
            .px(px(12.))
            .pb(px(ROW_GAP))
            .child(card)
            .into_any_element()
    }

    fn feed_row(&self, entry: &Entry) -> AnyElement {
        let p = self.p;
        let severity = match entry.kind {
            Kind::GuardOk => p.success,
            Kind::Synced => p.text_muted,
            Kind::Failed | Kind::Blocked => p.danger,
            Kind::Running => p.accent,
            Kind::Skipped => p.text_dim,
        };
        let footer = entry.kind == Kind::Synced && entry.agent_label().is_some();
        let mut card = v_flex()
            .id(ElementId::Name(format!("journal-card-{}", entry.id).into()))
            .test_support()
            .gap(px(GAP))
            .p(px(10.))
            .rounded(px(4.))
            .bg(p.journal_card)
            .border_1()
            .border_color(p.hairline)
            .overflow_hidden()
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(10.))
                            .font_family(fonts::MONO)
                            .text_color(p.text_dim)
                            .child(entry.clock()),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .font_family(fonts::MONO)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(severity)
                            .child(entry.label()),
                    ),
            );
        let mut repo = h_flex().gap(px(4.)).min_w_0().child(
            div()
                .flex_shrink_0()
                .text_size(px(11.))
                .font_family(fonts::MONO)
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.text)
                .child(entry.repo.clone()),
        );
        if entry.kind == Kind::Synced && entry.body.starts_with('→') {
            repo = repo.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(11.))
                    .font_family(fonts::MONO)
                    .text_color(p.text_dim)
                    .child(entry.body.clone()),
            );
        }
        card = card.child(repo);
        if entry.kind != Kind::Synced || !entry.body.starts_with('→') {
            card = card.child(
                div()
                    .text_size(px(11.))
                    .text_color(p.text_muted)
                    .line_height(px(17.))
                    .line_clamp(3)
                    .child(entry.body.clone()),
            );
        }
        if footer {
            card = card.child(
                h_flex()
                    .justify_between()
                    .pt(px(4.))
                    .border_t_1()
                    .border_color(p.hairline_soft)
                    .child(
                        div()
                            .text_size(px(10.))
                            .font_family(fonts::MONO)
                            .text_color(p.text_dim)
                            .child(entry.agent_label().unwrap_or_default()),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .font_family(fonts::MONO)
                            .text_color(p.text_dim)
                            .child(entry.latency_label().unwrap_or_default()),
                    ),
            );
        }
        card.into_any_element()
    }

    fn blocked_row(&self, entry: &Entry, expanded: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = self.p;
        let incident = entry.incident;
        let repo_id = entry.repo_id;
        let id = entry.id.clone();
        let stderr = entry.stderr.clone();
        let acknowledged = entry.acknowledged;
        let head = h_flex()
            .justify_between()
            .child(
                div()
                    .text_size(px(10.))
                    .font_family(fonts::MONO)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.danger)
                    .child(format!("{} · BLOCKED", entry.clock())),
            )
            .child(dot(px(6.), p.danger));
        let mut reason = v_flex()
            .gap(px(2.))
            .min_w_0()
            .child(
                div()
                    .text_size(px(11.))
                    .font_family(fonts::MONO)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.text)
                    .child(entry.repo.clone()),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(p.text_muted)
                    .line_height(px(16.))
                    .line_clamp(3)
                    .child(entry.body.clone()),
            )
            .into_any_element();
        if stderr.is_some() {
            let toggle = id.clone();
            reason = div()
                .id(ElementId::Name(
                    format!("journal-stderr-toggle-{id}").into(),
                ))
                .test_support()
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_stderr(&toggle, cx)))
                .child(reason)
                .into_any_element();
        }
        let mut card = v_flex()
            .id(ElementId::Name(format!("journal-card-{}", entry.id).into()))
            .test_support()
            .gap(px(BLOCK_GAP))
            .p(px(10.))
            .rounded(px(4.))
            .bg(p.danger_card)
            .border_1()
            .border_color(p.danger_border)
            .overflow_hidden()
            .child(head);
        card = card.child(reason);
        if expanded {
            if let Some(stderr) = stderr {
                card = card.child(
                    div()
                        .id(ElementId::Name(
                            format!("journal-stderr-{}", entry.id).into(),
                        ))
                        .test_support()
                        .p(px(4.))
                        .rounded(px(2.))
                        .bg(p.patch)
                        .border_1()
                        .border_color(p.hairline)
                        .text_size(px(10.))
                        .font_family(fonts::MONO)
                        .text_color(p.text_muted)
                        .line_clamp(4)
                        .child(stderr),
                );
            }
            let logs = self.journal.logs_for(&entry.repo);
            if !logs.is_empty() {
                let mut block = v_flex()
                    .id(ElementId::Name(format!("journal-logs-{}", entry.id).into()))
                    .test_support()
                    .p(px(4.))
                    .rounded(px(2.))
                    .gap(px(1.))
                    .bg(p.patch)
                    .border_1()
                    .border_color(p.hairline)
                    .text_size(px(10.))
                    .font_family(fonts::MONO)
                    .text_color(p.text_dim);
                for line in logs {
                    block = block.child(div().truncate().child(line.to_string()));
                }
                card = card.child(block);
            }
        }
        let review_id = ElementId::Name(format!("journal-review-{}", entry.id).into());
        let abort_id = ElementId::Name(format!("journal-abort-{}", entry.id).into());
        let retry_id = ElementId::Name(format!("journal-retry-{}", entry.id).into());
        let mut actions = h_flex().gap(px(6.)).pt(px(2.)).child(self.journal_button(
            review_id,
            "Review",
            false,
            true,
            cx.listener(move |this, _, _, cx| {
                this.nav = crate::Nav::All;
                this.select_repo(repo_id, cx);
            }),
        ));
        if acknowledged {
            actions = actions.child(
                div()
                    .flex_1()
                    .py(px(4.))
                    .text_center()
                    .text_size(px(10.))
                    .font_family(fonts::MONO)
                    .text_color(p.text_dim)
                    .child("Acknowledged"),
            );
        } else {
            let abortable = self
                .repos
                .iter()
                .find(|repo| repo.id == repo_id)
                .is_some_and(|repo| abort_operation(repo).is_some());
            actions = actions.child(self.journal_button(
                abort_id,
                "Abort",
                true,
                true,
                cx.listener(move |this, _, window, cx| {
                    if abortable {
                        this.confirm_abort(repo_id, window, cx);
                    } else if let Some(incident) = incident {
                        this.acknowledge_incident(incident, cx);
                    }
                }),
            ));
        }
        actions = actions.child(self.journal_button(
            retry_id,
            "Retry",
            false,
            false,
            cx.listener(move |this, _, _, cx| this.retry_repo(repo_id, cx)),
        ));
        card.child(actions).into_any_element()
    }

    fn journal_button(
        &self,
        id: ElementId,
        label: &str,
        danger: bool,
        grow: bool,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        let p = self.p;
        h_flex()
            .id(id)
            .test_support()
            .aria_label(label.to_string())
            .justify_center()
            .when(grow, |button| button.flex_1())
            .when(!grow, |button| button.px(px(8.)))
            .py(px(4.))
            .rounded(px(4.))
            .bg(if danger { p.danger_soft } else { p.button })
            .border_1()
            .border_color(if danger { p.danger_border } else { p.hairline })
            .cursor_pointer()
            .hover(|style| style.bg(p.button_hover))
            .on_click(handler)
            .text_size(px(10.))
            .font_family(fonts::MONO)
            .text_color(if danger { p.danger } else { p.text })
            .child(label.to_string())
            .into_any_element()
    }

    fn journal_footer(&self) -> AnyElement {
        let p = self.p;
        let rate = &self.journal.rate;
        let mut bars = h_flex().items_end().gap(px(3.)).h(px(24.)).w_full();
        for index in 0..journal::SPARK_BUCKETS {
            let bucket = rate.buckets[index];
            let latest = index == journal::SPARK_BUCKETS - 1;
            let empty = bucket.runs == 0;
            let color = if bucket.failed {
                fade(p.danger, 0.6)
            } else if latest && !empty {
                p.accent_subtle
            } else if empty {
                p.hairline
            } else {
                p.button_hover
            };
            let bar_height = if empty {
                3.
            } else {
                (24. * rate.height(index)).max(6.)
            };
            bars = bars.child(
                div()
                    .id(ElementId::Name(format!("journal-spark-{index}").into()))
                    .test_support()
                    .aria_label(format!(
                        "hour {index}: {} runs{}",
                        bucket.runs,
                        if bucket.failed { " failed" } else { "" }
                    ))
                    .flex_1()
                    .h(px(bar_height))
                    .rounded(px(2.))
                    .bg(color),
            );
        }
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
                        div()
                            .text_size(px(11.))
                            .font_family(fonts::MONO)
                            .text_color(p.text_dim)
                            .child("Daemon Sync Rate"),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_family(fonts::MONO)
                            .text_color(p.text_muted)
                            .child(rate.percent_label()),
                    ),
            )
            .child(bars)
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(10.))
                            .font_family(fonts::MONO)
                            .text_color(p.text_dim)
                            .child("Past 12h"),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .font_family(fonts::MONO)
                            .text_color(p.text_dim)
                            .child(rate.avg_label()),
                    ),
            )
            .into_any_element()
    }
}

fn body_lines(text: &str) -> f32 {
    let lines: usize = text
        .lines()
        .map(|line| line.chars().count().div_ceil(BODY_CHARS))
        .sum();
    lines.clamp(1, 3) as f32
}

fn stderr_lines(text: Option<&str>) -> f32 {
    text.map(|text| text.lines().count().clamp(1, 4) as f32)
        .unwrap_or(0.)
}

fn validate_retry(repo: &Path) -> Result<(), String> {
    let git = Git::new(repo);
    let status = status::read(&git).map_err(|error| format!("{error:#}"))?;
    if let LiveState::Refused(reason) = status.state {
        return Err(format!("{reason} is still present; resolve it first"));
    }
    if status.state == LiveState::Detached {
        return Err("HEAD is detached; check out a branch first".into());
    }
    if status.state == LiveState::Unborn {
        return Err("the repo has no commits yet".into());
    }
    let common = git.common_dir().map_err(|error| format!("{error:#}"))?;
    match lock::try_acquire(&common) {
        Ok(LockAttempt::Acquired(_)) => Ok(()),
        Ok(LockAttempt::Busy) => Err("another doki run holds this repo lock".into()),
        Err(error) => Err(format!("{error:#}")),
    }
}
