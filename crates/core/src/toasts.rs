//! The toast hub (docs/0058): core's raise API for user-visible alerts.
//!
//! The hub is an entity-global (the docs/0013 D2 `InFlight` pattern):
//! raisers exist *before and without* the window — early `main` wiring,
//! module init, background threads, tests — so a handle on the shell
//! entity would make raising impossible in exactly the cases the issue
//! asks for. The hub itself touches no UI; the shell observes it and
//! forwards into gpui-component's `Root` notification layer (docs/0058
//! D1/D5). Nothing here renders. The queues themselves die with the
//! process (D9); since docs/0063, the *unread log* persists — unread
//! rows survive restarts through the app database (docs/0063 D5/D7).
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
use std::time::{Duration, Instant};

use crate::usage_records::now_epoch_ms;

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

    /// All levels in badge/display order (docs/0062 D4).
    pub const ALL: [Level; 4] = [Level::Info, Level::Success, Level::Warning, Level::Error];

    /// Stable serde-safe name (docs/0062 D4): level names round-trip
    /// through `settings.json` as lowercase strings.
    pub fn name(self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Success => "success",
            Level::Warning => "warning",
            Level::Error => "error",
        }
    }

    /// Parse a level name; unknown names are `None` (load-side drop,
    /// docs/0062 D4).
    pub fn from_name(name: &str) -> Option<Level> {
        match name {
            "info" => Some(Level::Info),
            "success" => Some(Level::Success),
            "warning" => Some(Level::Warning),
            "error" => Some(Level::Error),
            _ => None,
        }
    }

    /// Display name for settings rows (docs/0062 §4).
    pub fn display_name(self) -> &'static str {
        match self {
            Level::Info => "Info",
            Level::Success => "Success",
            Level::Warning => "Warning",
            Level::Error => "Error",
        }
    }
}

/// The fallback source id (docs/0062 D2): raises without `.source(...)`
/// attribute to the app itself.
pub const APP_SOURCE: &str = "app";

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
#[non_exhaustive]
pub struct Toast {
    pub level: Level,
    pub title: Option<SharedString>,
    pub message: SharedString,
    /// Stable identity for dedupe: repeats *refresh* the visible toast
    /// instead of stacking (upstream replace-on-id, docs/0058 D4).
    pub dedupe_key: Option<SharedString>,
    /// Source attribution (docs/0062 D2): the raising surface's stable
    /// id (module registry id, or [`APP_SOURCE`]'s "app" when absent).
    pub source: Option<SharedString>,
    pub action: Option<ToastAction>,
}

impl Toast {
    pub fn new(level: Level, message: impl Into<SharedString>) -> Self {
        Self {
            level,
            title: None,
            message: message.into(),
            dedupe_key: None,
            source: None,
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

    /// Attribute the raise to a source (docs/0062 D2) — the id the
    /// settings page mutes and the panel tags display.
    pub fn source(mut self, source: impl Into<SharedString>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// The raise's effective source id (docs/0062 D2): explicit, or
    /// [`APP_SOURCE`] for unattributed raises.
    pub fn effective_source(&self) -> &str {
        self.source.as_deref().unwrap_or(APP_SOURCE)
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

// ---------------------------------------------------------------------------
// Persistence seam (docs/0063 D10)
// ---------------------------------------------------------------------------

/// One persisted unread notification (docs/0063 D3/D10): plain data —
/// the seam crosses onto the app DB's writer thread and carries no gpui
/// types. Only unread notifications persist; "read" is a delete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingNotification {
    pub level: Level,
    /// The raise's effective source id (docs/0062 D2) — the string the
    /// registry and mute filter key on, restored as-is.
    pub source: String,
    pub title: Option<String>,
    pub message: String,
    pub dedupe_key: Option<String>,
    /// Epoch millis (D9): the log renders ages from `Instant`, so this
    /// round-trips through wall clock and is reconstructed clamped.
    pub raised_at_ms: u64,
}

/// What the hub calls to keep the unread log durable (docs/0063 D10).
/// The writes are non-blocking (lock + push onto the shared writer
/// thread); `load_unread` is the one blocking read — the boot-time
/// restore runs it once, before the window exists. Implementations are
/// best-effort: failures log on their side, never surface here.
pub trait NotificationLog: Send + Sync {
    /// Persist one unread row (D3) — called from `append_log`, the
    /// single ingest choke point, *after* the mute filter.
    fn insert(&self, row: PendingNotification);
    /// `mark_read_by_key` (D7): the incident acknowledges — every row
    /// sharing the key deletes.
    fn delete_by_key(&self, key: &str);
    /// Panel open (D3): everything acknowledges — the persisted set
    /// clears with the badge.
    fn delete_all(&self);
    /// The persisted unread set, newest first (the caller reverses into
    /// log order); bounded by the store's restore window (D5). A failed
    /// read is an empty vec — restore is best-effort.
    fn load_unread(&self) -> Vec<PendingNotification>;
}

/// The default seam (docs/0063 D10): nothing persists. What core `init`
/// builds before `main` attaches the app DB — and every test that never
/// attaches one.
#[derive(Default)]
pub struct NoopLog;

impl NotificationLog for NoopLog {
    fn insert(&self, _row: PendingNotification) {}
    fn delete_by_key(&self, _key: &str) {}
    fn delete_all(&self) {}
    fn load_unread(&self) -> Vec<PendingNotification> {
        Vec::new()
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

/// A panel row's stable identity (docs/0059, #482): the toast's
/// dedupe key when present (dedupe guarantees uniqueness among live
/// rows), else the raise instant. UI element ids key on this — never
/// the row index, which shifts on every dismissal.
pub fn panel_row_key(entry: &HubEntry) -> SharedString {
    entry
        .toast
        .dedupe_key
        .clone()
        .unwrap_or_else(|| SharedString::from(format!("raised-{:?}", entry.raised_at)))
}

/// The app-wide toast queue (docs/0058 D2). Pure staging: raises land
/// here, the shell drains into the notification layer.
pub struct ToastHub {
    pending: VecDeque<Toast>,
    spill: ToastSpill,
    /// docs/0059 D1: the bounded unread log every raise appends to.
    entries: Vec<HubEntry>,
    /// docs/0063 D10: the persistence seam. Defaults to [`NoopLog`];
    /// `main` swaps in the app DB's impl before the window exists.
    log: Arc<dyn NotificationLog>,
    /// docs/0059 D3/D4: panel open flag + the open-time snapshot of rows.
    panel_open: bool,
    panel_view: Vec<HubEntry>,
}

impl ToastHub {
    /// Main-thread ingest (D2).
    fn push_local(&mut self, toast: Toast, cx: &mut Context<Self>) {
        toast.mirror_to_log();
        // Muted sources are suppressed here (docs/0062 D3): the log-file
        // mirror above already fired, so the paper trail survives; nothing
        // user-visible — toast, log entry, badge — lands.
        if is_muted(&toast, cx) {
            return;
        }
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
        // docs/0062 D3: muted sources are suppressed on this ingest path
        // too — the spill mirrored each toast to the log file at push
        // time; only the user-visible half is dropped here.
        let kept: Vec<Toast> = spilled
            .into_iter()
            .filter(|toast| !is_muted(toast, cx))
            .collect();
        let arrived = kept.len();
        kept.iter().for_each(|toast| self.append_log(toast));
        for toast in kept {
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
    /// Also the single persistence hook (docs/0063 D3): every raise that
    /// reaches here survived the mute filter, so it persists as unread —
    /// a non-blocking lock + push onto the app DB's shared writer.
    fn append_log(&mut self, toast: &Toast) {
        if self.entries.len() >= LOG_CAP {
            self.entries.remove(0);
        }
        self.log.insert(PendingNotification {
            level: toast.level,
            source: toast.effective_source().to_string(),
            title: toast.title.as_ref().map(|t| t.to_string()),
            message: toast.message.to_string(),
            dedupe_key: toast.dedupe_key.as_ref().map(|k| k.to_string()),
            raised_at_ms: now_epoch_ms(),
        });
        self.entries.push(HubEntry {
            toast: toast.clone(),
            raised_at: Instant::now(),
            read: false,
        });
    }

    /// Unread entries in the log — the badge count (docs/0059 D3).
    /// The unread log as-is (docs/0059 D1) — read access for surfaces
    /// and tests outside the hub's module.
    pub fn log(&self) -> &[HubEntry] {
        &self.entries
    }

    /// Unread entries in the log — the badge count (docs/0059 D3).
    pub fn unread_count(&self) -> usize {
        self.entries.iter().filter(|entry| !entry.read).count()
    }

    /// Unread count filtered to a level set (docs/0062 D4): what the
    /// badge shows. An empty set counts nothing — the badge-off state.
    /// The panel stays unfiltered: `unread_count` and [`Self::log`] are
    /// the full record.
    pub fn unread_count_for(&self, levels: &[Level]) -> usize {
        self.entries
            .iter()
            .filter(|entry| !entry.read && levels.contains(&entry.toast.level))
            .count()
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
        // docs/0063 D3/D7: opening the panel acknowledges everything —
        // the persisted unread set clears with the badge (read = delete).
        self.log.delete_all();
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

    /// Human action (docs/0060 D3): a click on a keyed raise's toast or
    /// OS banner marks that incident's entries read — the same intent as
    /// opening the panel (docs/0059 D2), one incident at a time. All
    /// entries sharing the key flip together: same incident, re-armed.
    /// Returns whether anything flipped (the badge only re-renders then).
    pub fn mark_read_by_key(&mut self, key: &SharedString, cx: &mut Context<Self>) -> bool {
        let mut marked = false;
        for entry in &mut self.entries {
            if entry.toast.dedupe_key.as_ref() == Some(key) {
                marked |= !entry.read;
                entry.read = true;
            }
        }
        // docs/0063 D7: the same acknowledgment persists — every row
        // sharing the key deletes. Unconditional: an entry evicted by
        // LOG_CAP can still own a persisted row.
        self.log.delete_by_key(key);
        if marked {
            cx.notify();
        }
        marked
    }
    /// Attach the persistence seam (docs/0063 D10) and restore the
    /// persisted unread set (D5): `main` calls this once, after the app
    /// DB opens and before the window exists, so the badge's first frame
    /// is correct. The one blocking read on this path runs here; a
    /// degraded or failing store reads as empty and the hub starts
    /// ephemeral — exactly the pre-0063 behavior (D8).
    pub fn attach_log_and_restore(
        &mut self,
        log: Arc<dyn NotificationLog>,
        cx: &mut Context<Self>,
    ) {
        let rows = log.load_unread();
        self.log = log;
        self.restore_unread(rows, cx);
    }

    /// Swap the seam without restoring — test wiring and any future
    /// mid-session re-attach; `main` wants
    /// [`Self::attach_log_and_restore`].
    pub fn attach_log(&mut self, log: Arc<dyn NotificationLog>) {
        self.log = log;
    }

    /// Seed the log from persisted rows (docs/0063 D5): rows arrive
    /// newest-first and reverse into log order. Restored entries are
    /// unread — the persisted set *is* the unread set (D7) — with ages
    /// reconstructed from wall clock (D9).
    pub fn restore_unread(&mut self, rows: Vec<PendingNotification>, cx: &mut Context<Self>) {
        let restored = rows.len();
        for row in rows.into_iter().rev() {
            if self.entries.len() >= LOG_CAP {
                self.entries.remove(0);
            }
            let raised_at = instant_from_epoch_ms(row.raised_at_ms);
            self.entries.push(HubEntry {
                toast: Toast {
                    level: row.level,
                    title: row.title.map(SharedString::from),
                    message: SharedString::from(row.message),
                    dedupe_key: row.dedupe_key.map(SharedString::from),
                    source: Some(SharedString::from(row.source)),
                    action: None,
                },
                raised_at,
                read: false,
            });
        }
        if restored > 0 {
            cx.notify();
        }
    }

    #[cfg(test)]
    fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

// ---------------------------------------------------------------------------
// Source registry (docs/0062 D2)
// ---------------------------------------------------------------------------

/// One known notification source: a stable id (module registry id, or
/// [`APP_SOURCE`]) plus its display label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceEntry {
    pub id: SharedString,
    pub label: SharedString,
}

struct SourcesGlobal(Vec<SourceEntry>);

impl Global for SourcesGlobal {}

/// Register (or re-label) a source (docs/0062 D2): `init` registers
/// `"app"`; each module registers itself in its own `init`. Idempotent
/// per id — first registration wins the slot, later calls update labels.
pub fn register_source(id: &str, label: &str, cx: &mut App) {
    if cx.has_global::<SourcesGlobal>() {
        let sources = &mut cx.global_mut::<SourcesGlobal>().0;
        match sources.iter_mut().find(|s| s.id == id) {
            Some(existing) => existing.label = label.into(),
            None => sources.push(SourceEntry {
                id: id.into(),
                label: label.into(),
            }),
        }
    } else {
        cx.set_global(SourcesGlobal(vec![SourceEntry {
            id: id.into(),
            label: label.into(),
        }]));
    }
}

/// The registered sources, in registration order (docs/0062 D2). The
/// settings page unions this with ids seen in the log.
pub fn sources(cx: &App) -> Vec<SourceEntry> {
    cx.try_global::<SourcesGlobal>()
        .map(|g| g.0.clone())
        .unwrap_or_default()
}

/// Display label for a toast's source (docs/0062 D8): the registry's
/// label when registered, else the raw id. Unattributed raises resolve
/// to [`APP_SOURCE`].
pub fn source_label(source: Option<&SharedString>, cx: &App) -> SharedString {
    let id = source.map(|s| s.as_ref()).unwrap_or(APP_SOURCE);
    if let Some(found) = sources(cx).into_iter().find(|s| s.id == id) {
        return found.label;
    }
    id.to_string().into()
}

/// Mute check (docs/0062 D3): absolute, per effective source. Reads the
/// settings global live; a missing global (settings not initialized —
/// tests) mutes nothing.
fn is_muted(toast: &Toast, cx: &App) -> bool {
    let source = toast.effective_source();
    cx.try_global::<crate::settings::AppSettings>()
        .map(|s| s.muted_sources().iter().any(|m| m.as_str() == source))
        .unwrap_or(false)
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
        log: Arc::new(NoopLog),
        panel_open: false,
        panel_view: Vec::new(),
    });
    register_source(APP_SOURCE, "App", cx);
    cx.set_global(ToastsGlobal(hub));
}

/// Fetch the hub entity. Panics if [`init`] was never called — the same
/// programming-error contract as `status::tracker`.
/// The hub entity when initialized (docs/0062 §4); `None` in tests that
/// never called [`init`] — surfaces degrade instead of panicking.
pub fn try_hub(cx: &App) -> Option<Entity<ToastHub>> {
    cx.try_global::<ToastsGlobal>().map(|g| g.0.clone())
}

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

/// Map a persisted epoch-ms stamp back to an `Instant` (docs/0063 D9):
/// the log renders ages from `Instant`, so restored rows reconstruct
/// `now − elapsed`. Clamped on both ends — a backwards clock skew
/// reads as fresh (saturating), an absurd age saturates at the cap, and
/// `checked_sub` makes the subtraction itself panic-free.
fn instant_from_epoch_ms(raised_at_ms: u64) -> Instant {
    /// The restore age cap: old-but-unread keeps badging (D6 has no
    /// TTL); only the *displayed* age saturates.
    const MAX_RESTORE_AGE_MS: u64 = 366 * 24 * 60 * 60 * 1000;
    let elapsed = now_epoch_ms()
        .saturating_sub(raised_at_ms)
        .min(MAX_RESTORE_AGE_MS);
    Instant::now()
        .checked_sub(Duration::from_millis(elapsed))
        .unwrap_or_else(Instant::now)
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

    #[test]
    fn level_names_roundtrip() {
        for level in Level::ALL {
            assert_eq!(Level::from_name(level.name()), Some(level));
        }
        assert_eq!(Level::from_name("bogus"), None);
        assert_eq!(Level::ALL.len(), 4);
    }

    #[gpui::test]
    fn source_defaults_to_app_and_registry_labels(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            // Unattributed raises resolve to the app source (D2).
            let toast = Toast::new(Level::Info, "hello");
            assert_eq!(toast.effective_source(), "app");
            assert_eq!(
                Toast::new(Level::Info, "hello")
                    .source("github")
                    .effective_source(),
                "github"
            );
            // init registers the app source; a second registration
            // relabels, a new id appends.
            init(cx);
            assert_eq!(sources(cx).len(), 1);
            assert_eq!(sources(cx)[0].id, "app");
            register_source("app", "Dashboard", cx);
            register_source("github", "GitHub", cx);
            let sources = sources(cx);
            assert_eq!(sources.len(), 2);
            assert_eq!(
                sources[0].label, "Dashboard",
                "re-registration relabels in place"
            );
            assert_eq!(sources[1].id, "github");
            // Labels resolve through the registry, degrade to the id.
            assert_eq!(
                source_label(Some(&"github".into()), cx),
                "GitHub".to_string()
            );
            assert_eq!(source_label(None, cx), "Dashboard".to_string());
            assert_eq!(
                source_label(Some(&"unknown".into()), cx),
                "unknown".to_string()
            );
        });
    }

    #[gpui::test]
    fn muted_sources_are_suppressed_at_both_ingest_paths(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            init(cx);
            cx.set_global(crate::settings::AppSettings::default());
            crate::settings::set_source_muted("github", true, cx);

            // Direct path: no pending, no log entry, no badge (D3).
            raise(cx, Toast::new(Level::Error, "boom").source("github"));
            let hub = hub(cx);
            assert_eq!(hub.read(cx).pending_len(), 0);
            assert_eq!(hub.read(cx).log().len(), 0);
            assert_eq!(hub.read(cx).unread_count(), 0);

            // Spill path: suppressed at the drain, not counted as arrived.
            spill(cx).push(Toast::new(Level::Warning, "spilled").source("github"));
            assert_eq!(hub.update(cx, |hub, cx| hub.drain_spill(cx)), 0);
            assert_eq!(hub.read(cx).pending_len(), 0);
            assert_eq!(hub.read(cx).log().len(), 0);

            // Unmuted sources — including unattributed app raises —
            // are unaffected.
            raise(cx, Toast::new(Level::Info, "kept").source("github-app"));
            error(cx, "kept too");
            assert_eq!(hub.read(cx).pending_len(), 2);
            assert_eq!(hub.read(cx).unread_count(), 2);

            // Unmuting restores delivery.
            crate::settings::set_source_muted("github", false, cx);
            raise(cx, Toast::new(Level::Error, "back").source("github"));
            assert_eq!(hub.read(cx).unread_count(), 3);
        });
    }

    #[gpui::test]
    fn unread_count_for_respects_the_level_set(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            init(cx);
            info(cx, "i");
            error(cx, "e");
            raise(cx, Toast::new(Level::Warning, "w"));
            let hub = hub(cx);
            assert_eq!(
                hub.read(cx).unread_count(),
                3,
                "unfiltered keeps the full record"
            );
            assert_eq!(hub.read(cx).unread_count_for(&[Level::Error]), 1);
            assert_eq!(
                hub.read(cx)
                    .unread_count_for(&[Level::Error, Level::Warning]),
                2
            );
            assert_eq!(
                hub.read(cx).unread_count_for(&[]),
                0,
                "an empty set is the badge-off preference (D4)"
            );
        });
    }

    // --- persistence seam (docs/0063) --------------------------------

    /// A recording seam: what the hub called, in order.
    #[derive(Default)]
    struct FakeLog {
        inserts: Mutex<Vec<PendingNotification>>,
        key_deletes: Mutex<Vec<String>>,
        all_deletes: Mutex<usize>,
    }

    impl NotificationLog for FakeLog {
        fn insert(&self, row: PendingNotification) {
            self.inserts.lock().unwrap().push(row);
        }
        fn delete_by_key(&self, key: &str) {
            self.key_deletes.lock().unwrap().push(key.to_string());
        }
        fn delete_all(&self) {
            *self.all_deletes.lock().unwrap() += 1;
        }
        fn load_unread(&self) -> Vec<PendingNotification> {
            Vec::new()
        }
    }

    fn attach_fake(cx: &mut gpui::TestAppContext) -> Arc<FakeLog> {
        let fake = Arc::new(FakeLog::default());
        cx.update(|cx| {
            hub(cx).update(cx, |hub, cx| hub.attach_log_and_restore(fake.clone(), cx));
        });
        fake
    }

    fn notif_row(tag: &str, key: Option<&str>, age_ms: u64) -> PendingNotification {
        PendingNotification {
            level: Level::Error,
            source: "github".to_string(),
            title: Some(format!("t-{tag}")),
            message: format!("m-{tag}"),
            dedupe_key: key.map(str::to_string),
            raised_at_ms: now_epoch_ms().saturating_sub(age_ms),
        }
    }

    #[gpui::test]
    fn raises_persist_unread_rows_at_the_choke_point(cx: &mut gpui::TestAppContext) {
        cx.update(init);
        let fake = attach_fake(cx);
        cx.update(|cx| {
            raise(
                cx,
                Toast::new(Level::Warning, "direct")
                    .title("t")
                    .dedupe_key("k1")
                    .source("github"),
            );
            spill(cx).push(Toast::new(Level::Error, "spilled").source("github-app"));
            hub(cx).update(cx, |hub, cx| hub.drain_spill(cx));
        });
        let inserts = fake.inserts.lock().unwrap();
        assert_eq!(inserts.len(), 2, "both ingest paths persist (D3)");
        assert_eq!(inserts[0].message, "direct");
        assert_eq!(inserts[0].source, "github");
        assert_eq!(inserts[0].title.as_deref(), Some("t"));
        assert_eq!(inserts[0].dedupe_key.as_deref(), Some("k1"));
        assert_eq!(inserts[0].level, Level::Warning);
        assert_eq!(inserts[1].source, "github-app", "effective source recorded");
    }

    #[gpui::test]
    fn muted_sources_never_persist(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            init(cx);
            cx.set_global(crate::settings::AppSettings::default());
            crate::settings::set_source_muted("github", true, cx);
        });
        let fake = attach_fake(cx);
        cx.update(|cx| {
            raise(cx, Toast::new(Level::Error, "boom").source("github"));
            raise(cx, Toast::new(Level::Info, "kept"));
        });
        let inserts = fake.inserts.lock().unwrap();
        assert_eq!(
            inserts.len(),
            1,
            "muted raises never reach the hook (0062 D3)"
        );
        assert_eq!(inserts[0].source, "app", "unattributed raise = app");
    }

    #[gpui::test]
    fn panel_open_and_key_mark_reach_the_seam(cx: &mut gpui::TestAppContext) {
        cx.update(init);
        let fake = attach_fake(cx);
        cx.update(|cx| {
            raise(cx, Toast::new(Level::Error, "a").dedupe_key("k1"));
            raise(cx, Toast::new(Level::Error, "b").dedupe_key("k2"));
            hub(cx).update(cx, |hub, cx| {
                assert!(hub.mark_read_by_key(&SharedString::from("k1"), cx));
            });
        });
        assert_eq!(*fake.key_deletes.lock().unwrap(), vec!["k1".to_string()]);
        cx.update(|cx| {
            let n = hub(cx).update(cx, |hub, _| hub.open_panel());
            assert_eq!(n, 1);
        });
        assert_eq!(*fake.all_deletes.lock().unwrap(), 1);
    }

    #[gpui::test]
    fn restore_seeds_the_log_and_badge_in_order(cx: &mut gpui::TestAppContext) {
        cx.update(init);
        // Newest-first, as the store returns them (D5).
        let rows = vec![
            notif_row("new", Some("k"), 0),
            notif_row("mid", None, 60_000),
            notif_row("old", None, 600_000),
        ];
        cx.update(|cx| {
            hub(cx).update(cx, |hub, cx| hub.restore_unread(rows, cx));
        });
        cx.update(|cx| {
            let hub = hub(cx);
            let log = hub.read(cx).log();
            assert_eq!(log.len(), 3);
            assert_eq!(log[0].toast.message, "m-old", "oldest first (log order)");
            assert_eq!(log[2].toast.message, "m-new");
            assert!(log[0].raised_at < log[2].raised_at, "ages stay ordered");
            assert_eq!(
                hub.read(cx).unread_count(),
                3,
                "badge counts restored unread"
            );
            // Restored rows keep their identity: the incident key-marks.
            hub.update(cx, |hub, cx| {
                assert!(hub.mark_read_by_key(&SharedString::from("k"), cx));
            });
            assert_eq!(hub.read(cx).unread_count(), 2);
        });
    }

    #[gpui::test]
    fn restore_clamps_absurd_ages_without_panicking(cx: &mut gpui::TestAppContext) {
        cx.update(init);
        // Newest-first: the future-stamped row first, the 1970 row last.
        let rows = vec![
            PendingNotification {
                raised_at_ms: now_epoch_ms() + 86_400_000,
                ..notif_row("future", None, 0)
            },
            PendingNotification {
                raised_at_ms: 0,
                ..notif_row("epoch", None, 0)
            },
        ];
        cx.update(|cx| {
            hub(cx).update(cx, |hub, cx| hub.restore_unread(rows, cx));
        });
        cx.update(|cx| {
            let hub = hub(cx);
            let now = Instant::now();
            for entry in hub.read(cx).log() {
                // Must not panic on either edge (D9).
                let _ = relative_time(entry.raised_at, now);
            }
            // A future stamp saturates to fresh — deterministic on every
            // clock; the 1970 row only needs to render, its exact age
            // text depends on host uptime (capped at 366 d).
            assert_eq!(
                relative_time(hub.read(cx).log()[1].raised_at, now).as_ref(),
                "just now",
                "the future-stamped row is the newest, i.e. log()[1]"
            );
        });
    }

    #[gpui::test]
    fn empty_restore_leaves_the_hub_ephemeral(cx: &mut gpui::TestAppContext) {
        cx.update(init);
        cx.update(|cx| {
            // A failed store read is an empty vec by contract (D5) —
            // the hub must start exactly like today: empty, no panic.
            hub(cx).update(cx, |hub, cx| hub.restore_unread(Vec::new(), cx));
            let hub = hub(cx);
            assert_eq!(hub.read(cx).log().len(), 0);
            assert_eq!(hub.read(cx).unread_count(), 0);
        });
    }

    /// Integration (docs/0063 §5): the full round trip through the real
    /// store — raise → flush → reopen → restored; read → flush →
    /// reopen → gone.
    #[gpui::test]
    fn store_roundtrip_restores_unread_across_restart(cx: &mut gpui::TestAppContext) {
        let path = std::env::temp_dir().join(format!(
            "toasts-notif-restart-{}-{}",
            std::process::id(),
            line!()
        ));
        cx.update(init);
        // Session 1: two raises, one keyed; the keyed one acknowledges
        // via mark-read (buffered insert + key-delete replay in order).
        let store = crate::app_db::SQLiteStore::open_at(path.clone());
        cx.update(|cx| {
            hub(cx).update(cx, |hub, _| hub.attach_log(store.clone()));
            raise(cx, Toast::new(Level::Error, "survivor"));
            raise(cx, Toast::new(Level::Error, "ack'd").dedupe_key("k1"));
            hub(cx).update(cx, |hub, cx| {
                assert!(hub.mark_read_by_key(&SharedString::from("k1"), cx));
            });
        });
        store.flush_blocking();
        drop(store);
        // "Restart": reopen the same file; a fresh hub (a new process
        // means a new hub entity seeded from disk) restores the survivor.
        let store = crate::app_db::SQLiteStore::open_at(path.clone());
        cx.update(|cx| {
            let hub = cx.new(|_| ToastHub {
                pending: VecDeque::new(),
                spill: ToastSpill::default(),
                entries: Vec::new(),
                log: Arc::new(NoopLog),
                panel_open: false,
                panel_view: Vec::new(),
            });
            hub.update(cx, |hub, cx| hub.restore_unread(store.load_unread(), cx));
            assert_eq!(hub.read(cx).unread_count(), 1);
            assert_eq!(hub.read(cx).log()[0].toast.message, "survivor");
            // Read → flush → reopen → gone (D7).
            hub.update(cx, |hub, _| hub.attach_log(store.clone()));
            hub.update(cx, |hub, _| hub.open_panel());
        });
        // Read → flush → reopen → gone (D7).
        cx.update(|cx| {
            hub(cx).update(cx, |hub, _| hub.attach_log(store.clone()));
            hub(cx).update(cx, |hub, _| hub.open_panel());
        });
        store.flush_blocking();
        drop(store);
        let store = crate::app_db::SQLiteStore::open_at(path.clone());
        assert!(!store.is_degraded());
        assert!(store.load_unread().is_empty());
    }
}
