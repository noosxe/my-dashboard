//! Which commits the Commits tab shows (#876). The detail query reads the
//! newest 100 (`commits(last: 100)`, still ascending), like comments and
//! reviews (#742): a long PR cuts its *oldest* commits, so the cap row
//! sits at the top, and a new push always shows.

use crate::gql;

/// Commits the query cut: all older than everything listed.
pub(super) fn hidden_older(d: &gql::PrDetail) -> i64 {
    (d.commits.total_count - d.commits.nodes.len() as i64).max(0)
}

/// The cap row's text.
pub(super) fn cap_caption(shown: usize, total: i64) -> String {
    format!("Showing the newest {shown} of {total} commits")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_detail_query_reads_the_newest_commits() {
        assert!(gql::PR_DETAIL_QUERY.contains("commits(last: 100)"));
        assert!(!gql::PR_DETAIL_QUERY.contains("commits(first:"));
    }

    #[test]
    fn a_long_pr_reports_what_the_window_cut() {
        let nodes: Vec<_> = (0..100)
            .map(|i| {
                serde_json::json!({"commit": {
                    "oid": format!("{i:040}"), "messageHeadline": format!("c{i}"),
                    "committedDate": "2026-11-01T10:00:00Z"
                }})
            })
            .collect();
        let detail = |total: i64| {
            gql::pr_detail_from_value(&serde_json::json!({"pullRequest": {
                "id": "PR_1", "number": 1, "title": "t", "url": "u",
                "state": "OPEN", "isDraft": false, "createdAt": "2026-10-01T00:00:00Z",
                "commits": {"totalCount": total, "nodes": nodes},
            }}))
            .expect("detail parses")
        };
        assert_eq!(hidden_older(&detail(137)), 37);
        assert_eq!(
            cap_caption(100, 137),
            "Showing the newest 100 of 137 commits"
        );
        assert_eq!(hidden_older(&detail(100)), 0);
    }
}
