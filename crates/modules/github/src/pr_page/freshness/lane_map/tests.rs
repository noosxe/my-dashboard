use super::*;
use crate::pr_page::freshness::probe_diff::PrChange;

fn findings(head: bool, review_comments: bool, other: bool, checks: bool) -> Findings {
    Findings {
        pr: PrChange {
            head,
            review_comments,
            other,
        },
        checks,
        failed: false,
    }
}

const NONE: LanePlan = LanePlan {
    detail: false,
    threads: false,
    checks_only: false,
};

/// (head, review comments, other, checks, threads loaded) → plan.
#[test]
fn the_lane_map_table() {
    let detail = LanePlan {
        detail: true,
        ..NONE
    };
    let with_threads = LanePlan {
        threads: true,
        ..detail
    };
    let checks = LanePlan {
        checks_only: true,
        ..detail
    };
    let table = [
        // All 304: nothing.
        ((false, false, false, false, true), NONE),
        // A new head: the detail; the fan-out owns Files and threads.
        ((true, false, false, false, true), detail),
        ((true, true, true, true, true), detail),
        // A review-comment count: threads only when loaded.
        ((false, true, false, false, true), with_threads),
        ((false, true, false, false, false), detail),
        ((false, true, true, false, true), with_threads),
        // Anything else on the PR.
        ((false, false, true, false, true), detail),
        ((false, false, true, true, true), detail),
        // Check runs or status alone: the rate-limited kind.
        ((false, false, false, true, true), checks),
    ];
    for ((head, rc, other, chk, loaded), want) in table {
        assert_eq!(
            plan(&findings(head, rc, other, chk), loaded),
            want,
            "head={head} review_comments={rc} other={other} checks={chk} threads={loaded}"
        );
    }
}
