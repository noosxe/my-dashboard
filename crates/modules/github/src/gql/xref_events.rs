//! Cross-reference rows' wire (docs/0085 D1, D2, D7): `CrossReferencedEvent`
//! read from its own aliased `references` window of `timelineItems`
//! (fields verified against GitHub's schema 2026-11-18). Shared by the PR
//! page and, later, the issue page. Parsing is tolerant (docs/0007 D3): a
//! node that is null, of another type, or whose source is unreadable or
//! missing a field the row needs drops out while the rest parse.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The source's state as the row's glyph reads it. Which glyph an issue or
/// a PR gets for `Closed` is the look's call (it knows the kind).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum XrefState {
    Open,
    Draft,
    Merged,
    Closed,
    /// An issue closed as not planned.
    NotPlanned,
}

/// The issue or PR that made the reference.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct XrefSource {
    pub is_pr: bool,
    pub number: i64,
    pub title: String,
    /// `nameWithOwner`.
    pub repo: String,
    pub url: String,
    pub state: XrefState,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct XrefEvent {
    pub id: String,
    /// `None` for a deleted account; the row then reads "Someone".
    pub actor_login: Option<String>,
    pub created_at: Option<String>,
    /// The source will close the target when merged. Only a PR source
    /// referencing an issue gives it a meaning (the sentence decides).
    pub will_close: bool,
    pub source: XrefSource,
}

/// The distinct references and how many the server counts; the cap row
/// reads the difference.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct XrefEvents {
    pub nodes: Vec<XrefEvent>,
    pub total_count: i64,
}

/// Parse the `references` alias of a PR or issue node. Repeats (same source,
/// same actor) fold into the first (docs/0085 D4).
pub(crate) fn parse_references(page: &Value) -> XrefEvents {
    let mut nodes: Vec<XrefEvent> = Vec::new();
    if let Some(list) = page.pointer("/references/nodes").and_then(Value::as_array) {
        for ev in list.iter().filter_map(parse_event) {
            let repeat = nodes.iter().any(|n| {
                n.actor_login == ev.actor_login
                    && n.source.repo == ev.source.repo
                    && n.source.number == ev.source.number
            });
            if !repeat {
                nodes.push(ev);
            }
        }
    }
    let total_count = page
        .pointer("/references/filteredCount")
        .and_then(Value::as_i64)
        .unwrap_or(nodes.len() as i64);
    XrefEvents { nodes, total_count }
}

fn parse_event(node: &Value) -> Option<XrefEvent> {
    if node.get("__typename")?.as_str()? != "CrossReferencedEvent" {
        return None;
    }
    let text = |p: &str| node.pointer(p).and_then(Value::as_str).map(str::to_string);
    Some(XrefEvent {
        id: text("/id").unwrap_or_default(),
        actor_login: text("/actor/login"),
        created_at: text("/createdAt"),
        will_close: node
            .get("willCloseTarget")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        source: parse_source(node.get("source")?)?,
    })
}

fn parse_source(src: &Value) -> Option<XrefSource> {
    let text = |p: &str| src.pointer(p).and_then(Value::as_str).map(str::to_string);
    let is_pr = match src.get("__typename")?.as_str()? {
        "PullRequest" => true,
        "Issue" => false,
        _ => return None,
    };
    let state = match (is_pr, src.get("state")?.as_str()?) {
        (true, "MERGED") => XrefState::Merged,
        (true, "OPEN") if src.get("isDraft").and_then(Value::as_bool) == Some(true) => {
            XrefState::Draft
        }
        (true, "CLOSED") if src.get("merged").and_then(Value::as_bool) == Some(true) => {
            XrefState::Merged
        }
        (_, "OPEN") => XrefState::Open,
        (false, "CLOSED") if text("/stateReason").as_deref() == Some("NOT_PLANNED") => {
            XrefState::NotPlanned
        }
        (_, "CLOSED") => XrefState::Closed,
        _ => return None,
    };
    let title = text("/title").filter(|t| !t.is_empty())?;
    Some(XrefSource {
        is_pr,
        number: src.get("number")?.as_i64()?,
        title,
        repo: text("/repository/nameWithOwner")?,
        url: text("/url")?,
        state,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn issue(n: i64, state: &str, reason: Value) -> Value {
        json!({"__typename": "Issue", "number": n, "title": format!("Issue {n}"),
            "state": state, "stateReason": reason, "url": format!("https://example.com/i/{n}"),
            "repository": {"nameWithOwner": "fixture-org/beacon", "isPrivate": false}})
    }

    fn pr(n: i64, state: &str, draft: bool, merged: bool) -> Value {
        json!({"__typename": "PullRequest", "number": n, "title": format!("Pr {n}"),
            "state": state, "isDraft": draft, "merged": merged,
            "url": format!("https://example.com/p/{n}"),
            "repository": {"nameWithOwner": "fixture-org/atlas", "isPrivate": true}})
    }

    fn event(id: &str, actor: Option<&str>, source: Value) -> Value {
        json!({"__typename": "CrossReferencedEvent", "id": id,
            "createdAt": "2026-11-01T00:00:00Z",
            "actor": actor.map(|a| json!({"login": a})),
            "isCrossRepository": false, "willCloseTarget": false, "source": source})
    }

    fn parse(nodes: Vec<Value>, count: i64) -> XrefEvents {
        parse_references(&json!({"references": {"filteredCount": count, "nodes": nodes}}))
    }

    #[test]
    fn every_state_of_both_source_kinds_parses() {
        let e = parse(
            vec![
                event("1", Some("octo-dev"), issue(1, "OPEN", Value::Null)),
                event(
                    "2",
                    Some("octo-dev"),
                    issue(2, "CLOSED", json!("COMPLETED")),
                ),
                event(
                    "3",
                    Some("octo-dev"),
                    issue(3, "CLOSED", json!("NOT_PLANNED")),
                ),
                event("4", Some("octo-dev"), pr(4, "OPEN", false, false)),
                event("5", Some("octo-dev"), pr(5, "OPEN", true, false)),
                event("6", Some("octo-dev"), pr(6, "MERGED", false, true)),
                event("7", Some("octo-dev"), pr(7, "CLOSED", false, false)),
            ],
            7,
        );
        let got: Vec<_> = e
            .nodes
            .iter()
            .map(|n| (n.source.is_pr, n.source.state))
            .collect();
        assert_eq!(
            got,
            vec![
                (false, XrefState::Open),
                (false, XrefState::Closed),
                (false, XrefState::NotPlanned),
                (true, XrefState::Open),
                (true, XrefState::Draft),
                (true, XrefState::Merged),
                (true, XrefState::Closed),
            ]
        );
        assert_eq!(e.nodes[3].source.repo, "fixture-org/atlas");
        assert_eq!(e.nodes[0].source.url, "https://example.com/i/1");
        assert_eq!(e.total_count, 7);
    }

    #[test]
    fn a_null_actor_is_kept_and_will_close_is_read() {
        let mut ev = event("1", None, pr(9, "OPEN", false, false));
        ev["willCloseTarget"] = json!(true);
        let e = parse(vec![ev], 1);
        assert_eq!(e.nodes[0].actor_login, None);
        assert!(e.nodes[0].will_close);
    }

    #[test]
    fn unreadable_nodes_drop_and_the_count_keeps_the_cap_honest() {
        let mut no_title = issue(3, "OPEN", Value::Null);
        no_title["title"] = Value::Null;
        let mut no_number = issue(4, "OPEN", Value::Null);
        no_number["number"] = Value::Null;
        let mut no_repo = issue(5, "OPEN", Value::Null);
        no_repo["repository"] = Value::Null;
        let e = parse(
            vec![
                Value::Null,
                event("1", Some("a"), Value::Null),
                event("2", Some("a"), json!({})),
                event("3", Some("a"), no_title),
                event("4", Some("a"), no_number),
                event("5", Some("a"), no_repo),
                json!({"__typename": "ReferencedEvent", "id": "x"}),
                event("6", Some("a"), issue(6, "OPEN", Value::Null)),
            ],
            8,
        );
        assert_eq!(e.nodes.len(), 1);
        assert_eq!(e.nodes[0].id, "6");
        assert_eq!(e.total_count - e.nodes.len() as i64, 7);
    }

    #[test]
    fn repeats_of_one_source_and_actor_fold_but_others_do_not() {
        let e = parse(
            vec![
                event("1", Some("a"), issue(1, "OPEN", Value::Null)),
                event("2", Some("a"), issue(1, "OPEN", Value::Null)),
                event("3", Some("b"), issue(1, "OPEN", Value::Null)),
                event("4", Some("a"), issue(2, "OPEN", Value::Null)),
                event("5", Some("a"), pr(1, "OPEN", false, false)),
            ],
            5,
        );
        let ids: Vec<_> = e.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["1", "3", "4", "5"]);
    }

    #[test]
    fn a_payload_without_references_is_empty() {
        assert_eq!(parse_references(&json!({})), XrefEvents::default());
    }

    /// Document-level tripwire: the alias is its own window of the newest
    /// 100 and every fragment carries the fields the parser reads.
    #[test]
    fn the_selection_is_its_own_newest_window_with_every_field() {
        let q = crate::gql::PR_DETAIL_QUERY
            .split("references: timelineItems")
            .nth(1)
            .unwrap()
            .split("isInMergeQueue")
            .next()
            .unwrap();
        let q = &format!("references: timelineItems{q}");
        assert!(
            q.contains("references: timelineItems(last: 100, itemTypes: [CROSS_REFERENCED_EVENT])")
        );
        assert!(!q.contains("REFERENCED_EVENT, ") && !q.contains(", REFERENCED_EVENT"));
        for f in [
            "filteredCount",
            "id createdAt actor { login }",
            "willCloseTarget",
            "... on Issue { number title state stateReason url",
            "... on PullRequest { number title state isDraft merged url",
            "repository { nameWithOwner isPrivate }",
        ] {
            assert!(q.contains(f), "missing {f}");
        }
        assert!(!q.contains("//"));
    }

    /// D7: GitHub nulls a list element whose non-null `source` the viewer
    /// cannot read and adds an `errors` entry. The page read keeps the
    /// partial data and the readable rows.
    #[test]
    fn a_partial_errors_envelope_keeps_the_page_and_the_readable_rows() {
        let body = json!({
            "data": {"repository": {"pullRequest": {
                "id": "P", "number": 1, "title": "t", "state": "OPEN", "isDraft": false,
                "references": {"filteredCount": 2, "nodes": [
                    null,
                    event("E2", Some("octo-dev"), issue(7, "OPEN", Value::Null))
                ]}
            }}},
            "errors": [{"message": "Resource not accessible", "type": "FORBIDDEN"}]
        })
        .to_string();
        let env: crate::gql::Envelope<crate::gql::PrDetailData> =
            serde_json::from_str(&body).unwrap();
        assert_eq!(env.errors.len(), 1);
        let detail = env
            .data
            .and_then(|d| d.repository)
            .as_ref()
            .and_then(crate::gql::pr_detail_from_value)
            .expect("the page survives a null list element");
        assert_eq!(detail.references.nodes.len(), 1);
        assert_eq!(detail.references.nodes[0].source.number, 7);
        assert_eq!(detail.references.total_count, 2);
    }
}
