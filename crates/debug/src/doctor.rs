//! `doctor` verdict logic (docs/0055 D13): pure classification and
//! rendering; the bin layer performs the checks and collects the facts.

/// The outcome of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Passed.
    Ok,
    /// Hard failure — the named next step is required.
    Fail,
    /// Degraded — a next step exists but the session may still work.
    Warn,
    /// Context that gates nothing (grim availability).
    Info,
    /// Not applicable (session-record checks on a default attach).
    Na,
}

/// One doctor line: the check, its outcome, its detail or next step.
#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
}

impl Check {
    pub fn new(name: &'static str, status: Status, detail: impl Into<String>) -> Self {
        Self {
            name,
            status,
            detail: detail.into(),
        }
    }
}

/// Classify `a11y_merge` counters (check 6): FAIL when nodes exist but
/// nothing enriched (the bus is dead — every node is blind), WARN when
/// a substantial share of dump nodes found no walk counterpart (the
/// #351 class: app-side nodes the bus never saw).
pub fn merge_verdict(nodes: usize, enriched: usize, unmatched_dump: usize) -> (Status, String) {
    if nodes == 0 {
        return (
            Status::Fail,
            "dump has no nodes — the tree is empty".to_string(),
        );
    }
    if enriched == 0 {
        return (
            Status::Fail,
            format!(
                "{nodes} nodes, 0 enriched — the a11y bus is dead; \
                 restart the session (`session stop` then `session start`)"
            ),
        );
    }
    let ratio = unmatched_dump as f64 / nodes as f64;
    if ratio > 0.25 {
        return (
            Status::Warn,
            format!(
                "{enriched}/{nodes} enriched, {unmatched_dump} unmatched \
                 ({ratio:.0}%) — the #351 class (app-side nodes the bus \
                 never saw); file or triage, the bridge still works"
            ),
        );
    }
    (
        Status::Ok,
        format!("{enriched}/{nodes} nodes enriched, {unmatched_dump} unmatched"),
    )
}

/// The one-line verdict after all checks.
pub fn verdict(checks: &[Check]) -> String {
    let fails = checks.iter().filter(|c| c.status == Status::Fail).count();
    let warns = checks.iter().filter(|c| c.status == Status::Warn).count();
    if fails > 0 {
        format!(
            "unhealthy: {fails} check(s) failed, {warns} warn(s) — run the FAIL next steps first"
        )
    } else if warns > 0 {
        format!("usable: {warns} warn(s) — nothing failed, address them when convenient")
    } else {
        "healthy: all checks passed".to_string()
    }
}

/// Render the report (one line per check plus the verdict).
pub fn render(checks: &[Check]) -> String {
    let mut out = String::new();
    for check in checks {
        let tag = match check.status {
            Status::Ok => "[ok]",
            Status::Fail => "[FAIL]",
            Status::Warn => "[warn]",
            Status::Info => "[info]",
            Status::Na => "[n/a]",
        };
        out.push_str(&format!("{tag} {}: {}\n", check.name, check.detail));
    }
    out.push_str(&verdict(checks));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_verdict_classes() {
        assert_eq!(
            merge_verdict(0, 0, 0),
            (
                Status::Fail,
                "dump has no nodes — the tree is empty".to_string()
            )
        );
        let (status, detail) = merge_verdict(50, 0, 50);
        assert_eq!(status, Status::Fail);
        assert!(detail.contains("bus is dead"));
        let (status, detail) = merge_verdict(100, 70, 30);
        assert_eq!(status, Status::Warn);
        assert!(detail.contains("#351"));
        let (status, detail) = merge_verdict(100, 99, 1);
        assert_eq!(status, Status::Ok);
        assert!(detail.contains("99/100"));
    }

    #[test]
    fn verdict_counts_fails_and_warns() {
        let ok = vec![Check::new("bridge", Status::Ok, "auth + window.list")];
        assert_eq!(verdict(&ok), "healthy: all checks passed");
        let warn = vec![
            Check::new("bridge", Status::Ok, "fine"),
            Check::new("flag", Status::Warn, "drift"),
        ];
        assert_eq!(
            verdict(&warn),
            "usable: 1 warn(s) — nothing failed, address them when convenient"
        );
        let fail = vec![
            Check::new("bridge", Status::Fail, "down"),
            Check::new("flag", Status::Warn, "drift"),
        ];
        assert_eq!(
            verdict(&fail),
            "unhealthy: 1 check(s) failed, 1 warn(s) — run the FAIL next steps first"
        );
    }

    #[test]
    fn render_tags_every_status() {
        let checks = vec![
            Check::new("a", Status::Ok, "1"),
            Check::new("b", Status::Fail, "2"),
            Check::new("c", Status::Warn, "3"),
            Check::new("d", Status::Info, "4"),
            Check::new("e", Status::Na, "5"),
        ];
        let text = render(&checks);
        assert!(text.contains("[ok] a: 1"));
        assert!(text.contains("[FAIL] b: 2"));
        assert!(text.contains("[warn] c: 3"));
        assert!(text.contains("[info] d: 4"));
        assert!(text.contains("[n/a] e: 5"));
        assert!(
            text.ends_with(
                "unhealthy: 1 check(s) failed, 1 warn(s) — run the FAIL next steps first"
            )
        );
    }
}
