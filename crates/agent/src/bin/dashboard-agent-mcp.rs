//! `dashboard-agent-mcp` — the agent bridge's stdio MCP front-end
//! (docs/0068 D8): a dumb, thin client that maps one MCP tool per
//! bridge protocol op and proxies to `agent.sock`. Nothing app-side
//! depends on this binary; any conforming MCP client launches it.
//!
//! ```text
//! dashboard-agent-mcp                              # stdio MCP server
//! dashboard-agent-mcp --call list_repos '{}'       # one op, one line out
//! dashboard-agent-mcp --runtime-dir /tmp/x --call app_status '{}'
//! ```
//!
//! `--call` is the human-check CLI (the agent-bridge answer to
//! `dashboard-debug` verbs) and is what the committed verify scripts
//! under `crates/agent/verify/` replay. In MCP mode stdout carries only
//! JSON-RPC messages (the stdio transport forbids anything else); all
//! diagnostics go to stderr.

use std::io::BufRead;
use std::path::PathBuf;
use std::process::ExitCode;

use dashboard_agent::client::{BridgeClient, LazyBridge};
use dashboard_agent::mcp::McpServer;
use dashboard_agent::protocol::RpcError;

const USAGE: &str = "\
usage:
  dashboard-agent-mcp                                   stdio MCP server (JSON-RPC, one message per line)
  dashboard-agent-mcp --call OP PARAMS_JSON             one bridge op; prints {\"ok\":…} and exits 0/1
  dashboard-agent-mcp --runtime-dir DIR …               override the runtime dir (default: AppPaths, honors DASHBOARD_STATE_DIR)
  dashboard-agent-mcp --help | --version";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut runtime_dir: Option<PathBuf> = None;
    let mut call: Option<(String, String)> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--version" | "-V" => {
                println!("dashboard-agent-mcp {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            "--runtime-dir" => match args.get(i + 1) {
                Some(dir) => {
                    runtime_dir = Some(PathBuf::from(dir));
                    i += 2;
                }
                None => return usage_error("--runtime-dir needs a path"),
            },
            "--call" => match (args.get(i + 1), args.get(i + 2)) {
                (Some(op), Some(params)) => {
                    call = Some((op.clone(), params.clone()));
                    i += 3;
                }
                _ => return usage_error("--call needs OP and PARAMS_JSON"),
            },
            other => return usage_error(&format!("unknown argument {other:?}")),
        }
    }

    let runtime_dir =
        runtime_dir.unwrap_or_else(|| dashboard_core::AppPaths::init().runtime_dir().to_path_buf());

    match call {
        Some((op, params)) => run_call(&runtime_dir, &op, &params),
        None => run_stdio(&runtime_dir),
    }
}

fn usage_error(message: &str) -> ExitCode {
    eprintln!("error: {message}\n{USAGE}");
    ExitCode::from(2)
}

/// MCP mode: newline-delimited JSON-RPC in, replies out. The bridge is
/// proxied lazily — a client may run `initialize`/`tools/list` while
/// the app is down, and `tools/call` reports `NOT_CONNECTED` as a
/// readable tool error instead of dying.
fn run_stdio(runtime_dir: &std::path::Path) -> ExitCode {
    let mut server = McpServer::new(Some(LazyBridge::new(runtime_dir.to_path_buf())));
    eprintln!(
        "dashboard-agent-mcp: stdio MCP over bridge {}",
        runtime_dir.join("agent.sock").display()
    );
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if let Some(reply) = server.handle_line(&line) {
            println!("{reply}");
        }
    }
    ExitCode::SUCCESS
}

/// `--call` mode: one op, the §6 envelope on stdout, exit code as the
/// verdict — scripts assert without parsing anything else.
fn run_call(runtime_dir: &std::path::Path, op: &str, params: &str) -> ExitCode {
    let params: serde_json::Value = match serde_json::from_str::<serde_json::Value>(params) {
        Ok(params) if params.is_object() => params,
        Ok(_) => return usage_error("PARAMS_JSON must be a JSON object"),
        Err(err) => return usage_error(&format!("PARAMS_JSON: {err}")),
    };

    let mut client = match BridgeClient::connect(runtime_dir) {
        Ok(client) => client,
        Err(error) => {
            let outcome: Result<serde_json::Value, RpcError> = Err(error);
            return print_envelope(&outcome);
        }
    };
    print_envelope(&client.call(op, &params))
}

/// Print exactly one line: `{"ok":true,"result":…}` or
/// `{"ok":false,"error":{…}}` — the §6 response shape, minus ids.
fn print_envelope(outcome: &Result<serde_json::Value, RpcError>) -> ExitCode {
    let line = match outcome {
        Ok(result) => serde_json::json!({ "ok": true, "result": result }),
        Err(error) => serde_json::json!({
            "ok": false,
            "error": { "code": error.code, "message": error.message },
        }),
    };
    println!("{line}");
    if outcome.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
