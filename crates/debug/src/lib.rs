//! Agent Debug Bridge (docs/0010): an in-process debug server that lets
//! AI agents (and humans with a terminal) attach to the running app over
//! a Unix socket, dump the component tree, and drive mouse + keyboard
//! input through gpui's own event pipeline — no OS-level injection, so
//! the user of the same machine is never disturbed.
//!
//! This crate carries the foundation (#45): wire protocol, token-gated
//! socket server, main-thread job marshalling, and the
//! `window.list` / `window.geometry` / `input.dispatch` handlers.
//! `tree.dump` / `input.click_node` / `input.focus_node` land with the
//! AT-SPI work (#46); `app.logs` and the `dashboard-debug` CLI with #47.

mod jobs;
mod protocol;
mod server;

/// Start the debug bridge: create the runtime dir, socket and token file,
/// spawn the accept thread and the main-thread job executor.
///
/// Returns the socket path on success. On failure, logs a warning and
/// returns `None` — the bridge must never take the app down with it.
pub fn start(cx: &mut gpui::App) -> Option<std::path::PathBuf> {
    server::start(cx)
}
