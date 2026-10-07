//! Agent Data Bridge (docs/0068): a read-only, token-gated Unix-socket
//! server that lets AI agents operate the running app from the **data
//! layer** — enumerate held repos, read unread notifications, inspect
//! session records and their own API spend — without touching the UI or
//! the input pipeline (that is the debug bridge's job, docs/0010).
//!
//! Architecture (docs/0068 D1–D9): `agent.sock` + per-run `agent.token`
//! in the runtime dir (the docs/0010 D2 pattern), NDJSON protocol (§6),
//! main-thread job marshalling (D9), and a module-agnostic ops seam —
//! modules register [`AgentOps`] providers (D3); the bridge core knows
//! no module. The kill switch is `agent_bridge` in `settings.json`,
//! default on (§9); the caller gates [`start`] on it.
//!
//! Phasing (§7): this crate carries the skeleton (#467). Phase-1 ops
//! (`app_status`, `list_repos`, `notifications_unread`, `repo_prs`,
//! `repo_issues`, `api_usage`) arrive with the github provider (#468);
//! the stdio MCP front-end `dashboard-agent-mcp` with #469.

pub mod jobs;
pub mod ops;
pub mod protocol;
mod server;

pub use ops::{AgentOps, OpsRegistry, register_ops};

/// Start the agent bridge: create the runtime dir, socket and token file,
/// spawn the accept thread and the main-thread job executor.
///
/// The caller applies the kill switch (`agent_bridge`, docs/0068 D2): a
/// disabled bridge never reaches this and never binds. Returns the socket
/// path on success. On failure, logs a warning and returns `None` — the
/// bridge must never take the app down with it.
pub fn start(cx: &mut gpui::App) -> Option<std::path::PathBuf> {
    server::start(cx)
}
