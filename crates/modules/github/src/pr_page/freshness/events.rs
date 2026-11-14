//! docs/0084 D2: the tab-entry floor, the module re-shown edge and the
//! notification landing that names this PR.

use dashboard_core::freshness::Freshness as _;
use gpui::Context;

use super::{RESHOWN_FLOOR, TAB_ENTRY_FLOOR};
use crate::pr_page::{PrDetailPage, PrTab};
use crate::service::{DetailClock, Kind, PrKey};
use crate::usage::{Origin, Surface, Trigger};

/// Which lanes entering a tab refetches (docs/0084 D2): Files reads its
/// own two lanes; the other tabs render the detail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TabLanes {
    pub(crate) detail: bool,
    pub(crate) files: bool,
    pub(crate) threads: bool,
}

pub(crate) fn tab_lanes(tab: PrTab) -> TabLanes {
    let files = tab == PrTab::Files;
    TabLanes {
        detail: !files,
        files,
        threads: files,
    }
}

impl PrDetailPage {
    /// `tab` was opened. The first entry to Files starts its lanes (0016
    /// D8 / 0030 D1); a later entry refetches a loaded lane older than
    /// [`TAB_ENTRY_FLOOR`]. Re-selecting the open tab is not an entry.
    pub(in crate::pr_page) fn enter_tab(
        &mut self,
        tab: PrTab,
        changed: bool,
        cx: &mut Context<Self>,
    ) {
        let lanes = tab_lanes(tab);
        if lanes.files {
            let first = Origin::new(Surface::PrPage, Trigger::Expand);
            if self.files.is_idle() {
                self.fetch_files(first, cx);
            }
            if self.threads.is_idle() {
                self.fetch_threads(first, cx);
            }
        }
        if !changed {
            return;
        }
        let origin = Origin::new(Surface::PrPage, Trigger::TabEntry);
        let open = |kind: Kind, page: &Self, cx: &Context<Self>| {
            !page.state.read(cx).gated_for(kind.lane())
        };
        if lanes.detail {
            let state = self.state.read(cx);
            let entry = state.pr_entry(&self.repo, self.number);
            let loaded = entry.is_some_and(|e| e.detail.is_some());
            if loaded
                && DetailClock(entry).wants_refresh(TAB_ENTRY_FLOOR)
                && open(Kind::PrDetail, self, cx)
            {
                self.fetch_detail(origin, cx);
            }
        }
        if lanes.files
            && self.files.data().is_some()
            && self.files.wants_refresh(TAB_ENTRY_FLOOR)
            && open(Kind::PrFiles, self, cx)
        {
            self.fetch_files(origin, cx);
        }
        if lanes.threads
            && self.threads.data().is_some()
            && self.threads.wants_refresh(TAB_ENTRY_FLOOR)
            && open(Kind::PrThreads, self, cx)
        {
            self.fetch_threads(origin, cx);
        }
    }

    /// The module became the visible one again with this page open: the
    /// refocus lanes at the same floor.
    pub(crate) fn module_reshown(&mut self, cx: &mut Context<Self>) {
        self.refresh_if_stale(RESHOWN_FLOOR, cx);
    }

    /// The store notified: when a notifications landing named this PR
    /// since the last look, send one immediate probe (docs/0084 D2). A page
    /// that can't probe yet, because it isn't watched or the comparison
    /// isn't seeded, refetches the detail instead.
    pub(in crate::pr_page) fn check_notification_landing(&mut self, cx: &mut Context<Self>) {
        let key = PrKey::new(self.repo.clone(), self.number);
        let (seq, named) = {
            let s = self.state.read(cx);
            (s.pr_notices.seq, s.pr_notices.names(&key))
        };
        if seq == self.notice_seen {
            return;
        }
        self.notice_seen = seq;
        if named && self.probe_now(cx) {
            return;
        }
        if named && !self.state.read(cx).gated_for(Kind::PrDetail.lane()) {
            self.fetch_detail(
                Origin::new(Surface::PrPage, Trigger::NotificationLanding),
                cx,
            );
        }
    }
}

#[cfg(test)]
mod tests;
