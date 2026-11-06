//! The check log cache and its lifetime (docs/0039 D5, #747). A completed
//! job's log never changes, so the text is cached per job id; which job a
//! row means does change. A re-run gets new job ids and a push a new head,
//! so a row is re-resolved on every expand and the cache only saves the
//! download.

use std::collections::{BTreeMap, BTreeSet};

use gpui::{Context, SharedString};

use crate::gql;
use crate::pr_page::PrDetailPage;
use crate::pr_page::checks::identity::CheckTarget;
use crate::service::Kind;

/// A log tail as it lands in a pane, stamped with the job it belongs to.
#[derive(Clone, Debug)]
pub(crate) struct LandedLog {
    pub job_id: u64,
    pub text: SharedString,
    pub truncated: bool,
}

impl LandedLog {
    pub(crate) fn new(job_id: u64, (text, truncated): (String, bool)) -> Self {
        Self {
            job_id,
            text: text.into(),
            truncated,
        }
    }
}

/// Log tails by job id. An entry stays valid for good; the page drops them
/// on a head change (nothing can resolve to them any more) and with itself.
#[derive(Default)]
pub(crate) struct JobLogCache(BTreeMap<u64, LandedLog>);

impl JobLogCache {
    pub(crate) fn get(&self, job_id: u64) -> Option<LandedLog> {
        self.0.get(&job_id).cloned()
    }

    pub(crate) fn insert(&mut self, log: &LandedLog) {
        self.0.insert(log.job_id, log.clone());
    }

    pub(crate) fn clear(&mut self) {
        self.0.clear();
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, job_id: u64) -> bool {
        self.0.contains_key(&job_id)
    }
}

impl PrDetailPage {
    /// Close the open pane, if any, and disarm its live watch (docs/0076
    /// D1): the generation bump makes the loop's next gate exit without
    /// touching the page, and clearing the arm lets a later expand start a
    /// fresh chain.
    pub(in crate::pr_page) fn disarm_expanded_check(&mut self, cx: &mut Context<Self>) {
        self.check_watch_gen += 1;
        if let Some(key) = self.expanded_check.take() {
            let ident = key.ident(&self.repo, self.number);
            self.state
                .update(cx, |s, _| s.in_flight_clear(&(Kind::JobLog, Some(ident))));
        }
    }

    /// A detail landed. A new head invalidates every log and pane. On the
    /// same head, a row that left the list (a re-run replaced its check
    /// run) takes its pane state with it, and an open pane on it closes
    /// rather than keep watching a run the page no longer lists.
    pub(in crate::pr_page) fn reconcile_check_logs(
        &mut self,
        detail: &gql::PrDetail,
        cx: &mut Context<Self>,
    ) {
        let head = detail.head_oid.as_deref();
        if self.checks_head.as_deref() != head {
            let had_a_head = self.checks_head.is_some();
            self.checks_head = detail.head_oid.clone();
            if had_a_head {
                self.disarm_expanded_check(cx);
                self.check_logs.clear();
                self.job_logs.clear();
                return;
            }
        }
        let rows: BTreeSet<_> = detail
            .checks
            .nodes
            .iter()
            .map(|check| CheckTarget::of(head, check).key)
            .collect();
        if self
            .expanded_check
            .as_ref()
            .is_some_and(|key| !rows.contains(key))
        {
            self.disarm_expanded_check(cx);
        }
        self.check_logs.retain(|key, _| rows.contains(key));
    }
}
