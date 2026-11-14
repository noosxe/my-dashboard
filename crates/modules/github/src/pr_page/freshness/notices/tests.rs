use super::PrNotices;
use crate::api::Notification;
use crate::service::PrKey;

fn thread(id: &str, kind: &str, number: i64, updated: &str, unread: bool) -> Notification {
    let path = if kind == "PullRequest" {
        "pulls"
    } else {
        "issues"
    };
    serde_json::from_value(serde_json::json!({
        "id": id,
        "unread": unread,
        "updated_at": updated,
        "subject": {
            "title": "t",
            "type": kind,
            "url": format!("https://api.github.com/repos/o/r/{path}/{number}"),
        },
        "repository": { "full_name": "o/r" },
    }))
    .unwrap()
}

#[test]
fn the_first_landing_seeds_silently() {
    let mut n = PrNotices::default();
    n.land(&[thread("1", "PullRequest", 7, "t1", true)]);
    assert_eq!(n.seq, 0);
    assert!(!n.names(&PrKey::new("o/r", 7)));
}

#[test]
fn a_moved_thread_names_its_pr_once() {
    let mut n = PrNotices::default();
    n.land(&[thread("1", "PullRequest", 7, "t1", true)]);
    n.land(&[thread("1", "PullRequest", 7, "t2", true)]);
    assert_eq!(n.seq, 1);
    assert!(n.names(&PrKey::new("o/r", 7)));
    assert!(!n.names(&PrKey::new("o/r", 8)));
    // The same landing replayed (a 304 restore) names nothing new.
    n.land(&[thread("1", "PullRequest", 7, "t2", true)]);
    assert_eq!(n.seq, 1);
}

#[test]
fn issues_and_read_threads_name_nothing() {
    let mut n = PrNotices::default();
    n.land(&[]);
    n.land(&[
        thread("1", "Issue", 7, "t1", true),
        thread("2", "PullRequest", 8, "t1", false),
    ]);
    assert_eq!(n.seq, 0);
}
