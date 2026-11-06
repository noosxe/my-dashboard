//! The fixture write record (docs/0081 §12.1, docs/0010 D11): every write
//! the offline GitHub transport answers is appended here, and the debug
//! bridge serves the list as `app.writes` so a receipt can assert what a
//! click actually sent. Only the fixture transport records; with no
//! fixture mode the log stays empty.
//!
//! A process-wide log, like the logger: the transport lives in a module
//! crate and the bridge in another, and both already depend on core.

use std::sync::{Mutex, OnceLock};

use serde::Serialize;
use serde_json::Value;

/// Records kept; older ones are dropped (a receipt asserts recent writes).
pub const CAPACITY: usize = 512;

/// One answered write. Field order is the wire order.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WriteRecord {
    /// 1-based, monotonic; the `since` cursor of `app.writes`.
    pub seq: u64,
    /// `graphql` or `rest`.
    pub kind: &'static str,
    /// GraphQL: the operation name (`PrMerge`). REST: `<VERB> /<path>`
    /// without the host or query (`POST /repos/o/r/actions/runs/7/approve`).
    pub op: String,
    /// GraphQL: the request variables (`{"input": {…}}`). REST: the
    /// parsed JSON body, `null` when the write sends none.
    pub vars: Value,
}

#[derive(Default)]
struct State {
    next: u64,
    records: Vec<WriteRecord>,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Mutex::default)
}

/// Append one write.
pub fn record(kind: &'static str, op: impl Into<String>, vars: Value) {
    let mut s = state().lock().unwrap_or_else(|e| e.into_inner());
    s.next += 1;
    let seq = s.next;
    s.records.push(WriteRecord {
        seq,
        kind,
        op: op.into(),
        vars,
    });
    if s.records.len() > CAPACITY {
        s.records.remove(0);
    }
}

/// Records with `seq > since`, oldest first, and the cursor to resume from.
pub fn since(since: u64) -> (Vec<WriteRecord>, u64) {
    let s = state().lock().unwrap_or_else(|e| e.into_inner());
    let out: Vec<_> = s
        .records
        .iter()
        .filter(|r| r.seq > since)
        .cloned()
        .collect();
    let cursor = out.last().map_or(since, |r| r.seq);
    (out, cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_resume_from_the_cursor() {
        let (before, cur0) = since(0);
        let base = before.last().map_or(cur0, |r| r.seq);
        record("graphql", "WlA", serde_json::json!({"input": {"a": 1}}));
        record("rest", "POST /x", Value::Null);
        let (got, cursor) = since(base);
        let ours: Vec<_> = got
            .iter()
            .filter(|r| r.op == "WlA" || r.op == "POST /x")
            .collect();
        assert_eq!(ours.len(), 2);
        assert_eq!(ours[0].kind, "graphql");
        let (none, same) = since(cursor);
        assert!(none.is_empty());
        assert_eq!(same, cursor);
    }
}
