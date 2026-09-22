//! `dashboard-debug` CLI argument parsing (docs/0010 D9): hand-rolled —
//! ≈8 verbs, no clap. The parser is pure (a `&[String]` slice in, a
//! [`Command`] out) so it is table-testable without spawning the binary;
//! the I/O half lives in `src/bin/dashboard-debug.rs`.

use crate::session;

/// One parsed invocation: the global `--session <name>` binding
/// (docs/0055 D4) plus the verb. `--session` may appear anywhere and is
/// stripped before verb parsing; bridge verbs resolve it against the
/// session record, `session` verbs name their target themselves.
#[derive(Debug, Clone, PartialEq)]
pub struct Invocation {
    pub session: Option<String>,
    pub command: Command,
}
/// One parsed command. Field order mirrors the usage text.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// `tree [--json] [--window N] [view]` — plain dump by default; the
    /// views are the D7/D8 receipts (snapshot/diff/tab-stops).
    Tree {
        json: bool,
        window: Option<u64>,
        view: TreeView,
    },
    /// `click <accesskit_id> [--window N] [--wait-frame[=ms]]`
    Click {
        node: String,
        window: Option<u64>,
        wait_frame: Option<u64>,
    },
    /// `focus <accesskit_id> [--window N] [--wait-frame[=ms]]`
    Focus {
        node: String,
        window: Option<u64>,
        wait_frame: Option<u64>,
    },
    /// `key <keystroke>` — one full press (down + up).
    Key {
        keystroke: String,
        window: Option<u64>,
        wait_frame: Option<u64>,
    },
    /// `type <text>` — per-char key loop.
    Type {
        text: String,
        window: Option<u64>,
        wait_frame: Option<u64>,
    },
    /// `mouse move|down|up <x> <y>` / `mouse scroll <x> <y> <dy>`
    Mouse {
        action: MouseAction,
        x: f32,
        y: f32,
        dy: f32,
        wait_frame: Option<u64>,
    },
    /// `wait-frame [--above N] [--timeout ms]` — block until the app
    /// drew again (docs/0055 D6). The synchronization primitive receipts
    /// are built on; a timeout is the named hidden-workspace failure.
    WaitFrame { above: Option<u64>, timeout_ms: u64 },
    /// `logs [-f]`
    Logs { follow: bool },
    /// `shell`
    Shell,
    /// `session start|status|stop|env` — the lifecycle verbs (docs/0055
    /// D5). I/O lives in the CLI's session module.
    Session(SessionCmd),
}

/// The `session` subverbs (docs/0055 D5).
#[derive(Debug, Clone, PartialEq)]
pub enum SessionCmd {
    /// `session start [--name n] [--binary path] [--size WxH]
    /// [--timeout secs]`
    Start {
        name: Option<String>,
        binary: Option<String>,
        size: (u32, u32),
        timeout_secs: u64,
    },
    /// `session status [name]` — the record plus liveness, or a list.
    Status { name: Option<String> },
    /// `session stop <name> [--keep]`
    Stop { name: String, keep: bool },
    /// `session env <name>` — the `env` prefix for ad-hoc use.
    Env { name: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Move,
    Down,
    Up,
    Scroll,
}

/// The `tree` views (docs/0055 D7/D8).
#[derive(Debug, Clone, PartialEq)]
pub enum TreeView {
    /// Plain merged dump.
    Plain,
    /// `--snapshot <name>` — write `snapshots/<name>.json`.
    Snapshot(String),
    /// `--diff <a> <b>` — compare two stored snapshots.
    Diff(String, String),
    /// `--tab-stops` — walk the focus cycle, numbered.
    TabStops,
}

/// Default `--wait-frame[=ms]` budget on drive verbs (D6).
pub const DEFAULT_WAIT_FRAME_MS: u64 = 2000;
/// Default `wait-frame` verb timeout (D6).
pub const DEFAULT_WAIT_FRAME_TIMEOUT_MS: u64 = 5000;

pub const USAGE: &str = "usage: dashboard-debug [--session <name>] <verb> [args]

  --session <name>                bind bridge verbs to a live session (docs/0055 D4)

  tree [--json] [--window N]      dump the component tree
  tree --snapshot <name>           write snapshots/<name>.json (D7)
  tree --diff <a> <b> [--json]     structural diff of two snapshots
  tree --tab-stops                 walk the focus cycle, numbered (D8)
  wait-frame [--above N] [--timeout ms]  block until the app drew again (D6)
  click <accesskit_id> [--window N] [--wait-frame[=ms]]  AT-SPI click;
                                   coordinate fallback on 4005/4006 (D9)
  focus <accesskit_id> [--window N] [--wait-frame[=ms]]  AT-SPI focus
  key <keystroke> [--window N] [--wait-frame[=ms]]  one key press, e.g. key ctrl-s
  type <text> [--window N] [--wait-frame[=ms]]       per-char key loop
  mouse move|down|up <x> <y> [--wait-frame[=ms]]     raw pointer dispatch
  mouse scroll <x> <y> <dy> [--wait-frame[=ms]]      scroll wheel dispatch
  logs [-f]                        ring log tail (follow with -f)
  shell                            REPL over these verbs

  session start [--name n] [--binary path] [--size WxH] [--timeout s]
                                   isolated headless session, ready to drive
  session status [name]            session records + liveness (list when no name)
  session stop <name> [--keep]     stop + purge (keep artifacts with --keep)
  session env <name>               env prefix for ad-hoc app runs";

/// Parse a full invocation: extract the global `--session` binding
/// (docs/0055 D4), then parse what remains as a verb.
pub fn parse_invocation(args: &[String]) -> Result<Invocation, String> {
    let mut session: Option<String> = None;
    let mut rest = Vec::with_capacity(args.len());
    let mut skip_next = false;
    for arg in args {
        if std::mem::take(&mut skip_next) {
            if let Some(current) = session {
                if current != arg.as_str() {
                    return Err("duplicate --session".into());
                }
            }
            session = Some(arg.clone());
        } else if arg == "--session" {
            skip_next = true;
        } else if let Some(value) = arg.strip_prefix("--session=") {
            if let Some(current) = &session {
                if current != value {
                    return Err("duplicate --session".into());
                }
            }
            session = Some(value.to_string());
        } else {
            rest.push(arg.clone());
        }
    }
    if skip_next {
        return Err("--session needs a value".into());
    }
    Ok(Invocation {
        session,
        command: parse_args(&rest)?,
    })
}

/// Parse `argv[1..]`.
pub fn parse_args(args: &[String]) -> Result<Command, String> {
    let Some((verb, rest)) = args.split_first() else {
        return Err(USAGE.to_string());
    };
    match verb.as_str() {
        "tree" => parse_tree(rest),
        "wait-frame" => parse_wait_frame(rest),
        "click" | "focus" => {
            let node = positional(rest, verb, "<accesskit_id>")?;
            let window = parse_window_flag(rest)?;
            let wait_frame = parse_wait_frame_flag(rest)?;
            Ok(if verb == "click" {
                Command::Click {
                    node: node.to_string(),
                    window,
                    wait_frame,
                }
            } else {
                Command::Focus {
                    node: node.to_string(),
                    window,
                    wait_frame,
                }
            })
        }
        "key" => {
            let keystroke = positional(rest, verb, "<keystroke>")?;
            let window = parse_window_flag(rest)?;
            let wait_frame = parse_wait_frame_flag(rest)?;
            Ok(Command::Key {
                keystroke: keystroke.to_string(),
                window,
                wait_frame,
            })
        }
        "type" => {
            let text = positional(rest, verb, "<text>")?;
            let window = parse_window_flag(rest)?;
            let wait_frame = parse_wait_frame_flag(rest)?;
            Ok(Command::Type {
                text: text.to_string(),
                window,
                wait_frame,
            })
        }
        "mouse" => parse_mouse(rest),
        "logs" => {
            let follow = rest.iter().any(|arg| arg == "-f");
            Ok(Command::Logs { follow })
        }
        "session" => parse_session(rest),
        "shell" if rest.is_empty() => Ok(Command::Shell),
        _ => Err(USAGE.to_string()),
    }
}

fn parse_session(rest: &[String]) -> Result<Command, String> {
    let Some((sub, rest)) = rest.split_first() else {
        return Err("usage: dashboard-debug session start|status|stop|env …".into());
    };
    match sub.as_str() {
        "start" => {
            let mut name = None;
            let mut binary = None;
            let mut size = (1920, 1080);
            let mut timeout_secs = 30u64;
            let mut pending: Option<&str> = None;
            for arg in rest {
                if let Some(flag) = pending.take() {
                    match flag {
                        "--name" => name = Some(arg.clone()),
                        "--binary" => binary = Some(arg.clone()),
                        "--size" => {
                            size = session::parse_size(arg).ok_or_else(|| {
                                format!("bad --size {arg:?} — want WxH, e.g. 1920x1080")
                            })?;
                        }
                        "--timeout" => {
                            timeout_secs = arg
                                .parse::<u64>()
                                .map_err(|_| format!("bad --timeout {arg:?} — want seconds"))?;
                        }
                        _ => unreachable!(),
                    }
                } else if arg == "--name"
                    || arg == "--binary"
                    || arg == "--size"
                    || arg == "--timeout"
                {
                    pending = Some(arg.as_str());
                } else if let Some(value) = arg.strip_prefix("--name=") {
                    name = Some(value.to_string());
                } else if let Some(value) = arg.strip_prefix("--binary=") {
                    binary = Some(value.to_string());
                } else if let Some(value) = arg.strip_prefix("--size=") {
                    size = session::parse_size(value).ok_or_else(|| {
                        format!("bad --size {value:?} — want WxH, e.g. 1920x1080")
                    })?;
                } else if let Some(value) = arg.strip_prefix("--timeout=") {
                    timeout_secs = value
                        .parse::<u64>()
                        .map_err(|_| format!("bad --timeout {value:?} — want seconds"))?;
                } else {
                    return Err(
                        "usage: dashboard-debug session start [--name n] [--binary path] [--size WxH] [--timeout s]"
                            .into(),
                    );
                }
            }
            if pending.is_some() {
                return Err("flag needs a value".into());
            }
            Ok(Command::Session(SessionCmd::Start {
                name,
                binary,
                size,
                timeout_secs,
            }))
        }
        "status" => {
            let mut name = None;
            for arg in rest {
                if arg.starts_with("--") {
                    return Err("usage: dashboard-debug session status [name]".into());
                }
                if name.replace(arg.clone()).is_some() {
                    return Err("usage: dashboard-debug session status [name]".into());
                }
            }
            Ok(Command::Session(SessionCmd::Status { name }))
        }
        "stop" => {
            let mut name = None;
            let mut keep = false;
            for arg in rest {
                if arg == "--keep" {
                    keep = true;
                } else if arg.starts_with("--") || name.replace(arg.clone()).is_some() {
                    return Err("usage: dashboard-debug session stop <name> [--keep]".into());
                }
            }
            let name = name.ok_or_else(|| "session stop needs a <name>".to_string())?;
            Ok(Command::Session(SessionCmd::Stop { name, keep }))
        }
        "env" => {
            let [name] = rest else {
                return Err("usage: dashboard-debug session env <name>".into());
            };
            if name.starts_with("--") {
                return Err("usage: dashboard-debug session env <name>".into());
            }
            Ok(Command::Session(SessionCmd::Env { name: name.clone() }))
        }
        other => Err(format!(
            "unknown session subverb {other:?} (start|status|stop|env)"
        )),
    }
}

/// `tree` views (D7/D8): one of `--snapshot <name>`, `--diff <a> <b>`,
/// `--tab-stops`, or none of them (plain dump). `--json` and
/// `--window N` combine with any view.
fn parse_tree(rest: &[String]) -> Result<Command, String> {
    const TREE_USAGE: &str = "usage: dashboard-debug tree [--json] [--window N] [--snapshot <name> | --diff <a> <b> | --tab-stops]";
    let mut json = false;
    let mut window = None;
    let mut snapshot = None;
    let mut diff: Option<(String, String)> = None;
    let mut tab_stops = false;
    let mut i = 0;
    while i < rest.len() {
        let arg = rest[i].as_str();
        if arg == "--window" {
            i += 1;
            let value = rest.get(i).ok_or("--window needs a value")?;
            window = Some(parse_window(value)?);
        } else if let Some(value) = arg.strip_prefix("--window=") {
            window = Some(parse_window(value)?);
        } else if arg == "--snapshot" {
            i += 1;
            let value = rest.get(i).ok_or("--snapshot needs a name")?;
            snapshot = Some(value.clone());
        } else if let Some(value) = arg.strip_prefix("--snapshot=") {
            snapshot = Some(value.to_string());
        } else if arg == "--diff" {
            if diff.is_some() {
                return Err("--diff takes exactly two names".into());
            }
            let first = rest.get(i + 1).ok_or("--diff takes two names")?;
            let second = rest.get(i + 2).ok_or("--diff takes two names")?;
            diff = Some((first.clone(), second.clone()));
            i += 2;
        } else if arg == "--json" {
            json = true;
        } else if arg == "--tab-stops" {
            tab_stops = true;
        } else {
            return Err(TREE_USAGE.into());
        }
        i += 1;
    }
    let selected = [snapshot.is_some(), diff.is_some(), tab_stops]
        .iter()
        .filter(|flag| **flag)
        .count();
    if selected > 1 {
        return Err("--snapshot, --diff and --tab-stops are mutually exclusive".into());
    }
    let view = if let Some(name) = snapshot {
        TreeView::Snapshot(name)
    } else if let Some((a, b)) = diff {
        TreeView::Diff(a, b)
    } else if tab_stops {
        TreeView::TabStops
    } else {
        TreeView::Plain
    };
    Ok(Command::Tree { json, window, view })
}

/// `wait-frame [--above N] [--timeout ms]` (D6). Both flags accept the
/// space and `=` forms.
fn parse_wait_frame(rest: &[String]) -> Result<Command, String> {
    const WAIT_USAGE: &str = "usage: dashboard-debug wait-frame [--above N] [--timeout ms]";
    let mut above = None;
    let mut timeout_ms = DEFAULT_WAIT_FRAME_TIMEOUT_MS;
    let mut pending: Option<&str> = None;
    for arg in rest {
        if let Some(flag) = pending.take() {
            let value: u64 = arg
                .parse()
                .map_err(|_| format!("bad {flag} value {arg:?} — want a number"))?;
            match flag {
                "--above" => above = Some(value),
                "--timeout" => timeout_ms = value,
                _ => unreachable!(),
            }
            continue;
        }
        if arg == "--above" || arg == "--timeout" {
            pending = Some(arg.as_str());
        } else if let Some(value) = arg.strip_prefix("--above=") {
            above = Some(
                value
                    .parse()
                    .map_err(|_| format!("bad --above {value:?}"))?,
            );
        } else if let Some(value) = arg.strip_prefix("--timeout=") {
            timeout_ms = value
                .parse()
                .map_err(|_| format!("bad --timeout {value:?} — want milliseconds"))?;
        } else {
            return Err(WAIT_USAGE.into());
        }
    }
    if pending.is_some() {
        return Err("flag needs a value".into());
    }
    Ok(Command::WaitFrame { above, timeout_ms })
}

/// `--wait-frame` (default budget) or `--wait-frame=ms` on drive verbs
/// (D6). Duplicates are an error — a repeated flag is a typo, not a
/// preference.
fn parse_wait_frame_flag(rest: &[String]) -> Result<Option<u64>, String> {
    let mut wait_frame: Option<u64> = None;
    for arg in rest {
        if arg == "--wait-frame" {
            if wait_frame.is_some() {
                return Err("duplicate --wait-frame".into());
            }
            wait_frame = Some(DEFAULT_WAIT_FRAME_MS);
        } else if let Some(value) = arg.strip_prefix("--wait-frame=") {
            if wait_frame.is_some() {
                return Err("duplicate --wait-frame".into());
            }
            wait_frame = Some(
                value
                    .parse()
                    .map_err(|_| format!("bad --wait-frame {value:?} — want milliseconds"))?,
            );
        }
    }
    Ok(wait_frame)
}

fn parse_mouse(rest: &[String]) -> Result<Command, String> {
    let Some((action, coords)) = rest.split_first() else {
        return Err("usage: dashboard-debug mouse move|down|up|scroll <x> <y> [dy]".into());
    };
    let action = match action.as_str() {
        "move" => MouseAction::Move,
        "down" => MouseAction::Down,
        "up" => MouseAction::Up,
        "scroll" => MouseAction::Scroll,
        other => {
            return Err(format!(
                "unknown mouse action {other:?} (move|down|up|scroll)"
            ));
        }
    };
    let wait_frame = parse_wait_frame_flag(rest)?;
    // Flags (e.g. `--wait-frame`) ride anywhere after the action.
    let coords: Vec<&String> = coords.iter().filter(|arg| !arg.starts_with("--")).collect();
    let want = if action == MouseAction::Scroll { 3 } else { 2 };
    if coords.len() != want {
        return Err(format!(
            "mouse {action:?} takes {want} number{}, got {}",
            if want == 1 { "" } else { "s" },
            coords.len()
        ));
    }
    let mut numbers = [0f32; 3];
    for (slot, arg) in coords.iter().enumerate() {
        numbers[slot] = arg
            .parse::<f32>()
            .map_err(|_| format!("{arg:?} is not a number"))?;
    }
    let [x, y, dy] = numbers;
    Ok(Command::Mouse {
        action,
        x,
        y,
        dy,
        wait_frame,
    })
}

/// The single non-flag argument of a verb that takes one. `--window N`,
/// `--window=N` and unknown flags don't count against it — unknown
/// flags are the parser's job to reject, not to silently swallow.
fn positional<'a>(rest: &'a [String], verb: &str, what: &str) -> Result<&'a str, String> {
    let mut candidates = Vec::new();
    let mut skip_next = false;
    for arg in rest {
        if skip_next {
            skip_next = false;
        } else if arg == "--window" {
            skip_next = true;
        } else if !arg.starts_with("--window=") && !arg.starts_with("--") {
            candidates.push(arg);
        }
    }
    match candidates.as_slice() {
        [one] => Ok(one),
        _ => Err(format!("usage: dashboard-debug {verb} {what} [--window N]")),
    }
}

fn parse_window_flag(rest: &[String]) -> Result<Option<u64>, String> {
    if let Some(pos) = rest.iter().position(|arg| arg == "--window") {
        let value = rest
            .get(pos + 1)
            .ok_or_else(|| "--window needs a value".to_string())?;
        return parse_window(value).map(Some);
    }
    if let Some(arg) = rest.iter().find(|arg| arg.starts_with("--window=")) {
        return parse_window(arg).map(Some);
    }
    Ok(None)
}

/// `--window=N` or the `N` following a bare `--window`.
fn parse_window(arg: &str) -> Result<u64, String> {
    let value = arg
        .strip_prefix("--window")
        .and_then(|rest| rest.strip_prefix('='))
        .unwrap_or(arg);
    value
        .parse::<u64>()
        .map_err(|_| format!("{arg:?} is not a window id"))
}

/// Quote-aware splitter for the `shell` REPL: whitespace-separated words,
/// double-quoted runs kept together (no escapes — REPL-grade, not shell
/// emulation).
pub fn split_line(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for ch in line.chars() {
        match ch {
            '"' => quoted = !quoted,
            ' ' | '\t' if !quoted => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            other => current.push(other),
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// A char as an upstream `Keystroke::parse` key: control-ish whitespace
/// gets its key name, everything else is itself.
pub fn keystroke_for_char(ch: char) -> String {
    match ch {
        ' ' => "space".into(),
        '\t' => "tab".into(),
        '\n' => "enter".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn verb_table() {
        assert_eq!(
            parse_args(&args(&["tree"])).unwrap(),
            Command::Tree {
                json: false,
                window: None,
                view: TreeView::Plain
            }
        );
        assert_eq!(
            parse_args(&args(&["tree", "--json", "--window", "2"])).unwrap(),
            Command::Tree {
                json: true,
                window: Some(2),
                view: TreeView::Plain
            }
        );
        assert_eq!(
            parse_args(&args(&["click", "42", "--window", "1"])).unwrap(),
            Command::Click {
                node: "42".into(),
                window: Some(1),
                wait_frame: None
            }
        );
        assert_eq!(
            parse_args(&args(&["focus", "18446744073709551617"])).unwrap(),
            Command::Focus {
                node: "18446744073709551617".into(),
                window: None,
                wait_frame: None
            }
        );
        assert_eq!(
            parse_args(&args(&["key", "ctrl-s"])).unwrap(),
            Command::Key {
                keystroke: "ctrl-s".into(),
                window: None,
                wait_frame: None
            }
        );
        assert_eq!(
            parse_args(&args(&["type", "hello world"])).unwrap(),
            Command::Type {
                text: "hello world".into(),
                window: None,
                wait_frame: None
            }
        );
        assert_eq!(
            parse_args(&args(&["mouse", "move", "10.5", "20"])).unwrap(),
            Command::Mouse {
                action: MouseAction::Move,
                x: 10.5,
                y: 20.0,
                dy: 0.0,
                wait_frame: None
            }
        );
        assert_eq!(
            parse_args(&args(&["mouse", "scroll", "5", "6", "-120"])).unwrap(),
            Command::Mouse {
                action: MouseAction::Scroll,
                x: 5.0,
                y: 6.0,
                dy: -120.0,
                wait_frame: None
            }
        );
        assert_eq!(
            parse_args(&args(&["logs", "-f"])).unwrap(),
            Command::Logs { follow: true }
        );
        assert_eq!(parse_args(&args(&["shell"])).unwrap(), Command::Shell);
    }

    #[test]
    fn tree_views_parse_with_flags() {
        assert_eq!(
            parse_args(&args(&["tree", "--snapshot", "before"])).unwrap(),
            Command::Tree {
                json: false,
                window: None,
                view: TreeView::Snapshot("before".into())
            }
        );
        assert_eq!(
            parse_args(&args(&["tree", "--diff", "a", "b", "--json"])).unwrap(),
            Command::Tree {
                json: true,
                window: None,
                view: TreeView::Diff("a".into(), "b".into())
            }
        );
        assert_eq!(
            parse_args(&args(&["tree", "--window=2", "--tab-stops"])).unwrap(),
            Command::Tree {
                json: false,
                window: Some(2),
                view: TreeView::TabStops
            }
        );
        assert_eq!(
            parse_args(&args(&["tree", "--snapshot=top"])).unwrap(),
            Command::Tree {
                json: false,
                window: None,
                view: TreeView::Snapshot("top".into())
            }
        );
    }

    #[test]
    fn tree_view_errors() {
        assert!(
            parse_args(&args(&["tree", "--diff", "a"])).is_err(),
            "one name"
        );
        assert!(
            parse_args(&args(&["tree", "--snapshot", "a", "--diff", "a", "b"])).is_err(),
            "conflicting views"
        );
        assert!(
            parse_args(&args(&["tree", "--tab-stops", "--snapshot", "a"])).is_err(),
            "conflicting views 2"
        );
        assert!(
            parse_args(&args(&["tree", "--snapshot"])).is_err(),
            "missing name"
        );
    }

    #[test]
    fn wait_frame_verb_parses_defaults_and_both_forms() {
        assert_eq!(
            parse_args(&args(&["wait-frame"])).unwrap(),
            Command::WaitFrame {
                above: None,
                timeout_ms: DEFAULT_WAIT_FRAME_TIMEOUT_MS
            }
        );
        assert_eq!(
            parse_args(&args(&["wait-frame", "--above", "41", "--timeout=250"])).unwrap(),
            Command::WaitFrame {
                above: Some(41),
                timeout_ms: 250
            }
        );
        assert!(parse_args(&args(&["wait-frame", "--above=x"])).is_err());
        assert!(parse_args(&args(&["wait-frame", "--above"])).is_err());
        assert!(parse_args(&args(&["wait-frame", "--tokens"])).is_err());
    }

    #[test]
    fn wait_frame_flag_parses_on_drive_verbs() {
        assert_eq!(
            parse_args(&args(&["click", "5", "--wait-frame"])).unwrap(),
            Command::Click {
                node: "5".into(),
                window: None,
                wait_frame: Some(DEFAULT_WAIT_FRAME_MS)
            }
        );
        assert_eq!(
            parse_args(&args(&["key", "tab", "--wait-frame=300"])).unwrap(),
            Command::Key {
                keystroke: "tab".into(),
                window: None,
                wait_frame: Some(300)
            }
        );
        assert_eq!(
            parse_args(&args(&["mouse", "move", "1", "2", "--wait-frame"])).unwrap(),
            Command::Mouse {
                action: MouseAction::Move,
                x: 1.0,
                y: 2.0,
                dy: 0.0,
                wait_frame: Some(DEFAULT_WAIT_FRAME_MS)
            }
        );
        assert_eq!(
            parse_args(&args(&["type", "hi", "--wait-frame=50"])).unwrap(),
            Command::Type {
                text: "hi".into(),
                window: None,
                wait_frame: Some(50)
            }
        );
        assert!(parse_args(&args(&["click", "5", "--wait-frame", "--wait-frame=1"])).is_err());
        assert!(parse_args(&args(&["key", "a", "--wait-frame=x"])).is_err());
    }

    #[test]
    fn arity_and_value_errors() {
        assert!(parse_args(&args(&["click"])).is_err(), "missing node");
        assert!(
            parse_args(&args(&["click", "1", "2"])).is_err(),
            "two positionals"
        );
        assert!(
            parse_args(&args(&["mouse", "warp", "1", "2"])).is_err(),
            "bad action"
        );
        assert!(
            parse_args(&args(&["mouse", "move", "1"])).is_err(),
            "too few coords"
        );
        assert!(
            parse_args(&args(&["mouse", "scroll", "1", "2", "x"])).is_err(),
            "bad dy"
        );
        assert!(
            parse_args(&args(&["tree", "--window", "x"])).is_err(),
            "bad window"
        );
        assert!(parse_args(&args(&[])).is_err(), "no verb");
        assert!(
            parse_args(&args(&["teleport", "1", "2"])).is_err(),
            "unknown verb"
        );
    }

    #[test]
    fn invocation_carries_global_session_flag() {
        let inv = parse_invocation(&args(&["--session", "qa", "tree"])).unwrap();
        assert_eq!(inv.session.as_deref(), Some("qa"));
        assert_eq!(
            inv.command,
            Command::Tree {
                json: false,
                window: None,
                view: TreeView::Plain
            }
        );

        let inv = parse_invocation(&args(&["tree", "--session=qa", "--json"])).unwrap();
        assert_eq!(inv.session.as_deref(), Some("qa"));
        assert_eq!(
            inv.command,
            Command::Tree {
                json: true,
                window: None,
                view: TreeView::Plain
            }
        );

        // Repeating the same binding is harmless; a different one is not.
        assert!(parse_invocation(&args(&["--session", "a", "--session", "a", "tree"])).is_ok());
        assert!(parse_invocation(&args(&["--session", "a", "--session", "b", "tree"])).is_err());
        assert!(
            parse_invocation(&args(&["tree", "--session"])).is_err(),
            "missing value"
        );
        assert!(parse_invocation(&args(&[])).is_err(), "no verb");
    }

    #[test]
    fn session_verb_table() {
        assert_eq!(
            parse_args(&args(&["session", "start"])).unwrap(),
            Command::Session(SessionCmd::Start {
                name: None,
                binary: None,
                size: (1920, 1080),
                timeout_secs: 30,
            })
        );
        assert_eq!(
            parse_args(&args(&[
                "session",
                "start",
                "--name",
                "qa",
                "--binary",
                "/abs/app",
                "--size=800x600",
                "--timeout=5"
            ]))
            .unwrap(),
            Command::Session(SessionCmd::Start {
                name: Some("qa".into()),
                binary: Some("/abs/app".into()),
                size: (800, 600),
                timeout_secs: 5,
            })
        );
        assert_eq!(
            parse_args(&args(&["session", "status"])).unwrap(),
            Command::Session(SessionCmd::Status { name: None })
        );
        assert_eq!(
            parse_args(&args(&["session", "status", "qa"])).unwrap(),
            Command::Session(SessionCmd::Status {
                name: Some("qa".into())
            })
        );
        assert_eq!(
            parse_args(&args(&["session", "stop", "qa", "--keep"])).unwrap(),
            Command::Session(SessionCmd::Stop {
                name: "qa".into(),
                keep: true
            })
        );
        assert_eq!(
            parse_args(&args(&["session", "env", "qa"])).unwrap(),
            Command::Session(SessionCmd::Env { name: "qa".into() })
        );
    }

    #[test]
    fn session_verb_errors() {
        assert!(parse_args(&args(&["session"])).is_err(), "no subverb");
        assert!(
            parse_args(&args(&["session", "teleport"])).is_err(),
            "bad subverb"
        );
        assert!(
            parse_args(&args(&["session", "start", "--size=1080"])).is_err(),
            "bad size"
        );
        assert!(
            parse_args(&args(&["session", "start", "--size"])).is_err(),
            "dangling size"
        );
        assert!(
            parse_args(&args(&["session", "start", "--name"])).is_err(),
            "dangling name"
        );
        assert!(
            parse_args(&args(&["session", "stop"])).is_err(),
            "missing name"
        );
        assert!(
            parse_args(&args(&["session", "env"])).is_err(),
            "missing name"
        );
        assert!(
            parse_args(&args(&["session", "status", "a", "b"])).is_err(),
            "two names"
        );
    }

    #[test]
    fn session_start_flags_accept_space_and_equals_forms() {
        let space = parse_args(&args(&[
            "session",
            "start",
            "--size",
            "800x600",
            "--timeout",
            "5",
        ]))
        .unwrap();
        let equals = parse_args(&args(&[
            "session",
            "start",
            "--size=800x600",
            "--timeout=5",
        ]))
        .unwrap();
        assert_eq!(space, equals);
        match space {
            Command::Session(SessionCmd::Start {
                size, timeout_secs, ..
            }) => {
                assert_eq!(size, (800, 600));
                assert_eq!(timeout_secs, 5);
            }
            other => panic!("wrong command: {other:?}"),
        }
    }
    #[test]
    fn shell_splitter_keeps_quoted_runs_together() {
        assert_eq!(
            split_line("type \"hello world\" --window 2"),
            ["type", "hello world", "--window", "2"]
        );
        assert_eq!(split_line("  tree   --json "), ["tree", "--json"]);
        assert_eq!(split_line(""), Vec::<String>::new());
    }

    #[test]
    fn char_to_keystroke_mapping() {
        assert_eq!(keystroke_for_char('a'), "a");
        assert_eq!(keystroke_for_char(' '), "space");
        assert_eq!(keystroke_for_char('\n'), "enter");
    }
}
