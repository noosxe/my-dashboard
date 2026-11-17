//! The PR conversation's event rows (#787, #911): merged, closed, reopened,
//! force-pushed, labeled/unlabeled, assigned/unassigned, review
//! requested/removed, renamed and milestoned/demilestoned, read from `timelineItems` (fields verified against
//! GitHub's schema 2026-11-17). Parsing is tolerant (docs/0007 D3): a node
//! of another type, or one missing a field its row needs, drops out while
//! the rest parse.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One event on the PR timeline. Read-only decoration: no row acts.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct PrEvent {
    /// The event's node id; the timeline's scroll anchor keys on it.
    pub id: String,
    /// `None` for a deleted account; the row then reads "Someone".
    pub actor: Option<String>,
    pub created_at: Option<String>,
    pub kind: PrEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub enum PrEventKind {
    Merged,
    Closed,
    Reopened,
    /// The abbreviated commit ids before and after the push, when readable.
    ForcePushed {
        before: Option<String>,
        after: Option<String>,
    },
    /// A label added or removed (#911). The colour is the label's hex.
    Labeled {
        name: String,
        color: Option<String>,
    },
    Unlabeled {
        name: String,
        color: Option<String>,
    },
    /// Someone assigned or unassigned, by login (#911). The actor may be
    /// the assignee: the row then reads "self-assigned".
    Assigned {
        assignee: String,
    },
    Unassigned {
        assignee: String,
    },
    /// A review requested of, or a request removed for, a user or a team
    /// (#911): the login, or the team's `org/slug`.
    ReviewRequested {
        reviewer: String,
    },
    ReviewRequestRemoved {
        reviewer: String,
    },
    /// The title changed from one text to another (#911).
    Renamed {
        from: String,
        to: String,
    },
    Milestoned {
        title: String,
    },
    Demilestoned {
        title: String,
    },
}

/// The events and how many the server counts for the filter; the cap row
/// reads the difference.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct PrEvents {
    pub total_count: i64,
    pub nodes: Vec<PrEvent>,
}

/// The two aliased selections of the detail query (`events`: state changes,
/// `activity`: labels, assignees, review requests, title, milestone), each
/// capped on its own so a burst of one kind cannot push the other out. The
/// nodes are the two lists together (the page sorts by time) and the count
/// is the sum of both `filteredCount`s.
pub(crate) fn parse_events(pr: &Value) -> PrEvents {
    let mut nodes = Vec::new();
    let mut total_count = 0;
    for alias in ["events", "activity"] {
        let parsed: Vec<PrEvent> = pr
            .pointer(&format!("/{alias}/nodes"))
            .and_then(Value::as_array)
            .map(|nodes| nodes.iter().filter_map(parse_event).collect())
            .unwrap_or_default();
        total_count += pr
            .pointer(&format!("/{alias}/filteredCount"))
            .and_then(Value::as_i64)
            .unwrap_or(parsed.len() as i64);
        nodes.extend(parsed);
    }
    PrEvents { total_count, nodes }
}

fn parse_event(node: &Value) -> Option<PrEvent> {
    let oid = |p: &str| node.pointer(p).and_then(Value::as_str).map(str::to_string);
    let kind = match node.get("__typename")?.as_str()? {
        "MergedEvent" => PrEventKind::Merged,
        "ClosedEvent" => PrEventKind::Closed,
        "ReopenedEvent" => PrEventKind::Reopened,
        "HeadRefForcePushedEvent" => PrEventKind::ForcePushed {
            before: oid("/beforeCommit/abbreviatedOid"),
            after: oid("/afterCommit/abbreviatedOid"),
        },
        "LabeledEvent" => {
            let (name, color) = label(node)?;
            PrEventKind::Labeled { name, color }
        }
        "UnlabeledEvent" => {
            let (name, color) = label(node)?;
            PrEventKind::Unlabeled { name, color }
        }
        "AssignedEvent" => PrEventKind::Assigned {
            assignee: oid("/assignee/login")?,
        },
        "UnassignedEvent" => PrEventKind::Unassigned {
            assignee: oid("/assignee/login")?,
        },
        "ReviewRequestedEvent" => PrEventKind::ReviewRequested {
            reviewer: reviewer(node)?,
        },
        "ReviewRequestRemovedEvent" => PrEventKind::ReviewRequestRemoved {
            reviewer: reviewer(node)?,
        },
        "RenamedTitleEvent" => PrEventKind::Renamed {
            from: oid("/previousTitle")?,
            to: oid("/currentTitle")?,
        },
        "MilestonedEvent" => PrEventKind::Milestoned {
            title: oid("/milestoneTitle")?,
        },
        "DemilestonedEvent" => PrEventKind::Demilestoned {
            title: oid("/milestoneTitle")?,
        },
        _ => return None,
    };
    Some(PrEvent {
        id: node
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        actor: oid("/actor/login"),
        created_at: oid("/createdAt"),
        kind,
    })
}

/// The label's name and colour; a node without a name drops out.
fn label(node: &Value) -> Option<(String, Option<String>)> {
    let name = node.pointer("/label/name")?.as_str()?.to_string();
    let color = node
        .pointer("/label/color")
        .and_then(Value::as_str)
        .map(str::to_string);
    Some((name, color))
}

/// The requested reviewer: a user's, bot's or mannequin's login, else a
/// team's `org/slug`. A deleted reviewer (null) drops the row.
fn reviewer(node: &Value) -> Option<String> {
    ["login", "combinedSlug"]
        .iter()
        .find_map(|k| node.pointer(&format!("/requestedReviewer/{k}")))
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_four_kinds_parse_and_others_drop() {
        let pr = json!({"events": {"filteredCount": 7, "nodes": [
            {"__typename": "HeadRefForcePushedEvent", "id": "E1",
             "createdAt": "2026-10-22T00:00:00Z", "actor": {"login": "a"},
             "beforeCommit": {"abbreviatedOid": "1111111"},
             "afterCommit": {"abbreviatedOid": "2222222"}},
            {"__typename": "ClosedEvent", "id": "E2", "actor": null},
            {"__typename": "ReopenedEvent", "id": "E3"},
            {"__typename": "MergedEvent", "id": "E4", "actor": {"login": "b"}},
            {"__typename": "LabeledEvent", "id": "E5"},
            {"id": "E6"}
        ]}});
        let e = parse_events(&pr);
        assert_eq!(e.total_count, 7);
        let kinds: Vec<_> = e.nodes.iter().map(|n| n.kind.clone()).collect();
        assert_eq!(
            kinds,
            vec![
                PrEventKind::ForcePushed {
                    before: Some("1111111".into()),
                    after: Some("2222222".into())
                },
                PrEventKind::Closed,
                PrEventKind::Reopened,
                PrEventKind::Merged,
            ]
        );
        assert_eq!(e.nodes[0].actor.as_deref(), Some("a"));
        assert_eq!(e.nodes[1].actor, None);
    }

    #[test]
    fn labels_parse_with_colour_and_a_nameless_one_drops() {
        let pr = json!({"events": {"nodes": [
            {"__typename": "LabeledEvent", "id": "L1", "actor": {"login": "a"},
             "label": {"name": "bug", "color": "d73a4a"}},
            {"__typename": "UnlabeledEvent", "id": "L2",
             "label": {"name": "wip"}},
            {"__typename": "LabeledEvent", "id": "L3", "label": null}
        ]}});
        let kinds: Vec<_> = parse_events(&pr)
            .nodes
            .into_iter()
            .map(|n| n.kind)
            .collect();
        assert_eq!(
            kinds,
            vec![
                PrEventKind::Labeled {
                    name: "bug".into(),
                    color: Some("d73a4a".into())
                },
                PrEventKind::Unlabeled {
                    name: "wip".into(),
                    color: None
                },
            ]
        );
    }

    #[test]
    fn assignments_parse_and_a_deleted_assignee_drops() {
        let pr = json!({"events": {"nodes": [
            {"__typename": "AssignedEvent", "id": "A1", "actor": {"login": "a"},
             "assignee": {"login": "b"}},
            {"__typename": "UnassignedEvent", "id": "A2", "assignee": {"login": "c"}},
            {"__typename": "AssignedEvent", "id": "A3", "assignee": null}
        ]}});
        let kinds: Vec<_> = parse_events(&pr)
            .nodes
            .into_iter()
            .map(|n| n.kind)
            .collect();
        assert_eq!(
            kinds,
            vec![
                PrEventKind::Assigned {
                    assignee: "b".into()
                },
                PrEventKind::Unassigned {
                    assignee: "c".into()
                },
            ]
        );
    }

    #[test]
    fn review_requests_name_a_user_or_a_team() {
        let pr = json!({"events": {"nodes": [
            {"__typename": "ReviewRequestedEvent", "id": "R1",
             "requestedReviewer": {"login": "b"}},
            {"__typename": "ReviewRequestRemovedEvent", "id": "R2",
             "requestedReviewer": {"combinedSlug": "org/core"}},
            {"__typename": "ReviewRequestedEvent", "id": "R3", "requestedReviewer": null},
            {"__typename": "ReviewRequestedEvent", "id": "R4", "requestedReviewer": {}}
        ]}});
        let kinds: Vec<_> = parse_events(&pr)
            .nodes
            .into_iter()
            .map(|n| n.kind)
            .collect();
        assert_eq!(
            kinds,
            vec![
                PrEventKind::ReviewRequested {
                    reviewer: "b".into()
                },
                PrEventKind::ReviewRequestRemoved {
                    reviewer: "org/core".into()
                },
            ]
        );
    }

    #[test]
    fn renames_and_milestones_parse_and_incomplete_ones_drop() {
        let pr = json!({"events": {"nodes": [
            {"__typename": "RenamedTitleEvent", "id": "T1",
             "previousTitle": "old", "currentTitle": "new"},
            {"__typename": "RenamedTitleEvent", "id": "T2", "currentTitle": "new"},
            {"__typename": "MilestonedEvent", "id": "M1", "milestoneTitle": "v1"},
            {"__typename": "DemilestonedEvent", "id": "M2", "milestoneTitle": "v1"},
            {"__typename": "MilestonedEvent", "id": "M3"}
        ]}});
        let kinds: Vec<_> = parse_events(&pr)
            .nodes
            .into_iter()
            .map(|n| n.kind)
            .collect();
        assert_eq!(
            kinds,
            vec![
                PrEventKind::Renamed {
                    from: "old".into(),
                    to: "new".into()
                },
                PrEventKind::Milestoned { title: "v1".into() },
                PrEventKind::Demilestoned { title: "v1".into() },
            ]
        );
    }

    /// The two selections are capped separately: a burst of chatty events
    /// fills only `activity`, so a state event keeps its place and the
    /// hidden count adds both connections' overflow.
    #[test]
    fn a_burst_of_activity_cannot_push_a_state_event_out() {
        let labels: Vec<Value> = (0..100)
            .map(|i| {
                json!({"__typename": "LabeledEvent", "id": format!("L{i}"),
                    "label": {"name": "x"}})
            })
            .collect();
        let pr = json!({
            "events": {"filteredCount": 1, "nodes": [
                {"__typename": "MergedEvent", "id": "M"}]},
            "activity": {"filteredCount": 250, "nodes": labels}
        });
        let e = parse_events(&pr);
        assert!(e.nodes.iter().any(|n| n.kind == PrEventKind::Merged));
        assert_eq!(e.nodes.len(), 101);
        assert_eq!(e.total_count, 251);
        assert_eq!(e.total_count - e.nodes.len() as i64, 150);
    }

    #[test]
    fn the_query_caps_state_events_apart_from_the_chatty_ones() {
        let q = crate::gql::PR_DETAIL_QUERY;
        let state = q.split("events: timelineItems").nth(1).unwrap();
        let state = state.split("activity: timelineItems").next().unwrap();
        assert!(state.contains("MERGED_EVENT") && state.contains("CLOSED_EVENT"));
        assert!(!state.contains("LABELED_EVENT"));
        let chatty = q.split("activity: timelineItems").nth(1).unwrap();
        assert!(chatty.starts_with("(last: 100,"));
        assert!(chatty.contains("LABELED_EVENT") && !chatty.contains("MERGED_EVENT"));
    }

    #[test]
    fn a_payload_without_events_is_empty() {
        let e = parse_events(&json!({}));
        assert_eq!(e, PrEvents::default());
    }
}
