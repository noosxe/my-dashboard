//! Wire protocol (docs/0010 D2/D6): NDJSON request/response envelopes and
//! the typed params for every command. The shapes are frozen here — later
//! issues (#46/#47) only add handlers, not new wire syntax.
//!
//! Coordinates are logical pixels, pre-scale-factor, window-relative for
//! input events (same space `tree.dump` bounds use, once they land).
//! `window` params reference the session-local ids `window.list` assigns.

use gpui::{
    KeyUpEvent, Keystroke, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PlatformInput, Point, ScrollDelta, ScrollWheelEvent, TouchPhase, px,
};
use serde::{Deserialize, Serialize};

pub const METHOD_AUTH: &str = "auth";
pub const METHOD_TREE_DUMP: &str = "tree.dump";
pub const METHOD_INPUT_DISPATCH: &str = "input.dispatch";
pub const METHOD_INPUT_CLICK_NODE: &str = "input.click_node";
pub const METHOD_INPUT_FOCUS_NODE: &str = "input.focus_node";
pub const METHOD_WINDOW_LIST: &str = "window.list";
pub const METHOD_WINDOW_GEOMETRY: &str = "window.geometry";
pub const METHOD_APP_LOGS: &str = "app.logs";

/// Error codes: JSON-RPC conventions where they fit, app-specific 4xxx/5xxx
/// beyond that.
pub mod error_code {
    pub const PARSE: i64 = -32700;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const AUTH_FAILED: i64 = 4001;
    pub const UNKNOWN_WINDOW: i64 = 4002;
    pub const BAD_KEYSTROKE: i64 = 4003;
    pub const A11Y_UNAVAILABLE: i64 = 4004;
    pub const UNKNOWN_NODE: i64 = 4005;
    pub const ACTION_UNAVAILABLE: i64 = 4006;
    pub const INTERNAL: i64 = 5000;
    /// Reserved in the code table; no command emits it since #47
    /// completed the seven-command set.
    #[allow(dead_code)]
    pub const NOT_IMPLEMENTED: i64 = 5001;
}

/// One request line: `{"id": n, "method": "input.dispatch", "params": {…}}`.
#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// One response line: exactly one of `result` / `error`.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Response {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// Parse typed params out of a request's `params` value; `None` when the
/// shape doesn't match (callers answer `INVALID_PARAMS`/auth failure).
pub fn parse<T: serde::de::DeserializeOwned>(params: &serde_json::Value) -> Option<T> {
    serde_json::from_value(params.clone()).ok()
}

impl Response {
    pub fn ok(id: u64, result: serde_json::Value) -> Self {
        Self {
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: u64, error: RpcError) -> Self {
        Self {
            id,
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
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Command params (D6 table).

#[derive(Debug, Deserialize)]
// Shape frozen per docs/0010 D6; the handler lands with #46/#47.
#[allow(dead_code)]
pub struct AuthParams {
    pub token: String,
}

#[derive(Debug, Deserialize)]
// Shape frozen per docs/0010 D6; the handler lands with #46/#47.
#[allow(dead_code)]
pub struct TreeDumpParams {
    pub window: Option<u64>,
}

#[derive(Debug, Deserialize)]
// Shape frozen per docs/0010 D6; the handler lands with #46/#47.
#[allow(dead_code)]
pub struct InputDispatchParams {
    pub window: Option<u64>,
    pub event: InputEvent,
}

/// `node` is the `accesskit_id` from `tree.dump` — serialized as a string
/// upstream (`window/a11y/debug.rs`: `json!(id.0.to_string())`).
#[derive(Debug, Deserialize)]
// Shape frozen per docs/0010 D6; the handler lands with #46/#47.
#[allow(dead_code)]
pub struct ClickNodeParams {
    pub window: Option<u64>,
    pub node: String,
    #[serde(default)]
    pub button: Button,
    #[serde(default = "default_click_count")]
    pub click_count: u32,
}

#[derive(Debug, Deserialize)]
// Shape frozen per docs/0010 D6; the handler lands with #46/#47.
#[allow(dead_code)]
pub struct FocusNodeParams {
    pub window: Option<u64>,
    pub node: String,
}

#[derive(Debug, Deserialize)]
// Shape frozen per docs/0010 D6; the handler lands with #46/#47.
#[allow(dead_code)]
pub struct WindowGeometryParams {
    pub window: Option<u64>,
}

#[derive(Debug, Deserialize)]
// Shape frozen per docs/0010 D6.
pub struct LogsParams {
    pub since: Option<u64>,
    pub limit: Option<usize>,
}

fn default_click_count() -> u32 {
    1
}

// ---------------------------------------------------------------------------
// input.dispatch event union — maps 1:1 onto the PlatformInput subset the
// bridge synthesizes (D6).

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Button {
    #[default]
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modifier {
    Ctrl,
    Alt,
    Shift,
    Cmd,
    Fn,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputEvent {
    MouseMove {
        x: f32,
        y: f32,
    },
    MouseDown {
        x: f32,
        y: f32,
        #[serde(default)]
        button: Button,
        #[serde(default = "default_click_count")]
        click_count: u32,
        #[serde(default)]
        modifiers: Vec<Modifier>,
    },
    MouseUp {
        x: f32,
        y: f32,
        #[serde(default)]
        button: Button,
        #[serde(default = "default_click_count")]
        click_count: u32,
        #[serde(default)]
        modifiers: Vec<Modifier>,
    },
    ScrollWheel {
        x: f32,
        y: f32,
        dx: f32,
        /// Positive scrolls down — screen convention, passed through raw.
        dy: f32,
        #[serde(default)]
        modifiers: Vec<Modifier>,
    },
    /// `keystroke` uses upstream `Keystroke::parse` syntax, e.g. `"a"`,
    /// `"ctrl-s"`, `"shift-tab"` (modifiers ride inside the keystroke).
    KeyDown {
        keystroke: String,
    },
    KeyUp {
        keystroke: String,
    },
}

/// The resolved form dispatched on the main thread. `key_down` rides
/// `Window::dispatch_keystroke` (which wraps KeyDown with ime simulation
/// and reports whether the stroke was consumed); everything else goes
/// through `Window::dispatch_event` and reports the
/// `DispatchEventResult`.
#[derive(Debug, Clone)]
pub enum SyntheticInput {
    Keystroke(Keystroke),
    Event(PlatformInput),
}

/// Result shape for `input.dispatch`: pointer/scroll/`key_up` events
/// report the `DispatchEventResult`; `key_down` reports the
/// `dispatch_keystroke` outcome.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum DispatchOutcome {
    Event {
        propagate: bool,
        default_prevented: bool,
    },
    Keystroke {
        handled: bool,
    },
}

impl InputEvent {
    /// Pure conversion into gpui input. Constructions mirror upstream's
    /// own syntheses: mouse events per `handle_a11y_action`'s Click
    /// fallback (`first_mouse: false`) and `simulate_mouse_move`
    /// (`pressed_button: None`); scroll uses `ScrollDelta::Pixels` with
    /// `TouchPhase::Moved`.
    pub fn to_synthetic_input(&self) -> Result<SyntheticInput, RpcError> {
        fn point(x: f32, y: f32) -> Point<gpui::Pixels> {
            Point { x: px(x), y: px(y) }
        }
        fn modifiers(list: &[Modifier]) -> Modifiers {
            let mut modifiers = Modifiers::default();
            for modifier in list {
                match modifier {
                    Modifier::Ctrl => modifiers.control = true,
                    Modifier::Alt => modifiers.alt = true,
                    Modifier::Shift => modifiers.shift = true,
                    Modifier::Cmd => modifiers.platform = true,
                    Modifier::Fn => modifiers.function = true,
                }
            }
            modifiers
        }
        fn button(button: Button) -> MouseButton {
            match button {
                Button::Left => MouseButton::Left,
                Button::Right => MouseButton::Right,
                Button::Middle => MouseButton::Middle,
            }
        }

        Ok(match self {
            InputEvent::MouseMove { x, y } => {
                SyntheticInput::Event(PlatformInput::MouseMove(MouseMoveEvent {
                    position: point(*x, *y),
                    pressed_button: None,
                    modifiers: Modifiers::default(),
                }))
            }
            InputEvent::MouseDown {
                x,
                y,
                button: b,
                click_count,
                modifiers: mods,
            } => SyntheticInput::Event(PlatformInput::MouseDown(MouseDownEvent {
                button: button(*b),
                position: point(*x, *y),
                modifiers: modifiers(mods),
                click_count: *click_count as usize,
                first_mouse: false,
            })),
            InputEvent::MouseUp {
                x,
                y,
                button: b,
                click_count,
                modifiers: mods,
            } => SyntheticInput::Event(PlatformInput::MouseUp(MouseUpEvent {
                button: button(*b),
                position: point(*x, *y),
                modifiers: modifiers(mods),
                click_count: *click_count as usize,
            })),
            InputEvent::ScrollWheel {
                x,
                y,
                dx,
                dy,
                modifiers: mods,
            } => SyntheticInput::Event(PlatformInput::ScrollWheel(ScrollWheelEvent {
                position: point(*x, *y),
                delta: ScrollDelta::Pixels(point(*dx, *dy)),
                modifiers: modifiers(mods),
                touch_phase: TouchPhase::Moved,
            })),
            InputEvent::KeyDown { keystroke } => {
                SyntheticInput::Keystroke(parse_keystroke(keystroke)?)
            }
            InputEvent::KeyUp { keystroke } => {
                SyntheticInput::Event(PlatformInput::KeyUp(KeyUpEvent {
                    keystroke: parse_keystroke(keystroke)?,
                }))
            }
        })
    }
}

fn parse_keystroke(source: &str) -> Result<Keystroke, RpcError> {
    Keystroke::parse(source).map_err(|err| {
        RpcError::new(
            error_code::BAD_KEYSTROKE,
            format!("invalid keystroke {source:?}: {err} — expected e.g. \"a\", \"ctrl-s\""),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(json: serde_json::Value) -> InputDispatchParams {
        serde_json::from_value(json).expect("valid dispatch params")
    }

    #[test]
    fn request_envelope_parses() {
        let req: Request =
            serde_json::from_str(r#"{"id":7,"method":"input.dispatch","params":{}}"#).unwrap();
        assert_eq!(req.id, 7);
        assert_eq!(req.method, "input.dispatch");
        assert!(req.params.is_object());
    }

    #[test]
    fn response_envelope_omits_absent_arm() {
        let ok = Response::ok(1, serde_json::json!({"ok": true}));
        let line = ok.to_line();
        assert!(line.ends_with('\n'));
        assert!(line.contains("\"result\""));
        assert!(!line.contains("\"error\""));

        let err = Response::err(2, RpcError::new(error_code::PARSE, "bad"));
        let line = err.to_line();
        assert!(line.contains("\"error\":{\"code\":-32700"));
        assert!(!line.contains("\"result\""));
    }

    #[test]
    fn mouse_events_default_to_left_button_single_click() {
        let p = params(
            serde_json::json!({"window": 0, "event": {"type": "mouse_down", "x": 12.5, "y": 40.0}}),
        );
        let InputEvent::MouseDown {
            button,
            click_count,
            modifiers,
            ..
        } = &p.event
        else {
            panic!("wrong variant");
        };
        assert_eq!(*button, Button::Left);
        assert_eq!(*click_count, 1);
        assert!(modifiers.is_empty());
    }

    #[test]
    fn mouse_down_converts_with_explicit_fields() {
        let p = params(serde_json::json!({
            "event": {"type": "mouse_down", "x": 1.0, "y": 2.0, "button": "right",
                       "click_count": 2, "modifiers": ["ctrl", "shift"]}
        }));
        let SyntheticInput::Event(PlatformInput::MouseDown(event)) =
            p.event.to_synthetic_input().unwrap()
        else {
            panic!("wrong variant");
        };
        assert_eq!(event.button, MouseButton::Right);
        assert_eq!(event.click_count, 2);
        assert!(!event.first_mouse);
        assert_eq!(event.position.x.as_f32(), 1.0);
        assert_eq!(event.position.y.as_f32(), 2.0);
        assert!(event.modifiers.control && event.modifiers.shift);
        assert!(!event.modifiers.alt);
    }

    #[test]
    fn mouse_move_has_no_pressed_button_or_modifiers() {
        let p = params(serde_json::json!({"event": {"type": "mouse_move", "x": 9.0, "y": 8.0}}));
        let SyntheticInput::Event(PlatformInput::MouseMove(event)) =
            p.event.to_synthetic_input().unwrap()
        else {
            panic!("wrong variant");
        };
        assert_eq!(event.pressed_button, None);
        assert_eq!(event.modifiers, Modifiers::default());
    }

    #[test]
    fn scroll_wheel_maps_to_pixel_delta() {
        let p = params(
            serde_json::json!({"event": {"type": "scroll_wheel", "x": 5.0, "y": 6.0, "dx": 0.0, "dy": -60.0}}),
        );
        let SyntheticInput::Event(PlatformInput::ScrollWheel(event)) =
            p.event.to_synthetic_input().unwrap()
        else {
            panic!("wrong variant");
        };
        let ScrollDelta::Pixels(delta) = event.delta else {
            panic!("expected pixel delta");
        };
        assert_eq!(delta.x.as_f32(), 0.0);
        assert_eq!(delta.y.as_f32(), -60.0);
        assert_eq!(event.touch_phase, TouchPhase::Moved);
    }

    #[test]
    fn cmd_modifier_maps_to_platform() {
        let p = params(serde_json::json!({
            "event": {"type": "mouse_up", "x": 0.0, "y": 0.0, "modifiers": ["cmd", "fn"]}
        }));
        let SyntheticInput::Event(PlatformInput::MouseUp(event)) =
            p.event.to_synthetic_input().unwrap()
        else {
            panic!("wrong variant");
        };
        assert!(event.modifiers.platform && event.modifiers.function);
    }

    #[test]
    fn key_down_parses_keystroke_with_modifiers() {
        let p = params(serde_json::json!({"event": {"type": "key_down", "keystroke": "ctrl-s"}}));
        let SyntheticInput::Keystroke(keystroke) = p.event.to_synthetic_input().unwrap() else {
            panic!("wrong variant");
        };
        assert!(keystroke.modifiers.control);
        assert_eq!(keystroke.key, "s");
    }

    #[test]
    fn bad_keystroke_yields_typed_error() {
        // `Keystroke::parse` accepts empty strings and lone keys; it
        // rejects a plain key followed by more components ("a-b").
        let p = params(serde_json::json!({"event": {"type": "key_up", "keystroke": "a-b"}}));
        let err = p.event.to_synthetic_input().unwrap_err();
        assert_eq!(err.code, error_code::BAD_KEYSTROKE);
    }

    #[test]
    fn click_and_log_params_parse() {
        let click: ClickNodeParams = serde_json::from_value(serde_json::json!({
            "window": 0, "node": "5"
        }))
        .unwrap();
        assert_eq!(click.node, "5");
        assert_eq!(click.click_count, 1);
        assert_eq!(click.button, Button::Left);

        let logs: LogsParams = serde_json::from_value(serde_json::json!({"since": 42})).unwrap();
        assert_eq!(logs.since, Some(42));
        assert_eq!(logs.limit, None);
    }
}
