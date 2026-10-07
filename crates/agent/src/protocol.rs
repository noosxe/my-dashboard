//! Wire protocol (docs/0068 §6): NDJSON request/response envelopes, the
//! typed auth params, the error-code table, and the mandatory
//! freshness/truncation envelope metadata every op result carries (D5).
//! Shapes are frozen here — later issues (#468/#469) only add ops and a
//! front-end, not new wire syntax.

use serde::{Deserialize, Serialize};

/// First line of every connection: `{"op":"auth","params":{"token":"…"}}`.
pub const OP_AUTH: &str = "auth";
/// Wire-syntax version carried by request lines after auth; the auth line
/// carries none. A present `v` must be exactly this.
pub const PROTOCOL_VERSION: u64 = 1;

/// Error codes (docs/0068 §6): string names mirroring the docs/0010
/// taxonomy. Two of them never come from the server: a disabled bridge
/// never binds (the #469 front-end answers `DISABLED`, naming the config
/// path), and phase 1 never touches the network (`NOT_CONNECTED` /
/// `RATE_LIMITED` are reserved for phase-2 fetch ops).
pub mod error_code {
    pub const PARSE: &str = "PARSE";
    pub const AUTH: &str = "AUTH";
    pub const UNKNOWN_OP: &str = "UNKNOWN_OP";
    pub const BAD_PARAMS: &str = "BAD_PARAMS";
    /// Phase-2 reserved: an op asked for state whose owning module has no
    /// live connection. Phase 1 reads held state only and never emits it.
    #[allow(dead_code)]
    pub const NOT_CONNECTED: &str = "NOT_CONNECTED";
    /// Phase-2 reserved: emitted once fetch ops meet `rate_limited_until`.
    #[allow(dead_code)]
    pub const RATE_LIMITED: &str = "RATE_LIMITED";
    /// Never server-side (see module doc); defined for the #469 front-end
    /// so the CLI/MCP layer can name the kill switch uniformly.
    #[allow(dead_code)]
    pub const DISABLED: &str = "DISABLED";
    /// Transport-level failure inside the server (queue or executor gone,
    /// serialization). Inherited from the docs/0010 taxonomy; the §6 table
    /// lists the protocol-level codes, this one answers infra failures.
    pub const INTERNAL: &str = "INTERNAL";
}

/// One request line: `{"v":1,"id":n,"op":"list_repos","params":{…}}`.
/// The auth line carries none of `v`/`id` — all three of `v`, `id` and
/// `params` default so one struct parses both shapes.
#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub v: Option<u64>,
    #[serde(default)]
    pub id: Option<u64>,
    pub op: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// The request's response id; the auth handshake answers with `0`.
pub fn request_id(request: &Request) -> u64 {
    request.id.unwrap_or(0)
}

/// One response line: `ok: true` with `result`, or `ok: false` with
/// `error` — never both, never neither.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Response {
    pub id: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// Parse typed params out of a request's `params` value; `None` when the
/// shape doesn't match (callers answer `BAD_PARAMS`/auth failure).
pub fn parse<T: serde::de::DeserializeOwned>(params: &serde_json::Value) -> Option<T> {
    serde_json::from_value(params.clone()).ok()
}

impl Response {
    pub fn ok(id: u64, result: serde_json::Value) -> Self {
        Self {
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: u64, error: RpcError) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(error),
        }
    }

    /// Serialize as one NDJSON line (newline included).
    pub fn to_line(&self) -> String {
        let mut line = serde_json::to_string(self).expect("responses are always serializable");
        line.push('\n');
        line
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RpcError {
    pub code: String,
    pub message: String,
}

impl RpcError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

/// `auth` params: `{"token": "…"}` — the per-run token from
/// `agent.token` next to the socket.
#[derive(Debug, Clone, Deserialize)]
pub struct AuthParams {
    pub token: String,
}

/// Mandatory per-op envelope metadata (docs/0068 D5): how stale and how
/// complete every success response is. Ops embed these; the server does
/// not inject them, and #468's tests assert their presence per op.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Freshness {
    /// Age of the backing state at response time; `None` only when the
    /// state carries no timestamp at all.
    pub age_s: Option<u64>,
    pub source: FreshnessSource,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FreshnessSource {
    /// Read back from the on-disk store (a restart-surviving snapshot).
    Disk,
    /// Read from held in-memory state (this session's snapshot).
    Cache,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Truncation {
    pub returned: u64,
    /// `None` when the true total is unknowable — e.g. a list capped
    /// below the cursor-catch-up horizon (docs/0029 §2).
    pub total_known: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- envelope round-trips ------------------------------------------------

    #[test]
    fn request_line_round_trip() {
        let request: Request =
            serde_json::from_str(r#"{"v":1,"id":7,"op":"list_repos","params":{"org":"noosxe"}}"#)
                .unwrap();
        assert_eq!(request.v, Some(1));
        assert_eq!(request.id, Some(7));
        assert_eq!(request.op, "list_repos");
        assert_eq!(request.params["org"], "noosxe");

        // The auth shape parses with the same struct: no v, no id.
        let auth: Request =
            serde_json::from_str(r#"{"op":"auth","params":{"token":"t"}}"#).unwrap();
        assert_eq!(auth.v, None);
        assert_eq!(auth.id, None);
        assert_eq!(request_id(&auth), 0);
    }

    #[test]
    fn response_carries_exactly_one_of_result_or_error() {
        let ok = Response::ok(3, serde_json::json!({"freshness": {}}));
        let line = ok.to_line();
        assert!(line.contains(r#""ok":true"#));
        assert!(!line.contains("error"));
        let parsed: Response = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(parsed, ok);

        let err = Response::err(4, RpcError::new(error_code::UNKNOWN_OP, "nope"));
        let line = err.to_line();
        assert!(line.contains(r#""ok":false"#));
        assert!(!line.contains("result"));
        let parsed: Response = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(parsed, err);
    }

    #[test]
    fn error_codes_are_strings() {
        let err = Response::err(0, RpcError::new(error_code::BAD_PARAMS, "bad v"));
        assert!(err.to_line().contains(r#""code":"BAD_PARAMS""#));
    }

    #[test]
    fn auth_params_parse() {
        let params = serde_json::json!({"token": "abc123"});
        let parsed: AuthParams = parse(&params).unwrap();
        assert_eq!(parsed.token, "abc123");
        assert!(parse::<AuthParams>(&serde_json::json!({})).is_none());
    }

    #[test]
    fn envelope_metadata_serializes_with_documented_field_names() {
        let freshness = Freshness {
            age_s: Some(42),
            source: FreshnessSource::Disk,
        };
        let line = serde_json::to_string(&freshness).unwrap();
        assert_eq!(line, r#"{"age_s":42,"source":"disk"}"#);

        let freshness = Freshness {
            age_s: Some(0),
            source: FreshnessSource::Cache,
        };
        assert!(
            serde_json::to_string(&freshness)
                .unwrap()
                .contains(r#""source":"cache""#)
        );

        let truncation = Truncation {
            returned: 100,
            total_known: None,
        };
        assert_eq!(
            serde_json::to_string(&truncation).unwrap(),
            r#"{"returned":100,"total_known":null}"#
        );
    }
}
