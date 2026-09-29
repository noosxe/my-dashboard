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
fn request_dump(
    job_tx: &JobSender,
    window: Option<u64>,
    dump_only: bool,
) -> Result<(u64, String), RpcError> {
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
                            if dump_only {
                                // No bus was activated, so nothing will
                                // flip the a11y flag either — name the
                                // two causes instead of a generic
                                // timeout (docs/0010 D4).
                                if cfg!(target_os = "macos") {
                                    "the a11y tree never became available \
                                     — macOS does not populate the a11y \
                                     tree yet (upstream gap); logs and \
                                     input.dispatch still work"
                                } else {
                                    "the a11y tree never became available \
                                     — the a11y bus is off and no frame \
                                     recorded the tree; retry with the \
                                     bus up"
                                }
                            } else {
                                "the a11y tree never became available \
                                 activation did not produce a recorded \
                                 frame"
                            },
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
    // D4: the walk is enrichment, not substrate. Activation failure
    // (no a11y bus — always on macOS, sometimes elsewhere) degrades
    // the dump instead of failing it, so the in-app half still serves
    // tree, snapshots, --tab-stops and wait-frame.
    let activated = a11y::ensure_activated();
    let (window_id, dump_json) = request_dump(job_tx, params.window, activated.is_err())?;
    let mut dump: Value = serde_json::from_str(&dump_json)
        .map_err(|err| internal(format!("dump re-parse: {err}")))?;
    let mut walk_note: Option<String> = None;
    let stats = match &activated {
        Ok(session) => match session.walk(window_id as usize) {
            Ok(walk) => merge::merge_dump(&mut dump, &walk),
            Err(err) => {
                walk_note = Some(short_reason(&err));
                dump_only_stats(&dump)
            }
        },
        Err(err) => {
            walk_note = Some(short_reason(err));
            dump_only_stats(&dump)
        }
    };
    // The dump's own focus pointers are the only focus signal on this
    // stack (AT-SPI State::Focused never fires); surface them on the
    // nodes so --tab-stops and snapshot diffs can see focus moves.
    merge::mark_pointer_focus(&mut dump);
    attach_summary(&mut dump, stats, walk_note.as_deref());
    Ok(dump)
}

/// Stats for a dump served without the walk: node counts stay real,
/// enrichment counts stay zero (docs/0010 D4 dump-only mode).
fn dump_only_stats(dump: &Value) -> MergeStats {
    let nodes = dump
        .get("nodes")
        .and_then(Value::as_object)
        .map_or(0, |nodes| nodes.len());
    MergeStats {
        dump_nodes: nodes,
        unmatched_dump: nodes,
        ..MergeStats::default()
    }
}

/// First sentence of an activation/walk error, bounded so the `walk`
/// note stays readable in the dump.
fn short_reason(err: &RpcError) -> String {
    let first = err
        .message
        .split(". ")
        .next()
        .unwrap_or_default()
        .to_string();
    if first.chars().count() <= 140 {
        first
    } else {
        let truncated: String = first.chars().take(140).collect();
        format!("{truncated}…")
    }
}

/// Attach `a11y_merge` (always) and the `walk` note (only when the
/// walk did not run — bus-present responses stay byte-identical).
fn attach_summary(dump: &mut Value, stats: MergeStats, walk_note: Option<&str>) {
    if let Some(object) = dump.as_object_mut() {
        object.insert(
            "a11y_merge".into(),
            serde_json::to_value(MergeStatsSummary::from(stats)).unwrap_or(Value::Null),
        );
        if let Some(reason) = walk_note {
            object.insert(
                "walk".into(),
                serde_json::json!({"status": "unavailable", "reason": reason}),
            );
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dump_only_summary_counts_nodes_and_notes_the_walk() {
        let mut dump = serde_json::json!({
            "frame": {"frame_number": 7},
            "nodes": {"a": {"accesskit_id": "1"}, "b": {"accesskit_id": "2"}}
        });
        let stats = dump_only_stats(&dump);
        assert_eq!(stats.dump_nodes, 2);
        assert_eq!(stats.unmatched_dump, 2);
        assert_eq!(stats.enriched, 0);
        attach_summary(&mut dump, stats, Some("no session bus"));
        assert_eq!(dump["a11y_merge"]["dump_nodes"], 2);
        assert_eq!(dump["a11y_merge"]["enriched"], 0);
        assert_eq!(dump["walk"]["status"], "unavailable");
        assert_eq!(dump["walk"]["reason"], "no session bus");
    }

    #[test]
    fn walk_present_responses_carry_no_walk_note() {
        // Bus-present responses stay byte-identical: no `walk` key.
        let mut dump = serde_json::json!({"nodes": {}});
        attach_summary(&mut dump, MergeStats::default(), None);
        assert!(dump.get("walk").is_none());
        assert!(dump.get("a11y_merge").is_some());
    }

    #[test]
    fn short_reason_takes_the_first_sentence_bounded() {
        let err = RpcError::new(
            error_code::A11Y_UNAVAILABLE,
            "AT-SPI unavailable (boom). The debug bridge needs. More.",
        );
        assert_eq!(short_reason(&err), "AT-SPI unavailable (boom)");
        let long = RpcError::new(0, format!("{}.", "x".repeat(200)));
        let reason = short_reason(&long);
        assert!(reason.chars().count() <= 141);
        assert!(reason.ends_with('…'));
    }
}
