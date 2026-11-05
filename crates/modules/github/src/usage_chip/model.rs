//! What the chip shows, derived purely (docs/0082 D13): the book's
//! verbatim snapshots (0050 D1) with each budget's own pause laid over
//! it (0050 D4, Q10), marked by the shared D5 ladder. Everything the
//! view draws or speaks comes from here; rendering never re-derives.

use dashboard_core::usage_analytics::ladder;
use dashboard_core::usage_analytics::surfaces::ThresholdState;

use crate::rate_pause::{Lane, RateLimitPause};
use crate::usage::{BucketDisplay, UsageDisplay};

/// One budget's chip state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BudgetChip {
    /// No response has reported this budget yet: `REST —`.
    Missing,
    /// The server's numbers, verbatim. `state` is the D5 ladder over the
    /// snapshot; `None` once the window has rolled past its reset (the
    /// stale number says nothing about the refilled budget).
    Live {
        d: BucketDisplay,
        state: Option<ThresholdState>,
        /// The window's reset, epoch seconds.
        reset_at: u64,
    },
    /// This budget's pause is in force (Q10): `GraphQL paused · resumes
    /// 15:41`. The other budget keeps its own state.
    Paused { until: u64, resumes_in_secs: u64 },
}

impl BudgetChip {
    fn of(d: Option<BucketDisplay>, pause: Option<u64>, now: u64) -> Self {
        if let Some(until) = pause {
            return BudgetChip::Paused {
                until,
                resumes_in_secs: until.saturating_sub(now),
            };
        }
        match d {
            None => BudgetChip::Missing,
            Some(d) => BudgetChip::Live {
                d,
                state: (!d.stale)
                    .then(|| ThresholdState::of(d.remaining, Some(d.limit)))
                    .flatten(),
                reset_at: now + d.resets_in_secs,
            },
        }
    }

    /// The ladder state the chip marks: `None` and Normal draw no icon.
    pub(crate) fn state(&self) -> Option<ThresholdState> {
        match self {
            BudgetChip::Missing => None,
            BudgetChip::Live { state, .. } => *state,
            BudgetChip::Paused { .. } => Some(ThresholdState::Exhausted),
        }
    }

    /// The window this state belongs to, epoch seconds: the Critical
    /// toast's dedupe key (D5).
    pub(crate) fn window_reset(&self) -> Option<u64> {
        match self {
            BudgetChip::Missing => None,
            BudgetChip::Live { reset_at, .. } => Some(*reset_at),
            BudgetChip::Paused { until, .. } => Some(*until),
        }
    }
}

/// Both budgets, REST first (D13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChipModel {
    pub(crate) rest: BudgetChip,
    pub(crate) graphql: BudgetChip,
}

impl ChipModel {
    pub(crate) fn of(display: &UsageDisplay, pause: &RateLimitPause, now: u64) -> Self {
        let (rest, graphql) = match display {
            UsageDisplay::Pending => (None, None),
            UsageDisplay::Budget { rest, graphql } => (*rest, *graphql),
        };
        ChipModel {
            rest: BudgetChip::of(rest, pause.in_force(Lane::Rest, now), now),
            graphql: BudgetChip::of(graphql, pause.in_force(Lane::Graphql, now), now),
        }
    }

    pub(crate) fn budgets(&self) -> [(Lane, BudgetChip); 2] {
        [(Lane::Rest, self.rest), (Lane::Graphql, self.graphql)]
    }
}

/// One budget's visible segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Segment {
    /// The state whose icon leads the segment (Low, Critical, paused or
    /// exhausted); `None` draws no icon.
    pub(crate) icon: Option<ThresholdState>,
    /// The words and number, `GraphQL 1,140 Low`.
    pub(crate) text: String,
    /// A marked budget's text is `foreground`, semibold (Q1); a normal
    /// or missing one stays `muted_foreground`.
    pub(crate) marked: bool,
}

/// The segment for one budget (D13).
pub(crate) fn segment(lane: Lane, chip: BudgetChip) -> Segment {
    let name = lane.name();
    match chip {
        BudgetChip::Missing => Segment {
            icon: None,
            text: format!("{name} —"),
            marked: false,
        },
        BudgetChip::Paused { until, .. } => Segment {
            icon: Some(ThresholdState::Exhausted),
            text: format!("{name} paused · resumes {}", local_hhmm(until)),
            marked: true,
        },
        BudgetChip::Live { d, state, .. } => {
            let number = format!("{name} {}", group_thousands(d.remaining));
            match state.and_then(ladder::word) {
                Some(word) => Segment {
                    icon: state,
                    text: format!("{number} {word}"),
                    marked: true,
                },
                None => Segment {
                    icon: None,
                    text: number,
                    marked: false,
                },
            }
        }
    }
}

fn unit(lane: Lane) -> &'static str {
    match lane {
        Lane::Rest => "requests",
        Lane::Graphql => "points",
    }
}

/// One budget's spoken fragment: numbers spelled out, the state word and
/// the pause named (0050 D5, 0082 D13).
fn spoken(lane: Lane, chip: BudgetChip) -> String {
    let name = lane.name();
    match chip {
        BudgetChip::Missing => format!("{name}: no response yet"),
        BudgetChip::Paused {
            until,
            resumes_in_secs,
        } => format!(
            "{name}: paused, resumes at {} in {}",
            local_hhmm(until),
            spoken_duration(resumes_in_secs)
        ),
        BudgetChip::Live { d, state, .. } => {
            let mut out = format!(
                "{name}: {} of {} {} remaining",
                group_thousands(d.remaining),
                group_thousands(d.limit),
                unit(lane)
            );
            if let Some(word) = state.and_then(ladder::word) {
                out = format!("{out}, {word}");
            }
            match d.snapshot_at.filter(|_| d.stale) {
                Some(at) => format!("{out}, last updated {}", local_hhmm(at)),
                None => format!("{out}, resets in {}", spoken_duration(d.resets_in_secs)),
            }
        }
    }
}

/// The chip's accessible name (0049 D1): the sole a11y carrier, since a
/// hover tooltip is mouse-only.
pub(crate) fn label(model: &ChipModel, session: (u64, u64, u64, u64)) -> String {
    let (total, graphql, rest, auth) = session;
    format!(
        "GitHub API: {}; {}; session: {total} requests ({graphql} GraphQL, {rest} REST, \
         {auth} auth)",
        spoken(Lane::Rest, model.rest),
        spoken(Lane::Graphql, model.graphql)
    )
}

/// One budget's tooltip line: limits and resets (D13).
fn tooltip_line(lane: Lane, chip: BudgetChip) -> String {
    let name = lane.name();
    match chip {
        BudgetChip::Missing => format!("{name} — no response yet"),
        BudgetChip::Paused {
            until,
            resumes_in_secs,
        } => format!(
            "{name} paused · resumes {} (in {})",
            local_hhmm(until),
            compact_duration(resumes_in_secs)
        ),
        BudgetChip::Live { d, .. } => {
            let head = format!(
                "{name} {}/{}",
                group_thousands(d.remaining),
                group_thousands(d.limit)
            );
            match d.snapshot_at.filter(|_| d.stale) {
                Some(at) => format!("{head} · as of {}", local_hhmm(at)),
                None => format!("{head} resets in {}", compact_duration(d.resets_in_secs)),
            }
        }
    }
}

/// The hover tooltip: one line per budget, then what a click does.
pub(crate) fn tooltip(model: &ChipModel) -> String {
    format!(
        "{}\n{}\nOpens the Budget view",
        tooltip_line(Lane::Rest, model.rest),
        tooltip_line(Lane::Graphql, model.graphql)
    )
}

/// Thousands-group a number (`4,821`).
pub(crate) fn group_thousands(n: u32) -> String {
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

/// Compact countdown (`41m`, `1h 05m`).
fn compact_duration(secs: u64) -> String {
    let m = secs / 60;
    if m < 60 {
        format!("{m}m")
    } else {
        format!("{}h {:02}m", m / 60, m % 60)
    }
}

/// Spoken countdown (`41 minutes`, `1 hours 5 minutes`).
fn spoken_duration(secs: u64) -> String {
    let m = secs / 60;
    if m < 60 {
        format!("{m} minutes")
    } else {
        format!("{} hours {} minutes", m / 60, m % 60)
    }
}

/// `14:03` from an epoch, local time.
pub(crate) fn local_hhmm(epoch: u64) -> String {
    use chrono::TimeZone as _;
    chrono::Local
        .timestamp_opt(epoch as i64, 0)
        .single()
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_else(|| "??:??".into())
}

#[cfg(test)]
mod tests;
