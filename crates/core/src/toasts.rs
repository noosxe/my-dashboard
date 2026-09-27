//! The toast hub (docs/0058): core's raise API for user-visible alerts.
//!
//! The hub is an entity-global (the docs/0013 D2 `InFlight` pattern):
//! raisers exist *before and without* the window — early `main` wiring,
//! module init, background threads, tests — so a handle on the shell
//! entity would make raising impossible in exactly the cases the issue
//! asks for. The hub itself touches no UI; the shell observes it and
//! forwards into gpui-component's `Root` notification layer (docs/0058
//! D1/D5). Nothing here renders, and nothing persists (D9): the queues
//! die with the process.
//!
//! Two ingest paths (D6):
//! - [`raise`] and the level helpers — `&mut App` callers on the main
//!   thread.
//! - [`ToastSpill`] — a `Send + Sync` bounded queue for non-UI threads
//!   (the docs/0057 usage writer thread cannot touch gpui state). The
//!   shell pumps the spill into the hub once a second.
//!
//! Every raise is mirrored to the log (D8) so the information never
//! lives only inside a possibly role-less overlay.

use gpui::{App, AppContext as _, Context, Entity, Global, SharedString, Window};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Pending-queue cap (docs/0058 D4): drop-oldest so a raiser storm stays
/// bounded even if no window ever opens.
const QUEUE_CAP: usize = 32;

/// Spill cap (D6): drop-oldest, guarded loudly.
const SPILL_CAP: usize = 16;

/// Severity, 1:1 with upstream `NotificationType` (docs/0058 D3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Success,
    Warning,
    Error,
}

impl Level {
    /// Errors are sticky (manual dismiss); everything else auto-hides on
    /// the upstream 5 s timer (docs/0058 D3).
    pub fn sticky(self) -> bool {
        matches!(self, Level::Error)
    }

    /// D8's log mirror: warnings and errors surface as `warn!`, the rest
    /// as `info!`.
    fn log_level(self) -> log::Level {
        match self {
            Level::Error | Level::Warning => log::Level::Warn,
            Level::Info | Level::Success => log::Level::Info,
        }
    }
}

/// The action-button callback for [`ToastAction`] (docs/0058 D3): runs on
/// the main thread when the user clicks the button.
pub type ToastHandler = Arc<dyn Fn(&mut Window, &mut App) + Send + Sync>;

/// An optional action button on a toast (docs/0058 D3). The type ships so
/// raise sites can carry intent; the UI wiring has no first consumer yet
/// (docs/0058 §10 defers mapping it into `Notification::action`).
#[derive(Clone)]
pub struct ToastAction {
    pub label: SharedString,
    /// Runs on the main thread when the user clicks the button.
    pub handler: ToastHandler,
}

/// One alert (docs/0058 D3).
pub struct Toast {
    pub level: Level,
    pub title: Option<SharedString>,
    pub message: SharedString,
    /// Stable identity for dedupe: repeats *refresh* the visible toast
    /// instead of stacking (upstream replace-on-id, docs/0058 D4).
    pub dedupe_key: Option<SharedString>,
    pub action: Option<ToastAction>,
}

impl Toast {
    pub fn new(level: Level, message: impl Into<SharedString>) -> Self {
        Self {
            level,
            title: None,
            message: message.into(),
            dedupe_key: None,
            action: None,
        }
    }

    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn dedupe_key(mut self, key: impl Into<SharedString>) -> Self {
        self.dedupe_key = Some(key.into());
        self
    }

    /// D8: every raise is mirrored to the log at its own priority, so a
    /// toast is never the only copy of the information.
    fn mirror_to_log(&self) {
        match &self.title {
            Some(title) => log::log!(
                self.level.log_level(),
                "toast [{:?}] {title}: {}",
                self.level,
                self.message
            ),
            None => log::log!(
                self.level.log_level(),
                "toast [{:?}]: {}",
                self.level,
                self.message
            ),
        }
    }
}

/// `Send + Sync` handle onto the hub's spill queue (docs/0058 D6). Cloned
/// into non-UI threads; pushes are bounded (drop-oldest) and mirrored to
/// the log. The shell's 1 s pump moves spilled toasts into the hub.
#[derive(Clone, Default)]
pub struct ToastSpill(Arc<Mutex<Vec<Toast>>>);

impl ToastSpill {
    /// Push from any thread. Bounded: at [`SPILL_CAP`] the oldest spilled
    /// toast is dropped, loudly.
    pub fn push(&self, toast: Toast) {
        toast.mirror_to_log();
        let mut q = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if q.len() >= SPILL_CAP {
            q.remove(0);
            log::warn!("toast spill overflow; dropped the oldest spilled toast");
        }
        q.push(toast);
    }

    /// Drain everything (hub thread only).
    pub(crate) fn take(&self) -> Vec<Toast> {
        std::mem::take(&mut self.0.lock().unwrap_or_else(|p| p.into_inner()))
    }
}

/// The app-wide toast queue (docs/0058 D2). Pure staging: raises land
/// here, the shell drains into the notification layer.
pub struct ToastHub {
    pending: VecDeque<Toast>,
    spill: ToastSpill,
}

impl ToastHub {
    /// Main-thread ingest (D2).
    fn push_local(&mut self, toast: Toast, cx: &mut Context<Self>) {
        toast.mirror_to_log();
        if self.pending.len() >= QUEUE_CAP {
            self.pending.pop_front();
            log::warn!("toast queue overflow; dropped the oldest pending toast");
        }
        self.pending.push_back(toast);
        cx.notify();
    }

    /// Move spilled (non-UI-thread) toasts into the pending queue; the
    /// notify makes the shell's observer forward them. Returns how many
    /// arrived (the pump logs nothing by itself — this is for tests).
    pub fn drain_spill(&mut self, cx: &mut Context<Self>) -> usize {
        let spilled = self.spill.take();
        let arrived = spilled.len();
        for toast in spilled {
            if self.pending.len() >= QUEUE_CAP {
                self.pending.pop_front();
                log::warn!("toast queue overflow; dropped the oldest pending toast");
            }
            self.pending.push_back(toast);
        }
        if arrived > 0 {
            cx.notify();
        }
        arrived
    }

    /// Everything staged but not yet forwarded (shell drains on notify).
    pub(crate) fn take_pending(&mut self) -> Vec<Toast> {
        self.pending.drain(..).collect()
    }

    #[cfg(test)]
    fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

struct ToastsGlobal(Entity<ToastHub>);

impl Global for ToastsGlobal {}

/// Create the hub global. Once, in `main`, right after `status::init` —
/// raisers may exist before the window does (docs/0058 D2).
pub fn init(cx: &mut App) {
    let hub = cx.new(|_| ToastHub {
        pending: VecDeque::new(),
        spill: ToastSpill::default(),
    });
    cx.set_global(ToastsGlobal(hub));
}

/// Fetch the hub entity. Panics if [`init`] was never called — the same
/// programming-error contract as `status::tracker`.
pub fn hub(cx: &App) -> Entity<ToastHub> {
    let ToastsGlobal(hub) = cx.global::<ToastsGlobal>();
    hub.clone()
}

/// The `Send + Sync` spill handle, for handing to non-UI threads (D6).
pub fn spill(cx: &App) -> ToastSpill {
    hub(cx).read(cx).spill.clone()
}

/// Queue a toast for display (docs/0058 D2). The shell forwards it to the
/// notification layer as soon as the window exists; raises before that
/// stage in the hub.
pub fn raise(cx: &mut App, toast: Toast) {
    hub(cx).update(cx, |hub, cx| hub.push_local(toast, cx));
}

/// Convenience raisers (docs/0058 D2). Errors stick; the rest auto-hide.
pub fn info(cx: &mut App, message: impl Into<SharedString>) {
    raise(cx, Toast::new(Level::Info, message));
}

pub fn success(cx: &mut App, message: impl Into<SharedString>) {
    raise(cx, Toast::new(Level::Success, message));
}

pub fn warning(cx: &mut App, message: impl Into<SharedString>) {
    raise(cx, Toast::new(Level::Warning, message));
}

pub fn error(cx: &mut App, message: impl Into<SharedString>) {
    raise(cx, Toast::new(Level::Error, message));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn raises_stage_in_order_before_any_window(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            init(cx);
            raise(cx, Toast::new(Level::Info, "first"));
            error(cx, "second");
            let hub = hub(cx);
            assert_eq!(hub.read(cx).pending_len(), 2);
            let drained = hub.update(cx, |hub, _| hub.take_pending());
            assert_eq!(drained.len(), 2);
            assert_eq!(drained[0].message, "first");
            assert_eq!(drained[0].level, Level::Info);
            assert_eq!(drained[1].message, "second");
            assert_eq!(drained[1].level, Level::Error);
            assert!(drained[1].level.sticky());
        });
    }

    #[gpui::test]
    fn queue_cap_drops_oldest(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            init(cx);
            for i in 0..QUEUE_CAP + 5 {
                raise(cx, Toast::new(Level::Info, format!("m{i}")));
            }
            let drained = hub(cx).update(cx, |hub, _| hub.take_pending());
            assert_eq!(drained.len(), QUEUE_CAP);
            assert_eq!(drained[0].message, "m5", "the oldest five were dropped");
            assert_eq!(
                drained.last().unwrap().message,
                format!("m{}", QUEUE_CAP + 4)
            );
        });
    }

    #[gpui::test]
    fn spill_pumps_into_the_queue_and_notifies(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            init(cx);
            let spill = spill(cx);
            spill.push(Toast::new(Level::Warning, "from a thread").dedupe_key("k"));
            // Spilled toasts do not appear until the pump drains (D6).
            assert_eq!(hub(cx).read(cx).pending_len(), 0);
            let arrived = hub(cx).update(cx, |hub, cx| hub.drain_spill(cx));
            assert_eq!(arrived, 1);
            assert_eq!(hub(cx).read(cx).pending_len(), 1);
            let drained = hub(cx).update(cx, |hub, _| hub.take_pending());
            assert_eq!(drained[0].dedupe_key.as_deref(), Some("k"));
        });
    }

    #[test]
    fn spill_is_bounded_and_fifo() {
        let spill = ToastSpill::default();
        for i in 0..SPILL_CAP + 3 {
            spill.push(Toast::new(Level::Warning, format!("s{i}")));
        }
        let drained = spill.take();
        assert_eq!(drained.len(), SPILL_CAP);
        assert_eq!(drained[0].message, "s3", "the oldest three were dropped");
        assert!(spill.take().is_empty(), "take empties the spill");
    }

    #[test]
    fn only_errors_are_sticky() {
        assert!(Level::Error.sticky());
        assert!(!Level::Info.sticky());
        assert!(!Level::Success.sticky());
        assert!(!Level::Warning.sticky());
    }
}
