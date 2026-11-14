use std::time::Instant;

use super::*;

const PR_URL: &str = "https://x/pulls/7";
const RUNS_URL: &str = "https://x/commits/aaa/check-runs";
const STATUS_URL: &str = "https://x/commits/aaa/status";

fn pr(head: &str, review_comments: i64, title: &str) -> String {
    serde_json::json!({
        "state": "open", "title": title, "head": { "sha": head },
        "review_comments": review_comments, "comments": 1,
    })
    .to_string()
}

fn runs(status: &str) -> String {
    serde_json::json!({ "check_runs": [{ "status": status }] }).to_string()
}

fn two(etag: &str, body: String) -> Reply {
    Reply::Changed {
        etag: Some(etag.into()),
        body,
    }
}

fn seeded() -> ProbeBook {
    let mut book = ProbeBook::default();
    book.seed_head(Some("aaa".into()));
    let now = Instant::now();
    book.begin_tick();
    book.land(Step::Pr, PR_URL, two("\"p1\"", pr("aaa", 3, "t")), now);
    book.land(
        Step::CheckRuns,
        RUNS_URL,
        two("\"c1\"", runs("in_progress")),
        now,
    );
    assert_eq!(book.finish_tick(), Findings::default());
    book
}

#[test]
fn the_first_200s_only_seed() {
    let book = seeded();
    assert!(book.seeded());
    assert!(book.checked_at.is_some());
    assert!(book.checks_running());
    assert_eq!(book.etag(PR_URL).as_deref(), Some("\"p1\""));
    assert_eq!(book.etag(RUNS_URL).as_deref(), Some("\"c1\""));
}

#[test]
fn a_304_stamps_checked_at_and_nothing_else() {
    let mut book = seeded();
    let before = book.checked_at;
    let later = Instant::now() + std::time::Duration::from_secs(10);
    book.begin_tick();
    book.land(Step::Pr, PR_URL, Reply::NotModified, later);
    book.land(Step::CheckRuns, RUNS_URL, Reply::NotModified, later);
    assert_eq!(book.finish_tick(), Findings::default());
    assert_eq!(book.checked_at, Some(later));
    assert_ne!(book.checked_at, before);
    assert_eq!(
        book.etag(PR_URL).as_deref(),
        Some("\"p1\""),
        "etags are kept"
    );
}

#[test]
fn a_pull_200_with_a_diff_reports_it_and_refreshes_the_etag() {
    let mut book = seeded();
    let stamp = book.checked_at;
    book.begin_tick();
    book.land(
        Step::Pr,
        PR_URL,
        two("\"p2\"", pr("aaa", 3, "new title")),
        Instant::now(),
    );
    let f = book.finish_tick();
    assert!(f.pr.other && !f.pr.head && !f.pr.review_comments);
    assert!(f.changed());
    assert_eq!(book.etag(PR_URL).as_deref(), Some("\"p2\""));
    assert_eq!(book.checked_at, stamp, "a change is not yet confirmed data");
    // The next 200 compares against this one, not the seed.
    book.begin_tick();
    book.land(
        Step::Pr,
        PR_URL,
        two("\"p3\"", pr("aaa", 3, "new title")),
        Instant::now(),
    );
    assert!(!book.finish_tick().changed());
}

#[test]
fn a_new_head_moves_the_check_runs_probe() {
    let mut book = seeded();
    assert_eq!(
        book.path(Step::CheckRuns, 7).as_deref(),
        Some("commits/aaa/check-runs?per_page=100")
    );
    book.begin_tick();
    book.land(
        Step::Pr,
        PR_URL,
        two("\"p2\"", pr("bbb", 3, "t")),
        Instant::now(),
    );
    assert!(book.finish_tick().pr.head);
    assert_eq!(
        book.path(Step::CheckRuns, 7).as_deref(),
        Some("commits/bbb/check-runs?per_page=100")
    );
    assert_eq!(
        book.path(Step::Status, 7).as_deref(),
        Some("commits/bbb/status")
    );
    // The new URL's first 200 seeds; it is not a "checks changed".
    let new_runs = "https://x/commits/bbb/check-runs";
    book.begin_tick();
    book.land(
        Step::CheckRuns,
        new_runs,
        two("\"n1\"", runs("completed")),
        Instant::now(),
    );
    assert_eq!(book.finish_tick(), Findings::default());
    assert!(!book.checks_running());
}

#[test]
fn a_check_runs_200_after_the_seed_is_a_checks_change() {
    let mut book = seeded();
    book.begin_tick();
    book.land(
        Step::CheckRuns,
        RUNS_URL,
        two("\"c2\"", runs("completed")),
        Instant::now(),
    );
    let f = book.finish_tick();
    assert!(f.checks && !f.pr.any());
    assert!(!book.checks_running());
}

#[test]
fn a_status_200_follows_the_same_rule() {
    let mut book = seeded();
    let body = r#"{"state":"success"}"#.to_string();
    book.begin_tick();
    book.land(
        Step::Status,
        STATUS_URL,
        two("\"s1\"", body.clone()),
        Instant::now(),
    );
    assert!(!book.finish_tick().checks, "first 200 seeds");
    book.begin_tick();
    book.land(
        Step::Status,
        STATUS_URL,
        two("\"s2\"", body),
        Instant::now(),
    );
    assert!(book.finish_tick().checks);
}

#[test]
fn a_failure_or_a_bad_body_stops_the_tick() {
    let mut book = seeded();
    book.begin_tick();
    book.land(Step::Pr, PR_URL, Reply::Failed, Instant::now());
    assert_eq!(book.next_step(Some(Step::Pr), false), None);
    assert!(book.finish_tick().failed);

    book.begin_tick();
    book.land(Step::Pr, PR_URL, two("\"x\"", "[]".into()), Instant::now());
    assert!(book.finish_tick().failed);
    assert!(book.seeded(), "the last good snapshot stays");
}

#[test]
fn the_step_order_follows_the_legacy_flag_and_the_head() {
    let mut book = ProbeBook::default();
    assert_eq!(book.next_step(None, false), Some(Step::Pr));
    // No head yet: nothing past the pull.
    assert_eq!(book.next_step(Some(Step::Pr), false), None);
    book.seed_head(Some("aaa".into()));
    assert_eq!(book.next_step(Some(Step::Pr), false), Some(Step::CheckRuns));
    assert_eq!(book.next_step(Some(Step::CheckRuns), false), None);
    assert_eq!(
        book.next_step(Some(Step::CheckRuns), true),
        Some(Step::Status)
    );
    assert_eq!(book.next_step(Some(Step::Status), true), None);
    assert_eq!(book.path(Step::Pr, 7).as_deref(), Some("pulls/7"));
}
