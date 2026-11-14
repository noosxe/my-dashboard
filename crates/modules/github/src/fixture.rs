//! Verification fixtures (docs/0081 §12, docs/0071): in debug builds,
//! `DASHBOARD_GITHUB_FIXTURE=<dir>` swaps the module's transport and
//! token store for an offline GitHub that answers from committed JSON,
//! so the containerized env (no network credentials, no keyring) can
//! drive and capture signed-in surfaces. Release builds never read the
//! variable.
//!
//! Layout under `<dir>`:
//! - `graphql/<Operation>.json` — the body for `query <Operation>`. A
//!   JSON **array** is a sequence: the n-th request gets element n, the
//!   last one repeats (paginated lanes).
//! - `graphql/<Operation>.author.json` — the body when the request's
//!   `$author` variable is set (the commits "mine" filter, docs/0081
//!   §14), so a filtered book serves its own history.
//! - `graphql/<Operation>.<number>.json` — the body when the request's
//!   `$number` variable is set (the PR-scoped reads, `PrDetail` and
//!   `PrThreads`), so one session serves several PR states.
//! - `graphql/<Operation>.<repo>.json` — the body when the request's
//!   `$name` variable names that repository (`RepoPulls.beacon.json`),
//!   so PR rows can live on one repo while the others' lanes stay empty.
//!   Both selectors fall back to `graphql/<Operation>.json`.
//! - `rest/<path>.json` — the body for `GET /<path>`, slashes as `_`,
//!   query string dropped (`/user/orgs?per_page=100` →
//!   `rest/user_orgs.json`).
//!
//! - Any of the above may have generation siblings `<name>.g<N>.json`
//!   (docs/0084 §8): the highest N at or below the current generation
//!   answers, else the plain name. The generation starts at 0 and
//!   `dashboard-debug fixture bump [N]` advances it; see [`generations`].
//! - `rate/<variant>.json` — a rate-limit variant (`healthy`, `low`,
//!   `critical`, `exhausted`, `graphql-exhausted`), selected with
//!   `DASHBOARD_GITHUB_FIXTURE_RATE`. Unset, no response carries rate
//!   data (the chip stays `REST — · GraphQL —`).
//!
//! Writes (mutations, REST POST/PATCH/DELETE) are answered from their own
//! files and recorded; see [`writes`]. Reads are recorded too
//! (`dashboard-debug requests`), and REST reads carry an ETag and answer
//! 304 to a matching `If-None-Match`; see [`etag`]. GraphQL never does.
//! Budgets can be set while the app runs (`fixture budget`); see
//! [`rate_headers`]. The bridge verbs reach the transport through
//! [`control`].
//!
//! A missing file answers 404 and logs a warning naming the path, so a
//! capture that hits an unfixtured lane says so in `logs`.
//!
//! **Rate variants (docs/0081 §12.1).** A variant is one static budget
//! snapshot per bucket: every REST response carries its `core` numbers
//! as `x-ratelimit-*` headers, every GraphQL response its `graphql`
//! numbers as headers *and* as the body's `data.rateLimit`, and
//! `GET /rate_limit` answers a synthesized probe body. Reset times are
//! offsets (`reset_in_secs`) resolved against the budget clock
//! ([`crate::usage::budget_now`]) at serve time — pinned under
//! `DASHBOARD_FIXED_NOW`, so the chip's countdown is the same on any
//! calendar day. The numbers never decrement: request order isn't
//! deterministic, a static snapshot is.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::json;

use crate::auth::{StoredCredential, TokenStore};
use crate::http::{Http, HttpError, HttpResult, Verb};
use crate::usage::budget_now;
use dashboard_core::fixture_control::record_request;
use generations::Generation;
use rate_headers::{Budgets, Lane, RateBucket, RateVariant, rate_variant};

/// The fixture directory variable.
pub(crate) const FIXTURE_ENV: &str = "DASHBOARD_GITHUB_FIXTURE";

/// The fixture directory, when this is a debug build and the variable
/// names one.
pub(crate) fn fixture_dir() -> Option<PathBuf> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var_os(FIXTURE_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// A token store that always holds one non-expiring fake token and
/// never touches the keychain.
pub(crate) struct FixtureTokenStore;

impl TokenStore for FixtureTokenStore {
    fn save(&self, _cred: &StoredCredential) -> Result<(), String> {
        Ok(())
    }
    fn load(&self) -> Result<Option<StoredCredential>, String> {
        Ok(Some(StoredCredential {
            access_token: "fixture".into(),
            refresh_token: None,
            access_expires_at: None,
        }))
    }
    fn delete(&self) -> Result<(), String> {
        Ok(())
    }
}

/// The offline transport: one file per GraphQL operation or REST path.
/// Cloning shares all state (the control handler holds a clone).
#[derive(Clone)]
pub(crate) struct FixtureHttp {
    dir: PathBuf,
    /// Requests served per file — the index into a sequence file.
    served: Arc<Mutex<HashMap<PathBuf, usize>>>,
    /// The effective rate budgets (module docs).
    budgets: Arc<Budgets>,
    /// The generation `fixture bump` advances.
    generation: Arc<Generation>,
}

impl FixtureHttp {
    /// The app's transport: also registers the `fixture …` bridge verbs.
    pub(crate) fn new(dir: PathBuf) -> Self {
        log::info!("github: serving fixtures from {}", dir.display());
        let rate = rate_variant(&dir);
        let http = Self::with_rate(dir, rate);
        control::register(&http);
        http
    }

    pub(crate) fn with_rate(dir: PathBuf, rate: Option<RateVariant>) -> Self {
        Self {
            dir,
            served: Arc::new(Mutex::new(HashMap::new())),
            budgets: Arc::new(Budgets::new(rate)),
            generation: Arc::new(Generation::default()),
        }
    }

    /// The synthesized `GET /rate_limit` body: `resources.core` from the
    /// REST budget, and the endpoint's measured phantom for
    /// `resources.graphql` (always full — docs/0050 §2).
    fn rate_limit_body(rest: RateBucket, now: u64) -> String {
        let core = rest.at(now);
        let used = core.limit.saturating_sub(core.remaining);
        let core = serde_json::json!({
            "limit": core.limit,
            "remaining": core.remaining,
            "reset": core.reset_epoch,
            "used": used,
        });
        serde_json::json!({
            "resources": {
                "core": core,
                "graphql": { "limit": 5000, "remaining": 5000, "reset": now + 3600, "used": 0 },
            },
            "rate": core,
        })
        .to_string()
    }

    /// Stamp a served GraphQL body with the GraphQL budget's
    /// `data.rateLimit` (the field every query selects; the committed
    /// files carry `null` there). Non-object bodies pass through untouched.
    fn with_graphql_rate(body: String, bucket: RateBucket, cost: u32, now: u64) -> String {
        let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&body) else {
            return body;
        };
        let Some(data) = value.get_mut("data").and_then(|d| d.as_object_mut()) else {
            return body;
        };
        let g = bucket.at(now);
        let reset = chrono::DateTime::from_timestamp(g.reset_epoch as i64, 0)
            .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
            .unwrap_or_default();
        data.insert(
            "rateLimit".into(),
            serde_json::json!({
                "cost": cost,
                "limit": g.limit,
                "remaining": g.remaining,
                "used": g.limit.saturating_sub(g.remaining),
                "resetAt": reset,
            }),
        );
        value.to_string()
    }

    /// Serve the first candidate that exists at the current generation;
    /// none → the last one's miss. GraphQL: never an ETag, never a 304.
    fn serve_first(&self, candidates: Vec<String>) -> HttpResult {
        let graphql = PathBuf::from("graphql");
        let current = self.generation.current();
        let picked = candidates
            .iter()
            .find(|c| generations::resolve(&self.dir.join(&graphql), c, current).is_some())
            .or(candidates.last());
        self.serve(graphql.join(picked.map_or("", |c| c.as_str())), None)
    }

    /// Answer `rel` (`rest/<key>.json`, `graphql/<name>.json`) from the
    /// file the current generation picks. `rest` is `Some(If-None-Match)`
    /// for a REST read — the answer then carries an ETag, and a match
    /// is a 304 with no body that does not advance a sequence — and
    /// `None` for GraphQL.
    fn serve(&self, rel: PathBuf, rest: Option<Option<&str>>) -> HttpResult {
        let miss = || {
            log::warn!("github fixture: no file {}", rel.display());
            HttpResult {
                status: 404,
                etag: None,
                rate: None,
                body: Some(r#"{"message":"Not Found (no fixture)"}"#.into()),
            }
        };
        let (Some(parent), Some(name)) = (rel.parent(), rel.file_name().and_then(|n| n.to_str()))
        else {
            return miss();
        };
        let current = self.generation.current();
        let Some(path) = generations::resolve(&self.dir.join(parent), name, current) else {
            return miss();
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return miss();
        };
        let mut served = self.served.lock().unwrap_or_else(|e| e.into_inner());
        let (body, next) = match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(serde_json::Value::Array(seq)) if !seq.is_empty() => {
                let n = served.get(&path).copied().unwrap_or(0);
                (seq[n.min(seq.len() - 1)].to_string(), Some(n + 1))
            }
            _ => (text, None),
        };
        let etag = rest.map(|_| etag::etag_of(&body));
        if let (Some(tag), Some(Some(sent))) = (&etag, rest)
            && etag::matches(sent, tag)
        {
            return HttpResult {
                status: 304,
                etag,
                rate: None,
                body: None,
            };
        }
        if let Some(next) = next {
            served.insert(path, next);
        }
        HttpResult {
            status: 200,
            etag,
            rate: None,
            body: Some(body),
        }
    }
}

/// `query RepoCommits(` / `mutation Foo {` → the operation name.
pub(crate) fn operation_name(query: &str) -> Option<&str> {
    let rest = query
        .split_whitespace()
        .skip_while(|w| *w != "query" && *w != "mutation")
        .nth(1)?;
    let name = rest.split(['(', '{']).next()?;
    (!name.is_empty()).then_some(name)
}

/// The fixture files for one GraphQL request, most specific first:
/// `<op>.author.json` alone when `$author` is set (a filter must never
/// answer unfiltered), else `<op>.<number>.json` for `$number`,
/// `<op>.<name>.json` for `$name`, and the plain `<op>.json`.
pub(crate) fn graphql_files(op: &str, variables: &serde_json::Value) -> Vec<String> {
    if variables.get("author").is_some_and(|a| !a.is_null()) {
        return vec![format!("{op}.author.json")];
    }
    let mut files = Vec::new();
    if let Some(number) = variables.get("number").and_then(|n| n.as_i64()) {
        files.push(format!("{op}.{number}.json"));
    }
    if let Some(name) = variables.get("name").and_then(|n| n.as_str()) {
        files.push(format!("{op}.{name}.json"));
    }
    files.push(format!("{op}.json"));
    files
}

/// `https://api.github.com/user/orgs?per_page=100` → `user/orgs`.
pub(crate) fn url_path(url: &str) -> &str {
    let path = url
        .split_once("://")
        .map(|(_, rest)| rest.split_once('/').map(|(_, p)| p).unwrap_or(""))
        .unwrap_or(url);
    path.split('?').next().unwrap_or("").trim_matches('/')
}

/// `https://api.github.com/user/orgs?per_page=100` → `user_orgs`.
pub(crate) fn rest_key(url: &str) -> String {
    url_path(url).replace('/', "_")
}

impl Http for FixtureHttp {
    fn request(
        &self,
        verb: Verb,
        url: &str,
        body: Option<&str>,
        etag: Option<&str>,
    ) -> Result<HttpResult, HttpError> {
        if verb != Verb::Get {
            // Answered from `rest/<path>.<verb>.json` or refused (writes.rs).
            return writes::rest(&self.dir, verb, url, body);
        }
        let key = rest_key(url);
        let now = budget_now();
        let rest = self.budgets.bucket(Lane::Rest);
        let mut res = match rest {
            Some(rest) if key == "rate_limit" => HttpResult {
                status: 200,
                etag: None,
                rate: None,
                body: Some(Self::rate_limit_body(rest, now)),
            },
            _ => self.serve(
                PathBuf::from("rest").join(format!("{key}.json")),
                Some(etag),
            ),
        };
        // A 304 carries the same rate data as a 200 and costs no quota
        // (docs/0084 F1), so the budget is stamped either way.
        res.rate = rest.map(|b| b.at(now));
        record_request(
            "rest",
            format!("GET /{}", url_path(url)),
            json!({
                "status": res.status,
                "etag": res.etag,
                "if_none_match": etag,
                "remaining": res.rate.map(|r| r.remaining),
                "generation": self.generation.current(),
            }),
        );
        Ok(res)
    }

    fn post_graphql(
        &self,
        query: &str,
        variables: &serde_json::Value,
    ) -> Result<HttpResult, HttpError> {
        let Some(op) = operation_name(query) else {
            return Err(HttpError("fixture: unnamed GraphQL operation".into()));
        };
        if writes::is_mutation(query) {
            return writes::graphql(&self.dir, op, variables);
        }
        let mut res = self.serve_first(graphql_files(op, variables));
        if let Some(bucket) = self.budgets.bucket(Lane::Graphql) {
            let now = budget_now();
            res.rate = Some(bucket.at(now));
            if res.status == 200 {
                let cost = self.budgets.graphql_cost();
                res.body = res
                    .body
                    .map(|b| Self::with_graphql_rate(b, bucket, cost, now));
            }
        }
        record_request(
            "graphql",
            op,
            json!({ "status": res.status, "generation": self.generation.current() }),
        );
        Ok(res)
    }
}

#[path = "fixture/control.rs"]
mod control;
#[path = "fixture/etag.rs"]
mod etag;
#[path = "fixture/generations.rs"]
mod generations;
#[path = "fixture/rate_headers.rs"]
mod rate_headers;
#[path = "fixture/writes.rs"]
mod writes;

#[cfg(test)]
#[path = "fixture/generation_one.rs"]
mod generation_one;

#[cfg(test)]
#[path = "fixture/pr_scenarios.rs"]
mod pr_scenarios;

#[cfg(test)]
mod tests {
    use super::rate_headers::{RateBucket, RateVariant};
    use super::{FixtureHttp, graphql_files, operation_name, rest_key};
    use crate::http::{Http, Verb};
    use crate::usage::{budget_now, parse_rate_limit_body};

    #[test]
    fn names_and_keys() {
        assert_eq!(
            operation_name("\nquery RepoCommits($owner: String!) {"),
            Some("RepoCommits")
        );
        assert_eq!(
            operation_name("query Viewer{ viewer { login } }"),
            Some("Viewer")
        );
        assert_eq!(operation_name("{ viewer { login } }"), None);
        assert_eq!(
            rest_key("https://api.github.com/user/orgs?per_page=100"),
            "user_orgs"
        );
        assert_eq!(rest_key("https://api.github.com/user"), "user");
        let mine = serde_json::json!({ "first": 50, "author": { "id": "U_x" } });
        assert_eq!(
            graphql_files("RepoCommits", &mine),
            ["RepoCommits.author.json"]
        );
        let all = serde_json::json!({ "first": 50, "author": null });
        assert_eq!(graphql_files("RepoCommits", &all), ["RepoCommits.json"]);
        assert_eq!(
            graphql_files("RepoCommits", &serde_json::Value::Null),
            ["RepoCommits.json"]
        );
        let pr = serde_json::json!({ "owner": "o", "name": "beacon", "number": 101 });
        assert_eq!(
            graphql_files("PrDetail", &pr),
            ["PrDetail.101.json", "PrDetail.beacon.json", "PrDetail.json"]
        );
    }

    /// One session serves several PR states: the number's own file, the
    /// plain file for every other number, and a 404 once neither exists.
    #[test]
    fn selectors_prefer_the_specific_file_and_fall_back() {
        let dir = scratch("selector");
        std::fs::write(dir.join("graphql/PrDetail.7.json"), r#"{"n":7}"#).unwrap();
        std::fs::write(dir.join("graphql/RepoPulls.beacon.json"), r#"{"n":"b"}"#).unwrap();
        std::fs::write(dir.join("graphql/RepoPulls.json"), r#"{"n":"plain"}"#).unwrap();
        let http = FixtureHttp::with_rate(dir.clone(), None);
        let ask = |q: &str, v: serde_json::Value| http.post_graphql(q, &v).unwrap();
        let detail = "query PrDetail { x }";
        let seven = ask(detail, serde_json::json!({ "name": "beacon", "number": 7 }));
        assert_eq!(seven.body.as_deref(), Some(r#"{"n":7}"#));
        let eight = ask(detail, serde_json::json!({ "name": "beacon", "number": 8 }));
        assert_eq!(eight.status, 404);
        let pulls = "query RepoPulls { x }";
        let beacon = ask(pulls, serde_json::json!({ "name": "beacon" }));
        assert_eq!(beacon.body.as_deref(), Some(r#"{"n":"b"}"#));
        let atlas = ask(pulls, serde_json::json!({ "name": "atlas" }));
        assert_eq!(atlas.body.as_deref(), Some(r#"{"n":"plain"}"#));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sequences_advance_then_repeat_and_misses_404() {
        let dir = std::env::temp_dir().join(format!("gh-fixture-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("graphql")).unwrap();
        std::fs::write(dir.join("graphql/Seq.json"), r#"[{"n":1},{"n":2}]"#).unwrap();
        let http = FixtureHttp::new(dir.clone());
        let q = "query Seq { x }";
        let body = |r: crate::http::HttpResult| r.body.unwrap();
        let v = serde_json::Value::Null;
        assert_eq!(body(http.post_graphql(q, &v).unwrap()), r#"{"n":1}"#);
        assert_eq!(body(http.post_graphql(q, &v).unwrap()), r#"{"n":2}"#);
        assert_eq!(body(http.post_graphql(q, &v).unwrap()), r#"{"n":2}"#);
        let miss = http
            .request(Verb::Get, "https://api.github.com/nope", None, None)
            .unwrap();
        assert_eq!(miss.status, 404);
        assert!(
            http.request(Verb::Post, "https://api.github.com/x", None, None)
                .is_err()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("gh-fixture-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("graphql")).unwrap();
        std::fs::create_dir_all(dir.join("rest")).unwrap();
        dir
    }

    const CRITICAL: RateVariant = RateVariant {
        rest: RateBucket {
            limit: 5000,
            remaining: 312,
            reset_in_secs: 2460,
        },
        graphql: RateBucket {
            limit: 5000,
            remaining: 188,
            reset_in_secs: 1380,
        },
        graphql_cost: 3,
    };

    #[test]
    fn rate_variant_stamps_headers_bodies_and_the_probe() {
        let dir = scratch("rate");
        std::fs::write(
            dir.join("graphql/Viewer.json"),
            r#"{"data":{"rateLimit":null,"viewer":{"login":"fixture-user"}}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("rest/user.json"), r#"{"login":"fixture-user"}"#).unwrap();
        let http = FixtureHttp::with_rate(dir.clone(), Some(CRITICAL));
        let now = budget_now();

        let gql = http
            .post_graphql("query Viewer { x }", &serde_json::Value::Null)
            .unwrap();
        let rate = gql.rate.unwrap();
        assert_eq!((rate.limit, rate.remaining), (5000, 188));
        assert!((now + 1380..=now + 1381).contains(&rate.reset_epoch));
        let body: serde_json::Value = serde_json::from_str(&gql.body.unwrap()).unwrap();
        let rl = &body["data"]["rateLimit"];
        assert_eq!(
            (rl["remaining"].as_u64(), rl["cost"].as_u64()),
            (Some(188), Some(3))
        );
        assert_eq!(body["data"]["viewer"]["login"], "fixture-user");
        let reset = chrono::DateTime::parse_from_rfc3339(rl["resetAt"].as_str().unwrap()).unwrap();
        assert_eq!(reset.timestamp() as u64, rate.reset_epoch);

        let rest = http
            .request(Verb::Get, "https://api.github.com/user", None, None)
            .unwrap();
        assert_eq!(rest.rate.map(|r| r.remaining), Some(312));
        assert_eq!(rest.body.as_deref(), Some(r#"{"login":"fixture-user"}"#));

        // The probe answers without a committed file; its body folds
        // `resources.core` only, matching the headers.
        let probe = http
            .request(Verb::Get, "https://api.github.com/rate_limit", None, None)
            .unwrap();
        assert_eq!(probe.status, 200);
        let core = parse_rate_limit_body(probe.body.as_deref().unwrap()).unwrap();
        assert_eq!(
            (core.remaining, core.reset_at),
            (312, probe.rate.unwrap().reset_epoch)
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn no_variant_keeps_responses_rate_free() {
        let dir = scratch("norate");
        std::fs::write(
            dir.join("graphql/Viewer.json"),
            r#"{"data":{"rateLimit":null}}"#,
        )
        .unwrap();
        let http = FixtureHttp::with_rate(dir.clone(), None);
        let gql = http
            .post_graphql("query Viewer { x }", &serde_json::Value::Null)
            .unwrap();
        assert!(gql.rate.is_none());
        assert_eq!(gql.body.as_deref(), Some(r#"{"data":{"rateLimit":null}}"#));
        let probe = http
            .request(Verb::Get, "https://api.github.com/rate_limit", None, None)
            .unwrap();
        assert_eq!(probe.status, 404);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The committed variants parse and sit on the docs/0082 D5 ladder
    /// they're named for (Low ≤ 25 %, Critical ≤ 10 %), and `exhausted`
    /// resets both buckets together, so the pause's countdown doesn't
    /// depend on which bucket answered first.
    #[test]
    fn committed_variants_parse() {
        use dashboard_core::usage_analytics::surfaces::ThresholdState;
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../debug/verify/fixtures/github/rate");
        let load = |name: &str| -> RateVariant {
            let text = std::fs::read_to_string(dir.join(format!("{name}.json"))).unwrap();
            serde_json::from_str(&text).unwrap()
        };
        let (healthy, low, critical, exhausted, gql_exhausted) = (
            load("healthy"),
            load("low"),
            load("critical"),
            load("exhausted"),
            load("graphql-exhausted"),
        );
        // Each variant lands on the state it's named for, per bucket.
        let states = |v: &RateVariant| {
            [v.rest, v.graphql].map(|b| ThresholdState::of(b.remaining, Some(b.limit)).unwrap())
        };
        assert_eq!(states(&healthy), [ThresholdState::Normal; 2]);
        assert_eq!(states(&low), [ThresholdState::Low; 2]);
        assert_eq!(states(&critical), [ThresholdState::Critical; 2]);
        assert_eq!(
            states(&gql_exhausted),
            [ThresholdState::Normal, ThresholdState::Exhausted]
        );
        assert_eq!(
            (exhausted.rest.remaining, exhausted.graphql.remaining),
            (0, 0)
        );
        assert_eq!(
            exhausted.rest.reset_in_secs,
            exhausted.graphql.reset_in_secs
        );
    }
}
