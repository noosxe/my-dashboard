//! The `run` script grammar (docs/0055 D12): a committed, replayable
//! receipt. Pure parsing and path allocation only — the runner lives in
//! the bin layer.

use crate::cli::MouseAction;

/// One parsed step of a script.
#[derive(Debug, Clone, PartialEq)]
pub enum StepOp {
    /// `--session <name>` — binds the run (must precede every verb).
    Bind(String),
    /// `click <accesskit_id>`
    Click(String),
    /// `focus <accesskit_id>`
    Focus(String),
    /// `key <keystroke>`
    Key(String),
    /// `type <text>`
    Type(String),
    /// `mouse move|down|up <x> <y>` / `mouse scroll <x> <y> <dy>`
    Mouse {
        action: MouseAction,
        x: f32,
        y: f32,
        dy: f32,
    },
    /// `wait-frame` — bare, the runner's default budget.
    WaitFrame,
    /// `snapshot <name>`
    Snapshot(String),
    /// `diff <a> <b>` — prints, never asserts.
    Diff(String, String),
    /// `assert-label <substr>` — some node label contains the substring.
    AssertLabel(String),
    /// `assert-no-label <substr>` — no node label contains it.
    AssertNoLabel(String),
    /// `assert-focus <accesskit_id>` — the focused node is this one.
    AssertFocus(String),
    /// `assert-diff-label <a> <b> <substr>` — a *changed* line of the
    /// diff mentions the substring.
    AssertDiffLabel {
        a: String,
        b: String,
        substr: String,
    },
    /// `screenshot [stem]` — grim capture into the session's
    /// `screenshots/` (D10).
    Screenshot(String),
}

/// A step with its provenance — failures must name the script line.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// 1-based line number in the source file.
    pub line_no: usize,
    /// The raw (trimmed) source line, echoed into the transcript.
    pub text: String,
    pub op: StepOp,
}

/// A parsed script: the optional session binding plus its steps.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Script {
    pub session: Option<String>,
    pub steps: Vec<Step>,
}

/// Parse a whole script file. Comments (`#`) and blank lines are
/// allowed anywhere; `--session` must be the first directive.
pub fn parse_script(source: &str) -> Result<Script, String> {
    let mut script = Script::default();
    let mut bound = false;
    for (index, raw) in source.lines().enumerate() {
        let text = raw.trim();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        let line_no = index + 1;
        let op = parse_line(text, line_no)?;
        if let StepOp::Bind(name) = &op {
            if bound || !script.steps.is_empty() {
                return Err(format!(
                    "line {line_no}: --session must be the first directive"
                ));
            }
            script.session = Some(name.clone());
            bound = true;
            // The binding is recorded, not executed as a step.
            continue;
        }
        script.steps.push(Step {
            line_no,
            text: text.to_string(),
            op,
        });
    }
    Ok(script)
}

fn parse_line(text: &str, line_no: usize) -> Result<StepOp, String> {
    let tokens = tokenize(text).map_err(|err| format!("line {line_no}: {err}"))?;
    let err = |message: &str| format!("line {line_no}: {message}");
    let Some((verb, rest)) = tokens.split_first() else {
        return Err(err("empty line"));
    };
    match verb.as_str() {
        "--session" => {
            let [name] = rest else {
                return Err(err("--session needs exactly one name"));
            };
            Ok(StepOp::Bind(name.clone()))
        }
        "click" | "focus" => {
            let [node] = rest else {
                return Err(err(&format!("{verb} needs exactly one <accesskit_id>")));
            };
            Ok(if verb == "click" {
                StepOp::Click(node.clone())
            } else {
                StepOp::Focus(node.clone())
            })
        }
        "key" => {
            let [keystroke] = rest else {
                return Err(err("key needs exactly one <keystroke>"));
            };
            Ok(StepOp::Key(keystroke.clone()))
        }
        "type" => {
            let [text] = rest else {
                return Err(err("type needs exactly one <text> (quote spaces)"));
            };
            Ok(StepOp::Type(text.clone()))
        }
        "mouse" => parse_mouse(rest).map_err(|message| err(&message)),
        "wait-frame" => {
            if !rest.is_empty() {
                return Err(err("wait-frame takes no arguments here"));
            }
            Ok(StepOp::WaitFrame)
        }
        "snapshot" => {
            let [name] = rest else {
                return Err(err("snapshot needs exactly one <name>"));
            };
            Ok(StepOp::Snapshot(name.clone()))
        }
        "diff" => {
            let [a, b] = rest else {
                return Err(err("diff needs exactly two snapshot names"));
            };
            Ok(StepOp::Diff(a.clone(), b.clone()))
        }
        "assert-label" | "assert-no-label" => {
            let [substr] = rest else {
                return Err(err(&format!(
                    "{verb} needs exactly one <substr> (quote spaces)"
                )));
            };
            Ok(if verb == "assert-label" {
                StepOp::AssertLabel(substr.clone())
            } else {
                StepOp::AssertNoLabel(substr.clone())
            })
        }
        "assert-focus" => {
            let [node] = rest else {
                return Err(err("assert-focus needs exactly one <accesskit_id>"));
            };
            Ok(StepOp::AssertFocus(node.clone()))
        }
        "assert-diff-label" => {
            let [a, b, substr] = rest else {
                return Err(err("assert-diff-label needs exactly <a> <b> <substr>"));
            };
            Ok(StepOp::AssertDiffLabel {
                a: a.clone(),
                b: b.clone(),
                substr: substr.clone(),
            })
        }
        "screenshot" => match rest {
            [] => Ok(StepOp::Screenshot("shot".to_string())),
            [stem] => Ok(StepOp::Screenshot(stem.clone())),
            _ => Err(err("screenshot takes at most one [stem]")),
        },
        other => Err(err(&format!(
            "unknown verb {other:?} — see docs/0055 D12 for the grammar"
        ))),
    }
}

fn parse_mouse(rest: &[String]) -> Result<StepOp, String> {
    let Some((sub, args)) = rest.split_first() else {
        return Err("mouse needs move|down|up|scroll".into());
    };
    let action = match sub.as_str() {
        "move" => MouseAction::Move,
        "down" => MouseAction::Down,
        "up" => MouseAction::Up,
        "scroll" => MouseAction::Scroll,
        other => return Err(format!("unknown mouse action {other:?}")),
    };
    let (x, y, dy) = if action == MouseAction::Scroll {
        let [x, y, dy] = args else {
            return Err("mouse scroll needs <x> <y> <dy>".into());
        };
        (parse_coord(x)?, parse_coord(y)?, parse_coord(dy)?)
    } else {
        let [x, y] = args else {
            return Err("mouse move|down|up needs <x> <y>".into());
        };
        (parse_coord(x)?, parse_coord(y)?, 0.0)
    };
    Ok(StepOp::Mouse { action, x, y, dy })
}

fn parse_coord(token: &str) -> Result<f32, String> {
    token
        .parse::<f32>()
        .map_err(|_| format!("not a coordinate: {token:?}"))
}

/// Whitespace tokenizer with double-quoted groups (a quoted token may
/// contain spaces; embedded quotes are not supported — receipts don't
/// need them).
fn tokenize(text: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for ch in text.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if in_quotes {
        return Err("unterminated quote".into());
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Ok(tokens)
}

/// Allocate `<dir>/<stem>-<n>.<ext>` with `n` one above the highest
/// existing counter for the stem (D10 screenshots, D12 transcripts).
/// Scanning — not state — is the source of truth: a purged directory
/// restarts at 1, a kept one never overwrites.
pub fn allocate_path(dir: &std::path::Path, stem: &str, ext: &str) -> std::path::PathBuf {
    let mut highest = 0u64;
    if let Ok(entries) = std::fs::read_dir(dir) {
        let prefix = format!("{stem}-");
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(rest) = name.strip_prefix(&prefix) else {
                continue;
            };
            if let Some(digits) = rest.strip_suffix(&format!(".{ext}")) {
                if let Ok(n) = digits.parse::<u64>() {
                    highest = highest.max(n);
                }
            }
        }
    }
    dir.join(format!("{stem}-{}.{}", highest + 1, ext))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_design_example() {
        let script = parse_script(
            "# comment\n\n--session qa\nclick #1851\nsnapshot opened\nkey tab\n\
             wait-frame\ndiff opened after-tabs\nassert-label \"as of\"\n\
             assert-no-label \"Quota placeholder\"\nscreenshot opened\n",
        )
        .unwrap();
        assert_eq!(script.session.as_deref(), Some("qa"));
        assert_eq!(script.steps.len(), 8);
        assert_eq!(script.steps[0].line_no, 4);
        assert_eq!(script.steps[0].op, StepOp::Click("#1851".into()));
        assert_eq!(
            script.steps[4].op,
            StepOp::Diff("opened".into(), "after-tabs".into())
        );
        assert_eq!(script.steps[5].op, StepOp::AssertLabel("as of".into()));
        assert_eq!(script.steps[7].op, StepOp::Screenshot("opened".into()));
    }

    #[test]
    fn mouse_and_assert_arity() {
        assert!(parse_script("mouse down 448 169").is_ok());
        assert!(parse_script("mouse scroll 10 20 -3").is_ok());
        assert!(
            parse_script("mouse wiggle 1 2")
                .unwrap_err()
                .contains("unknown mouse action")
        );
        assert!(
            parse_script("mouse move 1")
                .unwrap_err()
                .contains("needs <x> <y>")
        );
        assert!(
            parse_script("mouse move a b")
                .unwrap_err()
                .contains("not a coordinate")
        );
        assert!(
            parse_script("assert-focus")
                .unwrap_err()
                .contains("needs exactly one")
        );
        assert!(
            parse_script("assert-diff-label a b")
                .unwrap_err()
                .contains("<a> <b> <substr>")
        );
        assert!(
            parse_script("diff only-one")
                .unwrap_err()
                .contains("two snapshot names")
        );
    }

    #[test]
    fn quoted_text_groups_spaces() {
        let script =
            parse_script("type \"hello world\"\nassert-no-label \"Quota placeholder\"").unwrap();
        assert_eq!(script.steps[0].op, StepOp::Type("hello world".into()));
        assert_eq!(
            script.steps[1].op,
            StepOp::AssertNoLabel("Quota placeholder".into())
        );
        assert!(
            parse_script("type \"unterminated")
                .unwrap_err()
                .contains("unterminated quote")
        );
    }

    #[test]
    fn unknown_verb_names_the_line() {
        let err = parse_script("click #1\nexplode now").unwrap_err();
        assert_eq!(
            err,
            "line 2: unknown verb \"explode\" — see docs/0055 D12 for the grammar"
        );
    }

    #[test]
    fn session_binding_must_lead() {
        assert!(
            parse_script("key tab\n--session qa")
                .unwrap_err()
                .contains("first directive")
        );
        assert!(
            parse_script("--session a\n--session b")
                .unwrap_err()
                .contains("first directive")
        );
    }

    #[test]
    fn screenshot_stem_defaults_and_clashes() {
        assert_eq!(
            parse_script("screenshot").unwrap().steps[0].op,
            StepOp::Screenshot("shot".into())
        );
        assert!(
            parse_script("screenshot a b")
                .unwrap_err()
                .contains("at most one")
        );
    }

    #[test]
    fn allocate_path_increments_the_highest_counter() {
        let dir = std::env::temp_dir().join(format!("dash-script-alloc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Fresh directory starts at 1.
        assert!(allocate_path(&dir, "shot", "png").ends_with("shot-1.png"));
        std::fs::write(dir.join("shot-1.png"), b"").unwrap();
        std::fs::write(dir.join("shot-7.png"), b"").unwrap();
        // Gaps do not matter; the highest wins.
        assert!(allocate_path(&dir, "shot", "png").ends_with("shot-8.png"));
        // Other stems and extensions are invisible to this stem's counter.
        std::fs::write(dir.join("other-99.png"), b"").unwrap();
        std::fs::write(dir.join("shot-2.log"), b"").unwrap();
        assert!(allocate_path(&dir, "shot", "png").ends_with("shot-8.png"));
        // Log allocation is independent.
        assert!(allocate_path(&dir, "run", "log").ends_with("run-1.log"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn committed_usage_freeze_script_stays_grammatical() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/verify/usage-freeze.txt");
        let source = std::fs::read_to_string(path)
            .expect("crates/debug/verify/usage-freeze.txt is committed");
        let script = parse_script(&source).expect("the freeze script parses");
        assert!(
            script.steps.iter().any(|s| matches!(
                s.op,
                StepOp::AssertLabel(ref substr) if substr.contains("as of")
            )),
            "the freeze receipt must assert the honesty label"
        );
    }
}
