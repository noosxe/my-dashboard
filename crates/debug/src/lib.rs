//! Agent Debug Bridge (docs/0010): an in-process debug server that lets
//! AI agents (and humans with a terminal) attach to the running app over
//! a Unix socket, dump the component tree, and drive mouse + keyboard
//! input through gpui's own event pipeline — no OS-level injection, so
//! the user of the same machine is never disturbed.
//!
//! This crate carries the foundation (#45: wire protocol, token-gated
//! socket server, main-thread job marshalling, `window.list` /
//! `window.geometry` / `input.dispatch`), the AT-SPI half (#46:
//! `tree.dump` enrichment, `input.click_node`, `input.focus_node`,
//! auto-activation), and the log ring + `dashboard-debug` CLI (#47).

pub mod a11y;
pub mod audit;
pub mod cli;
pub mod doctor;
pub mod gate;
mod handlers;
mod jobs;
pub mod logs;
mod merge;
pub mod panel;
pub mod pixels;
pub mod protocol;
pub mod script;
mod server;
pub mod session;
pub mod snapshot;

/// Start the debug bridge: create the runtime dir, socket and token file,
/// spawn the accept thread and the main-thread job executor.
///
/// Returns the socket path on success. On failure, logs a warning and
/// returns `None` — the bridge must never take the app down with it.
pub fn start(cx: &mut gpui::App, ring: logs::RingLog) -> Option<std::path::PathBuf> {
    server::start(cx, ring)
}

/// Call when the app exits (after `app.run` returns): restores
/// `ScreenReaderEnabled` if the bridge auto-enabled it. Best-effort.
pub fn shutdown() {
    a11y::shutdown();
}
