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
//! - `rest/<path>.json` — the body for `GET /<path>`, slashes as `_`,
//!   query string dropped (`/user/orgs?per_page=100` →
//!   `rest/user_orgs.json`).
//!
//! A missing file answers 404 and logs a warning naming the path, so a
//! capture that hits an unfixtured lane says so in `logs`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::auth::{StoredCredential, TokenStore};
use crate::http::{Http, HttpError, HttpResult, Verb};

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
pub(crate) struct FixtureHttp {
    dir: PathBuf,
    /// Requests served per file — the index into a sequence file.
    served: Mutex<HashMap<PathBuf, usize>>,
}

impl FixtureHttp {
    pub(crate) fn new(dir: PathBuf) -> Self {
        log::info!("github: serving fixtures from {}", dir.display());
        Self {
            dir,
            served: Mutex::new(HashMap::new()),
        }
    }

    fn serve(&self, rel: PathBuf) -> HttpResult {
        let path = self.dir.join(&rel);
        let Ok(text) = std::fs::read_to_string(&path) else {
            log::warn!("github fixture: no file {}", rel.display());
            return HttpResult {
                status: 404,
                etag: None,
                rate: None,
                body: Some(r#"{"message":"Not Found (no fixture)"}"#.into()),
            };
        };
        let body = match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(serde_json::Value::Array(seq)) if !seq.is_empty() => {
                let mut served = self.served.lock().unwrap_or_else(|e| e.into_inner());
                let n = served.entry(rel).or_insert(0);
                let body = seq[(*n).min(seq.len() - 1)].to_string();
                *n += 1;
                body
            }
            _ => text,
        };
        HttpResult {
            status: 200,
            etag: None,
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

/// The fixture file for one GraphQL request: `<op>.json`, or
/// `<op>.author.json` when `$author` is set.
pub(crate) fn graphql_file(op: &str, variables: &serde_json::Value) -> String {
    if variables.get("author").is_some_and(|a| !a.is_null()) {
        format!("{op}.author.json")
    } else {
        format!("{op}.json")
    }
}

/// `https://api.github.com/user/orgs?per_page=100` → `user_orgs`.
pub(crate) fn rest_key(url: &str) -> String {
    let path = url
        .split_once("://")
        .map(|(_, rest)| rest.split_once('/').map(|(_, p)| p).unwrap_or(""))
        .unwrap_or(url);
    let path = path.split('?').next().unwrap_or("");
    path.trim_matches('/').replace('/', "_")
}

impl Http for FixtureHttp {
    fn request(
        &self,
        verb: Verb,
        url: &str,
        _body: Option<&str>,
        _etag: Option<&str>,
    ) -> Result<HttpResult, HttpError> {
        if verb != Verb::Get {
            // Writes are not part of any capture; refuse loudly.
            log::warn!("github fixture: refusing {verb:?} {url}");
            return Err(HttpError("fixture mode is read-only".into()));
        }
        Ok(self.serve(PathBuf::from("rest").join(format!("{}.json", rest_key(url)))))
    }

    fn post_graphql(
        &self,
        query: &str,
        variables: &serde_json::Value,
    ) -> Result<HttpResult, HttpError> {
        let Some(op) = operation_name(query) else {
            return Err(HttpError("fixture: unnamed GraphQL operation".into()));
        };
        Ok(self.serve(PathBuf::from("graphql").join(graphql_file(op, variables))))
    }
}

#[cfg(test)]
mod tests {
    use super::{FixtureHttp, graphql_file, operation_name, rest_key};
    use crate::http::{Http, Verb};

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
            graphql_file("RepoCommits", &mine),
            "RepoCommits.author.json"
        );
        let all = serde_json::json!({ "first": 50, "author": null });
        assert_eq!(graphql_file("RepoCommits", &all), "RepoCommits.json");
        assert_eq!(
            graphql_file("RepoCommits", &serde_json::Value::Null),
            "RepoCommits.json"
        );
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
}
