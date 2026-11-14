//! REST ETags and 304s for the fixture transport (docs/0084 §8, F2).
//!
//! The ETag of a REST answer is a hash of the bytes served, so it is the
//! same for the same body in any process and changes exactly when the
//! body does (a generation bump to a different file, the next element of
//! a sequence). A GET whose `If-None-Match` names it answers 304 with an
//! empty body. GraphQL never reaches this code: GitHub has no
//! conditional requests there (docs/0084 F2), so a POST always answers
//! 200 with a body.

/// FNV-1a, 64 bit: a hash that is specified, so an ETag in a committed
/// receipt or a doc stays valid across toolchains.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// The weak validator GitHub sends (`W/"<hex>"`) for these bytes.
pub(crate) fn etag_of(body: &str) -> String {
    format!("W/\"{:016x}\"", fnv1a(body.as_bytes()))
}

/// The bare opaque tag: weak prefix and quotes dropped (weak comparison,
/// RFC 9110 §8.8.3.2, which `If-None-Match` uses).
fn opaque(tag: &str) -> &str {
    tag.trim().trim_start_matches("W/").trim_matches('"')
}

/// Does an `If-None-Match` value (one tag, a comma list, or `*`) match
/// the current ETag?
pub(crate) fn matches(if_none_match: &str, current: &str) -> bool {
    if_none_match
        .split(',')
        .map(str::trim)
        .any(|t| t == "*" || (!t.is_empty() && opaque(t) == opaque(current)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etag_is_stable_and_follows_the_bytes() {
        // Pinned: a changed hash would silently invalidate stored ETags.
        assert_eq!(etag_of(""), "W/\"cbf29ce484222325\"");
        assert_eq!(etag_of("{\"a\":1}"), etag_of("{\"a\":1}"));
        assert_ne!(etag_of("{\"a\":1}"), etag_of("{\"a\":2}"));
    }

    #[test]
    fn matching_is_weak_and_accepts_lists_and_star() {
        let tag = etag_of("x");
        assert!(matches(&tag, &tag));
        assert!(matches(tag.trim_start_matches("W/"), &tag));
        assert!(matches(&format!("\"zzz\", {tag}"), &tag));
        assert!(matches("*", &tag));
        assert!(!matches("W/\"other\"", &tag));
        assert!(!matches("", &tag));
    }

    use super::super::FixtureHttp;
    use super::super::rate_headers::{Lane, RateBucket, RateVariant};
    use crate::http::{Http, Verb};

    const LOW: RateVariant = RateVariant {
        rest: RateBucket {
            limit: 5000,
            remaining: 1200,
            reset_in_secs: 600,
        },
        graphql: RateBucket {
            limit: 5000,
            remaining: 4000,
            reset_in_secs: 900,
        },
        graphql_cost: 1,
    };

    fn rig(tag: &str, rate: Option<RateVariant>) -> (std::path::PathBuf, FixtureHttp) {
        let dir = std::env::temp_dir().join(format!("gh-etag-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("rest")).unwrap();
        std::fs::create_dir_all(dir.join("graphql")).unwrap();
        (dir.clone(), FixtureHttp::with_rate(dir, rate))
    }

    const URL: &str = "https://api.github.com/repos/o/r/pulls/1?per_page=1";

    #[test]
    fn a_matching_if_none_match_is_a_304_with_no_body_and_the_same_rate_data() {
        let (dir, http) = rig("304", Some(LOW));
        std::fs::write(dir.join("rest/repos_o_r_pulls_1.json"), r#"{"n":1}"#).unwrap();
        let first = http.request(Verb::Get, URL, None, None).unwrap();
        assert_eq!(
            (first.status, first.body.as_deref()),
            (200, Some(r#"{"n":1}"#))
        );
        let tag = first.etag.clone().expect("REST answers carry an ETag");
        let again = http.request(Verb::Get, URL, None, Some(&tag)).unwrap();
        assert_eq!((again.status, again.body), (304, None));
        assert_eq!(again.etag, first.etag);
        assert_eq!(
            again.rate, first.rate,
            "a 304 leaves the rate headers as they were"
        );
        assert_eq!(again.rate.map(|r| r.remaining), Some(1200));
        // A stale tag is a plain 200.
        let stale = http
            .request(Verb::Get, URL, None, Some("W/\"old\""))
            .unwrap();
        assert_eq!(stale.status, 200);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_bump_changes_the_body_and_the_etag_so_the_old_tag_is_a_200() {
        let (dir, http) = rig("bump", None);
        std::fs::write(dir.join("rest/repos_o_r_pulls_1.json"), r#"{"n":1}"#).unwrap();
        std::fs::write(dir.join("rest/repos_o_r_pulls_1.g1.json"), r#"{"n":2}"#).unwrap();
        let first = http.request(Verb::Get, URL, None, None).unwrap();
        let tag = first.etag.unwrap();
        assert_eq!(
            http.request(Verb::Get, URL, None, Some(&tag))
                .unwrap()
                .status,
            304
        );
        http.generation.bump(None);
        let after = http.request(Verb::Get, URL, None, Some(&tag)).unwrap();
        assert_eq!(
            (after.status, after.body.as_deref()),
            (200, Some(r#"{"n":2}"#))
        );
        assert_ne!(after.etag.as_deref(), Some(tag.as_str()));
        // The ETag is a function of the bytes: a rebuilt transport agrees.
        let other = FixtureHttp::with_rate(dir.clone(), None);
        other.generation.bump(None);
        assert_eq!(
            other.request(Verb::Get, URL, None, None).unwrap().etag,
            after.etag
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_304_does_not_advance_a_sequence() {
        let (dir, http) = rig("seq", None);
        std::fs::write(
            dir.join("rest/repos_o_r_pulls_1.json"),
            r#"[{"n":1},{"n":2}]"#,
        )
        .unwrap();
        let first = http.request(Verb::Get, URL, None, None).unwrap();
        // The next element differs, so the first element's tag is stale: 200.
        let second = http
            .request(Verb::Get, URL, None, first.etag.as_deref())
            .unwrap();
        assert_eq!(second.body.as_deref(), Some(r#"{"n":2}"#));
        // The last element repeats, so its own tag is a 304 from here on.
        let third = http
            .request(Verb::Get, URL, None, second.etag.as_deref())
            .unwrap();
        assert_eq!(third.status, 304);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn graphql_never_answers_304_and_has_no_etag() {
        let (dir, http) = rig("gql", None);
        std::fs::write(dir.join("graphql/Q.json"), r#"{"data":{}}"#).unwrap();
        for _ in 0..2 {
            let res = http
                .post_graphql("query Q { x }", &serde_json::Value::Null)
                .unwrap();
            assert_eq!((res.status, res.etag), (200, None));
            assert!(res.body.is_some());
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The per-lane headers: a REST budget never leaks into GraphQL.
    #[test]
    fn budgets_are_stamped_per_lane() {
        let (dir, http) = rig("lanes", None);
        std::fs::write(dir.join("rest/repos_o_r_pulls_1.json"), "{}").unwrap();
        std::fs::write(dir.join("graphql/Q.json"), r#"{"data":{"rateLimit":null}}"#).unwrap();
        let rest = |http: &FixtureHttp| http.request(Verb::Get, URL, None, None).unwrap().rate;
        let gql = |http: &FixtureHttp| {
            http.post_graphql("query Q { x }", &serde_json::Value::Null)
                .unwrap()
                .rate
        };
        assert_eq!((rest(&http), gql(&http)), (None, None));
        http.budgets.set(Lane::Rest, 450);
        assert_eq!(
            rest(&http).map(|r| (r.limit, r.remaining)),
            Some((5000, 450))
        );
        assert_eq!(gql(&http), None);
        http.budgets.set(Lane::Graphql, 0);
        assert_eq!(gql(&http).map(|r| r.remaining), Some(0));
        assert_eq!(rest(&http).map(|r| r.remaining), Some(450));
        let _ = std::fs::remove_dir_all(dir);
    }
}
