//! What the PR page header claims about the PR (#763, #764): the state
//! badge, the "updated" age and the open-on-GitHub URL, derived from
//! whatever the page has so far — the detail, the store's summary, or
//! only the `repo` + `number` it was opened with.
//!
//! - The badge never guesses: with neither detail nor summary there is
//!   no tone, so no badge renders until one of them provides a state.
//! - The age is always the PR's *updated* time, labelled ("updated 1d
//!   ago"), so landing the detail never swaps the figure for another
//!   one. When both sources carry a time the newer wins.
//! - The URL is the source's when known, else built from `repo` and
//!   `number`, so the external-link button is never empty.

use super::{PrStateTone, pr_state_tone};
use crate::gql::{PrDetail, PullRequest};
use dashboard_core::freshness::relative_age;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct HeaderMeta {
    /// `None` until the detail or the summary says what the PR is.
    pub tone: Option<PrStateTone>,
    /// "updated 1d ago", or `None` when no timestamp is known.
    pub updated: Option<String>,
    pub url: String,
}

pub(crate) fn header_meta(
    detail: Option<&PrDetail>,
    summary: Option<&PullRequest>,
    repo: &str,
    number: i64,
) -> HeaderMeta {
    // The summary only lists OPEN PRs; a draft stays a draft until the
    // detail payload says otherwise (docs/0073 D4).
    let tone = match (detail, summary) {
        (Some(d), _) => Some(pr_state_tone(d.state.as_deref(), d.draft)),
        (None, Some(s)) if s.draft => Some(PrStateTone::Draft),
        (None, Some(_)) => Some(PrStateTone::Open),
        (None, None) => None,
    };
    let updated = newest(
        detail.and_then(|d| d.updated_at.as_deref()),
        summary.and_then(|s| s.updated_at.as_deref()),
    )
    .map(|ts| relative_age(Some(ts)))
    .filter(|age| !age.is_empty())
    .map(|age| match age.as_str() {
        "now" => "updated just now".to_string(),
        _ => format!("updated {age} ago"),
    });
    let url = [
        detail.map(|d| d.url.as_str()),
        summary.map(|s| s.url.as_str()),
    ]
    .into_iter()
    .flatten()
    .find(|u| !u.is_empty())
    .map_or_else(
        || format!("https://github.com/{repo}/pull/{number}"),
        str::to_string,
    );
    HeaderMeta { tone, updated, url }
}

/// The later of two RFC 3339 timestamps; an unparseable one loses.
fn newest<'a>(a: Option<&'a str>, b: Option<&'a str>) -> Option<&'a str> {
    let parse = |s: &str| chrono::DateTime::parse_from_rfc3339(s).ok();
    match (
        a.and_then(parse).map(|t| (t, a)),
        b.and_then(parse).map(|t| (t, b)),
    ) {
        (Some(x), Some(y)) => Some(if y.0 > x.0 { y.1? } else { x.1? }),
        (Some(x), None) => x.1,
        (None, y) => y.and_then(|y| y.1),
    }
}

#[cfg(test)]
mod tests;
