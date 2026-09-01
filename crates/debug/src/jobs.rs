//! Main-thread handlers (docs/0010 D3/D6): a parsed request becomes a
//! boxed closure that runs on the foreground executor with `&mut App`
//! access and reports back over its own reply channel. No gpui type ever
//! crosses a thread boundary — the closures only carry plain protocol
//! data.

use std::collections::HashMap;
use std::sync::mpsc::Sender;

use gpui::{AnyWindowHandle, App, Global, Window};
use serde::Serialize;
use serde_json::Value;

use crate::protocol::{
    DispatchOutcome, InputDispatchParams, METHOD_INPUT_DISPATCH, METHOD_WINDOW_GEOMETRY,
    METHOD_WINDOW_LIST, Request, RpcError, SyntheticInput, WindowGeometryParams, error_code,
};

/// A unit of main-thread work. The closure sends its own reply (unbounded,
/// never blocks) so the executor stays completely generic.
pub type Job = Box<dyn FnOnce(&mut App) + Send>;

/// Session-local window ids (D6): `window.list` assigns one id per OS
/// window, stable for the app's lifetime — unlike list indices, which can
/// shift when windows close.
#[derive(Default)]
pub struct WindowRegistry {
    next_id: u64,
    ids: HashMap<AnyWindowHandle, u64>,
}

impl Global for WindowRegistry {}

impl WindowRegistry {
    fn id_for(&mut self, handle: AnyWindowHandle) -> u64 {
        if let Some(id) = self.ids.get(&handle) {
            return *id;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.ids.insert(handle, id);
        id
    }

    fn handle_for(&self, id: u64) -> Option<AnyWindowHandle> {
        self.ids
            .iter()
            .find(|(_, assigned)| **assigned == id)
            .map(|(handle, _)| *handle)
    }
}

/// Build the main-thread job for a request. Returns `Err` for params that
/// don't parse (the socket thread answers those directly, without
/// bothering the main thread).
pub fn build(request: &Request, reply: Sender<Result<Value, RpcError>>) -> Result<Job, RpcError> {
    let respond = move |result: Result<Value, RpcError>| {
        // Unbounded channel; send cannot block. A dropped receiver means
        // the client hung up — nothing to do.
        let _ = reply.send(result);
    };

    match request.method.as_str() {
        METHOD_WINDOW_LIST => Ok(Box::new(move |app: &mut App| {
            respond(Ok(window_list(app)));
        })),
        METHOD_WINDOW_GEOMETRY => {
            let params: WindowGeometryParams = parse_params(&request.params)?;
            Ok(Box::new(move |app: &mut App| {
                respond(window_geometry(app, params));
            }))
        }
        METHOD_INPUT_DISPATCH => {
            let params: InputDispatchParams = parse_params(&request.params)?;
            Ok(Box::new(move |app: &mut App| {
                respond(input_dispatch(app, params));
            }))
        }
        // Stubs are answered on the socket thread (server::is_stub); a
        // stub reaching here is a programming error.
        other => Err(RpcError::new(
            error_code::METHOD_NOT_FOUND,
            format!("no handler for {other:?}"),
        )),
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

/// Resolve the target window: an explicit id must still be open; no id
/// means "the only window" and errors when that's ambiguous.
fn resolve_window(app: &App, window: Option<u64>) -> Result<AnyWindowHandle, RpcError> {
    let open = app.windows();
    match window {
        None => {
            let [only] = open.as_slice() else {
                return Err(RpcError::new(
                    error_code::INVALID_PARAMS,
                    format!("window param required — {} windows are open", open.len()),
                ));
            };
            Ok(*only)
        }
        Some(id) => {
            let handle = app
                .global::<WindowRegistry>()
                .handle_for(id)
                .ok_or_else(|| {
                    RpcError::new(
                        error_code::UNKNOWN_WINDOW,
                        format!("unknown window {id} — ids come from window.list"),
                    )
                })?;
            if !open.contains(&handle) {
                return Err(RpcError::new(
                    error_code::UNKNOWN_WINDOW,
                    format!("window {id} is closed"),
                ));
            }
            Ok(handle)
        }
    }
}

#[derive(Serialize)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

#[derive(Serialize)]
struct Size {
    w: f32,
    h: f32,
}

/// One `window.list` / `window.geometry` entry.
#[derive(Serialize)]
struct WindowInfo {
    id: u64,
    /// Global on-screen px (pre-scale) — `Window::bounds()`.
    bounds: Rect,
    /// Content viewport px (pre-scale) — `Window::viewport_size()`. This
    /// is the coordinate space input events and (later) tree bounds use.
    viewport: Size,
    scale_factor: f32,
}

fn window_info(id: u64, window: &Window) -> WindowInfo {
    let bounds = window.bounds();
    let viewport = window.viewport_size();
    WindowInfo {
        id,
        bounds: Rect {
            x: bounds.origin.x.as_f32(),
            y: bounds.origin.y.as_f32(),
            w: bounds.size.width.as_f32(),
            h: bounds.size.height.as_f32(),
        },
        viewport: Size {
            w: viewport.width.as_f32(),
            h: viewport.height.as_f32(),
        },
        scale_factor: window.scale_factor(),
    }
}

fn window_list(app: &mut App) -> Value {
    // Collect handles first: the registry borrow must not overlap the
    // `&mut App` the window updates need.
    let handles = app.windows();
    let mut windows = Vec::new();
    {
        let registry = app.global_mut::<WindowRegistry>();
        for handle in handles {
            windows.push((registry.id_for(handle), handle));
        }
    }
    let infos: Vec<WindowInfo> = windows
        .into_iter()
        .filter_map(|(id, handle)| {
            handle
                .update(app, |_, window, _| window_info(id, window))
                .ok()
        })
        .collect();
    serde_json::to_value(infos).unwrap_or(Value::Null)
}

fn window_geometry(app: &mut App, params: WindowGeometryParams) -> Result<Value, RpcError> {
    let handle = resolve_window(app, params.window)?;
    let id = app.global_mut::<WindowRegistry>().id_for(handle);
    handle
        .update(app, |_, window, _| window_info(id, window))
        .map_err(internal)
        .map(|info| serde_json::to_value(info).unwrap_or(Value::Null))
}

fn input_dispatch(app: &mut App, params: InputDispatchParams) -> Result<Value, RpcError> {
    let handle = resolve_window(app, params.window)?;
    handle
        .update(app, |_, window, cx| {
            let outcome = match params.event.to_synthetic_input()? {
                // Ime-aware path (D6): text entry works through the
                // simulated-ime wrapper upstream's own tests use.
                SyntheticInput::Keystroke(keystroke) => DispatchOutcome::Keystroke {
                    handled: window.dispatch_keystroke(keystroke, cx),
                },
                SyntheticInput::Event(event) => {
                    let result = window.dispatch_event(event, cx);
                    DispatchOutcome::Event {
                        propagate: result.propagate,
                        default_prevented: result.default_prevented,
                    }
                }
            };
            Ok(serde_json::to_value(outcome).unwrap_or(Value::Null))
        })
        .map_err(internal)?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_shape_error_is_invalid_params() {
        let err = parse_params::<WindowGeometryParams>(&serde_json::json!({"window": "zero"}))
            .unwrap_err();
        assert_eq!(err.code, error_code::INVALID_PARAMS);
        assert!(err.message.contains("shape"));
    }

    #[test]
    fn build_rejects_unknown_method() {
        let request = Request {
            id: 1,
            method: "bogus.command".into(),
            params: Value::Null,
        };
        let (tx, _rx) = std::sync::mpsc::channel();
        let Err(err) = build(&request, tx) else {
            panic!("unknown method must not build a job");
        };
        assert_eq!(err.code, error_code::METHOD_NOT_FOUND);
    }

    #[test]
    fn build_rejects_malformed_dispatch_params() {
        let request = Request {
            id: 2,
            method: METHOD_INPUT_DISPATCH.into(),
            // No `event` — must fail params parsing, not reach the main thread.
            params: serde_json::json!({"window": 3}),
        };
        let (tx, _rx) = std::sync::mpsc::channel();
        let Err(err) = build(&request, tx) else {
            panic!("malformed params must not build a job");
        };
        assert_eq!(err.code, error_code::INVALID_PARAMS);
    }
}
