//! Global in-flight request tracker + the app status bar (docs/0013).
//!
//! One `Entity<InFlight>` in a gpui global (D2): modules report fetch runs
//! with [`begin`]/[`end`] — one run of one `FetchSpec` is one tracked unit
//! (D4) — and the shell renders [`bar`] from the aggregated snapshot. The
//! bookkeeping is a plain inner struct, unit-tested without gpui (D3).
//!
//! Layout (docs/0013 D1/D5): the bar is app chrome below both main columns;
//! its right region lists one item per module with runs in flight —
//! spinner + module title, `×n` on concurrent runs — and renders nothing
//! when idle.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{App, AppContext as _, Entity, ParentElement as _, SharedString, Styled as _};
use gpui_component::{Sizable as _, h_flex, progress::ProgressCircle, status_bar::StatusBar};

use crate::registry::ModuleRegistry;

/// Entries older than this are swept on the next mutation (docs/0013 D3) —
/// the stuck-spinner backstop for a lost `end`. The worst legal single-spec
/// run is the 10-page notifications walk (10 × 30 s ureq timeout = 300 s);
/// 10 minutes covers it with headroom.
const STALE_AFTER: Duration = Duration::from_secs(600);

// ---------------------------------------------------------------------------
// Pure bookkeeping (D3) — no gpui types below this line except SharedString.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct InFlightBook {
    entries: BTreeMap<u64, Entry>,
    next_id: u64,
}

struct Entry {
    module: SharedString,
    started: Instant,
}

impl InFlightBook {
    /// Record a run; ids are unique per call, so two concurrent runs of the
    /// same module never share an entry.
    fn begin(&mut self, module: SharedString) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.entries.insert(
            id,
            Entry {
                module,
                started: Instant::now(),
            },
        );
        self.sweep();
        id
    }

    /// Drop a finished run; an unknown id is a no-op, never a panic.
    fn end(&mut self, id: u64) {
        self.entries.remove(&id);
        self.sweep();
    }

    /// module id → live run count, keyed in a `BTreeMap` so the bar's item
    /// order is deterministic (module id lexicographic).
    fn by_module(&self) -> BTreeMap<SharedString, usize> {
        let mut counts: BTreeMap<SharedString, usize> = BTreeMap::new();
        for entry in self.entries.values() {
            *counts.entry(entry.module.clone()).or_default() += 1;
        }
        counts
    }

    /// Drop entries whose `end` never arrived, loudly (D3). Sweeping on
    /// every mutation keeps the cost at O(live entries) and guarantees a
    /// connected app self-cleans within one 60 s tick of expiry.
    fn sweep(&mut self) {
        let now = Instant::now();
        let stale: Vec<u64> = self
            .entries
            .iter()
            .filter(|(_, entry)| now.duration_since(entry.started) >= STALE_AFTER)
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            if let Some(entry) = self.entries.remove(&id) {
                log::warn!(
                    "in-flight entry `{}` expired after {:?} — end() was never called",
                    entry.module,
                    now.duration_since(entry.started),
                );
            }
        }
    }

    /// Test hook: begin with an explicit start time (no clock mocking).
    #[cfg(test)]
    fn begin_at(&mut self, module: SharedString, started: Instant) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.entries.insert(id, Entry { module, started });
        id
    }
}

// ---------------------------------------------------------------------------
// The entity + global (D2)
// ---------------------------------------------------------------------------

/// The app-wide tracker — pure bookkeeping inside, observed by the shell.
pub struct InFlight(InFlightBook);

impl InFlight {
    fn by_module(&self) -> BTreeMap<SharedString, usize> {
        self.0.by_module()
    }
}

struct InFlightGlobal(Entity<InFlight>);

impl gpui::Global for InFlightGlobal {}

/// Create the tracker global. Once, in `main`, right after
/// `gpui_component::init` and before any module `init` runs.
pub fn init(cx: &mut App) {
    let tracker = cx.new(|_| InFlight(InFlightBook::default()));
    cx.set_global(InFlightGlobal(tracker));
}

/// Fetch the tracker entity. Panics if [`init`] was never called — the same
/// programming-error contract as using gpui-component before its init.
pub fn tracker(cx: &App) -> Entity<InFlight> {
    let InFlightGlobal(entity) = cx.global::<InFlightGlobal>();
    entity.clone()
}

/// A fetch run started (D4). Returns the id to hand back to [`end`].
pub fn begin(cx: &mut App, module: SharedString) -> u64 {
    tracker(cx).update(cx, |this, _| this.0.begin(module))
}

/// A fetch run finished.
pub fn end(cx: &mut App, id: u64) {
    tracker(cx).update(cx, |this, _| this.0.end(id));
}

// ---------------------------------------------------------------------------
// The bar (D1/D5)
// ---------------------------------------------------------------------------

/// The app status bar: one bar below both main columns; the right region
/// carries one item per module with runs in flight. Idle renders nothing —
/// the bar itself always stays at a stable minimum height, so nothing
/// shifts when indicators appear or disappear.
pub fn bar(tracker: &Entity<InFlight>, registry: &ModuleRegistry, cx: &App) -> StatusBar {
    let mut bar = StatusBar::new();
    for (module_id, count) in tracker.read(cx).by_module() {
        // Registry title for display; the raw id is the defensive fallback
        // (every reporter is a registered module).
        let title = registry
            .get(&module_id)
            .map(|entry| entry.meta.title.clone())
            .unwrap_or_else(|| module_id.clone());
        bar = bar.right(
            h_flex()
                .gap_1()
                .child(
                    ProgressCircle::new(SharedString::from(format!("status-spinner-{module_id}")))
                        .small()
                        .loading(true)
                        .accessibility_label(format!("{title} syncing")),
                )
                .child(title)
                .when(count > 1, |row| {
                    row.child(SharedString::from(format!("×{count}")))
                }),
        );
    }
    // Stable height (docs/0013 D5): with no items the component collapses
    // to its bare padding, and the first fetch would then grow it to the
    // content height — resizing the columns above. The minimum pins the
    // idle geometry to the active one (`h_6` step ≈ py_1 + text_xs line).
    bar.min_h_6()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(name: &str) -> SharedString {
        SharedString::from(name.to_string())
    }

    fn ages_ago(secs: u64) -> Instant {
        Instant::now() - Duration::from_secs(secs)
    }

    #[test]
    fn begin_end_pairing_empties_the_tracker() {
        let mut book = InFlightBook::default();
        let id = book.begin(module("github"));
        assert_eq!(book.by_module().get("github"), Some(&1));
        book.end(id);
        assert!(book.by_module().is_empty());
    }

    #[test]
    fn unknown_end_id_is_a_noop() {
        let mut book = InFlightBook::default();
        let id = book.begin(module("github"));
        book.end(4_096); // never allocated — must not panic or drop others
        assert_eq!(book.by_module().get("github"), Some(&1));
        book.end(id);
        assert!(book.by_module().is_empty());
    }

    #[test]
    fn same_module_concurrency_aggregates() {
        let mut book = InFlightBook::default();
        let a = book.begin(module("github"));
        let b = book.begin(module("github"));
        assert_eq!(book.by_module().get("github"), Some(&2));
        book.end(a);
        assert_eq!(book.by_module().get("github"), Some(&1));
        book.end(b);
        assert!(book.by_module().is_empty());
    }

    #[test]
    fn modules_stay_separate_and_ordered() {
        let mut book = InFlightBook::default();
        book.begin(module("github"));
        book.begin(module("clock"));
        book.begin(module("github"));
        let counts = book.by_module();
        let ordered: Vec<&str> = counts.keys().map(SharedString::as_ref).collect();
        assert_eq!(ordered, ["clock", "github"]); // BTreeMap: lexicographic
        assert_eq!(counts.get("github"), Some(&2));
        assert_eq!(counts.get("clock"), Some(&1));
    }

    #[test]
    fn stale_entries_sweep_on_the_next_mutation() {
        let mut book = InFlightBook::default();
        let stale = book.begin_at(module("github"), ages_ago(601));
        let young = book.begin_at(module("github"), ages_ago(30));

        // Any mutation sweeps: this fresh begin drops the expired entry.
        let _fresh = book.begin(module("clock"));
        assert_eq!(book.by_module().get("github"), Some(&1)); // only `young`
        assert_eq!(book.by_module().get("clock"), Some(&1));

        // The boundary is inclusive: an entry aged exactly STALE_AFTER
        // (plus the test's own run time) sweeps on the next mutation too.
        let boundary = book.begin_at(module("clock"), ages_ago(600));
        book.end(young); // mutation → sweep
        assert_eq!(book.by_module().get("github"), None); // `young` ended
        assert_eq!(book.by_module().get("clock"), Some(&1)); // `fresh` survives

        // A mutation with nothing stale left leaves the rest alone.
        book.end(boundary); // already swept — the end itself is the no-op
        assert_eq!(book.by_module().get("clock"), Some(&1));
        let _ = stale; // its id was consumed by the first sweep
    }
}
