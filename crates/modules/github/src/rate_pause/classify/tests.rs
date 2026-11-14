use super::*;

fn err(kind: Option<&str>, message: &str) -> GqlError {
    GqlError {
        message: message.into(),
        kind: kind.map(str::to_string),
    }
}

fn rate(remaining: u32, retry_after: Option<u64>) -> RateLimit {
    RateLimit {
        limit: 5000,
        remaining,
        reset_epoch: 2_000,
        retry_after,
    }
}

#[test]
fn error_types_classify_by_severity() {
    assert_eq!(classify_errors(&[]), None);
    assert_eq!(
        classify_errors(&[err(Some("NOT_FOUND"), "Could not resolve")]),
        Some(GqlFailure::NotFound)
    );
    assert_eq!(
        classify_errors(&[err(Some("FORBIDDEN"), "Resource not accessible")]),
        Some(GqlFailure::Forbidden)
    );
    assert_eq!(
        classify_errors(&[err(
            None,
            "Resource protected by organization SAML enforcement"
        )]),
        Some(GqlFailure::Forbidden)
    );
    assert_eq!(
        classify_errors(&[
            err(Some("NOT_FOUND"), "gone"),
            err(Some("RATE_LIMITED"), "API rate limit exceeded")
        ]),
        Some(GqlFailure::RateLimited),
        "a rate limit outranks the absence it caused"
    );
    assert_eq!(
        classify_errors(&[err(None, "You have exceeded a secondary rate limit")]),
        Some(GqlFailure::RateLimited)
    );
    assert_eq!(
        classify_errors(&[err(Some("SERVICE_UNAVAILABLE"), "try later")]),
        Some(GqlFailure::Other)
    );
}

#[test]
fn an_empty_primary_budget_pauses_until_its_reset_on_any_status() {
    for status in [200, 403, 429] {
        assert_eq!(
            pause_until(status, Some(&rate(0, None)), None, &[], 1_000),
            Some(2_000),
            "status {status}"
        );
    }
    assert_eq!(
        pause_until(403, Some(&rate(40, None)), Some("denied"), &[], 1_000),
        None
    );
}

#[test]
fn retry_after_pauses_a_secondary_limit_with_budget_left() {
    let body = r#"{"message":"You have exceeded a secondary rate limit."}"#;
    assert_eq!(
        pause_until(403, Some(&rate(4000, Some(90))), Some(body), &[], 1_000),
        Some(1_090)
    );
    // The longer of the two waits wins when both are present.
    assert_eq!(
        pause_until(429, Some(&rate(0, Some(30))), Some(body), &[], 1_000),
        Some(2_000)
    );
}

#[test]
fn a_secondary_limit_without_retry_after_waits_a_minute() {
    let body = r#"{"message":"You have exceeded a secondary rate limit."}"#;
    assert_eq!(
        pause_until(403, Some(&rate(4000, None)), Some(body), &[], 1_000),
        Some(1_060)
    );
}

#[test]
fn retry_after_on_a_plain_response_does_not_pause() {
    assert_eq!(
        pause_until(200, Some(&rate(4000, Some(30))), None, &[], 1_000),
        None
    );
}

#[test]
fn a_graphql_rate_limited_200_pauses() {
    let errors = [err(
        Some("RATE_LIMITED"),
        "API rate limit exceeded for user ID 1.",
    )];
    assert_eq!(
        pause_until(200, Some(&rate(0, None)), None, &errors, 1_000),
        Some(2_000)
    );
    // Headers lost: back off for the floor.
    assert_eq!(pause_until(200, None, None, &errors, 1_000), Some(1_060));
}

#[test]
fn the_copy_matches_the_chip() {
    let copy = paused_copy(Lane::Graphql, 1_800_000_000);
    assert!(copy.starts_with("GraphQL paused · resumes "), "{copy}");
}
