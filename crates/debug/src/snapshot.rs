//! Named snapshots and structural diff over `tree.dump` (docs/0055 D7).
//!
//! Pure: everything here works on [`serde_json::Value`]s and files are the
//! caller's problem. A snapshot is the merged dump flattened to the fields
//! receipts reason about, keyed on `accesskit_id` (stable for the run) —
//! order-insensitive by construction, so two dumps of the same frame with
//! different walk orders diff clean.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Window-relative content px, matching the dump's `bounds` object.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// One flattened node: the fields a receipt reasons about.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotNode {
    pub accesskit_id: String,
    pub role: String,
    pub label: Option<String>,
    pub bounds: Option<Bounds>,
    pub focusable: bool,
    pub focused: bool,
}

/// `snapshots/<name>.json` — written by the CLI, read back by `--diff`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub name: String,
    pub frame_number: u64,
    pub taken_at: u64,
    pub nodes: Vec<SnapshotNode>,
}

impl Snapshot {
    /// Flatten a `tree.dump` result. Nodes without an `accesskit_id` are
    /// skipped (unmatchable by construction); survivors are sorted by id
    /// (numeric when it parses, string otherwise) so the file is byte-
    /// stable for identical dumps.
    pub fn from_dump(name: &str, dump: &Value) -> Result<Self, String> {
        let frame_number = dump["frame"]["frame_number"]
            .as_u64()
            .ok_or_else(|| "dump has no frame.frame_number".to_string())?;
        let mut nodes = Vec::new();
        if let Some(map) = dump["nodes"].as_object() {
            for node in map.values() {
                let Some(accesskit_id) = node["accesskit_id"].as_str() else {
                    continue;
                };
                nodes.push(SnapshotNode {
                    accesskit_id: accesskit_id.to_string(),
                    role: node["aria"]["role"]
                        .as_str()
                        .unwrap_or("(no role)")
                        .to_string(),
                    label: node["aria"]["label"].as_str().map(normalize_wall_clock),
                    bounds: parse_bounds(node.get("bounds")),
                    focusable: node["focusable"].as_bool().unwrap_or(false),
                    focused: node["focused"].as_bool().unwrap_or(false),
                });
            }
        }
        nodes.sort_by_key(|node| sort_key(&node.accesskit_id));
        Ok(Self {
            name: name.to_string(),
            frame_number,
            taken_at: unix_now(),
            nodes,
        })
    }
}

/// Deterministic sort key: numeric ids numerically, anything else
/// lexicographic after all numeric ones.
fn sort_key(id: &str) -> (u8, u128, String) {
    match id.parse::<u128>() {
        Ok(n) => (0, n, String::new()),
        Err(_) => (1, 0, id.to_string()),
    }
}

fn parse_bounds(value: Option<&Value>) -> Option<Bounds> {
    let value = value.filter(|v| !v.is_null())?;
    Some(Bounds {
        x: value["x"].as_f64()? as f32,
        y: value["y"].as_f64()? as f32,
        w: value["w"].as_f64()? as f32,
        h: value["h"].as_f64()? as f32,
    })
}
/// Wall-clock times in status labels churn every capture ("0 flights ·
/// as of 23:15 · oldest retained —", the usage page's status line) and
/// are never structural — normalize them to fixed tokens so goldens
/// diff clean. Same class as the zeroed frame/timestamp fields: the
/// diff must never read capture-local values. A time is `H:MM` or
/// `HH:MM` (optionally `:SS`), not part of a longer digit run and not
/// glued to alphanumerics on either side ("v1.2.3", "a1:23" and
/// "123:45" are left alone).
fn normalize_wall_clock(label: &str) -> String {
    let b = label.as_bytes();
    let digit = |i: usize| b.get(i).is_some_and(|c| c.is_ascii_digit());
    let alnum = |i: usize| b.get(i).is_some_and(|c| c.is_ascii_alphanumeric());
    let mut out = String::with_capacity(label.len());
    let mut copied = 0;
    let mut i = 1; // a time colon is never the first byte
    while i < b.len() {
        if b[i] == b':' && digit(i + 1) && digit(i + 2) && !alnum(i + 3) {
            // Leading run of exactly one or two digits, bounded by a
            // non-alphanumeric (or the string start) on its left.
            let lead = if digit(i - 1) {
                if i >= 2 && digit(i - 2) {
                    if i >= 3 && alnum(i - 3) {
                        None
                    } else {
                        Some(i - 2)
                    }
                } else if i >= 2 && alnum(i - 2) {
                    None
                } else {
                    Some(i - 1)
                }
            } else {
                None
            };
            if let Some(start) = lead {
                out.push_str(&label[copied..start]);
                out.push_str("HH:MM");
                copied = i + 3;
                i = copied;
                if b.get(i) == Some(&b':') && digit(i + 1) && digit(i + 2) && !alnum(i + 3) {
                    out.push_str(":SS");
                    copied = i + 3;
                    i = copied;
                }
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&label[copied..]);
    out
}

/// Snapshot names become file names: `[A-Za-z0-9._-]{1,64}`, no leading
/// dot, no `..` — path traversal stays out of the grammar.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && !name.contains("..")
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

/// Where snapshot files live: next to the redirected state dir (the bound
/// session's `snapshots/`, 0055 D1). Derived from `DASHBOARD_STATE_DIR`
/// so both `--snapshot` and `--diff` agree without extra state.
pub fn snapshots_root(state_dir: &Path) -> PathBuf {
    state_dir
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("snapshots")
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One per-node difference. `label`/`bounds`/`focus` carry old → new for
/// the aspects that changed; the node line keeps enough context (role,
/// current label) to read without the other snapshot open.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeDiff {
    pub accesskit_id: String,
    pub kind: DiffKind,
    pub role: String,
    pub label: Option<String>,
    pub label_change: Option<(Option<String>, Option<String>)>,
    pub bounds_change: Option<(Option<Bounds>, Option<Bounds>)>,
    pub focus_change: Option<(bool, bool)>,
}

/// `--diff` output: the summary counts plus per-node entries sorted by id.
#[derive(Debug, Clone, PartialEq)]
pub struct Diff {
    pub frame_a: u64,
    pub frame_b: u64,
    pub entries: Vec<NodeDiff>,
}

impl Diff {
    pub fn changed(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| {
                e.label_change.is_some() || e.bounds_change.is_some() || e.focus_change.is_some()
            })
            .count()
    }

    pub fn added(&self) -> usize {
        self.entries.iter().filter(|e| e.is_added()).count()
    }

    pub fn removed(&self) -> usize {
        self.entries.iter().filter(|e| e.is_removed()).count()
    }
}

/// Which side of the pair the node exists on, or that it changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    Added,
    Removed,
    Changed,
}

impl DiffKind {
    fn as_str(self) -> &'static str {
        match self {
            DiffKind::Added => "added",
            DiffKind::Removed => "removed",
            DiffKind::Changed => "changed",
        }
    }
}

impl NodeDiff {
    fn is_added(&self) -> bool {
        self.kind == DiffKind::Added
    }

    fn is_removed(&self) -> bool {
        self.kind == DiffKind::Removed
    }
}

/// Build an added-node entry (context from snapshot b).
pub fn added(node: &SnapshotNode) -> NodeDiff {
    NodeDiff {
        accesskit_id: node.accesskit_id.clone(),
        kind: DiffKind::Added,
        role: node.role.clone(),
        label: node.label.clone(),
        label_change: None,
        bounds_change: None,
        focus_change: None,
    }
}

/// Build a removed-node entry (context from snapshot a).
pub fn removed(node: &SnapshotNode) -> NodeDiff {
    NodeDiff {
        accesskit_id: node.accesskit_id.clone(),
        kind: DiffKind::Removed,
        role: node.role.clone(),
        label: node.label.clone(),
        label_change: None,
        bounds_change: None,
        focus_change: None,
    }
}

/// Compare two snapshots keyed on `accesskit_id`. Bounds compare exactly —
/// fixed-size headless output makes geometry deterministic (D7).
pub fn diff(a: &Snapshot, b: &Snapshot) -> Diff {
    let map_a: BTreeMap<&str, &SnapshotNode> = a
        .nodes
        .iter()
        .map(|n| (n.accesskit_id.as_str(), n))
        .collect();
    let map_b: BTreeMap<&str, &SnapshotNode> = b
        .nodes
        .iter()
        .map(|n| (n.accesskit_id.as_str(), n))
        .collect();

    let mut entries = Vec::new();
    for (id, node_a) in &map_a {
        match map_b.get(id) {
            None => entries.push(removed(node_a)),
            Some(node_b) => {
                let label_change = if node_a.label == node_b.label {
                    None
                } else {
                    Some((node_a.label.clone(), node_b.label.clone()))
                };
                let bounds_change = if node_a.bounds == node_b.bounds {
                    None
                } else {
                    Some((node_a.bounds, node_b.bounds))
                };
                let focus_change = if node_a.focused == node_b.focused {
                    None
                } else {
                    Some((node_a.focused, node_b.focused))
                };
                if label_change.is_some() || bounds_change.is_some() || focus_change.is_some() {
                    entries.push(NodeDiff {
                        accesskit_id: (*id).to_string(),
                        kind: DiffKind::Changed,
                        role: node_b.role.clone(),
                        label: node_b.label.clone(),
                        label_change,
                        bounds_change,
                        focus_change,
                    });
                }
            }
        }
    }
    for (id, node_b) in &map_b {
        if !map_a.contains_key(id) {
            entries.push(added(node_b));
        }
    }

    entries.sort_by_key(|entry| sort_key(&entry.accesskit_id));

    Diff {
        frame_a: a.frame_number,
        frame_b: b.frame_number,
        entries,
    }
}

/// The human rendering: summary first, then one line per node with the
/// D7 markers `−` (removed), `+` (added), `~` (changed).
pub fn render_human(diff: &Diff) -> String {
    let mut out = format!(
        "frame {} → {} · {} changed · {} added · {} removed",
        diff.frame_a,
        diff.frame_b,
        diff.changed(),
        diff.added(),
        diff.removed()
    );
    for entry in &diff.entries {
        if entry.is_removed() {
            out.push_str(&format!(
                "\n− #{} {} {}",
                entry.accesskit_id,
                entry.role,
                quoted(&entry.label)
            ));
            continue;
        }
        if entry.is_added() {
            out.push_str(&format!(
                "\n+ #{} {} {}",
                entry.accesskit_id,
                entry.role,
                quoted(&entry.label)
            ));
            continue;
        }
        let mut line = format!("\n~ #{} {}", entry.accesskit_id, entry.role);
        if let Some((old, new)) = &entry.label_change {
            line.push_str(&format!(" {} → {}", quoted(old), quoted(new)));
        } else if let Some(label) = &entry.label {
            line.push_str(&format!(" {label:?}"));
        }
        if let Some((old, new)) = &entry.bounds_change {
            line.push_str(&format!(" {} → {}", bounds_text(*old), bounds_text(*new)));
        }
        if let Some((old, new)) = &entry.focus_change {
            line.push_str(&format!(" focused={old} → focused={new}"));
        }
        out.push_str(&line);
    }
    out
}

fn quoted(label: &Option<String>) -> String {
    match label {
        Some(label) => format!("{label:?}"),
        None => "(no label)".to_string(),
    }
}

fn bounds_text(bounds: Option<Bounds>) -> String {
    match bounds {
        Some(b) => format!("({},{}) {}×{}", b.x, b.y, b.w, b.h),
        None => "(no bounds)".to_string(),
    }
}

/// The `--json` rendering: the same diff as machine-readable pairs.
pub fn render_json(diff: &Diff) -> Value {
    let entries: Vec<Value> = diff
        .entries
        .iter()
        .map(|entry| {
            let mut object = serde_json::json!({
                "kind": entry.kind.as_str(),
                "accesskit_id": entry.accesskit_id,
                "role": entry.role,
            });
            if let Some(label) = &entry.label {
                object["label"] = serde_json::json!(label);
            }
            if let Some((old, new)) = &entry.label_change {
                object["label_change"] = serde_json::json!({"from": old, "to": new});
            }
            if let Some((old, new)) = &entry.bounds_change {
                object["bounds_change"] =
                    serde_json::json!({"from": bounds_json(*old), "to": bounds_json(*new)});
            }
            if let Some((old, new)) = &entry.focus_change {
                object["focus_change"] = serde_json::json!({"from": old, "to": new});
            }
            object
        })
        .collect();
    serde_json::json!({
        "frame": {"a": diff.frame_a, "b": diff.frame_b},
        "changed": diff.changed(),
        "added": diff.added(),
        "removed": diff.removed(),
        "entries": entries,
    })
}

fn bounds_json(bounds: Option<Bounds>) -> Value {
    match bounds {
        Some(b) => serde_json::json!({"x": b.x, "y": b.y, "w": b.w, "h": b.h}),
        None => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dump(frame: u64, nodes: Value) -> Value {
        json!({
            "frame": {"frame_number": frame, "rendered_at": "2026-09-22T00:00:00Z"},
            "root": "r",
            "nodes": nodes,
        })
    }

    fn node(id: &str, role: &str, label: Option<&str>, bounds: Option<Value>) -> Value {
        let mut object = json!({"accesskit_id": id, "aria": {"role": role}});
        if let Some(label) = label {
            object["aria"]["label"] = json!(label);
        }
        if let Some(bounds) = bounds {
            object["bounds"] = bounds;
        }
        object
    }

    #[test]
    fn from_dump_flattens_sorts_and_skips_idless() {
        let dump = dump(
            41,
            json!({
                "b": node("20", "Button", Some("Save"), Some(json!({"x": 0, "y": 0, "w": 80.0, "h": 24.0}))),
                "a": node("10", "Window", None, None),
                "c": {"aria": {"role": "Generic"}},
            }),
        );
        let snapshot = Snapshot::from_dump("a", &dump).unwrap();
        assert_eq!(snapshot.frame_number, 41);
        assert_eq!(snapshot.nodes.len(), 2);
        assert_eq!(snapshot.nodes[0].accesskit_id, "10");
        assert_eq!(snapshot.nodes[0].role, "Window");
        assert!(snapshot.nodes[0].bounds.is_none());
        assert_eq!(snapshot.nodes[1].label.as_deref(), Some("Save"));
        assert_eq!(
            snapshot.nodes[1].bounds,
            Some(Bounds {
                x: 0.0,
                y: 0.0,
                w: 80.0,
                h: 24.0
            })
        );
    }

    #[test]
    fn from_dump_requires_frame_number() {
        assert!(Snapshot::from_dump("a", &json!({"nodes": {}})).is_err());
    }

    #[test]
    fn wall_clock_labels_normalize_deterministically() {
        // The real churner: the usage page status line (#351 dump).
        assert_eq!(
            normalize_wall_clock("0 flights · as of 23:15 · oldest retained —"),
            "0 flights · as of HH:MM · oldest retained —"
        );
        assert_eq!(normalize_wall_clock("as of 9:05"), "as of HH:MM");
        assert_eq!(normalize_wall_clock("9:05:59"), "HH:MM:SS");
        // Lookalikes that must survive untouched.
        assert_eq!(
            normalize_wall_clock("v1.2.3, port 8080"),
            "v1.2.3, port 8080"
        );
        assert_eq!(
            normalize_wall_clock("123:45 is not a time"),
            "123:45 is not a time"
        );
        assert_eq!(normalize_wall_clock("a1:23"), "a1:23");
        assert_eq!(normalize_wall_clock("23:15x"), "23:15x");
    }

    #[test]
    fn from_dump_normalizes_clock_labels() {
        let dump = dump(
            7,
            json!({
                "a": node("10", "Status", Some("0 flights · as of 23:15 · oldest retained —"), None),
            }),
        );
        let snapshot = Snapshot::from_dump("usage", &dump).unwrap();
        assert_eq!(
            snapshot.nodes[0].label.as_deref(),
            Some("0 flights · as of HH:MM · oldest retained —")
        );
    }

    fn snap(frame: u64, nodes: Vec<SnapshotNode>) -> Snapshot {
        Snapshot {
            name: String::new(),
            frame_number: frame,
            taken_at: 0,
            nodes,
        }
    }

    fn simple(id: &str, role: &str, label: Option<&str>, bounds: Option<Bounds>) -> SnapshotNode {
        SnapshotNode {
            accesskit_id: id.to_string(),
            role: role.to_string(),
            label: label.map(str::to_string),
            bounds,
            focusable: true,
            focused: false,
        }
    }

    #[test]
    fn diff_reports_added_removed_and_label_change() {
        let a = snap(
            41,
            vec![
                simple("10", "Window", None, None),
                simple("20", "Status", Some("8 flights"), None),
                simple("30", "Button", Some("Back"), None),
            ],
        );
        let b = snap(
            57,
            vec![
                simple("10", "Window", None, None),
                simple("20", "Status", Some("9 flights"), None),
                simple("99", "Label", Some("new"), None),
            ],
        );
        let d = diff(&a, &b);
        assert_eq!((d.changed(), d.added(), d.removed()), (1, 1, 1));
        let text = render_human(&d);
        assert!(text.starts_with("frame 41 → 57 · 1 changed · 1 added · 1 removed"));
        assert!(text.contains("− #30 Button \"Back\""));
        assert!(text.contains("+ #99 Label \"new\""));
        assert!(text.contains("~ #20 Status \"8 flights\" → \"9 flights\""));
    }

    #[test]
    fn diff_reports_bounds_only_change() {
        let a = snap(
            10,
            vec![simple(
                "7",
                "Button",
                Some("OK"),
                Some(Bounds {
                    x: 0.0,
                    y: 0.0,
                    w: 50.0,
                    h: 20.0,
                }),
            )],
        );
        let b = snap(
            11,
            vec![simple(
                "7",
                "Button",
                Some("OK"),
                Some(Bounds {
                    x: 0.0,
                    y: 24.0,
                    w: 50.0,
                    h: 20.0,
                }),
            )],
        );
        let text = render_human(&diff(&a, &b));
        assert!(text.contains("~ #7 Button \"OK\" (0,0) 50×20 → (0,24) 50×20"));
    }

    #[test]
    fn diff_reports_focus_change_with_stable_label() {
        let mut a_node = simple("5", "TextInput", Some("query"), None);
        a_node.focused = false;
        let mut b_node = simple("5", "TextInput", Some("query"), None);
        b_node.focused = true;
        let text = render_human(&diff(&snap(3, vec![a_node]), &snap(4, vec![b_node])));
        assert!(text.contains("~ #5 TextInput \"query\" focused=false → focused=true"));
    }

    #[test]
    fn diff_identical_snapshots_is_all_zero() {
        let a = snap(9, vec![simple("1", "Window", None, None)]);
        let b = snap(9, vec![simple("1", "Window", None, None)]);
        let d = diff(&a, &b);
        assert_eq!((d.changed(), d.added(), d.removed()), (0, 0, 0));
        assert_eq!(
            render_human(&d),
            "frame 9 → 9 · 0 changed · 0 added · 0 removed"
        );
    }

    #[test]
    fn json_rendering_carries_machine_pairs() {
        let a = snap(1, vec![simple("30", "Button", Some("Back"), None)]);
        let b = snap(2, vec![simple("40", "Label", Some("new"), None)]);
        let value = render_json(&diff(&a, &b));
        assert_eq!(value["frame"]["a"], 1);
        assert_eq!(value["frame"]["b"], 2);
        assert_eq!(value["removed"], 1);
        assert_eq!(value["entries"][0]["kind"], "removed");
        assert_eq!(value["entries"][0]["accesskit_id"], "30");
    }

    #[test]
    fn names_reject_traversal_and_odd_chars() {
        assert!(valid_name("after-tab"));
        assert!(valid_name("s2"));
        assert!(valid_name("before_click.v2"));
        assert!(!valid_name(""));
        assert!(!valid_name("../escape"));
        assert!(!valid_name(".hidden"));
        assert!(!valid_name("a/b"));
        assert!(!valid_name("with space"));
        assert!(!valid_name(&"x".repeat(65)));
    }

    #[test]
    fn snapshots_root_is_sibling_of_state_dir() {
        let root = snapshots_root(Path::new("/run/user/1000/dashboard-debug/s/smoke/state"));
        assert_eq!(
            root,
            PathBuf::from("/run/user/1000/dashboard-debug/s/smoke/snapshots")
        );
    }
}
