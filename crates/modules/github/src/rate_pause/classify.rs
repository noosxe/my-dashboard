//! What a response says about the rate limits (#751). GitHub signals them
//! in more places than the `x-ratelimit-remaining: 0` header the pause
//! used to read:
//!
//! - REST primary: 403 or 429 with `x-ratelimit-remaining: 0`; wait until
//!   `x-ratelimit-reset`.
//! - REST secondary: 403 or 429 and a message naming the secondary limit;
//!   wait `Retry-After` seconds if sent, else until the reset when the
//!   primary budget is empty, else at least a minute.
//! - GraphQL primary: HTTP **200** with an `errors[].type` of
//!   `RATE_LIMITED` and `x-ratelimit-remaining: 0`.
//! - GraphQL secondary: 200 or 403, same wait rule as REST.
//!
//! (docs.github.com, "Rate limits for the REST API" and "Rate limits and
//! query limits for the GraphQL API", read 2026-11-14.) The functions
//! here are pure; `GithubState::note_limits` applies them.

use super::Lane;
use crate::gql::GqlError;
use crate::http::RateLimit;
use crate::usage_chip::local_hhmm;

/// The wait GitHub asks for when a secondary limit names none.
const SECONDARY_FLOOR_SECS: u64 = 60;

/// Why a GraphQL read came back without its data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GqlFailure {
    /// `RATE_LIMITED`, or a message naming a rate limit.
    RateLimited,
    /// `FORBIDDEN`, `INSUFFICIENT_SCOPES` or a SAML refusal: access, not
    /// absence.
    Forbidden,
    /// `NOT_FOUND`: the object isn't there.
    NotFound,
    /// Anything else the server said.
    Other,
}

fn names_rate_limit(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("rate limit") || m.contains("abuse detection")
}

/// The most telling failure in `errors`: a rate limit outranks a refusal,
/// which outranks absence.
pub(crate) fn classify_errors(errors: &[GqlError]) -> Option<GqlFailure> {
    let one = |e: &GqlError| match e.kind.as_deref() {
        Some("RATE_LIMITED") => GqlFailure::RateLimited,
        _ if names_rate_limit(&e.message) => GqlFailure::RateLimited,
        Some("FORBIDDEN" | "INSUFFICIENT_SCOPES") => GqlFailure::Forbidden,
        _ if e.message.to_ascii_lowercase().contains("saml") => GqlFailure::Forbidden,
        Some("NOT_FOUND") => GqlFailure::NotFound,
        _ => GqlFailure::Other,
    };
    let rank = |f: GqlFailure| match f {
        GqlFailure::RateLimited => 3,
        GqlFailure::Forbidden => 2,
        GqlFailure::NotFound => 1,
        GqlFailure::Other => 0,
    };
    errors.iter().map(one).max_by_key(|f| rank(*f))
}

fn is_secondary(body: &str) -> bool {
    body.to_ascii_lowercase().contains("secondary rate limit")
}

/// The epoch until which a response asks the lane to stay quiet, if it
/// asks at all. `errors` is the parsed GraphQL error list of a 200.
pub(crate) fn pause_until(
    status: u16,
    rate: Option<&RateLimit>,
    body: Option<&str>,
    errors: &[GqlError],
    now: u64,
) -> Option<u64> {
    let limited_status = matches!(status, 403 | 429);
    let secondary = body.is_some_and(is_secondary);
    let graphql_limited = classify_errors(errors) == Some(GqlFailure::RateLimited);
    let primary_empty = rate.filter(|r| r.remaining == 0).map(|r| r.reset_epoch);
    let retry = rate
        .and_then(|r| r.retry_after)
        .filter(|_| limited_status || secondary || graphql_limited)
        .map(|secs| now + secs);
    let mut until = [primary_empty, retry].into_iter().flatten().max();
    if until.is_none() && ((limited_status && secondary) || (graphql_limited && secondary)) {
        until = Some(now + SECONDARY_FLOOR_SECS);
    }
    if until.is_none() && graphql_limited {
        // A primary RATE_LIMITED whose headers were lost: the reset is
        // unknowable, so back off for the floor rather than spin.
        until = Some(now + SECONDARY_FLOOR_SECS);
    }
    until
}

/// `GraphQL paused · resumes 15:41` — the chip's words (docs/0082 D13),
/// so the page and the status bar say the same thing.
pub(crate) fn paused_copy(lane: Lane, until: u64) -> String {
    format!("{} paused · resumes {}", lane.name(), local_hhmm(until))
}

#[cfg(test)]
mod tests;
