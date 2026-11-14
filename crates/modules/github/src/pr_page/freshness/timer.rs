//! docs/0084 D3/D5: the page's watch loop. One timer per page, armed
//! after each probe with the next interval, dropped with the page. A tick
//! probes only while the page is *watched* (the open sub-page, the module
//! on screen, connected, REST not paused, the PR open) and skips while
//! any of the page's lanes is in flight.
//!
//! The interval is `cadence::Cadence::next` (D5); the budget guard
//! (`budget`) and the queue rule (`queue`, D6) feed it. This file keeps
//! the loop: gather the inputs, ask, arm the one timer, act on findings.

use std::time::{Duration, Instant};

use gpui::{Context, Task};

use super::budget::Guard;
use super::cadence::{Cadence, Next, SHOWN, Stop};
use super::lane_map::{self, LanePlan};
use super::probe::{Findings, ProbeBook, Reply, Step};
use super::queue::{QueueRefetch, Why};
use crate::api;
use crate::gql;
use crate::pr_page::{PrDetailPage, split_repo};
use crate::rate_pause::Lane;
use crate::service::{FetchSpec, Kind, SpecResult, run_spec_tracked};
use crate::usage::{Origin, Surface, Trigger};

/// The first probe after the page opens only seeds the comparison, so it
/// goes out soon.
pub(crate) const FIRST_PROBE: Duration = Duration::from_secs(2);
/// Checks-only changes refetch the detail at most this often (Q4).
pub(crate) const CHECKS_GAP: Duration = Duration::from_secs(20);

/// What a checks-only change does (Q4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refetch {
    Now,
    /// Too soon: schedule it after this wait. Never dropped.
    After(Duration),
}

/// The 20 s spacing of probe-caused detail reads for checks-only changes.
#[derive(Debug, Default)]
pub(crate) struct Coalesce {
    /// When a probe last caused a detail read.
    last: Option<Instant>,
    /// A checks change waits for its slot.
    pub(crate) pending: bool,
}

impl Coalesce {
    pub(crate) fn checks_change(&mut self, now: Instant) -> Refetch {
        match self.last {
            Some(t) if now.saturating_duration_since(t) < CHECKS_GAP => {
                self.pending = true;
                Refetch::After(CHECKS_GAP - now.saturating_duration_since(t))
            }
            _ => Refetch::Now,
        }
    }

    /// A probe-caused detail read went out; it covers anything pending.
    pub(crate) fn dispatched(&mut self, now: Instant) {
        self.last = Some(now);
        self.pending = false;
    }
}

/// Everything the page keeps for the watch loop.
pub(crate) struct Watch {
    pub(crate) book: ProbeBook,
    /// The module owns the screen (the nav gate). The page is created
    /// while it does.
    module_shown: bool,
    /// A tick's requests are on their way.
    in_tick: bool,
    /// The one timer, armed after each probe.
    probe_timer: Option<Task<()>>,
    /// The scheduled checks-only refetch (Q4), if one is waiting.
    flush_timer: Option<Task<()>>,
    pub(crate) coalesce: Coalesce,
    /// A change landed at this instant (the Hot tier's memory).
    changed_at: Option<Instant>,
    /// The last change, refocus, Refresh or the open: what Settled counts
    /// quiet time from.
    quiet_since: Instant,
    /// The window went inactive at this instant (lazily noticed).
    unfocused_since: Option<Instant>,
    /// D7: the rolling-hour counters and the breach state.
    pub(crate) guard: Guard,
    /// D6: the spacing of refetches no change asked for.
    queue: QueueRefetch,
    /// `X-Poll-Interval`, the last one GitHub sent.
    poll_floor: Option<Duration>,
    /// The last failure was a rate limit.
    limited: bool,
    fail_streak: u32,
    /// The loop has been started.
    running: bool,
    /// The delay the timer was last armed with.
    armed_for: Duration,
    /// Why the page isn't probing right now, for the header label.
    stop: Option<Stop>,
    /// Repaints the header while its label counts seconds.
    pub(super) label_tick: Option<Task<()>>,
    /// Failed checks in the last landed detail: a rise means a run
    /// stopped to wait for approval (docs/0066).
    pub(crate) last_failed: Option<u32>,
    /// Tests move the loop's clock forward by this much: the fake
    /// executor clock doesn't move `Instant`.
    #[cfg(test)]
    pub(crate) skew: Duration,
}

impl Watch {
    /// The loop's clock.
    pub(crate) fn now(&self) -> Instant {
        #[cfg(test)]
        return Instant::now() + self.skew;
        #[cfg(not(test))]
        Instant::now()
    }
}

impl Default for Watch {
    fn default() -> Self {
        Self {
            book: ProbeBook::default(),
            module_shown: true,
            in_tick: false,
            probe_timer: None,
            flush_timer: None,
            coalesce: Coalesce::default(),
            changed_at: None,
            quiet_since: Instant::now(),
            unfocused_since: None,
            guard: Guard::default(),
            queue: QueueRefetch::default(),
            poll_floor: None,
            limited: false,
            fail_streak: 0,
            running: false,
            armed_for: Duration::ZERO,
            stop: None,
            label_tick: None,
            last_failed: None,
            #[cfg(test)]
            skew: Duration::ZERO,
        }
    }
}

impl PrDetailPage {
    /// The page is built: seed the probe from the document it opened
    /// over and arm the first tick.
    pub(in crate::pr_page) fn start_watch(&mut self, cx: &mut Context<Self>) {
        let seed = self
            .detail(cx)
            .map(|d| (d.head_oid.clone(), d.checks.counts.failed));
        if let Some((head, failed)) = seed {
            self.watch.book.seed_head(head);
            self.watch.last_failed = Some(failed);
        }
        // The existing page tests advance the fake clock over mocks that
        // script exact request sequences; only the probe's own tests
        // start the loop (`start_probing`).
        // The page just read the detail: the queue rule's 30 s spacing
        // starts now, so the seed tick doesn't read it again.
        self.watch.queue.dispatched(self.watch.now());
        if !cfg!(test) {
            self.watch.running = true;
            self.arm_probe(FIRST_PROBE, cx);
            self.start_label_tick(cx);
        }
    }

    /// Start the loop in a test.
    #[cfg(test)]
    pub(crate) fn start_probing(&mut self, cx: &mut Context<Self>) {
        self.watch.running = true;
        self.arm_probe(FIRST_PROBE, cx);
    }

    /// The nav gate moved: the module is on screen or it isn't.
    pub(crate) fn set_module_shown(&mut self, shown: bool) {
        self.watch.module_shown = shown;
    }

    /// A landed detail: the probe follows its head, and the sweep rule
    /// sees its failed-check count. Returns the count before.
    pub(in crate::pr_page) fn watch_note_landed(&mut self, detail: &gql::PrDetail) -> Option<u32> {
        self.watch.book.follow_head(detail.head_oid.as_deref());
        self.watch.last_failed.replace(detail.checks.counts.failed)
    }

    /// docs/0084 D5: watched = the open sub-page (this page exists), the
    /// module on screen, connected, REST not paused, and the PR open.
    pub(crate) fn watched(&self, cx: &gpui::App) -> bool {
        let state = self.state.read(cx);
        self.watch.module_shown
            && state.auth.connected()
            && !state.gated_for(Lane::Rest)
            && self
                .detail(cx)
                .is_some_and(|d| d.state.as_deref() == Some("OPEN"))
    }

    /// Any lane of the page is mid-read: a probe would race it (F7).
    fn lanes_busy(&self, cx: &gpui::App) -> bool {
        let detail = self
            .state
            .read(cx)
            .pr_entry(&self.repo, self.number)
            .is_some_and(|e| e.books.in_flight);
        detail
            || self.files.in_flight
            || self.threads.in_flight
            || self.gated.in_flight
            || self.watch.in_tick
    }

    /// The probe's inputs as they stand. It also notes when the window
    /// went inactive, since nothing else tells the page.
    fn cadence(&mut self, cx: &gpui::App) -> Cadence {
        let now = self.watch.now();
        let window_active = self.state.read(cx).window_active;
        if window_active {
            self.watch.unfocused_since = None;
        } else {
            self.watch.unfocused_since.get_or_insert(now);
        }
        let guard = self.watch.guard.state(now);
        let d = self.detail(cx);
        Cadence {
            window_active,
            unfocused_for: self
                .watch
                .unfocused_since
                .map_or(Duration::ZERO, |t| now.saturating_duration_since(t)),
            open: d.is_some_and(|d| d.state.as_deref() == Some("OPEN")),
            checks_running: self.watch.book.checks_running()
                || d.is_some_and(|d| d.checks.counts.running > 0),
            mergeable_unknown: d.is_some_and(|d| d.mergeable.as_deref() == Some("UNKNOWN")),
            queued: d.is_some_and(|d| d.queueing.in_merge_queue),
            since_change: self
                .watch
                .changed_at
                .map(|t| now.saturating_duration_since(t)),
            quiet_for: now.saturating_duration_since(self.watch.quiet_since),
            guard,
            usage: crate::usage_chip::worst_state(self.state.read(cx)),
            poll_floor: self.watch.poll_floor,
            fail_streak: self.watch.fail_streak,
            limited: self.watch.limited,
        }
    }

    /// The next delay, or `SHOWN` as the local re-check while the page
    /// sends nothing: whatever lifted the stop resumes it on that beat.
    fn next_delay(&mut self, cx: &gpui::App) -> (Duration, Option<Stop>) {
        match self.cadence(cx).next() {
            Next::After(d) => (d, None),
            Next::Idle(why) => (SHOWN, Some(why)),
        }
    }

    /// Why the page isn't probing, when it isn't (docs/0084 D8: the
    /// header label reads it).
    pub(crate) fn probe_stop(&self) -> Option<Stop> {
        self.watch.stop
    }

    /// A refocus or Refresh: whoever is looking gets a fresh start. The
    /// guard's stop, the unattended cut-off and Settled all lift, and a
    /// timer armed for a long wait comes forward to the tier's interval.
    pub(in crate::pr_page) fn watch_resume(&mut self, cx: &mut Context<Self>) {
        self.watch.guard.resume();
        self.watch.quiet_since = self.watch.now();
        self.watch.unfocused_since = None;
        self.watch.fail_streak = 0;
        self.watch.limited = false;
        if !self.watch.running {
            return;
        }
        let (want, stop) = self.next_delay(cx);
        self.watch.stop = stop;
        if self.watch.probe_timer.is_some() && self.watch.armed_for > want {
            self.arm_probe(want, cx);
        }
    }

    fn has_legacy_contexts(&self, cx: &gpui::App) -> bool {
        self.detail(cx).is_some_and(|d| {
            d.checks
                .nodes
                .iter()
                .any(|c| c.origin == gql::CheckOrigin::StatusContext)
        })
    }

    fn arm_probe(&mut self, delay: Duration, cx: &mut Context<Self>) {
        self.watch.armed_for = delay;
        self.watch.probe_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |page, cx| page.watch_wake(cx));
        }));
    }

    /// The timer fired. An unwatched or busy page sends nothing and waits
    /// for the next interval.
    fn watch_wake(&mut self, cx: &mut Context<Self>) {
        self.watch.probe_timer = None;
        let watched = self.watched(cx);
        let (next, stop) = self.next_delay(cx);
        self.watch.stop = stop;
        if watched && stop.is_none() && !self.lanes_busy(cx) {
            self.run_probe(cx);
        } else if watched && stop.is_none() && !self.watch.book.seeded() {
            // The open's own read is still flying: the seed isn't worth a
            // whole interval's wait.
            self.arm_probe(FIRST_PROBE, cx);
        } else {
            self.arm_probe(next, cx);
        }
    }

    /// One immediate probe, outside the schedule (docs/0084 D2: a
    /// notification names this PR). Only when the comparison is seeded,
    /// since a 200 before that means nothing; the caller falls back to a
    /// refetch otherwise.
    pub(in crate::pr_page) fn probe_now(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.watch.book.seeded() || !self.watched(cx) || self.watch.in_tick {
            return false;
        }
        // A stopped page leaves the notification to the refetch.
        if self.next_delay(cx).1.is_some() {
            return false;
        }
        self.watch.probe_timer = None;
        self.run_probe(cx);
        true
    }

    fn run_probe(&mut self, cx: &mut Context<Self>) {
        self.watch.in_tick = true;
        self.watch.book.begin_tick();
        self.probe_step(None, cx);
    }

    fn probe_step(&mut self, after: Option<Step>, cx: &mut Context<Self>) {
        let legacy = self.has_legacy_contexts(cx);
        let Some(step) = self.watch.book.next_step(after, legacy) else {
            self.finish_probe(cx);
            return;
        };
        let Some(path) = self.watch.book.path(step, self.number) else {
            self.finish_probe(cx);
            return;
        };
        let (owner, name) = split_repo(&self.repo);
        let url = api::repo_rest_url(owner, name, &path);
        let spec = FetchSpec {
            origin: Origin::new(Surface::PrPage, Trigger::Watch),
            kind: Kind::PrProbe,
            etag: self.watch.book.etag(&url),
            url: url.clone(),
            ident: Some(format!("{}#{}:{}", self.repo, self.number, step.label())),
            max_pages: 1,
            variables: None,
        };
        let state = self.state.clone();
        cx.spawn(async move |this, cx| {
            let result = run_spec_tracked(state, spec, cx).await;
            let _ = this.update(cx, |page, cx| page.probe_landed(step, &url, result, cx));
        })
        .detach();
    }

    fn probe_landed(&mut self, step: Step, url: &str, result: SpecResult, cx: &mut Context<Self>) {
        if let Some(rate) = &result.rate {
            self.watch.poll_floor = rate.poll_interval.map(Duration::from_secs);
        }
        let reply = if result.network.is_some() {
            Reply::Failed
        } else if result.status == 304 {
            Reply::NotModified
        } else if result.status == 200 {
            // A 304 is free; a 200 counts against the page's ceiling.
            self.watch.guard.spend_rest(self.watch.now(), 1);
            Reply::Changed {
                etag: result.etag.clone(),
                body: result.body.clone().unwrap_or_default(),
            }
        } else {
            // 401/403/404/429…: the shared handling arms the pause or the
            // auth gate; the probe just stops this tick. A limit also
            // makes the next wait at least a minute (D5).
            self.watch.limited = matches!(result.status, 403 | 429);
            self.state.update(cx, |s, cx| {
                s.apply(result);
                cx.notify();
            });
            Reply::Failed
        };
        self.watch.book.land(step, url, reply, Instant::now());
        self.probe_step(Some(step), cx);
    }

    /// The tick is over: act on the findings, then arm the next one. A
    /// change just made the page Hot, so the interval is read after.
    fn finish_probe(&mut self, cx: &mut Context<Self>) {
        self.watch.in_tick = false;
        let findings = self.watch.book.finish_tick();
        if findings.failed {
            self.watch.fail_streak += 1;
        } else {
            self.watch.fail_streak = 0;
            self.watch.limited = false;
        }
        self.act_on(findings, cx);
        let (next, stop) = self.next_delay(cx);
        self.watch.stop = stop;
        self.arm_probe(next, cx);
        cx.notify();
    }

    fn act_on(&mut self, findings: Findings, cx: &mut Context<Self>) {
        if findings.changed() {
            let now = self.watch.now();
            self.watch.changed_at = Some(now);
            self.watch.quiet_since = now;
            let plan = lane_map::plan(&findings, self.threads.data().is_some());
            self.apply_plan(plan, cx);
        } else if self
            .watch
            .queue
            .due(self.watch.now(), self.queue_why(cx), !findings.failed)
        {
            self.dispatch_changed_detail(cx);
        }
    }

    /// D6: the facts a quiet tick still has to read the detail for.
    fn queue_why(&self, cx: &gpui::App) -> Option<Why> {
        let d = self.detail(cx)?;
        if d.queueing.in_merge_queue {
            Some(Why::Queued)
        } else if d.mergeable.as_deref() == Some("UNKNOWN") && self.watch.book.mergeable_known() {
            Some(Why::StaleUnknown)
        } else {
            None
        }
    }

    fn apply_plan(&mut self, plan: LanePlan, cx: &mut Context<Self>) {
        let origin = Origin::new(Surface::PrPage, Trigger::Changed);
        if plan.detail {
            if plan.checks_only {
                match self.watch.coalesce.checks_change(self.watch.now()) {
                    Refetch::Now => self.dispatch_changed_detail(cx),
                    Refetch::After(wait) => self.arm_flush(wait, cx),
                }
            } else {
                self.dispatch_changed_detail(cx);
            }
        }
        if plan.threads && !self.state.read(cx).gated_for(Kind::PrThreads.lane()) {
            self.watch.guard.spend_points(self.watch.now(), 1);
            self.fetch_threads(origin, cx);
        }
    }

    fn dispatch_changed_detail(&mut self, cx: &mut Context<Self>) {
        let now = self.watch.now();
        self.watch.coalesce.dispatched(now);
        self.watch.queue.dispatched(now);
        self.watch.guard.spend_points(now, 1);
        self.watch.flush_timer = None;
        self.fetch_detail(Origin::new(Surface::PrPage, Trigger::Changed), cx);
    }

    fn arm_flush(&mut self, wait: Duration, cx: &mut Context<Self>) {
        self.watch.flush_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |page, cx| page.flush_checks_change(cx));
        }));
    }

    /// The scheduled checks-only refetch is due.
    fn flush_checks_change(&mut self, cx: &mut Context<Self>) {
        self.watch.flush_timer = None;
        if !self.watch.coalesce.pending {
            return;
        }
        if self.watched(cx) {
            self.dispatch_changed_detail(cx);
        } else {
            // Not watched any more: refocus and the events refetch.
            self.watch.coalesce.pending = false;
        }
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tiers;
