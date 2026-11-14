//! docs/0084 D3: the change probe's state machine. Pure: it holds the
//! ETags and the last-seen fields, and turns each reply into what changed.
//! The page (`timer.rs`) sends the requests and acts on the findings.
//!
//! - ETags are held here, keyed by URL, and never persisted: a reopen
//!   starts with a fresh 200. Every request carries the same `Accept`
//!   header (the transport's constant), because the ETag depends on it.
//! - A 304 is "unchanged": it stamps `checked_at` and nothing else.
//! - The first 200 of a URL only seeds the comparison: the page just
//!   landed its data. That holds for the check-runs and status URLs of a
//!   new head too, because the head fan-out refetches the detail.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use super::probe_diff::{PrChange, PrSnapshot, checks_running};

/// One request of a probe tick, in the order they are sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Pr,
    CheckRuns,
    Status,
}

impl Step {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Step::Pr => "pulls",
            Step::CheckRuns => "check-runs",
            Step::Status => "status",
        }
    }
}

/// What came back for one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Reply {
    NotModified,
    Changed { etag: Option<String>, body: String },
    Failed,
}

/// What one tick found, input of the lane map.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Findings {
    pub(crate) pr: PrChange,
    /// A check-runs or status 200 after the first.
    pub(crate) checks: bool,
    /// A request failed or a body didn't parse; the tick stopped there.
    pub(crate) failed: bool,
}

impl Findings {
    pub(crate) fn changed(&self) -> bool {
        self.pr.any() || self.checks
    }
}

#[derive(Debug, Default)]
pub(crate) struct ProbeBook {
    etags: BTreeMap<String, String>,
    /// URLs whose first 200 has landed.
    seen: BTreeSet<String>,
    /// The last `pulls/{n}` 200, the comparison base.
    pr: Option<PrSnapshot>,
    /// The head the check-runs and status probes follow.
    head: Option<String>,
    /// The last probe that confirmed what the page shows (docs/0084 D8).
    pub(crate) checked_at: Option<Instant>,
    checks_running: bool,
    findings: Findings,
}

impl ProbeBook {
    /// The head the page opened over, so the first tick can already ask
    /// for its check runs.
    pub(crate) fn seed_head(&mut self, head: Option<String>) {
        self.head = head;
    }

    /// Whether the first `pulls/{n}` 200 has landed: only then does a
    /// 200 mean "changed".
    pub(crate) fn seeded(&self) -> bool {
        self.pr.is_some()
    }

    /// Whether any check run of the head was queued or running at the
    /// last look (the Hot tier, docs/0084 D5).
    pub(crate) fn checks_running(&self) -> bool {
        self.checks_running
    }

    /// A landed detail carries a head: follow it. The comparison base
    /// stays, so a push the probe hasn't seen yet still reads as one.
    pub(crate) fn follow_head(&mut self, head: Option<&str>) {
        if let Some(head) = head {
            self.head = Some(head.to_string());
        }
    }

    pub(crate) fn begin_tick(&mut self) {
        self.findings = Findings::default();
    }

    /// The tick is over; hand the findings to the lane map.
    pub(crate) fn finish_tick(&mut self) -> Findings {
        std::mem::take(&mut self.findings)
    }

    /// The path of a step, below `repos/{owner}/{repo}`. `None` while the
    /// head is unknown.
    pub(crate) fn path(&self, step: Step, number: i64) -> Option<String> {
        match step {
            Step::Pr => Some(format!("pulls/{number}")),
            Step::CheckRuns => Some(format!(
                "commits/{}/check-runs?per_page=100",
                self.head.as_deref()?
            )),
            Step::Status => Some(format!("commits/{}/status", self.head.as_deref()?)),
        }
    }

    pub(crate) fn etag(&self, url: &str) -> Option<String> {
        self.etags.get(url).cloned()
    }

    /// The step after `after` (`None` = the tick's first). The tick stops
    /// at a failure, and the status request only goes out when the last
    /// detail showed legacy status contexts.
    pub(crate) fn next_step(&self, after: Option<Step>, legacy: bool) -> Option<Step> {
        if self.findings.failed {
            return None;
        }
        let next = match after {
            None => Step::Pr,
            Some(Step::Pr) => Step::CheckRuns,
            Some(Step::CheckRuns) if legacy => Step::Status,
            Some(_) => return None,
        };
        // Without a head there is nothing to ask for past the PR itself.
        (next == Step::Pr || self.head.is_some()).then_some(next)
    }

    /// Fold one reply. `url` is the request's URL, the ETag key.
    pub(crate) fn land(&mut self, step: Step, url: &str, reply: Reply, now: Instant) {
        let (etag, body) = match reply {
            Reply::NotModified => {
                self.checked_at = Some(now);
                return;
            }
            Reply::Failed => {
                self.findings.failed = true;
                return;
            }
            Reply::Changed { etag, body } => (etag, body),
        };
        if let Some(etag) = etag {
            self.etags.insert(url.to_string(), etag);
        }
        match step {
            Step::Pr => self.land_pr(&body, now),
            Step::CheckRuns | Step::Status => {
                if step == Step::CheckRuns {
                    match checks_running(&body) {
                        Some(running) => self.checks_running = running,
                        None => {
                            self.findings.failed = true;
                            return;
                        }
                    }
                }
                if self.seen.insert(url.to_string()) {
                    self.checked_at = Some(now);
                } else {
                    self.findings.checks = true;
                }
            }
        }
    }

    fn land_pr(&mut self, body: &str, now: Instant) {
        let Some(next) = PrSnapshot::parse(body) else {
            self.findings.failed = true;
            return;
        };
        let change = self.pr.as_ref().map(|prev| prev.diff(&next));
        match change {
            // The seeding 200 refetches nothing.
            None => self.checked_at = Some(now),
            Some(change) if !change.any() => self.checked_at = Some(now),
            Some(change) => {
                self.findings.pr = PrChange {
                    head: self.findings.pr.head || change.head,
                    review_comments: self.findings.pr.review_comments || change.review_comments,
                    other: self.findings.pr.other || change.other,
                };
            }
        }
        if let Some(head) = next.head_sha() {
            self.head = Some(head.to_string());
        }
        self.pr = Some(next);
    }
}

#[cfg(test)]
mod tests;
