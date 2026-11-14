use std::time::Duration;

use super::{LabelInput, age_label};
use crate::pr_page::freshness::cadence::Stop;

fn s(n: u64) -> Option<Duration> {
    Some(Duration::from_secs(n))
}

fn input(
    watched: bool,
    stop: Option<Stop>,
    checked: Option<u64>,
    landed: Option<u64>,
) -> LabelInput {
    LabelInput {
        watched,
        stop,
        checked: checked.and_then(s),
        landed: landed.and_then(s),
    }
}

#[test]
fn the_label_text_for_every_state() {
    let table: &[(&str, LabelInput, Option<&str>)] = &[
        (
            "watched, just probed",
            input(true, None, Some(2), Some(300)),
            Some("checked just now"),
        ),
        (
            "watched, seconds",
            input(true, None, Some(37), Some(300)),
            Some("checked 35s ago"),
        ),
        (
            "watched, edge of a minute",
            input(true, None, Some(59), Some(300)),
            Some("checked 55s ago"),
        ),
        (
            "watched, minutes",
            input(true, None, Some(125), Some(900)),
            Some("checked 2m ago"),
        ),
        (
            "a newer landing confirms too",
            input(true, None, Some(125), Some(8)),
            Some("checked 5s ago"),
        ),
        (
            "watched, not probed yet: landed age",
            input(true, None, None, Some(30)),
            Some("synced just now"),
        ),
        (
            "not watched (offline, module hidden)",
            input(false, None, Some(10), Some(200)),
            Some("synced 3m ago"),
        ),
        (
            "closed PR",
            input(true, Some(Stop::Closed), Some(10), Some(200)),
            Some("synced 3m ago"),
        ),
        (
            "unattended",
            input(true, Some(Stop::Unattended), Some(10), Some(7200)),
            Some("synced 2h ago"),
        ),
        (
            "usage stop",
            input(true, Some(Stop::Usage), Some(10), Some(200)),
            Some("synced 3m ago"),
        ),
        (
            "guard stop adds the reason",
            input(true, Some(Stop::Guard), Some(10), Some(200)),
            Some("synced 3m ago · auto-refresh paused (budget)"),
        ),
        (
            "nothing landed, nothing to say",
            input(false, None, None, None),
            None,
        ),
        (
            "guard stop with nothing landed",
            input(true, Some(Stop::Guard), None, None),
            None,
        ),
    ];
    for (name, i, want) in table {
        assert_eq!(age_label(*i).as_deref(), *want, "{name}");
    }
}
