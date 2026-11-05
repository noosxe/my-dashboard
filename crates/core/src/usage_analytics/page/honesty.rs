//! The honesty line (docs/0082 D3, 0054 D2): what's on screen, the
//! frozen anchor and how far retention actually reaches, with dates.
//! Under `Current window` it names the selected budget's window (Q5).
//! It never pretends to more history than the retained records support.

use super::range::budget_label;
use crate::usage_analytics::surfaces::{Budget, CurrentWindow};

/// What the line states. Pure input: the page fills it from the frozen
/// read, so the line is testable without a window.
pub(super) struct Honesty {
    pub loading: bool,
    pub as_of_ms: u64,
    /// Requests inside the active view's bounds (D21: requests).
    pub requests: usize,
    /// `Some` only under `Current window` on the Budget view: the
    /// selected budget and its open window, if there is one.
    pub budget_window: Option<(Budget, Option<CurrentWindow>)>,
    /// The oldest retained record in the read, if any.
    pub oldest_ms: Option<u64>,
}

impl Honesty {
    pub(super) fn line(&self) -> String {
        let as_of = format!("as of {}", format_date_time(self.as_of_ms));
        if self.loading {
            return format!("Reading history · {as_of}");
        }
        let n = self.requests;
        let mut bits = vec![format!(
            "{} request{}",
            fmt_count(n as u64),
            if n == 1 { "" } else { "s" }
        )];
        if let Some((budget, window)) = self.budget_window {
            let name = budget_label(budget);
            bits.push(match window {
                Some(w) => format!(
                    "{name} window {}–{}",
                    format_clock(w.start_ms),
                    format_clock(w.reset_ms)
                ),
                None => format!("no open {name} window"),
            });
        }
        bits.push(as_of);
        bits.push(match self.oldest_ms {
            Some(oldest) => format!("history from {}", format_date_time(oldest)),
            None => "no history yet".into(),
        });
        bits.join(" · ")
    }
}

pub(super) fn format_clock(epoch_ms: u64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(epoch_ms as i64)
        .single()
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_else(|| "—".into())
}

/// The line's dated stamp (D3): `Nov 4, 19:21`.
pub(super) fn format_date_time(epoch_ms: u64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(epoch_ms as i64)
        .single()
        .map(|t| t.format("%b %-d, %H:%M").to_string())
        .unwrap_or_else(|| "—".into())
}

/// Thousands-grouped counts: `1,087 requests`.
fn fmt_count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const AS_OF: u64 = 10_000_000_000;

    fn honesty(requests: usize, oldest_ms: Option<u64>) -> Honesty {
        Honesty {
            loading: false,
            as_of_ms: AS_OF,
            requests,
            budget_window: None,
            oldest_ms,
        }
    }

    #[test]
    fn the_line_counts_requests_and_states_retention_with_dates() {
        let line = honesty(2, Some(AS_OF - 5_000)).line();
        // D3/D21: requests, not flights; dated anchors, oldest row last.
        assert_eq!(
            line,
            format!(
                "2 requests · as of {} · history from {}",
                format_date_time(AS_OF),
                format_date_time(AS_OF - 5_000)
            )
        );
        assert!(honesty(1, Some(AS_OF)).line().starts_with("1 request · "));
        assert!(honesty(1_087, None).line().starts_with("1,087 requests · "));
        assert_eq!(
            honesty(0, None).line().split(" · ").last(),
            Some("no history yet")
        );
    }

    #[test]
    fn current_window_names_the_selected_budget_or_says_none_is_open() {
        let w = CurrentWindow {
            start_ms: AS_OF - 20 * 60_000,
            reset_ms: AS_OF + 40 * 60_000,
        };
        let mut h = honesty(3, Some(AS_OF - 60_000));
        h.budget_window = Some((Budget::Graphql, Some(w)));
        assert!(
            h.line().contains(&format!(
                "3 requests · GraphQL window {}–{} · as of ",
                format_clock(w.start_ms),
                format_clock(w.reset_ms)
            )),
            "{}",
            h.line()
        );
        h.budget_window = Some((Budget::Rest, None));
        assert!(
            h.line()
                .starts_with("3 requests · no open REST window · as of ")
        );
    }

    #[test]
    fn loading_states_the_anchor_only() {
        let mut h = honesty(5, Some(AS_OF - 1));
        h.loading = true;
        assert_eq!(
            h.line(),
            format!("Reading history · as of {}", format_date_time(AS_OF))
        );
    }
}
