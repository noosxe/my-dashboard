//! The dispatch hold every page shares (#721, #887): a user-initiated run
//! on a paused lane is answered locally with a 429, without sending. The
//! caller's failure copy (`GithubState::http_failure_copy`) then says the
//! budget is paused and when it resumes.

use gpui::{AsyncApp, Entity};

use crate::service::{FetchSpec, GithubState, SpecResult, run_spec_tracked};

/// The answer a held run returns: a 429 nobody sent.
pub(crate) fn held_result(spec: FetchSpec) -> SpecResult {
    SpecResult {
        kind: spec.kind,
        ident: spec.ident,
        status: 429,
        etag: None,
        rate: None,
        body: None,
        network: None,
        cursor: None,
    }
}

/// Run `spec` tracked, unless its lane is paused: then answer 429 locally.
pub(crate) async fn run_unless_paused(
    state: Entity<GithubState>,
    spec: FetchSpec,
    cx: &mut AsyncApp,
) -> SpecResult {
    if cx.update(|cx| state.read(cx).pause_in_force(spec.kind.lane()).is_some()) {
        log::debug!(
            "{}: held — {} budget paused",
            spec.kind.kind_name(),
            spec.kind.lane().name()
        );
        return held_result(spec);
    }
    run_spec_tracked(state, spec, cx).await
}
