//! docs/0084 D2: which PRs a notifications landing names. The store
//! diffs each landing against the threads it already knew (the first
//! landing after launch seeds silently, as the announce diff does); a
//! page that sees the sequence move asks whether its PR is among the
//! names.

use std::collections::BTreeMap;

use crate::api::{Notification, subject_handoff};
use crate::service::PrKey;

#[derive(Debug, Default)]
pub(crate) struct PrNotices {
    /// Bumps on every landing that names at least one PR.
    pub(crate) seq: u64,
    /// The `updated_at` last seen per PR thread; `None` before the
    /// seeding landing.
    marks: Option<BTreeMap<PrKey, String>>,
    /// The PRs the latest naming landing moved.
    named: Vec<PrKey>,
}

impl PrNotices {
    /// Fold one landing. An unread pull-request thread whose
    /// `updated_at` differs from the one last seen names its PR.
    pub(crate) fn land(&mut self, items: &[Notification]) {
        let mut next = BTreeMap::new();
        let mut named = Vec::new();
        let seeded = self.marks.is_some();
        for n in items.iter().filter(|n| n.unread) {
            let Some((repo, number)) = subject_handoff(&n.subject.kind, n.subject.url.as_deref())
            else {
                continue;
            };
            if n.subject.kind != "PullRequest" {
                continue;
            }
            let key = PrKey::new(repo, number);
            let mark = n.updated_at.clone().unwrap_or_default();
            let moved = self
                .marks
                .as_ref()
                .is_none_or(|old| old.get(&key) != Some(&mark));
            if seeded && moved {
                named.push(key.clone());
            }
            next.insert(key, mark);
        }
        self.marks = Some(next);
        if !named.is_empty() {
            self.named = named;
            self.seq += 1;
        }
    }

    /// Whether the latest naming landing moved this PR.
    pub(crate) fn names(&self, key: &PrKey) -> bool {
        self.named.contains(key)
    }
}

#[cfg(test)]
mod tests;
