use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::{CheckmarkItem, StandardItem};
use ksni::{MenuItem, OfflineReason, Status, Tray as KsniTray};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Open,
    SyncAll,
    TogglePause,
    Quit,
}

pub type ActionQueue = Arc<Mutex<VecDeque<Action>>>;
pub type TrayHandle = (Handle<Tray>, ActionQueue);

#[derive(Debug, Default)]
pub struct TrayState {
    pub paused: AtomicBool,
    pub incidents: AtomicUsize,
}

#[derive(Clone)]
pub struct Tray {
    state: Arc<TrayState>,
    actions: ActionQueue,
}

impl KsniTray for Tray {
    fn id(&self) -> String {
        "doki".into()
    }

    fn icon_name(&self) -> String {
        "view-refresh".into()
    }

    fn title(&self) -> String {
        let incidents = self.state.incidents.load(Ordering::Relaxed);
        match incidents {
            0 => "doki".into(),
            1 => "doki · 1 incident".into(),
            count => format!("doki · {count} incidents"),
        }
    }

    fn status(&self) -> Status {
        if self.state.incidents.load(Ordering::Relaxed) > 0 {
            Status::NeedsAttention
        } else {
            Status::Active
        }
    }

    fn watcher_offline(&self, reason: OfflineReason) -> bool {
        tracing::warn!(
            ?reason,
            "no StatusNotifier host; background mode continues without a tray"
        );
        true
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let open = self.actions.clone();
        let sync = self.actions.clone();
        let pause = self.actions.clone();
        let pause_state = self.state.clone();
        let quit = self.actions.clone();
        vec![
            StandardItem {
                label: "Open dokidoki".into(),
                icon_name: "window-new".into(),
                activate: Box::new(move |_| push(&open, Action::Open)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Sync All".into(),
                icon_name: "view-refresh".into(),
                activate: Box::new(move |_| push(&sync, Action::SyncAll)),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: "Pause".into(),
                checked: pause_state.paused.load(Ordering::Relaxed),
                activate: Box::new(move |tray: &mut Tray| {
                    let paused = !pause_state.paused.load(Ordering::Relaxed);
                    pause_state.paused.store(paused, Ordering::Relaxed);
                    tray.state.paused.store(paused, Ordering::Relaxed);
                    push(&pause, Action::TogglePause);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(move |_| push(&quit, Action::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

fn push(queue: &ActionQueue, action: Action) {
    queue.lock().expect("tray actions").push_back(action);
}

/// A desktop without a StatusNotifier host is a warning, never a failure.
pub fn spawn(state: Arc<TrayState>) -> Option<TrayHandle> {
    let actions = Arc::new(Mutex::new(VecDeque::new()));
    let tray = Tray {
        state,
        actions: actions.clone(),
    };
    match tray.assume_sni_available(true).spawn() {
        Ok(handle) => Some((handle, actions)),
        Err(error) => {
            tracing::warn!(%error, "tray unavailable; background mode continues without it");
            None
        }
    }
}
