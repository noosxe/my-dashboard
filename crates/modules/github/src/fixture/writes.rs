//! Fixture writes (docs/0081 §12.1, #812): the offline transport answers
//! GraphQL mutations and REST writes from response files and records each
//! one for `dashboard-debug writes` / `assert-write`. Static answers, no
//! state: the refetch after a write still serves the pre-write read
//! files. A write with no file is refused with an error naming it.
//!
//! Layout under the fixture directory:
//! - `graphql/<Mutation>.json` — the body for `mutation <Mutation>`
//!   (`PrMerge.json`). Selectors, most specific first: `<Mutation>.<number>.json`
//!   (`$number`), `<Mutation>.<node id>.json` (the input's
//!   `pullRequestId` / `subjectId` / `id`), then the plain file.
//! - `rest/<path>.<verb>.json` — the response to `POST|PATCH|DELETE
//!   /<path>`, path keyed like the reads (`rest_key`), verb lowercase
//!   (`rest/repos_o_r_actions_runs_7_approve.post.json`). It is an
//!   envelope `{"status": <2xx>, "body": <json>}` because REST writes
//!   differ in status (201 approve, 200 pending deployments, 204 delete).
//!
//! This transport is only built when `fixture_dir()` is set, which is
//! debug builds with `DASHBOARD_GITHUB_FIXTURE`; release builds can never
//! reach it, so a write cannot leave the process.

use std::path::Path;

use dashboard_core::write_log;
use serde_json::Value;

use super::{graphql_files, rest_key};
use crate::http::{HttpError, HttpResult, Verb};

/// `mutation Foo(…)` as opposed to `query Foo(…)`.
pub(crate) fn is_mutation(query: &str) -> bool {
    query
        .split_whitespace()
        .find(|w| *w == "query" || *w == "mutation")
        == Some("mutation")
}

/// Mutation selector: the read selector plus the target node id.
pub(crate) fn mutation_files(op: &str, variables: &Value) -> Vec<String> {
    let mut files = graphql_files(op, variables);
    let input = variables.get("input");
    let node = ["pullRequestId", "subjectId", "id"]
        .iter()
        .find_map(|k| input?.get(k)?.as_str());
    if let Some(node) = node {
        let at = files.len() - 1; // before the plain file
        files.insert(at, format!("{op}.{node}.json"));
    }
    files
}

fn ok(body: String, status: u16) -> HttpResult {
    HttpResult {
        status,
        etag: None,
        rate: None,
        body: Some(body),
    }
}

/// Answer a mutation from `graphql/`, recording it.
pub(crate) fn graphql(dir: &Path, op: &str, variables: &Value) -> Result<HttpResult, HttpError> {
    let files = mutation_files(op, variables);
    let hit = files
        .iter()
        .find_map(|f| std::fs::read_to_string(dir.join("graphql").join(f)).ok());
    let Some(body) = hit else {
        let last = files.last().map_or("", String::as_str);
        log::warn!("github fixture: refusing write {op}: no file graphql/{last}");
        return Err(HttpError(format!(
            "fixture mode has no response for mutation {op}: add graphql/{last} (docs/0081 §12.1)"
        )));
    };
    log::info!("github fixture: write {op}");
    write_log::record("graphql", op, variables.clone());
    Ok(ok(body, 200))
}

/// Answer a REST write from `rest/<key>.<verb>.json`, recording it.
pub(crate) fn rest(
    dir: &Path,
    verb: Verb,
    url: &str,
    body: Option<&str>,
) -> Result<HttpResult, HttpError> {
    let verb_name = format!("{verb:?}");
    let rel = format!("rest/{}.{}.json", rest_key(url), verb_name.to_lowercase());
    let path = super::url_path(url);
    let envelope = std::fs::read_to_string(dir.join(&rel))
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()))
        .ok()
        .and_then(|v| {
            Some((
                v.get("status")?.as_u64()? as u16,
                v.get("body")?.to_string(),
            ))
        });
    let Some((status, answer)) = envelope else {
        log::warn!("github fixture: refusing write {verb_name} {url}: no usable file {rel}");
        return Err(HttpError(format!(
            "fixture mode has no response for {verb_name} /{path}: add {rel} as {{\"status\": 2xx, \"body\": …}} (docs/0081 §12.1)"
        )));
    };
    let sent = body.map_or(Value::Null, |b| {
        serde_json::from_str(b).unwrap_or_else(|_| Value::String(b.to_string()))
    });
    log::info!("github fixture: write {verb_name} /{path}");
    write_log::record(
        "rest",
        format!("{} /{path}", verb_name.to_uppercase()),
        sent,
    );
    Ok(ok(answer, status))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("fixture-writes-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("graphql")).unwrap();
        std::fs::create_dir_all(dir.join("rest")).unwrap();
        dir
    }

    #[test]
    fn mutation_vs_query() {
        assert!(is_mutation("mutation PrMerge($i: X!) { a }"));
        assert!(!is_mutation("query PrDetail { a }"));
        assert!(!is_mutation("\n  query Q { mutation }"));
    }

    #[test]
    fn selectors_add_the_node_id_before_the_plain_file() {
        let v = json!({"input": {"pullRequestId": "PR_107"}});
        assert_eq!(
            mutation_files("PrMerge", &v),
            ["PrMerge.PR_107.json", "PrMerge.json"]
        );
        assert_eq!(mutation_files("PrMerge", &json!({})), ["PrMerge.json"]);
    }

    #[test]
    fn graphql_write_answers_and_records_or_refuses_naming_the_file() {
        let dir = scratch("gql");
        let v = json!({"input": {"pullRequestId": "PR_9"}});
        let err = graphql(&dir, "WlOp", &v).unwrap_err();
        assert!(err.0.contains("graphql/WlOp.json"), "{}", err.0);
        std::fs::write(dir.join("graphql/WlOp.json"), r#"{"data":{}}"#).unwrap();
        let res = graphql(&dir, "WlOp", &v).unwrap();
        assert_eq!(
            (res.status, res.body.as_deref()),
            (200, Some(r#"{"data":{}}"#))
        );
        let (all, _) = write_log::since(0);
        let rec = all.iter().rev().find(|r| r.op == "WlOp").unwrap();
        assert_eq!(rec.vars, v);
    }

    #[test]
    fn rest_write_uses_the_envelope_status_and_records_the_body() {
        let dir = scratch("rest");
        let url = "https://api.github.com/repos/o/r/actions/runs/7/approve";
        let err = rest(&dir, Verb::Post, url, None).unwrap_err();
        assert!(
            err.0
                .contains("rest/repos_o_r_actions_runs_7_approve.post.json")
        );
        std::fs::write(
            dir.join("rest/repos_o_r_actions_runs_7_approve.post.json"),
            r#"{"status":201,"body":{}}"#,
        )
        .unwrap();
        let res = rest(&dir, Verb::Post, url, Some(r#"{"a":1}"#)).unwrap();
        assert_eq!(res.status, 201);
        let (all, _) = write_log::since(0);
        let rec = all
            .iter()
            .rev()
            .find(|r| r.op.ends_with("/runs/7/approve"))
            .unwrap();
        assert_eq!(rec.op, "POST /repos/o/r/actions/runs/7/approve");
        assert_eq!(rec.vars, json!({"a": 1}));
        // A GET-keyed file never answers a write.
        assert!(rest(&dir, Verb::Delete, url, None).is_err());
    }
}
