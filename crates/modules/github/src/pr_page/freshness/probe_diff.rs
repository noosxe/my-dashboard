//! docs/0084 D3: the probe's minimal REST parse and the field diff. The
//! probe never renders what it reads; it only asks "did anything the page
//! shows move?", so it keeps the few fields the lane map (D4) branches on
//! and compares them with the previous 200's.

use serde::Deserialize;

/// The fields of `GET /repos/{o}/{r}/pulls/{n}` the probe compares. A
/// missing field reads as its default on both sides, so a body from an
/// older fixture or a trimmed proxy compares fine.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct PrSnapshot {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    merged: Option<bool>,
    #[serde(default)]
    draft: Option<bool>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    head: Head,
    #[serde(default)]
    base: Base,
    #[serde(default)]
    labels: Vec<Named>,
    #[serde(default)]
    requested_reviewers: Vec<Login>,
    #[serde(default)]
    requested_teams: Vec<Slug>,
    #[serde(default)]
    comments: Option<i64>,
    #[serde(default)]
    review_comments: Option<i64>,
    #[serde(default)]
    mergeable: Option<bool>,
    #[serde(default)]
    mergeable_state: Option<String>,
    /// Only whether auto-merge is armed matters, not its settings.
    #[serde(default, deserialize_with = "armed")]
    auto_merge: bool,
    #[serde(default)]
    updated_at: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
struct Head {
    #[serde(default)]
    sha: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
struct Base {
    #[serde(default)]
    sha: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct Named {
    name: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct Login {
    login: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct Slug {
    slug: String,
}

fn armed<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(Option::<serde_json::Value>::deserialize(d)?.is_some_and(|v| !v.is_null()))
}

/// What differs between two snapshots, in the terms of the lane map.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PrChange {
    /// The head commit moved.
    pub(crate) head: bool,
    /// The review-comment count moved.
    pub(crate) review_comments: bool,
    /// Anything else the page shows moved: state, title, labels,
    /// reviewers, counts, mergeability, `updated_at`.
    pub(crate) other: bool,
}

impl PrChange {
    pub(crate) fn any(self) -> bool {
        self.head || self.review_comments || self.other
    }
}

impl PrSnapshot {
    /// Parse a `pulls/{n}` body; `None` for a body that isn't one.
    pub(crate) fn parse(body: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(body).ok()?;
        if !value.is_object() {
            return None;
        }
        let mut snap: Self = serde_json::from_value(value).ok()?;
        // Order never matters to the page: a reshuffle is not a change.
        snap.labels.sort_by(|a, b| a.name.cmp(&b.name));
        snap.requested_reviewers
            .sort_by(|a, b| a.login.cmp(&b.login));
        snap.requested_teams.sort_by(|a, b| a.slug.cmp(&b.slug));
        Some(snap)
    }

    /// The head commit, the key of the check-runs and status probes.
    /// REST has computed `mergeable` (GitHub answers null while it works).
    pub(crate) fn mergeable_known(&self) -> bool {
        self.mergeable.is_some()
    }

    pub(crate) fn head_sha(&self) -> Option<&str> {
        self.head.sha.as_deref()
    }

    pub(crate) fn diff(&self, next: &Self) -> PrChange {
        let head = self.head != next.head;
        let review_comments = self.review_comments != next.review_comments;
        // Everything else, with the two fields above neutralised.
        let mut a = self.clone();
        let mut b = next.clone();
        for s in [&mut a, &mut b] {
            s.head = Head::default();
            s.review_comments = None;
        }
        PrChange {
            head,
            review_comments,
            other: a != b,
        }
    }
}

/// What the Hot tier reads off a `check-runs` body: whether any run is
/// still queued or running. `None` for a body that isn't one.
pub(crate) fn checks_running(body: &str) -> Option<bool> {
    #[derive(Deserialize)]
    struct Runs {
        #[serde(default)]
        check_runs: Vec<Run>,
    }
    #[derive(Deserialize)]
    struct Run {
        #[serde(default)]
        status: Option<String>,
    }
    let runs: Runs = serde_json::from_str(body).ok()?;
    Some(
        runs.check_runs
            .iter()
            .any(|r| r.status.as_deref() != Some("completed")),
    )
}

#[cfg(test)]
mod tests;
