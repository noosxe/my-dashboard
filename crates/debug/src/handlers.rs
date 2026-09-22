//! Socket-thread orchestration for the AT-SPI-backed commands
//! (docs/0010 D4/D7): activation + bus work happen here; only window
//! resolution and the dump read cross to the main thread as jobs.
//!
//! These commands can block for seconds on first use (activation flips
//! `ScreenReaderEnabled`, then a frame must record the tree). NDJSON keeps
//! per-connection response order, so a slow `tree.dump` delays only later
//! requests on the *same* connection — other connections are unaffected.

use std::sync::mpsc::channel;
use std::time::Duration;

use futures::channel::mpsc::UnboundedSender;
use gpui::App;
use serde_json::Value;

use crate::a11y;
use crate::jobs::{self, Job, WindowRegistry};
use crate::merge::{self, MergeStats};
use crate::protocol::{
    Button, ClickNodeParams, FocusNodeParams, METHOD_INPUT_CLICK_NODE, METHOD_INPUT_FOCUS_NODE,
    METHOD_TREE_DUMP, METHOD_WINDOW_LIST, Request, RpcError, TreeDumpParams, error_code,
};

/// How long the main-thread side waits for a frame to record the tree
/// after activation (activation itself forces a frame upstream).
const DUMP_WAIT: Duration = Duration::from_secs(5);

pub type JobSender = UnboundedSender<Job>;

/// Entry point used by the server: AT-SPI commands get the two-phase
/// treatment here, everything else is a plain main-thread job.
pub fn dispatch(job_tx: &JobSender, request: Request) -> Result<Value, RpcError> {
    match request.method.as_str() {
        METHOD_TREE_DUMP => tree_dump(job_tx, parse_params::<TreeDumpParams>(&request.params)?),
        METHOD_INPUT_CLICK_NODE => {
            click_node(job_tx, parse_params::<ClickNodeParams>(&request.params)?)
        }
        METHOD_INPUT_FOCUS_NODE => {
            focus_node(job_tx, parse_params::<FocusNodeParams>(&request.params)?)
        }
        method => {
            let is_window_list = method == METHOD_WINDOW_LIST;
            let mut value = dispatch_via(job_tx, request)?;
            // Title enrichment rides along once AT-SPI is alive — but
            // window.list itself never activates (cheap, always-works).
            if is_window_list {
                enrich_titles(&mut value);
            }
            Ok(value)
        }
    }
}

fn parse_params<T: serde::de::DeserializeOwned>(params: &Value) -> Result<T, RpcError> {
    serde_json::from_value(params.clone()).map_err(|err| {
        RpcError::new(
            error_code::INVALID_PARAMS,
            format!("params don't match the documented shape: {err}"),
        )
    })
}

fn internal(err: impl std::fmt::Display) -> RpcError {
    RpcError::new(error_code::INTERNAL, err.to_string())
}

fn dispatch_via(job_tx: &JobSender, request: Request) -> Result<Value, RpcError> {
    let (reply_tx, reply_rx) = channel();
    let job = jobs::build(&request, reply_tx)?;
    job_tx
        .unbounded_send(job)
        .map_err(|_| RpcError::new(error_code::INTERNAL, "app is shutting down"))?;
    reply_rx
        .recv()
        .map_err(|_| RpcError::new(error_code::INTERNAL, "executor dropped the job"))?
}

/// Ask the main thread to resolve the window and read
fn request_dump(job_tx: &JobSender, window: Option<u64>) -> Result<(u64, String), RpcError> {
    let (tx, rx) = channel::<Result<(u64, String), RpcError>>();
    let job: Job = Box::new(move |app: &mut App| {
        let handle = match jobs::resolve_window(app, window) {
            Ok(handle) => handle,
            Err(err) => {
                let _ = tx.send(Err(err));
                return;
            }
        };
        let id = app.global_mut::<WindowRegistry>().id_for(handle);
        match handle.update(app, |_, window, _| window.debug_a11y_tree_json()) {
            Ok(Some(dump)) => {
                let _ = tx.send(Ok((id, dump)));
            }
            Ok(None) => {
                app.spawn(async move |cx| {
                    let reply = 'done: {
                        for _ in 0..40 {
                            cx.background_executor()
                                .timer(Duration::from_millis(100))
                                .await;
                            let polled = cx.update(|app| {
                                handle.update(app, |_, window, _| window.debug_a11y_tree_json())
                            });
                            match polled {
                                Ok(Some(dump)) => break 'done Ok((id, dump)),
                                Ok(None) => continue,
                                Err(err) => break 'done Err(internal(err)),
                            }
                        }
                        Err(RpcError::new(
                            error_code::INTERNAL,
                            "the a11y tree never became available — \
                             activation did not produce a recorded frame",
                        ))
                    };
                    // The receiver may be gone (client hung up): ignore.
                    let _ = tx.send(reply);
                })
                .detach();
            }
            Err(err) => {
                let _ = tx.send(Err(internal(err)));
            }
        }
    });
    job_tx
        .unbounded_send(job)
        .map_err(|_| RpcError::new(error_code::INTERNAL, "app is shutting down"))?;
    rx.recv_timeout(DUMP_WAIT).map_err(|_| {
        RpcError::new(
            error_code::INTERNAL,
            "timed out waiting for the a11y tree dump",
        )
    })?
}
/// Resolve the session-stable window id on the main thread (no dump).
fn resolve_window_id(job_tx: &JobSender, window: Option<u64>) -> Result<u64, RpcError> {
    let (tx, rx) = channel::<Result<u64, RpcError>>();
    let job: Job = Box::new(move |app: &mut App| {
        let reply = jobs::resolve_window(app, window)
            .map(|handle| app.global_mut::<WindowRegistry>().id_for(handle));
        let _ = tx.send(reply);
    });
    job_tx
        .unbounded_send(job)
        .map_err(|_| RpcError::new(error_code::INTERNAL, "app is shutting down"))?;
    rx.recv_timeout(Duration::from_secs(2))
        .map_err(|_| RpcError::new(error_code::INTERNAL, "timed out resolving the window"))?
}

fn tree_dump(job_tx: &JobSender, params: TreeDumpParams) -> Result<Value, RpcError> {
    let session = a11y::ensure_activated()?;
    let (window_id, dump_json) = request_dump(job_tx, params.window)?;
    let mut dump: Value = serde_json::from_str(&dump_json)
        .map_err(|err| internal(format!("dump re-parse: {err}")))?;
    let walk = session.walk(window_id as usize)?;
    let stats = merge::merge_dump(&mut dump, &walk);
    // The dump's own focus pointers are the only focus signal on this
    // stack (AT-SPI State::Focused never fires); surface them on the
    // nodes so --tab-stops and snapshot diffs can see focus moves.
    merge::mark_pointer_focus(&mut dump);
    if let Some(object) = dump.as_object_mut() {
        object.insert(
            "a11y_merge".into(),
            serde_json::to_value(MergeStatsSummary::from(stats)).unwrap_or(Value::Null),
        );
    }
    Ok(dump)
}

#[derive(serde::Serialize)]
struct MergeStatsSummary {
    dump_nodes: usize,
    enriched: usize,
    unmatched_dump: usize,
    unmatched_walk: usize,
}

impl From<MergeStats> for MergeStatsSummary {
    fn from(stats: MergeStats) -> Self {
        Self {
            dump_nodes: stats.dump_nodes,
            enriched: stats.enriched,
            unmatched_dump: stats.unmatched_dump,
            unmatched_walk: stats.unmatched_walk,
        }
    }
}

fn click_node(job_tx: &JobSender, params: ClickNodeParams) -> Result<Value, RpcError> {
    // AT-SPI's synthesized Click is a single left click; anything else is
    // the documented input.dispatch-with-coordinates split (D7).
    if params.button != Button::Left || params.click_count != 1 {
        return Err(RpcError::new(
            error_code::INVALID_PARAMS,
            "input.click_node synthesizes a single left click only — use \
             input.dispatch with coordinates from tree.dump bounds for \
             other buttons or click counts",
        ));
    }
    let session = a11y::ensure_activated()?;
    let window_id = resolve_window_id(job_tx, params.window)?;
    session.click_node(window_id as usize, &params.node)?;
    Ok(serde_json::json!({"ok": true}))
}

fn focus_node(job_tx: &JobSender, params: FocusNodeParams) -> Result<Value, RpcError> {
    let session = a11y::ensure_activated()?;
    let window_id = resolve_window_id(job_tx, params.window)?;
    session.focus_node(window_id as usize, &params.node)?;
    Ok(serde_json::json!({"ok": true}))
}

/// Fill in `title` from the a11y root labels when AT-SPI is already
/// active. Always inserts the key (null when unknown) so the shape is
/// stable across sessions.
fn enrich_titles(list: &mut Value) {
    let Some(session) = a11y::try_session() else {
        if let Some(entries) = list.as_array_mut() {
            for entry in entries {
                if let Some(object) = entry.as_object_mut() {
                    object.insert("title".into(), Value::Null);
                }
            }
        }
        return;
    };
    let Some(entries) = list.as_array_mut() else {
        return;
    };
    for entry in entries.iter_mut() {
        let id = entry.get("id").and_then(Value::as_u64);
        let title = id
            .and_then(|id| session.window_title(id as usize))
            .map(Value::String)
            .unwrap_or(Value::Null);
        if let Some(object) = entry.as_object_mut() {
            object.insert("title".into(), title);
        }
    }
}
