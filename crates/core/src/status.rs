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
use gpui::{
    Anchor, AnyElement, App, AppContext as _, Entity, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, accesskit, div,
    px,
};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _,
    badge::Badge,
    button::{Button, ButtonVariants as _},
    h_flex,
    popover::Popover,
    progress::ProgressCircle,
    status_bar::StatusBar,
    v_flex,
};

use crate::registry::ModuleRegistry;
use crate::ui::{empty_state, section_header};

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
pub fn bar(
    tracker: &Entity<InFlight>,
    registry: &ModuleRegistry,
    nav: &Entity<crate::nav::Navigator>,
    active: &crate::nav::Page,
    cx: &App,
) -> StatusBar {
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
    // Module-contributed items (docs/0050 D5) after the spinners —
    // e.g. GitHub's usage chip, visible only on its module pages.
    for item in registry.status_items(active, cx) {
        bar = bar.right(item);
    }
    // The notification indicator is the rightmost slot (docs/0059 D3),
    // after module-contributed items.
    bar = bar.right(notifications_indicator(cx, nav));
    // Stable height (docs/0013 D5): with no items the component collapses
    // to its bare padding, and the first fetch would then grow it to the
    // content height — resizing the columns above. The minimum pins the
    // idle geometry to the active one (`h_6` step ≈ py_1 + text_xs line).
    bar.min_h_6()
}

// ---------------------------------------------------------------------------
// Notification indicator (docs/0059 D3/D4) — the bar's rightmost slot.
// ---------------------------------------------------------------------------

/// The bell trigger's a11y label (docs/0010 D10): announces the count to
/// the bridge and screen readers.
pub fn indicator_label(unread: usize) -> SharedString {
    format!("Notifications, {unread} unread").into()
}

/// What the trigger's badge renders (docs/0062 D5), decided purely so
/// tests can pin it: `Hidden` wraps the trigger bare (zero unread, or
/// an empty counted-level set), `Count` is the numeric 99+-capped
/// badge, `Dot` is the numberless dot.
pub enum BadgeView {
    Hidden,
    Count(usize),
    Dot,
}

pub fn badge_view(unread: usize, style: crate::settings::BadgeStyle) -> BadgeView {
    if unread == 0 {
        return BadgeView::Hidden;
    }
    match style {
        crate::settings::BadgeStyle::Count => BadgeView::Count(unread),
        crate::settings::BadgeStyle::Dot => BadgeView::Dot,
    }
}

/// The rightmost bar slot (docs/0059 D3): a bell button under an unread
/// `Badge` (upstream hides the number at 0); clicking opens the panel.
fn notifications_indicator(cx: &App, nav: &Entity<crate::nav::Navigator>) -> AnyElement {
    let hub = crate::toasts::hub(cx);
    // docs/0062 D4/D5: the badge counts the level-filtered unread and
    // takes the configured style; the trigger's label carries the same
    // (filtered) count in both modes. Missing settings = factory.
    let settings = cx.try_global::<crate::settings::AppSettings>();
    let style = settings.map(|s| s.badge_style()).unwrap_or_default();
    let levels = settings
        .map(|s| s.badge_levels())
        .unwrap_or_else(|| crate::toasts::Level::ALL.to_vec());
    let unread = hub.read(cx).unread_count_for(&levels);

    let trigger = Button::new("status-notifications")
        // debug_bounds/bridge findability (docs/0010 D10) — ids alone are
        // not matched by `dashboard-debug tree` selectors in tests.
        .debug_selector(|| "status-notifications".to_string())
        .ghost()
        .small()
        .icon(IconName::Bell)
        .accessibility_label(indicator_label(unread));

    let popover = Popover::new("status-notifications-popover")
        // Tip at the trigger's bottom-right corner: the panel hangs
        // upward-left from the bar (§6 T2).
        .anchor(Anchor::BottomRight)
        .trigger(trigger)
        .on_open_change({
            let hub = hub.clone();
            move |open, _, cx| {
                hub.update(cx, |hub, cx| {
                    if *open {
                        hub.open_panel();
                    } else {
                        hub.close_panel();
                    }
                    cx.notify();
                });
            }
        })
        .content({
            let nav = nav.clone();
            move |_, _, cx| {
                let hub = crate::toasts::hub(cx);
                let rows = hub.read(cx).panel_snapshot().to_vec();
                notification_panel(rows, &nav, cx)
            }
        });

    // docs/0062 D5: count caps at 99+, dot hides the number, hidden
    // wraps the trigger with no badge at all (upstream's Dot variant
    // would render at zero unread).
    let badge = match badge_view(unread, style) {
        BadgeView::Hidden => Badge::new(),
        BadgeView::Count(count) => Badge::new().count(count).max(99),
        BadgeView::Dot => Badge::new().dot(),
    };
    badge.child(popover).into_any_element()
}

/// The panel body (docs/0059 D4): header + rows from the open-time
/// snapshot. Opening already marked everything read — this is a review
/// surface, not a live filter.
fn notification_panel(
    rows: Vec<crate::toasts::HubEntry>,
    nav: &Entity<crate::nav::Navigator>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let mut list = v_flex().w(px(384.)); // ~24 rem (docs/0059 D4)
    if rows.is_empty() {
        list = list.child(empty_state("No unread notifications", cx));
    }
    for (index, entry) in rows.iter().enumerate() {
        let toast = &entry.toast;
        list = list.child(
            h_flex()
                .id(SharedString::from(format!("notification-row-{index}")))
                .role(accesskit::Role::ListItem)
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .child(level_icon(toast.level, cx).small())
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .children(
                            toast
                                .title
                                .clone()
                                .map(|title| div().text_sm().font_medium().child(title)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(toast.message.clone()),
                        ),
                )
                // docs/0062 D8: a dim source tag so mutes are informed
                // decisions, then the relative time.
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(crate::toasts::source_label(toast.source.as_ref(), cx)),
                )
                .child(div().text_xs().text_color(theme.muted_foreground).child(
                    crate::toasts::relative_time(entry.raised_at, Instant::now()),
                ))
                .child(
                    Button::new(SharedString::from(format!("notification-dismiss-{index}")))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        // docs/0049 D1: icon-only buttons get an
                        // explicit accessible name.
                        .accessibility_label("Dismiss notification")
                        .on_click(move |_, _, cx| {
                            crate::toasts::hub(cx).update(cx, |hub, cx| {
                                hub.dismiss_panel_row(index);
                                cx.notify();
                            });
                        }),
                ),
        );
    }
    div()
        .id("notification-panel")
        .debug_selector(|| "notification-panel".to_string())
        .max_h(px(420.))
        .overflow_y_scroll()
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .px_3()
                .py_2()
                .child(section_header("Notifications"))
                .child(
                    h_flex()
                        .gap_1()
                        .child(
                            // docs/0062 D1: the panel's settings entry —
                            // navigates to Settings (deep-linking the
                            // Notifications subpage when the subnav state
                            // is fresh); no toggles inside the popover.
                            Button::new("notification-settings")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Settings)
                                .accessibility_label("Notification settings")
                                .debug_selector(|| "notification-settings".to_string())
                                .on_click({
                                    let nav = nav.clone();
                                    move |_, _, cx| {
                                        crate::settings::request_notifications_settings(cx);
                                        nav.update(cx, |nav, cx| {
                                            nav.go(crate::nav::Page::Settings, cx)
                                        });
                                    }
                                }),
                        )
                        .child(
                            Button::new("notification-clear-all")
                                .ghost()
                                .xsmall()
                                .label("Clear all")
                                .on_click(|_, _, cx| {
                                    crate::toasts::hub(cx).update(cx, |hub, cx| {
                                        hub.clear_panel();
                                        cx.notify();
                                    });
                                }),
                        ),
                ),
        )
        .child(list)
        .into_any_element()
}

/// Severity icon per level — mirrors upstream's own mapping
/// (notification.rs `NotificationType::icon`) so both surfaces agree.
fn level_icon(level: crate::toasts::Level, cx: &App) -> Icon {
    match level {
        crate::toasts::Level::Info => Icon::new(IconName::Info).text_color(cx.theme().info),
        crate::toasts::Level::Success => {
            Icon::new(IconName::CircleCheck).text_color(cx.theme().success)
        }
        crate::toasts::Level::Warning => {
            Icon::new(IconName::TriangleAlert).text_color(cx.theme().warning)
        }
        crate::toasts::Level::Error => Icon::new(IconName::CircleX).text_color(cx.theme().danger),
    }
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
    #[test]
    fn indicator_label_announces_the_count() {
        assert_eq!(indicator_label(0), "Notifications, 0 unread");
        assert_eq!(indicator_label(12), "Notifications, 12 unread");
        assert_eq!(indicator_label(105), "Notifications, 105 unread");
    }

    #[test]
    fn badge_view_modes() {
        // docs/0062 D5: zero unread hides the badge in either style; the
        // count style carries the number (99+ capping happens at render
        // via .max(99)); dot never shows a number.
        use crate::settings::BadgeStyle;
        assert!(matches!(
            badge_view(0, BadgeStyle::Count),
            BadgeView::Hidden
        ));
        assert!(matches!(badge_view(0, BadgeStyle::Dot), BadgeView::Hidden));
        assert!(matches!(
            badge_view(3, BadgeStyle::Count),
            BadgeView::Count(3)
        ));
        assert!(matches!(
            badge_view(105, BadgeStyle::Count),
            BadgeView::Count(105)
        ));
        assert!(matches!(badge_view(105, BadgeStyle::Dot), BadgeView::Dot));
    }
}
