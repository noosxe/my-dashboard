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
use std::time::Instant;

/// Pending-queue cap (docs/0058 D4): drop-oldest so a raiser storm stays
/// bounded even if no window ever opens.
const QUEUE_CAP: usize = 32;

/// Spill cap (D6): drop-oldest, guarded loudly.
const SPILL_CAP: usize = 16;

/// Log cap (docs/0059 D1): drop-oldest, same policy as the pending queue.
const LOG_CAP: usize = 100;
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
#[derive(Clone)]
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

/// One entry in the hub's unread log (docs/0059 D1): every raise lands
/// here exactly once — direct or spilled — survives until the cap evicts
/// it, and carries its own read flag (docs/0059 D2: only human actions
/// flip it).
#[derive(Clone)]
pub struct HubEntry {
    pub toast: Toast,
    pub raised_at: Instant,
    pub read: bool,
}

/// The app-wide toast queue (docs/0058 D2). Pure staging: raises land
/// here, the shell drains into the notification layer.
pub struct ToastHub {
    pending: VecDeque<Toast>,
    spill: ToastSpill,
    /// docs/0059 D1: the bounded unread log every raise appends to.
    entries: Vec<HubEntry>,
    /// docs/0059 D3/D4: panel open flag + the open-time snapshot of rows.
    panel_open: bool,
    panel_view: Vec<HubEntry>,
}

impl ToastHub {
    /// Main-thread ingest (D2).
    fn push_local(&mut self, toast: Toast, cx: &mut Context<Self>) {
        toast.mirror_to_log();
        self.append_log(&toast);
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
        spilled.iter().for_each(|toast| self.append_log(toast));
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

    /// Append to the unread log (docs/0059 D1): bounded, drop-oldest.
    fn append_log(&mut self, toast: &Toast) {
        if self.entries.len() >= LOG_CAP {
            self.entries.remove(0);
        }
        self.entries.push(HubEntry {
            toast: toast.clone(),
            raised_at: Instant::now(),
            read: false,
        });
    }

    /// Unread entries in the log — the badge count (docs/0059 D3).
    pub fn unread_count(&self) -> usize {
        self.entries.iter().filter(|entry| !entry.read).count()
    }

    /// Whether the notification panel is open (docs/0059 D3).
    pub fn panel_open(&self) -> bool {
        self.panel_open
    }

    /// The panel's displayed rows: the snapshot captured at open. Rows
    /// leave only via per-row dismissal or "Clear all" (docs/0059 D4).
    pub fn panel_snapshot(&self) -> &[HubEntry] {
        &self.panel_view
    }

    /// Human action (docs/0059 D2/D4): opening the panel marks every
    /// unread entry read — the badge clears at open — while the displayed
    /// list freezes as the open-time snapshot so it doesn't vanish
    /// mid-read. Returns how many rows the panel shows.
    pub fn open_panel(&mut self) -> usize {
        self.panel_view = self
            .entries
            .iter()
            .filter(|entry| !entry.read)
            .cloned()
            .collect();
        for entry in &mut self.entries {
            entry.read = true;
        }
        self.panel_open = true;
        self.panel_view.len()
    }

    /// Human action: the panel closed (outside click / Esc / re-click).
    /// The snapshot dies with it; the next open re-snapshots.
    pub fn close_panel(&mut self) {
        self.panel_open = false;
        self.panel_view.clear();
    }

    /// Human action: dismiss one panel row by visible index; an
    /// out-of-range index is a no-op, never a panic.
    pub fn dismiss_panel_row(&mut self, index: usize) {
        if index < self.panel_view.len() {
            self.panel_view.remove(index);
        }
    }

    /// Human action: empty the panel view (docs/0059 D4). The underlying
    /// entries are already read — this clears what the panel shows.
    pub fn clear_panel(&mut self) {
        self.panel_view.clear();
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
        entries: Vec::new(),
        panel_open: false,
        panel_view: Vec::new(),
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

/// Coarse relative time for panel rows (docs/0059 D4). Pure so tests pin
/// the boundaries without a clock.
pub fn relative_time(raised_at: Instant, now: Instant) -> SharedString {
    let secs = now.duration_since(raised_at).as_secs();
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", secs / 60).into(),
        3600..=86_399 => format!("{}h ago", secs / 3600).into(),
        _ => format!("{}d ago", secs / 86_400).into(),
    }
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
    #[gpui::test]
    fn every_raise_lands_in_the_unread_log(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            init(cx);
            raise(cx, Toast::new(Level::Info, "first"));
            error(cx, "second");
            spill(cx).push(Toast::new(Level::Warning, "from a thread"));
            hub(cx).update(cx, |hub, cx| {
                hub.drain_spill(cx);
            });
            let hub = hub(cx);
            assert_eq!(hub.read(cx).unread_count(), 3);
            // Forwarding into the notification layer is not a read
            // (docs/0059 D2): autohide/programmatic paths never flip it.
            hub.update(cx, |hub, _| assert_eq!(hub.take_pending().len(), 3));
            assert_eq!(hub.read(cx).unread_count(), 3);
            assert_eq!(hub.update(cx, |hub, _| hub.open_panel()), 3);
            let view = hub.read(cx).panel_snapshot();
            assert_eq!(view[0].toast.message, "first");
            assert_eq!(view[1].toast.message, "second");
            assert_eq!(view[2].toast.message, "from a thread");
        });
    }

    #[gpui::test]
    fn log_cap_drops_oldest(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            init(cx);
            for i in 0..LOG_CAP + 5 {
                raise(cx, Toast::new(Level::Info, format!("m{i}")));
            }
            let hub = hub(cx);
            assert_eq!(hub.read(cx).unread_count(), LOG_CAP);
            assert_eq!(hub.update(cx, |hub, _| hub.open_panel()), LOG_CAP);
            let view = hub.read(cx).panel_snapshot();
            assert_eq!(view[0].toast.message, "m5", "the oldest five were dropped");
            assert_eq!(
                view.last().unwrap().toast.message,
                format!("m{}", LOG_CAP + 4)
            );
        });
    }

    #[gpui::test]
    fn panel_open_marks_all_read_and_freezes_the_snapshot(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            init(cx);
            raise(cx, Toast::new(Level::Info, "first"));
            raise(cx, Toast::new(Level::Info, "second"));
            let hub = hub(cx);
            assert_eq!(hub.update(cx, |hub, _| hub.open_panel()), 2);
            assert_eq!(hub.read(cx).unread_count(), 0, "badge clears at open");
            assert!(hub.read(cx).panel_open());
            // A raise while the panel is open is unseen: the badge nags
            // again, the frozen view doesn't grow (docs/0059 D4).
            raise(cx, Toast::new(Level::Info, "third"));
            assert_eq!(hub.read(cx).unread_count(), 1);
            assert_eq!(hub.read(cx).panel_snapshot().len(), 2);
            // Close and reopen: a fresh snapshot of the unseen entry only.
            hub.update(cx, |hub, _| hub.close_panel());
            assert!(!hub.read(cx).panel_open());
            assert_eq!(hub.update(cx, |hub, _| hub.open_panel()), 1);
            assert_eq!(hub.read(cx).panel_snapshot()[0].toast.message, "third");
        });
    }

    #[gpui::test]
    fn panel_row_dismiss_and_clear_are_index_scoped(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            init(cx);
            for message in ["a", "b", "c"] {
                raise(cx, Toast::new(Level::Info, message));
            }
            let hub = hub(cx);
            hub.update(cx, |hub, _| hub.open_panel());
            hub.update(cx, |hub, _| hub.dismiss_panel_row(1));
            let view = hub.read(cx).panel_snapshot();
            assert_eq!(view.len(), 2);
            assert_eq!(view[0].toast.message, "a");
            assert_eq!(view[1].toast.message, "c");
            // Out-of-range dismissal is a no-op, never a panic.
            hub.update(cx, |hub, _| hub.dismiss_panel_row(9));
            assert_eq!(hub.read(cx).panel_snapshot().len(), 2);
            hub.update(cx, |hub, _| hub.clear_panel());
            assert_eq!(hub.read(cx).panel_snapshot().len(), 0);
        });
    }

    #[test]
    fn relative_time_boundaries() {
        let now = Instant::now();
        assert_eq!(relative_time(now, now), "just now");
        assert_eq!(
            relative_time(now - std::time::Duration::from_secs(59), now),
            "just now"
        );
        assert_eq!(
            relative_time(now - std::time::Duration::from_secs(60), now),
            "1m ago"
        );
        assert_eq!(
            relative_time(now - std::time::Duration::from_secs(3599), now),
            "59m ago"
        );
        assert_eq!(
            relative_time(now - std::time::Duration::from_secs(3600), now),
            "1h ago"
        );
        assert_eq!(
            relative_time(now - std::time::Duration::from_secs(86_399), now),
            "23h ago"
        );
        assert_eq!(
            relative_time(now - std::time::Duration::from_secs(86_400), now),
            "1d ago"
        );
    }
}
