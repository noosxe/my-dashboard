//! `dashboard-debug` CLI argument parsing (docs/0010 D9): hand-rolled —
//! ≈8 verbs, no clap. The parser is pure (a `&[String]` slice in, a
//! [`Command`] out) so it is table-testable without spawning the binary;
//! the I/O half lives in `src/bin/dashboard-debug.rs`.

/// One parsed invocation. Field order mirrors the usage text.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// `tree [--json] [--window N]`
    Tree { json: bool, window: Option<u64> },
    /// `click <accesskit_id> [--window N]`
    Click { node: String, window: Option<u64> },
    /// `focus <accesskit_id> [--window N]`
    Focus { node: String, window: Option<u64> },
    /// `key <keystroke>` — one full press (down + up).
    Key {
        keystroke: String,
        window: Option<u64>,
    },
    /// `type <text>` — per-char key loop.
    Type { text: String, window: Option<u64> },
    /// `mouse move|down|up <x> <y>` / `mouse scroll <x> <y> <dy>`
    Mouse {
        action: MouseAction,
        x: f32,
        y: f32,
        dy: f32,
    },
    /// `logs [-f]`
    Logs { follow: bool },
    /// `shell`
    Shell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Move,
    Down,
    Up,
    Scroll,
}

pub const USAGE: &str = "usage: dashboard-debug <verb> [args]

  tree [--json] [--window N]      dump the component tree
  click <accesskit_id> [--window N]  AT-SPI click (single left)
  focus <accesskit_id> [--window N] AT-SPI focus
  key <keystroke> [--window N]     one key press, e.g. key ctrl-s
  type <text> [--window N]         per-char key loop
  mouse move|down|up <x> <y>       raw pointer dispatch
  mouse scroll <x> <y> <dy>        scroll wheel dispatch
  logs [-f]                        ring log tail (follow with -f)
  shell                            REPL over these verbs";

/// Parse `argv[1..]`.
pub fn parse_args(args: &[String]) -> Result<Command, String> {
    let Some((verb, rest)) = args.split_first() else {
        return Err(USAGE.to_string());
    };
    match verb.as_str() {
        "tree" => parse_tree(rest),
        "click" | "focus" => {
            let node = positional(rest, verb, "<accesskit_id>")?;
            let window = parse_window_flag(rest)?;
            Ok(if verb == "click" {
                Command::Click {
                    node: node.to_string(),
                    window,
                }
            } else {
                Command::Focus {
                    node: node.to_string(),
                    window,
                }
            })
        }
        "key" => {
            let keystroke = positional(rest, verb, "<keystroke>")?;
            let window = parse_window_flag(rest)?;
            Ok(Command::Key {
                keystroke: keystroke.to_string(),
                window,
            })
        }
        "type" => {
            let text = positional(rest, verb, "<text>")?;
            let window = parse_window_flag(rest)?;
            Ok(Command::Type {
                text: text.to_string(),
                window,
            })
        }
        "mouse" => parse_mouse(rest),
        "logs" => {
            let follow = rest.iter().any(|arg| arg == "-f");
            Ok(Command::Logs { follow })
        }
        "shell" if rest.is_empty() => Ok(Command::Shell),
        _ => Err(USAGE.to_string()),
    }
}

fn parse_tree(rest: &[String]) -> Result<Command, String> {
    let mut json = false;
    let mut window = None;
    let mut skip_next = false;
    for arg in rest {
        if std::mem::take(&mut skip_next) {
            // The value of a bare `--window`.
            window = Some(parse_window(arg)?);
        } else if arg == "--window" {
            skip_next = true;
        } else if let Some(value) = arg.strip_prefix("--window=") {
            window = Some(parse_window(value)?);
        } else if arg == "--json" {
            json = true;
        } else {
            return Err("usage: dashboard-debug tree [--json] [--window N]".into());
        }
    }
    Ok(Command::Tree { json, window })
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
    Ok(Command::Mouse { action, x, y, dy })
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
                window: None
            }
        );
        assert_eq!(
            parse_args(&args(&["tree", "--json", "--window", "2"])).unwrap(),
            Command::Tree {
                json: true,
                window: Some(2)
            }
        );
        assert_eq!(
            parse_args(&args(&["click", "42", "--window", "1"])).unwrap(),
            Command::Click {
                node: "42".into(),
                window: Some(1)
            }
        );
        assert_eq!(
            parse_args(&args(&["focus", "18446744073709551617"])).unwrap(),
            Command::Focus {
                node: "18446744073709551617".into(),
                window: None
            }
        );
        assert_eq!(
            parse_args(&args(&["key", "ctrl-s"])).unwrap(),
            Command::Key {
                keystroke: "ctrl-s".into(),
                window: None
            }
        );
        assert_eq!(
            parse_args(&args(&["type", "hello world"])).unwrap(),
            Command::Type {
                text: "hello world".into(),
                window: None
            }
        );
        assert_eq!(
            parse_args(&args(&["mouse", "move", "10.5", "20"])).unwrap(),
            Command::Mouse {
                action: MouseAction::Move,
                x: 10.5,
                y: 20.0,
                dy: 0.0
            }
        );
        assert_eq!(
            parse_args(&args(&["mouse", "scroll", "5", "6", "-120"])).unwrap(),
            Command::Mouse {
                action: MouseAction::Scroll,
                x: 5.0,
                y: 6.0,
                dy: -120.0
            }
        );
        assert_eq!(
            parse_args(&args(&["logs", "-f"])).unwrap(),
            Command::Logs { follow: true }
        );
        assert_eq!(parse_args(&args(&["shell"])).unwrap(), Command::Shell);
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
