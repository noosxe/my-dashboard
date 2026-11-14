//! The cadence function as a table, like docs/0038's.

use super::*;

const MIN: u64 = 60;

fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

fn quiet_for(minutes: u64) -> Duration {
    secs(minutes * MIN)
}

fn after(c: Cadence) -> Duration {
    match c.next() {
        Next::After(d) => d,
        Next::Idle(why) => panic!("expected a probe, got {why:?} for {c:?}"),
    }
}

/// An unfocused page, `minutes` into it.
fn unfocused(minutes: u64) -> Cadence {
    Cadence {
        window_active: false,
        unfocused_for: quiet_for(minutes),
        quiet_for: quiet_for(10),
        ..Cadence::default()
    }
}

/// A focused page that settled `minutes` ago.
fn settled(minutes: u64) -> Cadence {
    Cadence {
        quiet_for: quiet_for(minutes),
        ..Cadence::default()
    }
}

/// A focused page with a check running.
fn hot() -> Cadence {
    Cadence {
        checks_running: true,
        ..Cadence::default()
    }
}

#[test]
fn every_tier() {
    let quiet = settled(10);
    assert_eq!(after(quiet), ACTIVE, "active");
    assert_eq!(after(unfocused(5)), SHOWN, "shown");
    for hot in [
        Cadence {
            checks_running: true,
            ..quiet
        },
        Cadence {
            mergeable_unknown: true,
            ..quiet
        },
        Cadence {
            queued: true,
            ..quiet
        },
        Cadence {
            since_change: Some(secs(119)),
            ..quiet
        },
    ] {
        assert_eq!(after(hot), HOT, "{hot:?}");
        // Unfocused, a Hot condition other than focus holds it at 30 s.
        let away = Cadence {
            window_active: false,
            unfocused_for: quiet_for(5),
            ..hot
        };
        assert_eq!(after(away), ACTIVE, "{away:?}");
    }
    assert_eq!(
        after(Cadence {
            since_change: Some(secs(120)),
            ..quiet
        }),
        ACTIVE,
        "two minutes is no longer recent"
    );
}

#[test]
fn settling_doubles_every_30_quiet_minutes_up_to_five() {
    // Active: 30 s, then 60, 120, 240, and the cap.
    let table = [
        (29, 30),
        (30, 60),
        (59, 60),
        (60, 120),
        (89, 120),
        (90, 240),
        (120, 300),
        (600, 300),
    ];
    for (minutes, want) in table {
        assert_eq!(after(settled(minutes)), secs(want), "{minutes} min");
    }
    // Shown starts higher and reaches the cap sooner.
    for (minutes, want) in [(10, 60), (30, 120), (60, 240), (90, 300)] {
        let c = Cadence {
            quiet_for: quiet_for(minutes),
            ..unfocused(5)
        };
        assert_eq!(after(c), secs(want), "shown, {minutes} min");
    }
    // Hot doubles too, when only a running check keeps it Hot.
    let c = Cadence {
        checks_running: true,
        ..settled(30)
    };
    assert_eq!(after(c), secs(20));
}

#[test]
fn an_hour_unfocused_stops_the_probe_and_focus_resumes_it() {
    assert!(matches!(unfocused(60).next(), Next::After(_)), "at 60 min");
    let past = Cadence {
        unfocused_for: quiet_for(60) + secs(1),
        ..unfocused(60)
    };
    assert_eq!(past.next(), Next::Idle(Stop::Unattended));
    // Focus clears the unfocused time, and a refocus the quiet time.
    let back = Cadence {
        window_active: true,
        unfocused_for: Duration::ZERO,
        quiet_for: Duration::ZERO,
        ..past
    };
    assert_eq!(after(back), ACTIVE);
}

#[test]
fn a_closed_pr_never_probes() {
    let c = Cadence {
        open: false,
        checks_running: true,
        ..Cadence::default()
    };
    assert_eq!(c.next(), Next::Idle(Stop::Closed));
}

#[test]
fn the_poll_interval_header_is_a_floor() {
    let hot = Cadence {
        poll_floor: Some(secs(45)),
        ..hot()
    };
    assert_eq!(after(hot), secs(45), "raises a shorter interval");
    let shown = Cadence {
        poll_floor: Some(secs(45)),
        ..unfocused(5)
    };
    assert_eq!(after(shown), SHOWN, "leaves a longer one");
}

#[test]
fn a_failing_probe_backs_off_up_to_a_cap() {
    let at = |base: Cadence, streak| {
        after(Cadence {
            fail_streak: streak,
            ..base
        })
    };
    let active = settled(10);
    assert_eq!(at(hot(), 1), secs(20));
    assert_eq!(at(active, 2), secs(120));
    assert_eq!(at(unfocused(5), 9), secs(300));
}

#[test]
fn a_secondary_limit_waits_a_minute_then_backs_off() {
    let limited = |streak| Cadence {
        fail_streak: streak,
        limited: true,
        ..hot()
    };
    assert_eq!(after(limited(1)), SECONDARY_FLOOR, "20 s is lifted to 60 s");
    assert_eq!(after(limited(2)), SECONDARY_FLOOR);
    assert_eq!(after(limited(3)), secs(80), "then it backs off");
    assert_eq!(after(limited(9)), secs(160));
}

#[test]
fn usage_thresholds_slow_then_stop() {
    let at = |usage| Cadence {
        usage,
        ..settled(10)
    };
    assert_eq!(after(at(ThresholdState::Normal)), ACTIVE);
    assert_eq!(after(at(ThresholdState::Low)), ACTIVE * 4, "Low is ×4");
    let hot_low = Cadence {
        usage: ThresholdState::Low,
        ..hot()
    };
    assert_eq!(after(hot_low), HOT * 4);
    for stopped in [ThresholdState::Critical, ThresholdState::Exhausted] {
        assert_eq!(at(stopped).next(), Next::Idle(Stop::Usage), "{stopped:?}");
    }
}

#[test]
fn the_guard_drops_to_shown_then_stops() {
    let at = |guard| Cadence { guard, ..hot() };
    assert_eq!(after(at(GuardState::Open)), HOT);
    assert_eq!(after(at(GuardState::Shown)), SHOWN);
    // Shown is a cap on speed, not a reason to speed up a slow page.
    let slow = Cadence {
        guard: GuardState::Shown,
        ..settled(120)
    };
    assert_eq!(after(slow), SETTLED_CAP);
    assert_eq!(at(GuardState::Stopped).next(), Next::Idle(Stop::Guard));
}
