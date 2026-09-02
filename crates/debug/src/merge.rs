//! `tree.dump` merge (docs/0010 D4): upstream's
//! `debug_a11y_tree_json()` ⊕ the AT-SPI walk, joined on `accesskit_id`.
//!
//! The dump is a flat `nodes` map keyed by ephemeral ids (`a`, `b`, …);
//! each node object carries its stable `accesskit_id` (the decimal
//! `u128(NodeId)` — the same value that forms the AT-SPI object path).
//! The merge injects `bounds` / `focusable` / `focused` into every node
//! present on both sides and leaves the rest untouched, reporting counts
//! so a drifting tree is visible in the response instead of silent.

use std::collections::HashMap;

use serde_json::Value;

use crate::a11y::NodeInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MergeStats {
    pub dump_nodes: usize,
    pub enriched: usize,
    /// Dump nodes the AT-SPI walk didn't report (unexposed/pruned or a
    /// frame racing the walk) — no bounds/focus fields were added.
    pub unmatched_dump: usize,
    /// Walk entries with no dump node (stale AT-SPI side).
    pub unmatched_walk: usize,
}

/// Merge AT-SPI enrichment into a `tree.dump` JSON value, in place.
pub fn merge_dump(dump: &mut Value, walk: &HashMap<String, NodeInfo>) -> MergeStats {
    let mut stats = MergeStats {
        unmatched_walk: walk.len(),
        ..MergeStats::default()
    };
    let Some(nodes) = dump.get_mut("nodes").and_then(Value::as_object_mut) else {
        return stats;
    };
    stats.dump_nodes = nodes.len();

    for node in nodes.values_mut() {
        let Some(accesskit_id) = node.get("accesskit_id").and_then(Value::as_str) else {
            stats.unmatched_dump += 1;
            continue;
        };
        let Some(info) = walk.get(accesskit_id) else {
            stats.unmatched_dump += 1;
            continue;
        };
        stats.unmatched_walk = stats.unmatched_walk.saturating_sub(1);
        stats.enriched += 1;
        let Some(object) = node.as_object_mut() else {
            continue;
        };
        if let Some((x, y, w, h)) = info.bounds {
            object.insert(
                "bounds".into(),
                serde_json::json!({"x": x, "y": y, "w": w, "h": h}),
            );
        }
        object.insert("focusable".into(), Value::Bool(info.focusable));
        object.insert("focused".into(), Value::Bool(info.focused));
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dump_fixture() -> Value {
        serde_json::json!({
            "root": "a",
            "gpui_focus": "c",
            "frame": {"frame_number": 7, "window_title": "My Dashboard"},
            "nodes": {
                "a": {"accesskit_id": "10", "children": ["b", "c"], "aria": {"role": "Window"}},
                "b": {"accesskit_id": "20", "children": ["d"], "aria": {"role": "Button", "label": "Save"}},
                "c": {"accesskit_id": "30", "aria": {"role": "TextInput"}},
                "d": {"aria": {"role": "Generic"}}
            }
        })
    }

    fn info(bounds: Option<(i32, i32, i32, i32)>, focusable: bool, focused: bool) -> NodeInfo {
        NodeInfo {
            bounds,
            focusable,
            focused,
        }
    }

    #[test]
    fn enrichment_lands_on_matching_nodes() {
        let mut dump = dump_fixture();
        let mut walk = HashMap::new();
        walk.insert("10".into(), info(Some((0, 0, 800, 600)), false, false));
        walk.insert("20".into(), info(Some((10, 20, 90, 30)), true, false));
        walk.insert("30".into(), info(Some((10, 60, 200, 28)), true, true));

        let stats = merge_dump(&mut dump, &walk);

        assert_eq!(
            stats,
            MergeStats {
                dump_nodes: 4,
                enriched: 3,
                unmatched_dump: 1, // "d" has no accesskit_id at all
                unmatched_walk: 0,
            }
        );
        let nodes = &dump["nodes"];
        assert_eq!(
            nodes["b"]["bounds"],
            serde_json::json!({"x": 10, "y": 20, "w": 90, "h": 30})
        );
        assert_eq!(nodes["b"]["focusable"], Value::Bool(true));
        assert_eq!(nodes["c"]["focused"], Value::Bool(true));
        // Untouched nodes stay untouched.
        assert!(nodes["d"].get("bounds").is_none());
        assert!(nodes["d"].get("focused").is_none());
        // Existing fields survive.
        assert_eq!(nodes["b"]["aria"]["label"], "Save");
    }

    #[test]
    fn nodes_missing_on_the_atspi_side_keep_their_shape() {
        let mut dump = dump_fixture();
        let walk = HashMap::new(); // AT-SPI reported nothing (e.g. raced frame)

        let stats = merge_dump(&mut dump, &walk);

        assert_eq!(stats.dump_nodes, 4);
        assert_eq!(stats.enriched, 0);
        assert_eq!(stats.unmatched_dump, 4);
        assert_eq!(stats.unmatched_walk, 0);
        assert!(dump["nodes"]["a"].get("bounds").is_none());
    }

    #[test]
    fn stale_walk_entries_are_counted_not_injected() {
        let mut dump = dump_fixture();
        let mut walk = HashMap::new();
        walk.insert("20".into(), info(Some((1, 2, 3, 4)), true, false));
        walk.insert("9999".into(), info(Some((9, 9, 9, 9)), false, false));

        let stats = merge_dump(&mut dump, &walk);

        assert_eq!(stats.enriched, 1);
        assert_eq!(stats.unmatched_walk, 1);
        assert!(dump["nodes"]["a"].get("bounds").is_none());
        // No node for 9999 was fabricated.
        assert!(dump["nodes"].as_object().unwrap().len() == 4);
    }

    #[test]
    fn missing_bounds_leaves_the_field_out() {
        let mut dump = dump_fixture();
        let mut walk = HashMap::new();
        // Node exposes Accessible but no Component interface.
        walk.insert("20".into(), info(None, true, false));

        merge_dump(&mut dump, &walk);

        assert!(dump["nodes"]["b"].get("bounds").is_none());
        assert_eq!(dump["nodes"]["b"]["focusable"], Value::Bool(true));
    }

    #[test]
    fn malformed_dump_returns_zeroes_without_panicking() {
        let mut dump = serde_json::json!({"root": "a"}); // no "nodes"
        let stats = merge_dump(&mut dump, &HashMap::new());
        assert_eq!(stats, MergeStats::default());
    }
}
