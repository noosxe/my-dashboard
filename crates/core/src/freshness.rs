//! Freshness — the shared refresh/age contract for every module
//! (docs/0037).
//!
//! Core owns the *contract*: what it means for a surface to be
//! "refreshable" (the age policy), how a failing surface backs off,
//! what the synced label says, and the header row all pages render.
//! A module implements [`Freshness`] over its own records and gets
//! the standard UI/UX; overriding `wants_refresh` is the escape hatch.

use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    spinner::Spinner,
};
/// docs/0036 D2: the first retry of a failing surface waits one beat,
/// then the wait doubles per consecutive failure, capped — a dead
/// transport settles at ~6 attempts/hour per surface instead of 60.
/// Success zeroes the streak.
pub const BACKOFF_BASE: Duration = Duration::from_secs(60);
pub const BACKOFF_CAP: Duration = Duration::from_secs(600);

/// The wait before the next attempt after `streak` consecutive
/// failures: `min(BACKOFF_BASE * 2^(streak-1), BACKOFF_CAP)`. Streak 0
/// has no wait — only call this for streaks >= 1.
pub fn backoff_delay(streak: u32) -> Duration {
    debug_assert!(streak >= 1);
    let exp = streak.saturating_sub(1).min(20);
    BACKOFF_BASE.saturating_mul(1u32 << exp).min(BACKOFF_CAP)
}

/// A refreshable surface's freshness: what the header shows and the
/// beat gates on (docs/0037 D2).
pub trait Freshness {
    /// Apply-time age of what the surface shows; `None` = nothing
    /// confirmed yet (an honest blank beats a fabricated timestamp).
    fn age(&self) -> Option<Duration>;
    /// A dispatch is in flight with nothing landed yet.
    fn is_loading(&self) -> bool;
    /// The composed dispatch gate (docs/0037 D4): stale-or-failed per
    /// the 0032 policy AND past the 0036 backoff. One call-site per
    /// lane.
    fn wants_refresh(&self, min_age: Duration) -> bool;
}

/// The 0032 refresh policy (D2): a lane wants a re-dispatch iff it is
/// not in flight and either failed (self-heal — failed lanes qualify
/// at any age) or holds landed data at least `min_age` old. Never-
/// dispatched surfaces never qualify, so a background beat cannot
/// fetch a surface the user has not opened.
pub fn standard_needs_refresh(
    age: Option<Duration>,
    loading: bool,
    failed: bool,
    min_age: Duration,
) -> bool {
    !loading && (failed || age.is_some_and(|age| age >= min_age))
}

/// The 0036 backoff gate: a surface whose last attempt failed waits
/// `backoff_delay(fail_streak)` (stale-at-`>=`) before re-firing. A
/// healthy surface (streak 0) never waits; a never-dispatched one has
/// no clock to wait on.
pub fn standard_backoff_elapsed(fail_streak: u32, attempted_at: Option<Instant>) -> bool {
    if fail_streak == 0 {
        return true;
    }
    attempted_at.is_none_or(|t| t.elapsed() >= backoff_delay(fail_streak))
}

/// The composed standard gate — [`standard_needs_refresh`] and
/// [`standard_backoff_elapsed`] in one call. This is what a
/// `Freshness::wants_refresh` impl over a standard record delegates
/// to (docs/0037 D2).
pub fn standard_wants_refresh(
    age: Option<Duration>,
    loading: bool,
    failed: bool,
    fail_streak: u32,
    attempted_at: Option<Instant>,
    min_age: Duration,
) -> bool {
    standard_needs_refresh(age, loading, failed, min_age)
        && standard_backoff_elapsed(fail_streak, attempted_at)
}

/// "synced 3m ago" — `None` when nothing has been confirmed yet
/// (docs/0033 D2/D3): an honest blank beats a fabricated timestamp.
pub fn synced_label(age: Option<Duration>) -> Option<SharedString> {
    let secs = age?.as_secs();
    Some(match secs {
        0..=59 => "synced just now".into(),
        60..=3599 => format!("synced {}m ago", secs / 60).into(),
        3600..=86399 => format!("synced {}h ago", secs / 3600).into(),
        _ => format!("synced {}d ago", secs / 86400).into(),
    })
}

/// GitHub-side relative timestamp for rows — "now" / "3m" / "2h" /
/// "5d"; empty when the payload carries no parseable timestamp.
pub fn relative_age(updated_at: Option<&str>) -> String {
    let Some(ts) = updated_at.and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()) else {
        return String::new();
    };
    let secs = (chrono::Utc::now() - ts.with_timezone(&chrono::Utc))
        .num_seconds()
        .max(0);
    match secs {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}

/// The freshness cluster (docs/0037 D3): synced label (blank while
/// nothing is confirmed), a spinner while the first load is in
/// flight, then the ghost refresh button calling `on_refresh`. The
/// "what else a refresh refetches" quirks live in the caller's
/// closure. Debug selectors: `{id}-synced` on the label,
/// `{id}-refresh` as the button's element id.
pub fn freshness_row<E, F>(
    id: &'static str,
    clock: &F,
    cx: &Context<E>,
    on_refresh: impl Fn(&mut E, &mut Window, &mut Context<E>) + 'static,
) -> AnyElement
where
    E: 'static,
    F: Freshness,
{
    h_flex()
        .gap_2()
        .items_center()
        .when_some(synced_label(clock.age()), |row, label| {
            row.child(
                div()
                    .debug_selector(move || format!("{id}-synced"))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
        })
        .when(clock.is_loading(), |row| row.child(Spinner::new().small()))
        .child(
            Button::new(SharedString::from(format!("{id}-refresh")))
                .ghost()
                .small()
                .icon(IconName::RotateCw)
                .tooltip("Refresh")
                // docs/0049 D1: icon-only refresh affordance —
                // named explicitly.
                .accessibility_label("Refresh")
                // Test/bridge-addressable: the same "{id}-refresh" the
                // element id carries (docs/0010 D10).
                .debug_selector({
                    let id = SharedString::from(format!("{id}-refresh"));
                    move || id.to_string()
                })
                .on_click(cx.listener(move |this, _, window, cx| on_refresh(this, window, cx))),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // Explicit imports: `use super::*` would also pull the
    // `use gpui::*` glob — and gpui exports its own `test`
    // proc-macro at the root, which shadows std's `#[test]` and
    // recurses gpui_macros to a stack overflow (observed 2026-09-17).
    use std::time::{Duration, Instant};

    use super::{
        BACKOFF_CAP, backoff_delay, relative_age, standard_backoff_elapsed, standard_needs_refresh,
        standard_wants_refresh, synced_label,
    };

    #[test]
    fn synced_label_buckets_and_hides_blanks() {
        assert!(synced_label(None).is_none(), "no sync, no label (0033 D2)");
        let label = |secs: u64| synced_label(Some(Duration::from_secs(secs)));
        assert_eq!(label(0).as_deref(), Some("synced just now"));
        assert_eq!(label(59).as_deref(), Some("synced just now"));
        assert_eq!(label(60).as_deref(), Some("synced 1m ago"));
        assert_eq!(label(3599).as_deref(), Some("synced 59m ago"));
        assert_eq!(label(3600).as_deref(), Some("synced 1h ago"));
        assert_eq!(label(90000).as_deref(), Some("synced 1d ago"));
    }

    #[test]
    fn relative_age_buckets() {
        // Moved from the github module with the helper (docs/0037 D1).
        assert_eq!(relative_age(None), "");
        let now = chrono::Utc::now();
        assert_eq!(relative_age(Some(&now.to_rfc3339())), "now");
        let hours_ago = (chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
        assert_eq!(relative_age(Some(&hours_ago)), "2h");
    }

    #[test]
    fn standard_needs_refresh_policy_table() {
        // docs/0032 D2: loading never; None (never dispatched) never;
        // failed at any age; landed at stale-at->=.
        let fresh = Some(Duration::from_secs(0));
        assert!(!standard_needs_refresh(fresh, true, false, Duration::ZERO));
        assert!(!standard_needs_refresh(None, false, false, Duration::ZERO));
        assert!(standard_needs_refresh(
            fresh,
            false,
            true,
            Duration::from_secs(3600)
        ));
        assert!(standard_needs_refresh(fresh, false, false, Duration::ZERO));
        assert!(!standard_needs_refresh(
            fresh,
            false,
            false,
            Duration::from_secs(1)
        ));
    }

    #[test]
    fn standard_backoff_policy_table() {
        // docs/0036 D2: streak -> delay, stale-at->=, clock moves at
        // dispatch only.
        assert_eq!(backoff_delay(1), Duration::from_secs(60));
        assert_eq!(backoff_delay(2), Duration::from_secs(120));
        assert_eq!(backoff_delay(3), Duration::from_secs(240));
        assert_eq!(backoff_delay(4), Duration::from_secs(480));
        assert_eq!(backoff_delay(5), BACKOFF_CAP);
        assert_eq!(backoff_delay(50), BACKOFF_CAP);

        assert!(standard_backoff_elapsed(0, None), "healthy never waits");
        assert!(
            standard_backoff_elapsed(3, None),
            "never-dispatched: no clock to wait on"
        );
        let now = Instant::now();
        assert!(!standard_backoff_elapsed(1, Some(now)));
        assert!(standard_backoff_elapsed(
            1,
            Some(now - Duration::from_secs(60))
        ));
        // The clock only moves at dispatch: the second failure keeps
        // the old stamp — 60s elapsed against a 120s delay.
        assert!(!standard_backoff_elapsed(
            2,
            Some(now - Duration::from_secs(60))
        ));
        assert!(standard_backoff_elapsed(
            2,
            Some(now - Duration::from_secs(120))
        ));
    }

    #[test]
    fn standard_wants_refresh_composes_the_halves() {
        let now = Instant::now();
        // Stale enough, healthy: yes. Fresh: no. Failed but in
        // backoff: no. Failed, backoff elapsed: yes.
        assert!(standard_wants_refresh(
            Some(Duration::from_secs(120)),
            false,
            false,
            0,
            None,
            Duration::from_secs(60)
        ));
        assert!(!standard_wants_refresh(
            Some(Duration::from_secs(0)),
            false,
            false,
            0,
            None,
            Duration::from_secs(60)
        ));
        assert!(!standard_wants_refresh(
            None,
            false,
            true,
            1,
            Some(now),
            Duration::ZERO
        ));
        assert!(standard_wants_refresh(
            None,
            false,
            true,
            1,
            Some(now - Duration::from_secs(60)),
            Duration::ZERO
        ));
    }
}
