use super::*;
use crate::gql::{DiffSide, PrThread, ThreadSubject};

fn thread(resolved: bool) -> PrThread {
    PrThread {
        id: "t".into(),
        path: "a.rs".into(),
        line: Some(1),
        start_line: None,
        side: DiffSide::Right,
        subject: ThreadSubject::Line,
        is_outdated: false,
        is_resolved: resolved,
        resolved_by: None,
        viewer_can_reply: true,
        viewer_can_resolve: true,
        viewer_can_unresolve: true,
        comments: Vec::new(),
        comments_total: 0,
    }
}

fn threads(resolved: &[bool], total: i64) -> PrThreads {
    PrThreads {
        total_count: total,
        nodes: resolved.iter().map(|r| thread(*r)).collect(),
    }
}

#[test]
fn not_loaded_says_nothing() {
    assert_eq!(label(None), None);
}

#[test]
fn zero_unresolved_says_nothing() {
    assert_eq!(label(Some(&threads(&[true, true], 2))), None);
    assert_eq!(label(Some(&threads(&[], 0))), None);
}

#[test]
fn counts_only_the_unresolved() {
    assert_eq!(
        label(Some(&threads(&[false, true, false], 3))).as_deref(),
        Some("2 unresolved")
    );
}

#[test]
fn a_capped_read_is_a_floor() {
    assert_eq!(
        label(Some(&threads(&[false], 150))).as_deref(),
        Some("1+ unresolved")
    );
}
