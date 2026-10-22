//! AT-SPI client half of the bridge (docs/0010 D4/D5/D7).
//!
//! The app publishes its accessibility tree over AT-SPI only while the
//! session bus reports `org.a11y.Status.ScreenReaderEnabled == true`
//! (verified against accesskit_unix 0.21.1's event loop; zbus property
//! streams replay the current value at startup, so "already true" also
//! works). `ensure_activated` flips that flag if needed — the auto-enable
//! decision from the #46 review — and restores it on shutdown.
//!
//! Identity bridge (the load-bearing fact): accesskit_unix registers each
//! node at `/org/a11y/atspi/accessible/{adapter}/{u128 NodeId}`, and
//! `tree.dump`'s `accesskit_id` is exactly `u128(NodeId).to_string()`
//! (gpui `a11y/debug.rs`). So a dump node maps 1:1 to an AT-SPI object
//! path with no walk required — the walk only enriches (extents, states).
//!
//! All bus I/O is async (zbus 5 generates async methods) but the bridge
//! is thread-based: each entry point wraps its whole body in a single
//! `zbus::block_on` (nested `block_on` is not allowed, so internals stay
//! `async`/`.await` all the way down).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use atspi::ObjectRefOwned;
use atspi::proxy::accessible::AccessibleProxy;
use atspi::proxy::action::ActionProxy;
use atspi::proxy::bus::{BusProxy, StatusProxy};
use atspi::proxy::component::ComponentProxy;
use atspi::{CoordType, State};
use zbus::connection::Connection;

use crate::protocol::{RpcError, error_code};

const NODE_PATH_PREFIX: &str = "/org/a11y/atspi/accessible/";
const ROOT_PATH: &str = "/org/a11y/atspi/accessible/root";
const DESKTOP_DESTINATION: &str = "org.a11y.atspi.Registry";

/// How often / how long we wait for our own app to appear on the a11y bus
/// after flipping `ScreenReaderEnabled` (in-process activation + registration
/// is fast, but not instantaneous).
const DISCOVER_ATTEMPTS: usize = 5;
const DISCOVER_DELAY: Duration = Duration::from_millis(200);

/// Safety valve for the walk: never visit more objects than this.
const WALK_NODE_LIMIT: usize = 2_000;

/// AT-SPI enrichment for one node, merged into the dump by `merge.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NodeInfo {
    /// Window-relative px (`Component.GetExtents` with
    /// `CoordType::Window`). `None` when the node exposes no Component
    /// interface or the call failed.
    pub bounds: Option<(i32, i32, i32, i32)>,
    pub focusable: bool,
    pub focused: bool,
}

/// A live connection to the session's AT-SPI bus, resolved to our own app.
pub struct A11ySession {
    session_bus: Connection,
    atspi_bus: Connection,
    /// Our process's unique bus name on the a11y bus (`:1.nn`).
    app_bus_name: String,
    /// Whether `ensure_activated` flipped `ScreenReaderEnabled` and
    /// `shutdown` must flip it back.
    flipped_screen_reader: bool,
}

static SESSION: OnceLock<Mutex<Option<Arc<A11ySession>>>> = OnceLock::new();

fn cell() -> &'static Mutex<Option<Arc<A11ySession>>> {
    SESSION.get_or_init(|| Mutex::new(None))
}

/// The already-activated session, if any — never connects or flips flags.
pub fn try_session() -> Option<Arc<A11ySession>> {
    cell().lock().ok()?.clone()
}

/// Activate AT-SPI for this app if necessary and return the session.
///
/// Idempotent: the first successful call is cached for the process
/// lifetime. Blocking (bus round-trips + bounded retries) — socket-thread
/// side only.
pub fn ensure_activated() -> Result<Arc<A11ySession>, RpcError> {
    if let Some(session) = cell().lock().ok().and_then(|guard| guard.clone()) {
        return Ok(session);
    }
    let session = Arc::new(connect_and_discover()?);
    if let Ok(mut guard) = cell().lock() {
        *guard = Some(session.clone());
    }
    Ok(session)
}

/// Best-effort shutdown hook (main thread, app exiting): restore
/// `ScreenReaderEnabled` if we flipped it. A crash skips this — the flag
/// stays true, which is inert on compositors that don't ship a screen
/// reader (documented in docs/0010 D5).
pub fn shutdown() {
    let Some(session) = cell().lock().ok().and_then(|mut guard| guard.take()) else {
        return;
    };
    if session.flipped_screen_reader {
        let restore = zbus::block_on(async {
            let status = StatusProxy::new(&session.session_bus).await?;
            status.set_screen_reader_enabled(false).await
        });
        match restore {
            Ok(()) => log::info!("debug bridge: ScreenReaderEnabled restored to false"),
            Err(err) => {
                log::warn!("debug bridge: restoring ScreenReaderEnabled failed: {err}")
            }
        }
    }
}

fn unavailable(err: impl std::fmt::Display) -> RpcError {
    // Advice is platform-scoped: the at-spi2 fixes are noise on macOS,
    // where no bus can exist (docs/0010 D4 dump-only mode, D7 fallbacks).
    let advice = if cfg!(target_os = "macos") {
        "AT-SPI is Linux-only: tree.dump serves the in-app half without the \
         bus; drive the UI with the key/type/mouse verbs (input.dispatch) \
         — node-id click/focus has no bus to ride."
    } else {
        "The debug bridge needs the at-spi2 bus. In a dev shell: \
         `at-spi-bus-launcher --launch-immediately &` before running the app. \
         On NixOS: services.gnome.at-spi2.enable + re-login. Then retry."
    };
    RpcError::new(
        error_code::A11Y_UNAVAILABLE,
        format!("AT-SPI unavailable ({err}). {advice}"),
    )
}

fn connect_and_discover() -> Result<A11ySession, RpcError> {
    zbus::block_on(async {
        let session_bus = Connection::session().await.map_err(unavailable)?;
        let bus = BusProxy::new(&session_bus).await.map_err(unavailable)?;
        let address = bus.get_address().await.map_err(unavailable)?;

        let status = StatusProxy::new(&session_bus).await.map_err(unavailable)?;
        let mut flipped = false;
        let enabled = status
            .screen_reader_enabled()
            .await
            .map_err(|err| unavailable(format!("reading ScreenReaderEnabled: {err}")))?;
        if !enabled {
            // Auto-enable (owner decision, #46 review): this is the
            // session-wide "screen reader present" signal. On compositors
            // without an Orca-style watcher it only makes accesskit apps
            // publish their trees. Restored by `shutdown`.
            status
                .set_screen_reader_enabled(true)
                .await
                .map_err(|err| unavailable(format!("setting ScreenReaderEnabled: {err}")))?;
            flipped = true;
            log::info!(
                "debug bridge: ScreenReaderEnabled was false — enabled it \
                 (restored on exit)"
            );
        }

        let atspi_bus = zbus::connection::Builder::address(address.as_str())
            .map_err(unavailable)?
            .build()
            .await
            .map_err(unavailable)?;

        let expected = expected_app_name();
        let mut last_err = String::from("app not found on a11y bus yet");
        for _ in 0..DISCOVER_ATTEMPTS {
            match find_app_root(&atspi_bus, &expected).await {
                Ok(app_bus_name) => {
                    return Ok(A11ySession {
                        session_bus,
                        atspi_bus,
                        app_bus_name,
                        flipped_screen_reader: flipped,
                    });
                }
                Err(err) => {
                    last_err = err;
                    std::thread::sleep(DISCOVER_DELAY);
                }
            }
        }
        Err(unavailable(format!(
            "our app ({expected:?}) never appeared on the a11y bus: {last_err}"
        )))
    })
}

/// The name accesskit_unix publishes us under: the executable's file name.
fn expected_app_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "unknown".into())
}

/// Find our app among the desktop's embedded applications by executable
/// name; a successful read of its children doubles as the activation touch.
async fn find_app_root(atspi_bus: &Connection, expected: &str) -> Result<String, String> {
    let desktop = accessible_at(atspi_bus, DESKTOP_DESTINATION, ROOT_PATH)
        .await
        .map_err(|err| format!("desktop object: {err}"))?;
    let children = desktop
        .get_children()
        .await
        .map_err(|err| format!("desktop children: {err}"))?;
    for child in &children {
        let Some(name) = child.name().map(|name| name.to_string()) else {
            continue;
        };
        let proxy = accessible_at(atspi_bus, &name, child.path_as_str())
            .await
            .map_err(|err| format!("app root {name}: {err}"))?;
        let matched = proxy
            .name()
            .await
            .map(|app_name| app_name == expected)
            .unwrap_or(false);
        if matched {
            // Touch: proves the adapter registered its tree (this is the
            // read that made activation real).
            proxy
                .get_children()
                .await
                .map_err(|err| format!("app root children: {err}"))?;
            return Ok(name);
        }
    }
    Err(format!(
        "no embedded app named {expected:?} (found: {:?})",
        children
            .iter()
            .filter_map(|child| child.name().map(|name| name.to_string()))
            .collect::<Vec<_>>()
    ))
}

async fn accessible_at<'a>(
    bus: &'a Connection,
    destination: &'a str,
    path: &'a str,
) -> Result<AccessibleProxy<'a>, zbus::Error> {
    AccessibleProxy::builder(bus)
        .destination(destination)?
        .path(path)?
        .build()
        .await
}

/// Split `/org/a11y/atspi/accessible/{adapter}/{node}` into its parts.
/// Anything else (the app root, foreign paths) yields `None`.
///
/// `node` is accesskit_consumer's `(local_id << 64) | tree_index` u128,
/// where `local_id` is exactly the dump's `accesskit_id`.
pub fn parse_node_path(path: &str) -> Option<(usize, u128)> {
    let rest = path.strip_prefix(NODE_PATH_PREFIX)?;
    let (adapter, node) = rest.split_once('/')?;
    let adapter = adapter.parse::<usize>().ok()?;
    if node.is_empty() || node.contains('/') {
        return None;
    }
    let node = node.parse::<u128>().ok()?;
    Some((adapter, node))
}

/// The dump-side (`accesskit_id`) half of a bus node id.
fn local_id(node: u128) -> u128 {
    node >> 64
}

/// The per-tree index half (gpui uses a single tree, so this is the
/// adapter root's id — but derive it from the bus, never assume).
fn tree_index(node: u128) -> u128 {
    node & u64::MAX as u128
}

impl A11ySession {
    /// Walk the AT-SPI tree of one adapter (window) and collect
    /// `accesskit_id -> NodeInfo` (extents + focus states).
    pub fn walk(&self, adapter: usize) -> Result<HashMap<String, NodeInfo>, RpcError> {
        zbus::block_on(async { self.walk_inner(adapter).await })
    }

    async fn walk_inner(&self, adapter: usize) -> Result<HashMap<String, NodeInfo>, RpcError> {
        let desktop = accessible_at(&self.atspi_bus, DESKTOP_DESTINATION, ROOT_PATH)
            .await
            .map_err(unavailable)?;
        let roots = desktop
            .get_children()
            .await
            .map_err(|err| unavailable(format!("desktop children: {err}")))?;
        let our_root = roots
            .iter()
            .find(|child| {
                child
                    .name()
                    .map(|name| name.to_string() == self.app_bus_name)
                    .unwrap_or(false)
            })
            .ok_or_else(|| unavailable("our app left the a11y bus"))?;

        // `our_root` is an ObjectRefOwned, not a proxy: materialize it
        // once, then BFS its children.
        let root_proxy = accessible_at(&self.atspi_bus, &self.app_bus_name, our_root.path_as_str())
            .await
            .map_err(|err| unavailable(format!("app root proxy: {err}")))?;
        let mut queue: VecDeque<_> = root_proxy
            .get_children()
            .await
            .map_err(|err| unavailable(format!("app root children: {err}")))?
            .into_iter()
            .collect();

        let mut out = HashMap::new();
        let mut visited = 0usize;
        while let Some(reference) = queue.pop_front() {
            if visited >= WALK_NODE_LIMIT {
                log::warn!("debug bridge: a11y walk hit the {WALK_NODE_LIMIT}-node limit");
                break;
            }
            visited += 1;
            let path = reference.path_as_str().to_string();
            let Some((node_adapter, node)) = parse_node_path(&path) else {
                continue;
            };
            let Ok(proxy) = accessible_at(&self.atspi_bus, &self.app_bus_name, &path).await else {
                continue;
            };
            // Children first: the walk crosses adapters, we only collect
            // the requested one.
            if let Ok(children) = proxy.get_children().await {
                queue.extend(children);
            }
            if node_adapter != adapter {
                continue;
            }
            let info = node_info(&proxy, &self.atspi_bus, &self.app_bus_name, &path).await;
            // Key by the local id: that's what the dump's accesskit_id is.
            out.insert(local_id(node).to_string(), info);
        }
        Ok(out)
    }

    /// Coordinate-free click (D7): `do_action(0)` is always `Click` in
    /// accesskit_atspi_common 0.18.1 — the only action it exposes.
    pub fn click_node(&self, adapter: usize, node: &str) -> Result<(), RpcError> {
        zbus::block_on(async {
            let index = self.adapter_tree_index(adapter).await?;
            let path = bus_node_path(adapter, node, index)?;
            let built = ActionProxy::builder(&self.atspi_bus)
                .destination(self.app_bus_name.as_str())
                .and_then(|builder| builder.path(path.as_str()));
            match built {
                Err(err) => Err(self.node_error(adapter, node, err).await),
                Ok(builder) => match builder.build().await {
                    Ok(action) => match action.do_action(0).await {
                        Ok(true) => Ok(()),
                        // index 0 always maps to Click, so Ok(false) is
                        // unexpected — treat as action-less node.
                        Ok(false) => Err(self.action_unavailable(node, "Click")),
                        Err(err) => Err(self.node_error(adapter, node, err).await),
                    },
                    Err(err) => Err(self.node_error(adapter, node, err).await),
                },
            }
        })
    }

    /// Pure focus (D7): `grab_focus` → `ActionRequest{Focus}` →
    /// `window.focus(handle)` upstream — no click synthesis.
    pub fn focus_node(&self, adapter: usize, node: &str) -> Result<(), RpcError> {
        zbus::block_on(async {
            let index = self.adapter_tree_index(adapter).await?;
            let path = bus_node_path(adapter, node, index)?;
            let built = ComponentProxy::builder(&self.atspi_bus)
                .destination(self.app_bus_name.as_str())
                .and_then(|builder| builder.path(path.as_str()));
            match built {
                Err(err) => Err(self.node_error(adapter, node, err).await),
                Ok(builder) => match builder.build().await {
                    Ok(component) => match component.grab_focus().await {
                        Ok(true) => Ok(()),
                        Ok(false) => {
                            Err(self.action_unavailable(node, "GrabFocus (node not focusable)"))
                        }
                        Err(err) => Err(self.node_error(adapter, node, err).await),
                    },
                    Err(err) => Err(self.node_error(adapter, node, err).await),
                },
            }
        })
    }

    /// The window title as published on the a11y root (gpui seeds the root
    /// node's label from the window title).
    pub fn window_title(&self, adapter: usize) -> Option<String> {
        zbus::block_on(async {
            let desktop = accessible_at(&self.atspi_bus, DESKTOP_DESTINATION, ROOT_PATH)
                .await
                .ok()?;
            let roots = desktop.get_children().await.ok()?;
            let our_root = roots.iter().find(|child| {
                child
                    .name()
                    .map(|name| name.to_string() == self.app_bus_name)
                    .unwrap_or(false)
            })?;
            let root_proxy =
                accessible_at(&self.atspi_bus, &self.app_bus_name, our_root.path_as_str())
                    .await
                    .ok()?;
            let windows = root_proxy.get_children().await.ok()?;
            let window = windows.get(adapter)?;
            let proxy = accessible_at(&self.atspi_bus, &self.app_bus_name, window.path_as_str())
                .await
                .ok()?;
            proxy.name().await.ok()
        })
    }

    /// Distinguish "no such node" from "node exists but has no Action /
    /// Component interface" on the error path, so agents get an actionable
    /// message (the documented div-split: fall back to input.dispatch).
    async fn node_error(&self, adapter: usize, node: &str, err: zbus::Error) -> RpcError {
        // A proxy build alone proves nothing — objects are only resolved
        // on call. Probe with a cheap real call.
        let index = self.adapter_tree_index(adapter).await.unwrap_or_default();
        let probe_path = match bus_node_path(adapter, node, index) {
            Ok(path) => path,
            Err(probe) => return probe,
        };
        let exists = match accessible_at(&self.atspi_bus, &self.app_bus_name, &probe_path).await {
            Ok(proxy) => proxy.get_role().await.is_ok(),
            Err(_) => false,
        };
        if exists {
            self.action_unavailable(node, first_line(&err))
        } else {
            RpcError::new(
                error_code::UNKNOWN_NODE,
                format!(
                    "node {node:?} not found on the a11y bus — \
                     ids come from tree.dump's accesskit_id ({})",
                    first_line(&err)
                ),
            )
        }
    }

    fn action_unavailable(&self, node: &str, what: impl std::fmt::Display) -> RpcError {
        RpcError::new(
            error_code::ACTION_UNAVAILABLE,
            format!(
                "node {node:?} has no {what} — plain elements without actions \
                 need input.dispatch with coordinates from tree.dump bounds"
            ),
        )
    }
}

fn first_line(err: &zbus::Error) -> String {
    err.to_string()
        .lines()
        .next()
        .unwrap_or("interface")
        .to_owned()
}

/// The adapter root's ObjectRef, or `None` when it can't be reached.
async fn adapter_root_reference(session: &A11ySession, adapter: usize) -> Option<ObjectRefOwned> {
    let desktop = accessible_at(&session.atspi_bus, DESKTOP_DESTINATION, ROOT_PATH)
        .await
        .ok()?;
    let roots = desktop.get_children().await.ok()?;
    let our_root = roots.iter().find(|child| {
        child
            .name()
            .map(|name| name.to_string() == session.app_bus_name)
            .unwrap_or(false)
    })?;
    let root_proxy = accessible_at(
        &session.atspi_bus,
        &session.app_bus_name,
        our_root.path_as_str(),
    )
    .await
    .ok()?;
    root_proxy.get_children().await.ok()?.get(adapter).cloned()
}

impl A11ySession {
    /// The tree index embedded in every node id of this adapter. Derived
    /// from the bus (the adapter root's path), never assumed: gpui's root
    /// node id is 0, so the root's whole bus id *is* the tree index.
    async fn adapter_tree_index(&self, adapter: usize) -> Result<u128, RpcError> {
        let root = adapter_root_reference(self, adapter)
            .await
            .ok_or_else(|| unavailable("adapter root left the a11y bus"))?;
        let (_, node) = parse_node_path(root.path_as_str())
            .ok_or_else(|| unavailable("adapter root path not a node path"))?;
        Ok(tree_index(node))
    }
}

/// Full bus object path for a dump-side `accesskit_id` under a known
/// tree index: `(local_id << 64) | tree_index`.
fn bus_node_path(adapter: usize, dump_id: &str, tree_index: u128) -> Result<String, RpcError> {
    let local = dump_id
        .parse::<u128>()
        .map_err(|_| unknown_node_error(dump_id, "not a valid accesskit_id"))?;
    let node = (local << 64) | tree_index;
    Ok(format!("{NODE_PATH_PREFIX}{adapter}/{node}"))
}

fn unknown_node_error(node: &str, why: &str) -> RpcError {
    RpcError::new(
        error_code::UNKNOWN_NODE,
        format!("node {node:?}: {why} — ids come from tree.dump's accesskit_id"),
    )
}

async fn node_info(
    accessible: &AccessibleProxy<'_>,
    bus: &Connection,
    destination: &str,
    path: &str,
) -> NodeInfo {
    let mut info = NodeInfo::default();
    if let Ok(states) = accessible.get_state().await {
        info.focusable = states.contains(State::Focusable);
        info.focused = states.contains(State::Focused);
    }
    if let Ok(builder) = ComponentProxy::builder(bus)
        .destination(destination)
        .and_then(|builder| builder.path(path))
        && let Ok(component) = builder.build().await
        && let Ok((x, y, w, h)) = component.get_extents(CoordType::Window).await
    {
        // AT-SPI's "no extent" sentinel is the all-(-1) rect.
        if (x, y, w, h) != (-1, -1, -1, -1) {
            info.bounds = Some((x, y, w, h));
        }
    }
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_paths_parse_into_adapter_and_id() {
        assert_eq!(
            parse_node_path("/org/a11y/atspi/accessible/0/42"),
            Some((0, 42))
        );
        // A real bus id: (5861265405525568973 << 64) | 0.
        let bus = 5861265405525568973u128 << 64;
        assert_eq!(
            parse_node_path(&format!("/org/a11y/atspi/accessible/12/{bus}")),
            Some((12, bus))
        );
    }

    #[test]
    fn non_node_paths_are_rejected() {
        // The app root (no adapter/id pair) and anything foreign.
        assert_eq!(parse_node_path("/org/a11y/atspi/accessible/root"), None);
        assert_eq!(parse_node_path("/org/a11y/atspi/accessible/0/"), None);
        assert_eq!(parse_node_path("/org/a11y/atspi/accessible/0/1/2"), None);
        assert_eq!(parse_node_path("/org/a11y/atspi/accessible/x/1"), None);
        assert_eq!(parse_node_path("/com/example/thing"), None);
    }

    #[test]
    fn bus_id_halves_split_cleanly() {
        // The identity bridge: dump accesskit_id = high half, tree index =
        // low half (accesskit_consumer node.rs).
        let dump_id = 5861265405525568973u128;
        let bus = (dump_id << 64) | 7;
        assert_eq!(local_id(bus), dump_id);
        assert_eq!(tree_index(bus), 7);

        // The adapter root (dump id 0) carries only the tree index.
        let root_bus = 7;
        assert_eq!(local_id(root_bus), 0);
        assert_eq!(tree_index(root_bus), 7);
    }

    #[test]
    fn bus_node_path_round_trips_through_parse() {
        let path = bus_node_path(3, "77", 9).unwrap();
        assert_eq!(parse_node_path(&path), Some((3, (77 << 64) | 9)));
    }

    #[test]
    fn non_numeric_dump_ids_are_unknown_nodes() {
        let err = bus_node_path(0, "not-a-number", 0).unwrap_err();
        assert_eq!(err.code, error_code::UNKNOWN_NODE);
    }

    #[test]
    fn state_set_membership_maps_to_flags() {
        let focused: atspi::StateSet = State::Focused.into();
        assert!(focused.contains(State::Focused));
        assert!(!focused.contains(State::Focusable));

        let focusable: atspi::StateSet = State::Focusable.into();
        assert!(focusable.contains(State::Focusable));
        assert!(!focusable.contains(State::Focused));
    }
}
