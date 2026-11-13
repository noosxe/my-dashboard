//! Reading a review-threads response (docs/0030 D1, #745).
//!
//! A GraphQL 200 can carry `data: null` plus `errors` (rate limit, SAML
//! enforcement, a transient fault). That is a failed read, not an empty
//! PR: the page keeps the last good threads and shows the error on the
//! retry row, like its other reads. Only a null node *without* errors
//! degrades to an empty set (docs/0030 D1).

use crate::gql;

/// The threads a body carries, or the message of the failed read.
pub(crate) fn threads_outcome(body: Option<&str>) -> Result<gql::PrThreads, String> {
    let body = body.ok_or_else(|| "Empty response".to_string())?;
    let env = serde_json::from_str::<gql::Envelope<gql::PrThreadsData>>(body)
        .map_err(|_| "Malformed payload".to_string())?;
    for e in &env.errors {
        log::warn!("pr-threads: {}", e.message);
    }
    let first_error = env.errors.first().map(|e| e.message.clone());
    let parsed = env
        .data
        .and_then(|d| d.repository)
        .as_ref()
        .and_then(gql::pr_threads_from_value);
    match (parsed, first_error) {
        (Some(t), _) => Ok(t),
        (None, Some(message)) => Err(format!("GitHub error: {message}")),
        (None, None) => {
            log::warn!("pr-threads: repository/pullRequest null — empty set");
            Ok(gql::PrThreads::default())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_null_with_errors_fails_the_read() {
        // Before the fix this landed `PrThreads::default()` over the
        // good data.
        let body = r#"{"data":null,"errors":[{"message":"API rate limit exceeded"}]}"#;
        let err = threads_outcome(Some(body)).unwrap_err();
        assert!(err.contains("API rate limit exceeded"), "{err}");
    }

    #[test]
    fn a_null_pull_request_with_errors_fails_the_read() {
        let body = r#"{"data":{"repository":{"pullRequest":null}},
            "errors":[{"message":"Resource protected by SAML enforcement"}]}"#;
        assert!(threads_outcome(Some(body)).is_err());
    }

    #[test]
    fn a_null_node_without_errors_stays_an_empty_set() {
        let body = r#"{"data":{"repository":{"pullRequest":null}}}"#;
        assert!(threads_outcome(Some(body)).unwrap().nodes.is_empty());
    }

    #[test]
    fn unreadable_bodies_fail_the_read() {
        assert!(threads_outcome(None).is_err());
        assert!(threads_outcome(Some("<html>")).is_err());
    }

    #[test]
    fn partial_errors_with_data_still_land() {
        let body = r#"{"data":{"repository":{"pullRequest":{"id":"PR",
            "reviewThreads":{"totalCount":0,"nodes":[]}}}},
            "errors":[{"message":"one field failed"}]}"#;
        assert!(threads_outcome(Some(body)).is_ok());
    }
}
