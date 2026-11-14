//! docs/0084 D8: what the PR header says about the age of its data.
//!
//! - While the page is watched and a probe has confirmed the data, the
//!   label reads "checked 15s ago": the time of the last probe that
//!   confirmed what the page shows (a landing confirms it too, so the
//!   newer of the two counts), even when nothing changed.
//! - Otherwise it is today's landed age ("synced 3m ago"). That covers a
//!   closed PR, an unattended app, offline and a usage stop.
//! - When the budget guard stopped the probe the landed age adds the
//!   reason: "synced 3m ago · auto-refresh paused (budget)".
//!
//! The text is a pure function of [`LabelInput`]; the page feeds it.

use std::time::Duration;

use dashboard_core::freshness::synced_label;
use gpui::{Context, SharedString};

use super::cadence::Stop;
use crate::pr_page::PrDetailPage;

/// How often a watched header repaints so "checked Ns ago" keeps up.
pub(crate) const TICK: Duration = Duration::from_secs(5);

/// Seconds shown in steps of the repaint tick, so the figure never
/// reads fresher or staler than the paint that shows it.
const STEP: u64 = TICK.as_secs();

/// What the label depends on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LabelInput {
    /// The page is watched (docs/0084 D5): shown, connected, REST not
    /// paused, PR open.
    pub(crate) watched: bool,
    /// Why the probe isn't running, when it isn't.
    pub(crate) stop: Option<Stop>,
    /// Age of the last probe that confirmed the data.
    pub(crate) checked: Option<Duration>,
    /// Age of the last landing of the detail.
    pub(crate) landed: Option<Duration>,
}

/// The label text, or `None` while nothing is confirmed yet.
pub(crate) fn age_label(i: LabelInput) -> Option<String> {
    let probing = i.watched && i.stop.is_none();
    if probing && let Some(checked) = i.checked {
        // A landing confirms the data as well as a probe does.
        let age = i.landed.map_or(checked, |l| l.min(checked));
        return Some(checked_text(age));
    }
    let landed = synced_label(i.landed)?;
    Some(match i.stop {
        Some(Stop::Guard) => format!("{landed} · {GUARD_REASON}"),
        _ => landed.to_string(),
    })
}

/// The guard's reason, beside the landed age.
pub(crate) const GUARD_REASON: &str = "auto-refresh paused (budget)";

fn checked_text(age: Duration) -> String {
    let secs = age.as_secs();
    match secs {
        0..STEP => "checked just now".into(),
        STEP..60 => format!("checked {}s ago", secs / STEP * STEP),
        60..3600 => format!("checked {}m ago", secs / 60),
        3600..86400 => format!("checked {}h ago", secs / 3600),
        _ => format!("checked {}d ago", secs / 86400),
    }
}

impl PrDetailPage {
    /// The header's age label right now.
    pub(in crate::pr_page) fn age_label(&self, cx: &gpui::App) -> Option<SharedString> {
        use dashboard_core::freshness::Freshness as _;
        let entry = self.state.read(cx).pr_entry(&self.repo, self.number);
        age_label(LabelInput {
            watched: self.watched(cx),
            stop: self.probe_stop(),
            checked: self
                .watch
                .book
                .checked_at
                .map(|t| self.watch.now().saturating_duration_since(t)),
            landed: crate::service::DetailClock(entry).age(),
        })
        .map(SharedString::from)
    }

    /// Repaint the header every [`TICK`] while its label counts seconds.
    pub(in crate::pr_page) fn start_label_tick(&mut self, cx: &mut Context<Self>) {
        self.watch.label_tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let live = this.update(cx, |page, cx| {
                    if page.watch.book.checked_at.is_some() && page.watched(cx) {
                        cx.notify();
                    }
                });
                if live.is_err() {
                    break;
                }
            }
        }));
    }
}

#[cfg(test)]
mod tests;
