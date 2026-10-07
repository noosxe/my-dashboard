//! The MCP front-end's brain (docs/0068 D8): a dumb, thin JSON-RPC
//! layer over stdio that maps **one MCP tool per bridge protocol op**.
//! It holds no state of its own — tool calls proxy straight to the
//! socket connection ([`Bridge`]), auth is the first-line handshake
//! ([`crate::client`]), and every tool description states the freshness
//! semantics so a model knows it is reading held state, not live data.
//!
//! Framing follows the MCP stdio transport: newline-delimited JSON-RPC
//! 2.0, one message per line, nothing but messages on stdout. The
//! hand-written input schemas are unit-tested against the ops' serde
//! param types (the coupling test lives next to those types, in the
//! github module — no JSON-Schema dependency).

use serde_json::{Value, json};
use std::sync::LazyLock;

use crate::protocol::{self, RpcError};

/// The stdio MCP protocol versions this front-end speaks, newest first
/// (spec 2025-06-18; older clients get an echo when we support them).
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// JSON-RPC error codes the front-end answers with (RFC / JSON-RPC 2.0).
mod jsonrpc {
    pub const PARSE_ERROR: i64 = -32700;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
}

/// The transport seam: one bridge op call. The real implementation is
/// [`crate::client::BridgeClient`]; tests stub it.
pub trait Bridge {
    fn call(&mut self, op: &str, params: &Value) -> Result<Value, RpcError>;
}

impl Bridge for crate::client::BridgeClient {
    fn call(&mut self, op: &str, params: &Value) -> Result<Value, RpcError> {
        crate::client::BridgeClient::call(self, op, params)
    }
}

/// One tool = one protocol op. `input_schema` is hand-written and
/// coupled to the owning module's serde params by test (see the module
/// doc); `description` carries the freshness semantics from docs/0068
/// §6 so agents read honesty, not optimism.
pub struct ToolSpec {
    pub op: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
}

/// The tool table, in ops-table order (§6). `LazyLock` because `json!`
/// values are runtime-built.
pub static TOOL_SPECS: LazyLock<Vec<ToolSpec>> = LazyLock::new(|| {
    vec![
        ToolSpec {
            op: "app_status",
            description: "Github module connectivity and data-freshness snapshot: whether the \
                      app is authenticated, the connected user, org and repo counts, how old \
                      the held snapshot/notifications are (freshness.age_s, source \
                      disk|cache), and rate-limit/token expiry. Pure held state — never \
                      touches the network.",
            input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        },
        ToolSpec {
            op: "list_repos",
            description: "The held repo inventory: one metadata row per repo the app has synced \
                      (nameWithOwner, owner, archived flag, how many PR/issue rows are held). \
                      Reads the cached snapshot, up to ~5 min old; the app caps the snapshot \
                      at 300 repos, so truncation.total_known is null at the cap — null means \
                      unknown, not zero.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "org": { "type": "string", "description": "Filter by owner login" }
                },
                "additionalProperties": false
            }),
        },
        ToolSpec {
            op: "notifications_unread",
            description: "The app's currently UNREAD notifications (held state; the app polls \
                      roughly every minute). Filter by repository full_name. The list never \
                      includes notifications already read in the app.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "repo": { "type": "string", "description": "Filter by repository full_name (owner/name)" }
                },
                "additionalProperties": false
            }),
        },
        ToolSpec {
            op: "repo_prs",
            description: "Open pull requests held for ONE repo. The app only holds repo-scoped \
                      rows after that repo's view was opened in the session — a repo that \
                      was never opened returns a structured not_held result (held: false, \
                      never an empty array). Reads held rows only, no network flight.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "repo": { "type": "string", "description": "Repository key or full_name (owner/name)" },
                    "state": { "type": "string", "enum": ["open"], "description": "Only \"open\" is held (default)" }
                },
                "required": ["repo"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            op: "repo_issues",
            description: "Open issues held for ONE repo, with the same not_held honesty as \
                      repo_prs: rows exist only if the repo's view was opened this session.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "repo": { "type": "string", "description": "Repository key or full_name (owner/name)" },
                    "state": { "type": "string", "enum": ["open"], "description": "Only \"open\" is held (default)" }
                },
                "required": ["repo"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            op: "api_usage",
            description: "The app's own Github network-usage ledger, grouped: request counts, \
                      REST point cost sums, minimum rate-limit remaining, errors and 304 \
                      revalidations. Reads the ledger without adding to it. Retention \
                      evicts old rows, so truncation.total_known is null — a group list is \
                      never a complete billing record.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "from_ms": { "type": "integer", "description": "Window start (epoch ms, inclusive)" },
                    "to_ms": { "type": "integer", "description": "Window end (epoch ms, exclusive; default now)" },
                    "group_by": { "type": "string", "enum": ["op", "surface", "trigger", "status"], "description": "Grouping key (default op)" },
                    "window": { "type": "string", "enum": ["1h", "1d", "1w", "1m"], "description": "Named window ending now (alternative to from_ms/to_ms)" }
                },
                "additionalProperties": false
            }),
        },
    ]
});

/// The stdio MCP server: owns the (optional) bridge connection and
/// answers one JSON-RPC line at a time.
pub struct McpServer<B: Bridge> {
    bridge: Option<B>,
}

impl<B: Bridge> McpServer<B> {
    pub fn new(bridge: Option<B>) -> Self {
        Self { bridge }
    }

    pub fn bridge_mut(&mut self) -> &mut Option<B> {
        &mut self.bridge
    }

    /// Handle one stdin line; `None` for notifications and anything
    /// that must not produce output (the stdio transport forbids
    /// non-message bytes on stdout).
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        if line.trim().is_empty() {
            return None;
        }
        let message: Value = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(err) => {
                return Some(error_reply(
                    Value::Null,
                    jsonrpc::PARSE_ERROR,
                    &err.to_string(),
                ));
            }
        };

        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let method = message.get("method").and_then(Value::as_str);
        match method {
            // Requests (they carry an id).
            Some("initialize") => Some(result_reply(id, self.initialize(&message))),
            Some("tools/list") => Some(result_reply(id, json!({ "tools": tool_descriptors() }))),
            Some("tools/call") => Some(self.tools_call(&message, id)),
            Some("ping") => Some(result_reply(id, json!({}))),
            // Unknown *requests* are a protocol error; unknown
            // *notifications* (no id) are silently ignored.
            Some(other) if !id.is_null() => Some(error_reply(id, jsonrpc::METHOD_NOT_FOUND, other)),
            Some(_) => None,
            // Responses have no method — nothing this server awaits.
            None => None,
        }
    }

    /// Capability negotiation: echo the client's version when we speak
    /// it, otherwise answer with our newest (the client decides whether
    /// that works for it).
    fn initialize(&self, message: &Value) -> Value {
        let requested = message
            .pointer("/params/protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or("");
        let negotiated = if SUPPORTED_PROTOCOL_VERSIONS.contains(&requested) {
            requested
        } else {
            SUPPORTED_PROTOCOL_VERSIONS[0]
        };
        json!({
            "protocolVersion": negotiated,
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": "dashboard-agent-mcp",
                "version": env!("CARGO_PKG_VERSION"),
            },
        })
    }

    /// tools/call: find the tool, proxy to the bridge. Per the MCP
    /// tools spec, unknown tools and malformed arguments are protocol
    /// errors (JSON-RPC error), while failures *executing* a known tool
    /// (bridge unreachable, op error) are tool results with
    /// `isError: true` — the model reads those.
    fn tools_call(&mut self, message: &Value, id: Value) -> String {
        let name = message.pointer("/params/name").and_then(Value::as_str);
        let Some(spec) = name.and_then(|name| TOOL_SPECS.iter().find(|spec| spec.op == name))
        else {
            return error_reply(
                id,
                jsonrpc::METHOD_NOT_FOUND,
                "unknown tool; see tools/list",
            );
        };
        let arguments = message
            .pointer("/params/arguments")
            .cloned()
            .unwrap_or(json!({}));
        if !arguments.is_object() {
            return error_reply(id, jsonrpc::INVALID_PARAMS, "arguments must be an object");
        }

        let Some(bridge) = self.bridge.as_mut() else {
            return tool_error(
                id,
                json!({
                    "error": protocol::error_code::NOT_CONNECTED,
                    "message": "no bridge connection (is the app running with agent_bridge enabled?)",
                }),
            );
        };
        match bridge.call(spec.op, &arguments) {
            Ok(result) => tool_text(id, &result),
            Err(error) => tool_error(
                id,
                json!({
                    "error": error.code,
                    "message": error.message,
                }),
            ),
        }
    }
}

/// One MCP tool descriptor per spec entry.
fn tool_descriptors() -> Value {
    Value::Array(
        TOOL_SPECS
            .iter()
            .map(|spec| {
                json!({
                    "name": spec.op,
                    "description": spec.description,
                    "inputSchema": spec.input_schema,
                })
            })
            .collect(),
    )
}

/// A successful tool result: the op's JSON payload verbatim as the
/// single text content — `not_held` and honesty fields reach the model
/// unfiltered (D5).
fn tool_text(id: Value, result: &Value) -> String {
    result_reply(
        id,
        json!({ "content": [ { "type": "text", "text": result.to_string() } ] }),
    )
}

/// A tool-level failure: same shape plus `isError: true`.
fn tool_error(id: Value, payload: Value) -> String {
    result_reply(
        id,
        json!({
            "content": [ { "type": "text", "text": payload.to_string() } ],
            "isError": true,
        }),
    )
}

fn result_reply(id: Value, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

fn error_reply(id: Value, code: i64, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::error_code;
    use std::cell::RefCell;

    /// Records calls; answers per the configured script. `calls` is
    /// shared with the test so assertions survive the bridge moving
    /// into the server.
    type Recorded = std::rc::Rc<RefCell<Vec<(String, Value)>>>;

    struct StubBridge {
        calls: Recorded,
        reply: RefCell<Result<Value, RpcError>>,
    }

    impl StubBridge {
        fn ok() -> (Self, Recorded) {
            let calls: Recorded = std::rc::Rc::new(RefCell::new(vec![]));
            (
                Self {
                    calls: std::rc::Rc::clone(&calls),
                    reply: RefCell::new(Ok(json!({ "held": true, "rows": [] }))),
                },
                calls,
            )
        }
    }

    impl Bridge for StubBridge {
        fn call(&mut self, op: &str, params: &Value) -> Result<Value, RpcError> {
            self.calls
                .borrow_mut()
                .push((op.to_owned(), params.clone()));
            self.reply.borrow().clone()
        }
    }

    fn parse(line: &str) -> Value {
        serde_json::from_str(line).expect("valid JSON-RPC line")
    }

    fn request(id: Value, method: &str, params: Value) -> String {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string()
    }

    #[test]
    fn initialize_echoes_a_supported_version() {
        let mut server = McpServer::new(None::<StubBridge>);
        let reply = parse(
            &server
                .handle_line(&request(
                    json!(1),
                    "initialize",
                    json!({ "protocolVersion": "2025-03-26", "capabilities": {} }),
                ))
                .unwrap(),
        );
        assert_eq!(reply["result"]["protocolVersion"], json!("2025-03-26"));
        assert_eq!(reply["result"]["serverInfo"]["name"], "dashboard-agent-mcp");
        assert_eq!(reply["result"]["capabilities"]["tools"], json!({}));
    }

    #[test]
    fn initialize_falls_back_to_our_newest_on_unknown_version() {
        let mut server = McpServer::new(None::<StubBridge>);
        let reply = parse(
            &server
                .handle_line(&request(
                    json!(1),
                    "initialize",
                    json!({ "protocolVersion": "1999-01-01" }),
                ))
                .unwrap(),
        );
        assert_eq!(
            reply["result"]["protocolVersion"],
            json!(SUPPORTED_PROTOCOL_VERSIONS[0])
        );
    }

    #[test]
    fn tools_list_exposes_one_tool_per_protocol_op() {
        let mut server = McpServer::new(None::<StubBridge>);
        let reply = parse(
            &server
                .handle_line(&request(json!(2), "tools/list", json!({})))
                .unwrap(),
        );
        let tools = reply["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "app_status",
                "list_repos",
                "notifications_unread",
                "repo_prs",
                "repo_issues",
                "api_usage"
            ]
        );
        for tool in tools {
            assert!(!tool["description"].as_str().unwrap().is_empty());
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn repo_row_schemas_require_repo_and_tolerate_open_state() {
        // The hand-written schemas must admit exactly the params the
        // protocol accepts: repo required, optional open state, nothing
        // else. (The deeper coupling to the serde structs — the real
        // acceptance test — lives in the github module's agent_ops
        // tests, next to those types.)
        for spec in TOOL_SPECS
            .iter()
            .filter(|spec| spec.op == "repo_prs" || spec.op == "repo_issues")
        {
            assert_eq!(spec.input_schema["required"], json!(["repo"]));
            assert!(spec.input_schema["properties"]["state"].is_object());
        }
    }

    #[test]
    fn tools_call_proxies_name_and_arguments_to_the_bridge() {
        let (bridge, calls) = StubBridge::ok();
        let mut server = McpServer::new(Some(bridge));
        let reply = parse(
            &server
                .handle_line(&request(
                    json!(7),
                    "tools/call",
                    json!({ "name": "repo_prs", "arguments": { "repo": "o/r" } }),
                ))
                .unwrap(),
        );
        assert!(reply["error"].is_null());
        // The proxy passes the tool's op name and arguments verbatim.
        let recorded = calls.borrow();
        assert_eq!(recorded[0].0, "repo_prs");
        assert_eq!(recorded[0].1, json!({ "repo": "o/r" }));
        // …and the reply carries the caller's id with JSON text content.
        assert_eq!(reply["id"], json!(7));
        let content = &reply["result"]["content"][0];
        assert_eq!(content["type"], "text");
        let payload: Value = serde_json::from_str(content["text"].as_str().unwrap()).unwrap();
        assert_eq!(payload["held"], json!(true));
    }

    #[test]
    fn bridge_op_errors_become_tool_results_not_protocol_errors() {
        let (bridge, _) = StubBridge::ok();
        *bridge.reply.borrow_mut() = Err(RpcError::new(
            error_code::BAD_PARAMS,
            "params: {repo: string}",
        ));
        let mut server = McpServer::new(Some(bridge));
        let reply = parse(
            &server
                .handle_line(&request(
                    json!(8),
                    "tools/call",
                    json!({ "name": "repo_prs", "arguments": {} }),
                ))
                .unwrap(),
        );
        assert_eq!(reply["error"], Value::Null);
        assert_eq!(reply["result"]["isError"], json!(true));
        let payload: Value =
            serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(payload["error"], error_code::BAD_PARAMS);
    }

    #[test]
    fn unknown_tool_is_a_protocol_error_per_the_tools_spec() {
        let (bridge, _) = StubBridge::ok();
        let mut server = McpServer::new(Some(bridge));
        let reply = parse(
            &server
                .handle_line(&request(
                    json!(9),
                    "tools/call",
                    json!({ "name": "delete_everything", "arguments": {} }),
                ))
                .unwrap(),
        );
        assert_eq!(reply["error"]["code"], jsonrpc::METHOD_NOT_FOUND);
    }

    #[test]
    fn non_object_arguments_are_invalid_params() {
        let (bridge, _) = StubBridge::ok();
        let mut server = McpServer::new(Some(bridge));
        let reply = parse(
            &server
                .handle_line(&request(
                    json!(10),
                    "tools/call",
                    json!({ "name": "repo_prs", "arguments": "o/r" }),
                ))
                .unwrap(),
        );
        assert_eq!(reply["error"]["code"], jsonrpc::INVALID_PARAMS);
    }

    #[test]
    fn no_bridge_is_a_tool_error_the_model_can_read() {
        let mut server = McpServer::new(None::<StubBridge>);
        let reply = parse(
            &server
                .handle_line(&request(
                    json!(11),
                    "tools/call",
                    json!({ "name": "app_status", "arguments": {} }),
                ))
                .unwrap(),
        );
        assert_eq!(reply["result"]["isError"], json!(true));
        assert!(
            reply["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains(protocol::error_code::NOT_CONNECTED)
        );
    }

    #[test]
    fn ping_and_unknown_notifications_follow_the_framing_rules() {
        let mut server = McpServer::new(None::<StubBridge>);
        let reply = parse(
            &server
                .handle_line(&request(json!(12), "ping", json!({})))
                .unwrap(),
        );
        assert_eq!(reply["result"], json!({}));

        // Notifications get no reply; responses to nothing get none either.
        assert!(
            server
                .handle_line(
                    &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string()
                )
                .is_none()
        );
        assert!(
            server
                .handle_line(&json!({ "jsonrpc": "2.0", "id": 1, "result": {} }).to_string())
                .is_none()
        );
    }

    #[test]
    fn unparseable_lines_get_the_parse_error_with_null_id() {
        let mut server = McpServer::new(None::<StubBridge>);
        let reply = parse(&server.handle_line("not json at all").unwrap());
        assert_eq!(reply["error"]["code"], jsonrpc::PARSE_ERROR);
        assert!(reply["id"].is_null());
    }

    #[test]
    fn the_clients_id_is_echoed_verbatim_even_as_a_string() {
        let mut server = McpServer::new(None::<StubBridge>);
        let reply = parse(
            &server
                .handle_line(&request(json!("string-id-1"), "ping", json!({})))
                .unwrap(),
        );
        assert_eq!(reply["id"], json!("string-id-1"));
    }

    #[test]
    fn blank_lines_produce_nothing() {
        let mut server = McpServer::new(None::<StubBridge>);
        assert!(server.handle_line("   \n").is_none());
    }
}
