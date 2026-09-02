//! `app.logs` backing store (docs/0010 D8): an in-memory ring of the
//! last [`CAPACITY`] log lines, captured as a flexi_logger extra writer
//! and served over the debug socket.
//!
//! The ring is created in `main` before `logging::init` (log capture must
//! start as early as possible) and handed to both the logger (as a
//! `LogWriter`) and the debug server (as the `app.logs` data source).
//! Pull-based in v1: no streaming, no notifications — `app.logs` with a
//! cursor, polled by the CLI's `logs -f`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use flexi_logger::DeferredNow;
use flexi_logger::writers::LogWriter;
use log::Record;
use serde::Serialize;

/// Lines kept per docs/0010 D8.
pub const CAPACITY: usize = 4096;
/// Default `app.logs` batch size.
pub const DEFAULT_LIMIT: usize = 200;

/// One captured record. Field order is the wire order.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Line {
    pub cursor: u64,
    pub level: String,
    pub target: String,
    pub message: String,
}

#[derive(Debug)]
struct RingState {
    lines: VecDeque<Line>,
    /// Cursor of the next line to append; first line is 1 so the `since`
    /// default of 0 means "from the beginning".
    next_cursor: u64,
}

/// Shared handle: cloned for the writer half and the reader half.
#[derive(Debug, Clone, Default)]
pub struct RingLog {
    state: Arc<Mutex<RingState>>,
}

impl Default for RingState {
    fn default() -> Self {
        Self {
            lines: VecDeque::new(),
            // First line is 1 so the `since` default of 0 means "from the
            // beginning" (see the field docs above).
            next_cursor: 1,
        }
    }
}

impl RingLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// The `flexi_logger` side. Captures `level`/`target`/`message` as
    /// structured fields — the ring never stores a pre-formatted line.
    pub fn writer(&self) -> Box<dyn LogWriter> {
        Box::new(RingWriter {
            state: Arc::clone(&self.state),
        })
    }

    /// Lines with `cursor > since`, oldest first, at most `limit`.
    /// Returns them plus the cursor to pass next time: the last returned
    /// line's cursor, or `since` unchanged when nothing (new) matched —
    /// a continuation cursor, not a high-water mark, so a truncated
    /// batch picks up exactly where it stopped.
    pub fn snapshot(&self, since: u64, limit: usize) -> (Vec<Line>, u64) {
        let state = self.state.lock().expect("ring poisoned");
        let mut next = since;
        let lines: Vec<Line> = state
            .lines
            .iter()
            .filter(|line| line.cursor > since)
            .take(limit)
            .map(|line| {
                next = line.cursor;
                line.clone()
            })
            .collect();
        (lines, next)
    }
}

/// `LogWriter` adapter — interior mutability via the shared `Arc`, so the
/// `Box<dyn LogWriter>` handed to flexi_logger and the reader half see
/// the same state.
struct RingWriter {
    state: Arc<Mutex<RingState>>,
}

impl LogWriter for RingWriter {
    fn write(&self, _now: &mut DeferredNow, record: &Record<'_>) -> std::io::Result<()> {
        let mut state = self.state.lock().expect("ring poisoned");
        if state.lines.len() >= CAPACITY {
            state.lines.pop_front();
        }
        let cursor = state.next_cursor;
        state.lines.push_back(Line {
            cursor,
            level: record.level().to_string(),
            target: record.target().to_string(),
            message: record.args().to_string(),
        });
        state.next_cursor = cursor + 1;
        Ok(())
    }

    fn flush(&self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log_line(writer: &dyn LogWriter, level: log::Level, target: &str, message: &str) {
        let mut now = DeferredNow::new();
        let owned = message.to_string();
        let arguments = format_args!("{owned}");
        let record = log::Record::builder()
            .level(level)
            .target(target)
            .args(arguments)
            .build();
        writer.write(&mut now, &record).unwrap();
    }

    #[test]
    fn cursors_start_at_one_and_advance() {
        let ring = RingLog::new();
        let writer = ring.writer();
        log_line(writer.as_ref(), log::Level::Info, "t", "one");
        log_line(writer.as_ref(), log::Level::Warn, "t", "two");

        let (lines, next) = ring.snapshot(0, DEFAULT_LIMIT);
        assert_eq!(
            lines,
            vec![
                Line {
                    cursor: 1,
                    level: "INFO".into(),
                    target: "t".into(),
                    message: "one".into()
                },
                Line {
                    cursor: 2,
                    level: "WARN".into(),
                    target: "t".into(),
                    message: "two".into()
                },
            ]
        );
        assert_eq!(next, 2);
    }

    #[test]
    fn since_filters_and_empty_snapshots_keep_the_cursor() {
        let ring = RingLog::new();
        let writer = ring.writer();
        log_line(writer.as_ref(), log::Level::Info, "t", "one");
        log_line(writer.as_ref(), log::Level::Info, "t", "two");

        let (lines, next) = ring.snapshot(1, DEFAULT_LIMIT);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].message, "two");
        assert_eq!(next, 2);

        let (lines, next) = ring.snapshot(2, DEFAULT_LIMIT);
        assert!(lines.is_empty());
        assert_eq!(next, 2, "no new lines → cursor unchanged, not advanced");
    }

    #[test]
    fn capacity_evicts_oldest_but_cursors_stay_monotonic() {
        let ring = RingLog::new();
        let writer = ring.writer();
        for i in 0..(CAPACITY + 10) {
            log_line(writer.as_ref(), log::Level::Info, "t", &format!("m{i}"));
        }

        let (lines, next) = ring.snapshot(0, DEFAULT_LIMIT);
        assert_eq!(lines.len(), DEFAULT_LIMIT);
        assert_eq!(lines[0].cursor, 11, "first 10 evicted");
        assert_eq!(lines[0].message, "m10");
        assert_eq!(next, 210, "limit truncates → continuation cursor");

        // …and the continuation picks up exactly where it stopped.
        let (rest, last) = ring.snapshot(next, CAPACITY);
        assert_eq!(rest.first().unwrap().cursor, 211);
        assert_eq!(rest.last().unwrap().cursor, CAPACITY as u64 + 10);
        assert_eq!(last, CAPACITY as u64 + 10);
        assert_eq!(rest.len(), CAPACITY + 10 - 210);
    }

    #[test]
    fn writer_flush_is_a_noop_success() {
        assert!(RingLog::new().writer().flush().is_ok());
    }
}
