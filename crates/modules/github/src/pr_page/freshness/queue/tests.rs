use super::*;

#[test]
fn a_quiet_queued_tick_refetches_at_most_every_30_s() {
    let now = Instant::now();
    let mut q = QueueRefetch::default();
    assert!(q.due(now, Some(Why::Queued), true), "nothing sent before");
    q.dispatched(now);
    for s in [1, 10, 29] {
        assert!(
            !q.due(now + Duration::from_secs(s), Some(Why::Queued), true),
            "{s} s"
        );
    }
    assert!(q.due(now + GAP, Some(Why::Queued), true));
}

#[test]
fn only_a_quiet_tick_of_a_queued_pr_asks() {
    let now = Instant::now();
    let q = QueueRefetch::default();
    assert!(!q.due(now, None, true), "not queued");
    assert!(!q.due(now, Some(Why::Queued), false), "a change or failure");
}

#[test]
fn any_probe_caused_read_spaces_the_next() {
    let now = Instant::now();
    let mut q = QueueRefetch::default();
    // A changed probe read the detail 5 s ago: the queue rule waits.
    q.dispatched(now);
    assert!(!q.due(now + Duration::from_secs(5), Some(Why::Queued), true));
    assert!(q.due(now + Duration::from_secs(30), Some(Why::StaleUnknown), true));
}
