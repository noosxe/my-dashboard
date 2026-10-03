//! The `audit` verb (docs/0065 D3): deterministic rules over a
//! `tree.dump` result. Pure functions — parsing, geometry and reporting
//! only; the CLI and the receipt runner drive them.
//!
//! Design virtues lifted from axe-core: rules are deterministic and
//! biased toward zero false positives; anything a rule cannot decide is
//! reported [`Severity::Incomplete`] and routed to the owner checklist
//! instead of hard-failing. [`Severity::Fail`] is reserved for what the
//! app's own rules already mandate (docs/0010 D10 role+label, real
//! bounds). [`Severity::Finding`] flags the "look at this" class
//! (off-scale gaps, overlaps, duplicate labels) — surfaced, never
//! fatal.

use serde_json::Value;

use crate::snapshot::Bounds;

/// The D10 spacing scale (docs/0002): the only legal rhythm gaps.
pub const SPACING_SCALE: [f32; 9] = [2.0, 4.0, 6.0, 8.0, 10.0, 12.0, 16.0, 24.0, 32.0];

/// Gap-audit ceiling: gaps larger than this are deliberate layout
/// separation (sections, panels), not rhythm — exempt.
const GAP_CEILING: f32 = 64.0;

/// Bounds tolerance: sub-pixel rendering noise never reports.
const BOUNDS_EPSILON: f32 = 1.0;

/// Roles that legitimately layer over siblings — overlays are exempt
/// from the overlap rule by design.
const OVERLAY_ROLES: [&str; 7] = [
    "Overlay",
    "Popover",
    "Dialog",
    "AlertDialog",
    "Tooltip",
    "Menu",
    "MenuBar",
];

/// Container-flavored roles: focusable but expected to carry no label
/// of their own (children provide the content) — no finding at all.
const CONTAINER_ROLES: [&str; 12] = [
    "Group",
    "Pane",
    "Section",
    "Container",
    "Window",
    "ScrollPane",
    "List",
    "Table",
    "Grid",
    "Tree",
    "Toolbar",
    "SplitGroup",
];

/// Interactive roles — a missing/empty label is a hard failure
/// (docs/0010 D10: every interactive element is id'd and labeled).
const INTERACTIVE_ROLES: [&str; 14] = [
    "Button",
    "ToggleButton",
    "CheckBox",
    "RadioButton",
    "Switch",
    "Tab",
    "Link",
    "TextInput",
    "TextField",
    "SearchInput",
    "ComboBox",
    "Slider",
    "MenuItem",
    "TreeItem",
];

/// Outcome class of one rule hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Mechanical violation of a mandated rule — receipt-red.
    Fail,
    /// Suspicious but decidable only with eyes — owner checklist.
    Finding,
    /// The rule cannot decide on this input — owner checklist.
    Incomplete,
}

impl Severity {
    fn label(self) -> &'static str {
        match self {
            Severity::Fail => "FAIL",
            Severity::Finding => "FINDING",
            Severity::Incomplete => "INCOMPLETE",
        }
    }
}

/// One rule hit.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    /// Rule id, e.g. `role-label`.
    pub rule: &'static str,
    pub severity: Severity,
    /// The offending node's `accesskit_id`, when the rule is per-node.
    pub node: Option<String>,
    pub detail: String,
}

/// The whole report plus counts for receipts.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuditReport {
    pub findings: Vec<Finding>,
    pub nodes_audited: usize,
}

impl AuditReport {
    pub fn fails(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Fail)
            .count()
    }

    pub fn findings_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Finding)
            .count()
    }

    pub fn incomplete(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Incomplete)
            .count()
    }

    /// Human receipt text. Empty (besides the summary) when clean.
    pub fn render_human(&self) -> String {
        let mut out = String::new();
        for finding in &self.findings {
            let node = finding
                .node
                .as_deref()
                .map(|id| format!(" #{id}"))
                .unwrap_or_default();
            out.push_str(&format!(
                "{}  {}{node}  {}\n",
                finding.severity.label(),
                finding.rule,
                finding.detail
            ));
        }
        out.push_str(&format!(
            "audit: {} nodes — {} fail, {} findings, {} incomplete \
             (tab-order coverage: pair with `tree --tab-stops`)\n",
            self.nodes_audited,
            self.fails(),
            self.findings_count(),
            self.incomplete()
        ));
        out
    }

    /// `--json` shape: findings plus counts.
    pub fn render_json(&self) -> Value {
        let mut findings = Vec::new();
        for finding in &self.findings {
            findings.push(serde_json::json!({
                "rule": finding.rule,
                "severity": finding.severity.label(),
                "node": finding.node,
                "detail": finding.detail,
            }));
        }
        serde_json::json!({
            "nodes": self.nodes_audited,
            "fails": self.fails(),
            "findings": self.findings_count(),
            "incomplete": self.incomplete(),
            "entries": findings,
        })
    }
}

/// One flattened dump node with tree edges resolved to arena indexes.
#[derive(Debug, Clone)]
struct AuditNode {
    id: String,
    role: String,
    label: Option<String>,
    bounds: Option<Bounds>,
    focusable: bool,
    children: Vec<usize>,
}

/// Run every rule over a `tree.dump` result. `Err` only for malformed
/// dumps (no nodes map) — never for findings.
pub fn audit_dump(dump: &Value) -> Result<AuditReport, String> {
    let nodes_map = dump["nodes"]
        .as_object()
        .ok_or_else(|| "dump has no nodes map".to_string())?;

    // Flatten into the arena; key → index for edge resolution.
    let mut arena: Vec<AuditNode> = Vec::new();
    let mut index_of: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (key, node) in nodes_map {
        let Some(id) = node["accesskit_id"].as_str() else {
            // Ephemeral wrapper nodes without ids cannot be addressed;
            // snapshot skips them and so do we.
            continue;
        };
        index_of.insert(key.clone(), arena.len());
        arena.push(AuditNode {
            id: id.to_string(),
            role: node["aria"]["role"]
                .as_str()
                .unwrap_or("(no role)")
                .to_string(),
            label: node["aria"]["label"].as_str().map(str::to_string),
            bounds: parse_bounds(node.get("bounds")),
            focusable: node["focusable"].as_bool().unwrap_or(false),
            children: Vec::new(),
        });
    }

    // Resolve children lists (mutating via index_of, then fix indexes).
    let mut edges: Vec<(usize, Vec<usize>)> = Vec::new();
    for (key, node) in nodes_map {
        let Some(parent_index) = index_of.get(key).copied() else {
            continue;
        };
        let mut children = Vec::new();
        if let Some(list) = node["children"].as_array() {
            for child in list {
                if let Some(child_key) = child.as_str() {
                    if let Some(child_index) = index_of.get(child_key) {
                        children.push(*child_index);
                    }
                }
            }
        }
        edges.push((parent_index, children));
    }
    for (parent, children) in edges {
        arena[parent].children = children;
    }

    let mut report = AuditReport {
        findings: Vec::new(),
        nodes_audited: arena.len(),
    };
    for node in &arena {
        rule_role_label(node, &mut report);
        rule_bounds(node, &mut report);
    }
    for node in &arena {
        rule_gap_scale(node, &arena, &mut report);
        rule_overlap(node, &arena, &mut report);
        rule_duplicate_label(node, &arena, &mut report);
    }
    report.findings.sort_by(|a, b| {
        (a.severity, a.rule, a.node.clone()).cmp(&(b.severity, b.rule, b.node.clone()))
    });
    Ok(report)
}

/// D10/D10 rule: interactive nodes must be labeled; focusable
/// containers may rely on their children.
fn rule_role_label(node: &AuditNode, report: &mut AuditReport) {
    let labeled = node.label.as_deref().is_some_and(|l| !l.trim().is_empty());
    if labeled {
        return;
    }
    let interactive = INTERACTIVE_ROLES.contains(&node.role.as_str());
    let container = CONTAINER_ROLES.contains(&node.role.as_str());
    if interactive {
        report.findings.push(Finding {
            rule: "role-label",
            severity: Severity::Fail,
            node: Some(node.id.clone()),
            detail: format!(
                "interactive role {:?} has no label (docs/0010 D10)",
                node.role
            ),
        });
    } else if node.focusable && !container {
        report.findings.push(Finding {
            rule: "role-label",
            severity: Severity::Incomplete,
            node: Some(node.id.clone()),
            detail: format!(
                "focusable role {:?} without a label — container or unlabeled control?",
                node.role
            ),
        });
    }
}

/// Bounds sanity: degenerate boxes fail; hairlines and missing bounds
/// are routed, not failed (a focusable without bounds still works via
/// node addressing but is invisible to the coordinate fallback).
fn rule_bounds(node: &AuditNode, report: &mut AuditReport) {
    let Some(b) = node.bounds else {
        if node.focusable {
            report.findings.push(Finding {
                rule: "bounds",
                severity: Severity::Finding,
                node: Some(node.id.clone()),
                detail: "focusable node has no bounds — coordinate fallback cannot target it"
                    .to_string(),
            });
        }
        return;
    };
    if b.w < -BOUNDS_EPSILON || b.h < -BOUNDS_EPSILON {
        report.findings.push(Finding {
            rule: "bounds",
            severity: Severity::Fail,
            node: Some(node.id.clone()),
            detail: format!("negative bounds {b:?}"),
        });
    } else if b.w <= 0.0 && b.h <= 0.0 {
        report.findings.push(Finding {
            rule: "bounds",
            severity: Severity::Fail,
            node: Some(node.id.clone()),
            detail: format!("zero-size bounds {b:?} — nothing renders, nothing is clickable"),
        });
    } else if b.w <= 0.0 || b.h <= 0.0 {
        report.findings.push(Finding {
            rule: "bounds",
            severity: Severity::Incomplete,
            node: Some(node.id.clone()),
            detail: format!("one zero dimension {b:?} — hairline/separator, or a bug?"),
        });
    }
}

/// Rhythm rule: gaps between aligned siblings belong to the D10 scale.
/// Off-scale gaps under the ceiling are findings — deliberate, never
/// fatal (D10 exempts geometry; this is the watchtower, not the wall).
fn rule_gap_scale(parent: &AuditNode, arena: &[AuditNode], report: &mut AuditReport) {
    let children: Vec<&AuditNode> = parent
        .children
        .iter()
        .map(|i| &arena[*i])
        .filter(|c| c.bounds.is_some())
        .collect();
    if children.len() < 2 {
        return;
    }
    let mut off_scale = Vec::new();
    // Row runs: children whose vertical spans overlap, ordered by x.
    let mut row = children.clone();
    row.sort_by(|a, b| {
        a.bounds
            .unwrap()
            .x
            .partial_cmp(&b.bounds.unwrap().x)
            .unwrap()
    });
    for pair in row.windows(2) {
        let (a, b) = (pair[0].bounds.unwrap(), pair[1].bounds.unwrap());
        let overlap = vertical_overlap(a, b);
        if overlap > 0.0 {
            check_gap(
                a.x + a.w,
                b.x,
                pair.try_into().unwrap(),
                &mut off_scale,
                "row",
            );
        }
    }
    // Column runs: children whose horizontal spans overlap, ordered by y.
    let mut column = children.clone();
    column.sort_by(|a, b| {
        a.bounds
            .unwrap()
            .y
            .partial_cmp(&b.bounds.unwrap().y)
            .unwrap()
    });
    for pair in column.windows(2) {
        let (a, b) = (pair[0].bounds.unwrap(), pair[1].bounds.unwrap());
        if horizontal_overlap(a, b) > 0.0 {
            check_gap(
                a.y + a.h,
                b.y,
                pair.try_into().unwrap(),
                &mut off_scale,
                "column",
            );
        }
    }
    for (gap, kind, id) in off_scale {
        report.findings.push(Finding {
            rule: "gap-scale",
            severity: Severity::Finding,
            node: Some(id),
            detail: format!(
                "{kind} gap {gap:.1}px off the D10 scale — deliberate layout or drift?"
            ),
        });
    }
}

fn check_gap(
    edge: f32,
    next_start: f32,
    pair: &[&AuditNode; 2],
    off_scale: &mut Vec<(f32, &'static str, String)>,
    kind: &'static str,
) {
    let gap = next_start - edge;
    if gap <= BOUNDS_EPSILON || gap >= GAP_CEILING {
        return;
    }
    let on_scale = SPACING_SCALE
        .iter()
        .any(|step| (gap - step).abs() <= BOUNDS_EPSILON);
    if !on_scale {
        off_scale.push((gap, kind, pair[1].id.clone()));
    }
}

/// Sibling overlap rule: siblings shouldn't stack unless they are
/// overlays. Small intersections are rendering noise; real stacking is
/// a finding for the owner (it hides hit targets).
fn rule_overlap(parent: &AuditNode, arena: &[AuditNode], report: &mut AuditReport) {
    let children: Vec<&AuditNode> = parent
        .children
        .iter()
        .map(|i| &arena[*i])
        .filter(|c| c.bounds.is_some())
        .collect();
    for pair in children
        .iter()
        .flat_map(|c| children.iter().map(move |d| (c, d)))
    {
        let (a, b) = pair;
        if a.id >= b.id {
            continue; // each unordered pair once
        }
        if is_overlay(a) || is_overlay(b) {
            continue;
        }
        let (a_b, b_b) = (a.bounds.unwrap(), b.bounds.unwrap());
        let w = horizontal_overlap(a_b, b_b);
        let h = vertical_overlap(a_b, b_b);
        if w > 2.0 * BOUNDS_EPSILON && h > 2.0 * BOUNDS_EPSILON {
            report.findings.push(Finding {
                rule: "overlap",
                severity: Severity::Finding,
                node: Some(b.id.clone()),
                detail: format!(
                    "overlaps sibling #{} by {w:.1}×{h:.1}px — layering or a hidden hit target?",
                    a.id
                ),
            });
        }
    }
}

fn is_overlay(node: &AuditNode) -> bool {
    OVERLAY_ROLES.contains(&node.role.as_str())
        || node.id.starts_with("overlay-")
        || node.id.starts_with("popover-")
}

/// Two interactive siblings with the same role *and* the same label
/// read as one control to assistive tech — and to users.
fn rule_duplicate_label(parent: &AuditNode, arena: &[AuditNode], report: &mut AuditReport) {
    let mut seen: std::collections::HashMap<(String, String), Vec<String>> =
        std::collections::HashMap::new();
    for index in &parent.children {
        let node = &arena[*index];
        let Some(label) = node.label.as_deref() else {
            continue;
        };
        if label.trim().is_empty() || !INTERACTIVE_ROLES.contains(&node.role.as_str()) {
            continue;
        }
        seen.entry((node.role.clone(), label.to_string()))
            .or_default()
            .push(node.id.clone());
    }
    for ((role, label), ids) in seen {
        if ids.len() > 1 {
            report.findings.push(Finding {
                rule: "duplicate-label",
                severity: Severity::Finding,
                node: Some(ids[0].clone()),
                detail: format!(
                    "{ids_len} sibling {role:?} controls labeled {label:?} (#{ids}) — \
                     disambiguate",
                    ids_len = ids.len(),
                    ids = ids.join(", #")
                ),
            });
        }
    }
}

fn vertical_overlap(a: Bounds, b: Bounds) -> f32 {
    (a.y + a.h).min(b.y + b.h) - a.y.max(b.y)
}

fn horizontal_overlap(a: Bounds, b: Bounds) -> f32 {
    (a.x + a.w).min(b.x + b.w) - a.x.max(b.x)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(
        id: &str,
        role: &str,
        label: Option<&str>,
        bounds: (f32, f32, f32, f32),
        focusable: bool,
        children: &[&str],
    ) -> Value {
        let mut v = json!({
            "accesskit_id": id,
            "aria": {"role": role},
            "focusable": focusable,
            "focused": false,
            "bounds": {"x": bounds.0, "y": bounds.1, "w": bounds.2, "h": bounds.3},
            "children": children,
        });
        if let Some(label) = label {
            v["aria"]["label"] = json!(label);
        }
        v
    }

    fn dump(nodes: Vec<(&str, Value)>) -> Value {
        let mut map = serde_json::Map::new();
        for (key, value) in nodes {
            map.insert(key.to_string(), value);
        }
        json!({"frame": {"frame_number": 7}, "nodes": map})
    }

    #[test]
    fn clean_surface_reports_nothing() {
        let d = dump(vec![
            (
                "w",
                node(
                    "1",
                    "Window",
                    Some("Dashboard"),
                    (0., 0., 1280., 800.),
                    false,
                    &["b"],
                ),
            ),
            (
                "b",
                node("2", "Button", Some("Export"), (8., 8., 80., 24.), true, &[]),
            ),
        ]);
        let report = audit_dump(&d).expect("audit");
        assert_eq!(report.fails(), 0, "{}", report.render_human());
        assert_eq!(report.findings_count(), 0, "{}", report.render_human());
        assert_eq!(report.nodes_audited, 2);
    }

    #[test]
    fn interactive_without_label_fails() {
        let d = dump(vec![(
            "b",
            node("2", "Button", None, (8., 8., 80., 24.), true, &[]),
        )]);
        let report = audit_dump(&d).expect("audit");
        assert_eq!(report.fails(), 1);
        assert!(report.findings[0].detail.contains("docs/0010 D10"));
    }

    #[test]
    fn focusable_container_without_label_is_incomplete() {
        let d = dump(vec![(
            "g",
            node("5", "Group", None, (0., 0., 100., 100.), true, &[]),
        )]);
        let report = audit_dump(&d).expect("audit");
        assert_eq!(report.fails(), 0);
        assert_eq!(report.incomplete(), 0); // container roles: expected, no finding
    }

    #[test]
    fn focusable_unknown_role_without_label_is_incomplete() {
        let d = dump(vec![(
            "x",
            node("9", "MysteryRow", None, (0., 0., 100., 20.), true, &[]),
        )]);
        let report = audit_dump(&d).expect("audit");
        assert_eq!(report.fails(), 0);
        assert_eq!(report.incomplete(), 1);
    }

    #[test]
    fn zero_size_bounds_fail_but_hairlines_route() {
        let d = dump(vec![
            ("z", node("1", "Group", None, (0., 0., 0., 0.), false, &[])),
            (
                "h",
                node("2", "Separator", None, (0., 4., 100., 0.), false, &[]),
            ),
        ]);
        let report = audit_dump(&d).expect("audit");
        assert_eq!(report.fails(), 1);
        assert_eq!(report.incomplete(), 1);
    }

    #[test]
    fn focusable_without_bounds_is_a_finding() {
        let mut n = node("7", "Button", Some("Go"), (0., 0., 10., 10.), true, &[]);
        n["bounds"] = serde_json::Value::Null;
        let d = dump(vec![("b", n)]);
        let report = audit_dump(&d).expect("audit");
        assert_eq!(report.findings_count(), 1);
        assert_eq!(report.fails(), 0);
    }

    #[test]
    fn off_scale_sibling_gap_is_a_finding_on_scale_is_not() {
        // Row of three: an 8px gap (on scale) and an 18px gap (off
        // scale — the ±1px tolerance absorbs 3–17px).
        let d = dump(vec![
            (
                "w",
                node(
                    "1",
                    "Window",
                    Some("W"),
                    (0., 0., 400., 40.),
                    false,
                    &["a", "b", "c"],
                ),
            ),
            (
                "a",
                node("2", "Button", Some("A"), (0., 0., 40., 24.), true, &[]),
            ),
            (
                "b",
                node("3", "Button", Some("B"), (48., 0., 40., 24.), true, &[]),
            ),
            (
                "c",
                node("4", "Button", Some("C"), (106., 0., 40., 24.), true, &[]),
            ),
        ]);
        let report = audit_dump(&d).expect("audit");
        let gaps: Vec<&Finding> = report
            .findings
            .iter()
            .filter(|f| f.rule == "gap-scale")
            .collect();
        assert_eq!(gaps.len(), 1, "{}", report.render_human());
        assert!(gaps[0].detail.contains("18.0px"));
    }

    #[test]
    fn overlapping_siblings_find_but_overlays_exempt() {
        let plain = dump(vec![
            (
                "w",
                node(
                    "1",
                    "Window",
                    Some("W"),
                    (0., 0., 400., 40.),
                    false,
                    &["a", "b"],
                ),
            ),
            (
                "a",
                node("2", "Button", Some("A"), (0., 0., 40., 24.), true, &[]),
            ),
            (
                "b",
                node("3", "Button", Some("B"), (20., 0., 40., 24.), true, &[]),
            ),
        ]);
        let report = audit_dump(&plain).expect("audit");
        assert_eq!(
            report
                .findings
                .iter()
                .filter(|f| f.rule == "overlap")
                .count(),
            1
        );

        let overlay = dump(vec![
            (
                "w",
                node(
                    "1",
                    "Window",
                    Some("W"),
                    (0., 0., 400., 40.),
                    false,
                    &["a", "o"],
                ),
            ),
            (
                "a",
                node("2", "Button", Some("A"), (0., 0., 40., 24.), true, &[]),
            ),
            (
                "o",
                node("3", "Popover", Some("Menu"), (20., 0., 40., 24.), true, &[]),
            ),
        ]);
        let report = audit_dump(&overlay).expect("audit");
        assert_eq!(
            report
                .findings
                .iter()
                .filter(|f| f.rule == "overlap")
                .count(),
            0,
            "{}",
            report.render_human()
        );
    }

    #[test]
    fn duplicate_labels_within_one_parent_find() {
        let d = dump(vec![
            (
                "w",
                node(
                    "1",
                    "Window",
                    Some("W"),
                    (0., 0., 400., 40.),
                    false,
                    &["a", "b", "c"],
                ),
            ),
            (
                "a",
                node("2", "Button", Some("Open"), (0., 0., 40., 24.), true, &[]),
            ),
            (
                "b",
                node("3", "Button", Some("Open"), (48., 0., 40., 24.), true, &[]),
            ),
            (
                "c",
                node("4", "Button", Some("Close"), (96., 0., 40., 24.), true, &[]),
            ),
        ]);
        let report = audit_dump(&d).expect("audit");
        assert_eq!(
            report
                .findings
                .iter()
                .filter(|f| f.rule == "duplicate-label")
                .count(),
            1
        );
    }

    #[test]
    fn malformed_dump_errors() {
        assert!(audit_dump(&json!({"nodes": []})).is_err());
    }

    #[test]
    fn severity_ordering_puts_fails_first() {
        let d = dump(vec![
            (
                "w",
                node(
                    "1",
                    "Window",
                    Some("W"),
                    (0., 0., 400., 40.),
                    false,
                    &["a", "b", "c"],
                ),
            ),
            (
                "a",
                node("2", "Button", None, (0., 0., 40., 24.), true, &[]),
            ), // FAIL
            (
                "b",
                node("3", "Button", Some("B"), (106., 0., 40., 24.), true, &[]),
            ), // gap finding
            (
                "c",
                node("4", "Mystery", None, (0., 30., 40., 24.), true, &[]),
            ), // incomplete
        ]);
        let report = audit_dump(&d).expect("audit");
        assert_eq!(report.findings[0].severity, Severity::Fail);
    }
}
