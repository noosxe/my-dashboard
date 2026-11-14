//! Arming and reading the pause from a store's point of view (#721, #751).

use super::{Lane, classify_errors, pause_until, paused_copy};
use crate::gql;
use crate::service::{GithubState, Kind, SpecResult};
use crate::usage::budget_now;

impl GithubState {
    /// Arm the pause from the limits a finished run reports: the primary
    /// header, `Retry-After`, a secondary limit's message, and a GraphQL
    /// `RATE_LIMITED` error on a 200. Page-owned runs never pass through
    /// `apply`, so this is the one place their 200s are read (#751).
    pub(crate) fn note_limits(&mut self, result: &SpecResult) {
        if result.network.is_some() || result.status == 304 {
            return;
        }
        let lane = result.kind.lane();
        let body = result.body.as_deref();
        let parse = body.is_some_and(|b| {
            b.contains("RATE_LIMITED")
                || (lane == Lane::Graphql && result.status == 200 && b.contains("ate limit"))
        });
        let errors = match body {
            Some(b) if parse => serde_json::from_str::<gql::Envelope<serde_json::Value>>(b)
                .map(|e| e.errors)
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let Some(until) = pause_until(
            result.status,
            result.rate.as_ref(),
            body,
            &errors,
            budget_now(),
        ) else {
            return;
        };
        if self.rate_limited_until.in_force(lane, budget_now()) >= Some(until) {
            return;
        }
        self.pause_lane(
            lane,
            until,
            format_args!("{}: rate-limited", result.kind.kind_name()),
        );
    }

    /// The lane's pause deadline while it is in force.
    pub(crate) fn pause_in_force(&self, lane: Lane) -> Option<u64> {
        self.rate_limited_until.in_force(lane, budget_now())
    }

    /// `GraphQL paused · resumes 15:41` while `lane` is paused.
    pub(crate) fn paused_note(&self, lane: Lane) -> Option<String> {
        self.pause_in_force(lane)
            .map(|until| paused_copy(lane, until))
    }

    /// Every paused lane's note, joined (`REST paused … · GraphQL paused …`).
    pub(crate) fn paused_notes(&self) -> Option<String> {
        let notes: Vec<String> = Lane::ALL
            .into_iter()
            .filter_map(|lane| self.paused_note(lane))
            .collect();
        (!notes.is_empty()).then(|| notes.join(" · "))
    }

    /// The copy for a non-200 answer: the pause note when the answer armed
    /// (or ran into) its lane's pause, else the bare status.
    pub(crate) fn http_failure_copy(&self, kind: Kind, status: u16) -> String {
        self.paused_note(kind.lane())
            .unwrap_or_else(|| format!("GitHub answered HTTP {status}"))
    }
}

/// The page-facing copy for a null `repository` / `pullRequest` read.
pub(crate) fn null_read_copy(errors: &[gql::GqlError], state: &GithubState, lane: Lane) -> String {
    use super::GqlFailure::*;
    match classify_errors(errors) {
        Some(RateLimited) => state
            .paused_note(lane)
            .unwrap_or_else(|| "GitHub rate limit reached — try again shortly".into()),
        Some(Forbidden) => {
            "GitHub refused access to this PR (permissions, or SAML enforcement)".into()
        }
        Some(Other) => match errors.first() {
            Some(e) => format!("GitHub returned an error: {}", e.message),
            None => "PR not found (removed, or access lost)".into(),
        },
        Some(NotFound) | None => "PR not found (removed, or access lost)".into(),
    }
}
