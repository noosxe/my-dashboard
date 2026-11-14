use super::*;

fn t0() -> Instant {
    Instant::now()
}

fn later(t: Instant, secs: u64) -> Instant {
    t + Duration::from_secs(secs)
}

#[test]
fn under_the_ceilings_nothing_happens() {
    let mut g = Guard::default();
    let now = t0();
    g.spend_rest(now, REST_CEILING - 1);
    g.spend_points(now, POINTS_CEILING - 1);
    assert_eq!(g.state(now), GuardState::Open);
}

#[test]
fn the_first_breach_drops_to_shown() {
    for (rest, points) in [(REST_CEILING, 0), (0, POINTS_CEILING)] {
        let mut g = Guard::default();
        let now = t0();
        g.spend_rest(now, rest);
        g.spend_points(now, points);
        assert_eq!(g.state(now), GuardState::Shown, "{rest} {points}");
        assert!(!g.stopped());
    }
}

#[test]
fn one_stretch_over_the_line_is_one_breach() {
    let mut g = Guard::default();
    let now = t0();
    g.spend_rest(now, REST_CEILING);
    // More spending while still over doesn't count again.
    for i in 1..30 {
        g.spend_rest(later(now, i), 1);
    }
    assert_eq!(g.state(later(now, 30)), GuardState::Shown);
}

/// Two breaches inside one hour: 300 requests at the start, 300 half an
/// hour later (the first breach), the early ones leave the window at the
/// hour, and 300 more cross the line again.
fn breached_twice(g: &mut Guard, start: Instant) -> Instant {
    g.spend_rest(start, 300);
    g.spend_rest(later(start, 1_800), 300);
    assert_eq!(g.state(later(start, 1_800)), GuardState::Shown);
    let below = later(start, 3_600);
    assert_eq!(g.spent(below).0, 300, "the early 300 left the window");
    assert_eq!(g.state(below), GuardState::Shown, "one breach in the hour");
    g.spend_rest(below, 300);
    below
}

#[test]
fn a_second_breach_in_the_hour_stops_the_probe() {
    let mut g = Guard::default();
    let at = breached_twice(&mut g, t0());
    assert_eq!(g.state(at), GuardState::Stopped);
    assert!(g.stopped());
    // Time alone doesn't lift a stop.
    assert_eq!(g.state(later(at, 20_000)), GuardState::Stopped);
}

#[test]
fn a_breach_an_hour_old_is_forgotten() {
    let mut g = Guard::default();
    let now = t0();
    g.spend_rest(now, REST_CEILING);
    assert_eq!(g.state(later(now, 3_600)), GuardState::Open);
    // So the next one is a first breach again.
    let next = later(now, 3_601);
    g.spend_rest(next, REST_CEILING);
    assert_eq!(g.state(next), GuardState::Shown);
}

#[test]
fn the_window_rolls() {
    let mut g = Guard::default();
    let now = t0();
    g.spend_rest(now, 400);
    g.spend_rest(later(now, 1_800), 150);
    assert_eq!(g.spent(later(now, 3_599)), (550, 0));
    assert_eq!(g.spent(later(now, 3_600)), (150, 0), "the first 400 left");
}

#[test]
fn refocus_or_refresh_lifts_the_stop_and_starts_over() {
    let mut g = Guard::default();
    let at = breached_twice(&mut g, t0());
    assert_eq!(g.state(at), GuardState::Stopped);
    g.clear();
    assert_eq!(g.state(at), GuardState::Open);
    assert!(!g.stopped());
    assert_eq!(g.spent(at), (0, 0));
    // The fresh start can breach again, as a first breach.
    g.spend_rest(later(at, 1), REST_CEILING);
    assert_eq!(g.state(later(at, 1)), GuardState::Shown);
}
