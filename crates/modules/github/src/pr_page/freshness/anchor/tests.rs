use super::{Anchor, capture, find, restored_offset};

fn keys(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_timeline_at_the_top_keeps_nothing() {
    assert_eq!(capture(&keys(&["a", "b"]), 0, 0.0, false), None);
}

#[test]
fn the_first_visible_card_is_remembered_with_its_place() {
    let a = capture(&keys(&["desc", "c1", "c2"]), 1, -40.0, true).unwrap();
    assert_eq!(
        a,
        Anchor {
            key: "c1".into(),
            rel_top: -40.0
        }
    );
}

#[test]
fn an_unknown_child_is_not_an_anchor() {
    assert_eq!(capture(&keys(&["a"]), 3, 0.0, true), None);
}

#[test]
fn items_added_below_leave_the_offset_alone() {
    // c1 started 40 px above the viewport top, laid out at 100: the
    // offset was -140, and appending cards below doesn't move c1.
    let a = Anchor {
        key: "c1".into(),
        rel_top: -40.0,
    };
    let after = keys(&["desc", "c1", "c2", "c3"]);
    assert_eq!(find(&after, &a), Some(1));
    assert_eq!(restored_offset(&a, 100.0, 0.0), -140.0);
}

#[test]
fn items_added_above_push_the_offset_down_by_their_height() {
    // An older comment of 300 px appears above: c1 is now laid out at
    // 400 and the offset follows, so c1 is still 40 px above the top.
    let a = Anchor {
        key: "c1".into(),
        rel_top: -40.0,
    };
    let after = keys(&["desc", "old", "c1", "c2"]);
    assert_eq!(find(&after, &a), Some(2));
    assert_eq!(restored_offset(&a, 400.0, 0.0), -440.0);
}

#[test]
fn items_removed_above_pull_the_offset_back_and_never_past_the_top() {
    let a = Anchor {
        key: "c1".into(),
        rel_top: 20.0,
    };
    assert_eq!(restored_offset(&a, 100.0, 0.0), -80.0);
    // Everything above went away: the card sits at the top, offset 0.
    assert_eq!(restored_offset(&a, 0.0, 0.0), 0.0);
    assert_eq!(restored_offset(&a, 10.0, 0.0), 0.0);
}

#[test]
fn a_vanished_anchor_is_not_found() {
    let a = Anchor {
        key: "gone".into(),
        rel_top: 0.0,
    };
    assert_eq!(find(&keys(&["desc", "c2"]), &a), None);
}
