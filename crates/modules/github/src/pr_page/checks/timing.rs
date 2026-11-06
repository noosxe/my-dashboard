//! A check row's time label (docs/0016 D7, #792), from `CheckRun.startedAt`
//! and `completedAt`. Status contexts have neither and show nothing.

use chrono::DateTime;
use dashboard_core::freshness::relative_age;

use crate::gql::{CheckState, PrCheck};

/// `"42s"`, `"3m 05s"`, `"1h 02m"`.
fn format_duration(secs: i64) -> String {
    match secs {
        ..60 => format!("{secs}s"),
        60..3600 => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{}h {:02}m", secs / 3600, secs % 3600 / 60),
    }
}

/// Finished runs show how long they took. A skipped run reports the same
/// start and end, and a skewed clock an end before the start: neither is a
/// duration, so both show nothing.
fn run_duration(started_at: &str, completed_at: &str) -> Option<String> {
    let started = DateTime::parse_from_rfc3339(started_at).ok()?;
    let completed = DateTime::parse_from_rfc3339(completed_at).ok()?;
    let secs = (completed - started).num_seconds();
    (secs > 0).then(|| format_duration(secs))
}

/// The label a row shows, if the data has one. A running row says when it
/// started rather than an elapsed time: the page only repaints on a refresh
/// (docs/0052), and a frozen counter would read as a stuck run.
pub(crate) fn check_timing(check: &PrCheck) -> Option<String> {
    let started = check.started_at.as_deref()?;
    if let Some(completed) = check.completed_at.as_deref() {
        return run_duration(started, completed);
    }
    if check.state != CheckState::Pending {
        return None;
    }
    match relative_age(Some(started)).as_str() {
        "" => None,
        "now" => Some("started just now".into()),
        age => Some(format!("started {age} ago")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gql::CheckOrigin;

    fn check(state: CheckState, started: Option<&str>, completed: Option<&str>) -> PrCheck {
        PrCheck {
            name: "build".into(),
            state,
            details_url: None,
            origin: CheckOrigin::Actions,
            check_run_id: None,
            suite_id: None,
            workflow: None,
            started_at: started.map(str::to_string),
            completed_at: completed.map(str::to_string),
        }
    }

    #[test]
    fn durations_read_at_the_scale_they_are() {
        assert_eq!(format_duration(42), "42s");
        assert_eq!(format_duration(60), "1m 00s");
        assert_eq!(format_duration(185), "3m 05s");
        assert_eq!(format_duration(3720), "1h 02m");
    }

    #[test]
    fn a_finished_run_shows_how_long_it_took() {
        let c = check(
            CheckState::Failure,
            Some("2026-10-23T10:00:00Z"),
            Some("2026-10-23T10:01:42Z"),
        );
        assert_eq!(check_timing(&c).as_deref(), Some("1m 42s"));
    }

    #[test]
    fn no_timestamps_or_bad_ones_show_nothing() {
        // A status context, a queued run, garbage, a skew, a skipped run.
        let at = "2026-10-23T10:00:00Z";
        let earlier = "2026-10-23T09:00:00Z";
        for c in [
            check(CheckState::Success, None, None),
            check(CheckState::Pending, None, None),
            check(CheckState::Success, Some("soon"), Some(at)),
            check(CheckState::Success, Some(at), Some(earlier)),
            check(CheckState::Neutral, Some(at), Some(at)),
        ] {
            assert_eq!(check_timing(&c), None, "{c:?}");
        }
    }

    #[test]
    fn a_running_row_says_when_it_started() {
        let started = (chrono::Utc::now() - chrono::Duration::minutes(3)).to_rfc3339();
        let c = check(CheckState::Pending, Some(&started), None);
        assert_eq!(check_timing(&c).as_deref(), Some("started 3m ago"));
        let just = chrono::Utc::now().to_rfc3339();
        let c = check(CheckState::Pending, Some(&just), None);
        assert_eq!(check_timing(&c).as_deref(), Some("started just now"));
    }

    #[test]
    fn a_settled_run_without_an_end_claims_nothing() {
        let c = check(CheckState::Failure, Some("2026-10-23T10:00:00Z"), None);
        assert_eq!(check_timing(&c), None);
    }
}
