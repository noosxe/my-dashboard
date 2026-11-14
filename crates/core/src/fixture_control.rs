//! The debug bridge's two views into the offline GitHub transport
//! (docs/0081 §12.1, docs/0084 §8): the **request log** of every read the
//! fixture transport answered, served as `app.requests`, and a **control
//! hook** the transport registers so `fixture bump|budget|get` reach it.
//!
//! Process-wide, like [`crate::write_log`]: the transport lives in a
//! module crate and the bridge in another, and both depend on core. With
//! no fixture mode nothing records and no handler is registered, so the
//! log stays empty and every control command answers an error.

use std::sync::{Arc, Mutex, OnceLock};

use serde::Serialize;
use serde_json::Value;

/// Records kept; older ones are dropped (a receipt asserts recent reads).
pub const CAPACITY: usize = 2048;

/// One answered read. Field order is the wire order.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RequestRecord {
    /// 1-based, monotonic; the `since` cursor of `app.requests`.
    pub seq: u64,
    /// `graphql` or `rest`.
    pub kind: &'static str,
    /// GraphQL: the operation name (`PrDetail`). REST: `GET /<path>`
    /// without the host or query.
    pub op: String,
    /// What the answer was: `status`, `generation`, and for REST the
    /// `etag` served, the `if_none_match` received and the `remaining` REST
    /// budget stamped (null with no budget).
    pub vars: Value,
}

#[derive(Default)]
struct Log {
    next: u64,
    records: Vec<RequestRecord>,
}

fn log() -> &'static Mutex<Log> {
    static LOG: OnceLock<Mutex<Log>> = OnceLock::new();
    LOG.get_or_init(Mutex::default)
}

/// Append one read.
pub fn record_request(kind: &'static str, op: impl Into<String>, vars: Value) {
    let mut l = log().lock().unwrap_or_else(|e| e.into_inner());
    l.next += 1;
    let seq = l.next;
    l.records.push(RequestRecord {
        seq,
        kind,
        op: op.into(),
        vars,
    });
    if l.records.len() > CAPACITY {
        l.records.remove(0);
    }
}

/// Records with `seq > since`, oldest first, and the cursor to resume from.
pub fn requests_since(since: u64) -> (Vec<RequestRecord>, u64) {
    let l = log().lock().unwrap_or_else(|e| e.into_inner());
    let out: Vec<_> = l
        .records
        .iter()
        .filter(|r| r.seq > since)
        .cloned()
        .collect();
    let cursor = out.last().map_or(since, |r| r.seq);
    (out, cursor)
}

/// A control command handler: `args` are the verb's words after
/// `fixture` (`["bump", "1"]`); the answer is the JSON the verb prints.
pub type Handler = Arc<dyn Fn(&[String]) -> Result<Value, String> + Send + Sync>;

fn slot() -> &'static Mutex<Option<Handler>> {
    static SLOT: OnceLock<Mutex<Option<Handler>>> = OnceLock::new();
    SLOT.get_or_init(Mutex::default)
}

/// Register the transport's handler (the latest registration wins).
pub fn set_handler(handler: Handler) {
    *slot().lock().unwrap_or_else(|e| e.into_inner()) = Some(handler);
}

/// Run one `fixture …` command against the registered transport.
pub fn run(args: &[String]) -> Result<Value, String> {
    let handler = slot().lock().unwrap_or_else(|e| e.into_inner()).clone();
    match handler {
        Some(h) => h(args),
        None => Err("no fixture transport: start the app with DASHBOARD_GITHUB_FIXTURE set".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_resume_from_the_cursor() {
        let (before, cur0) = requests_since(0);
        let base = before.last().map_or(cur0, |r| r.seq);
        record_request("rest", "GET /fc/a", serde_json::json!({"status": 200}));
        record_request("graphql", "FcB", Value::Null);
        let (got, cursor) = requests_since(base);
        let ours: Vec<_> = got
            .iter()
            .filter(|r| r.op == "GET /fc/a" || r.op == "FcB")
            .collect();
        assert_eq!(ours.len(), 2);
        assert_eq!(ours[0].kind, "rest");
        assert!(requests_since(cursor).0.is_empty());
    }
}
