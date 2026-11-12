//! The `run` script grammar (docs/0055 D12): a committed, replayable
//! receipt. Pure parsing and path allocation only — the runner lives in
//! the bin layer.

use crate::cli::MouseAction;

/// One parsed step of a script.
#[derive(Debug, Clone, PartialEq)]
pub enum StepOp {
    /// `--session <name>` — binds the run (must precede every verb).
    Bind(String),
    /// `click <accesskit_id> [--wait-frame[=ms]]` — the atomic form
    /// samples the frame counter before the dispatch; a separate
    /// `wait-frame` step can lose that race on a fast page (#530).
    Click {
        node: String,
        wait_frame: Option<u64>,
    },
    /// `focus <accesskit_id>`
    Focus(String),
    /// `key <keystroke>`
    Key(String),
    /// `type <text>`
    Type(String),
    /// `mouse move|down|up <x> <y>` / `mouse scroll <x> <y> <dy>`, each
    /// with an optional atomic `--wait-frame[=ms]`.
    Mouse {
        action: MouseAction,
        x: f32,
        y: f32,
        dy: f32,
        wait_frame: Option<u64>,
    },
    /// `wait-frame` — bare, the runner's default budget.
    WaitFrame,
    /// `snapshot <name>`
    Snapshot(String),
    /// `diff <a> <b>` — prints, never asserts.
    Diff(String, String),
    /// `assert-label <substr>` — some node label contains the substring.
    AssertLabel(String),
    /// `wait-label <substr>` — poll until some node label contains the
    /// substring (bounded, 5 s unless `--timeout <secs>` widens it):
    /// settles on a page whose content lands after a background read
    /// (docs/0082 D11), where a frame wait can race.
    WaitLabel {
        substr: String,
        timeout_secs: Option<u64>,
        /// `--poke <accesskit_id>`: click this node each poll round the
        /// label is still missing, for a page that reads its store once
        /// per open and never re-reads when the data lands late.
        poke: Option<String>,
    },
    /// `assert-no-label <substr>` — no node label contains it.
    AssertNoLabel(String),
    /// `assert-write <op> [key=value…]` — a fixture write was recorded
    /// (docs/0010 D11).
    AssertWrite {
        op: String,
        pairs: Vec<(String, String)>,
    },
    /// `assert-focus <accesskit_id>` — the focused node is this one.
    AssertFocus(String),
    /// `assert-diff-label <a> <b> <substr>` — a *changed* line of the
    /// diff mentions the substring.
    AssertDiffLabel {
        a: String,
        b: String,
        substr: String,
    },
    /// `audit` — run the 0065 D3 rules; receipt-red on FAIL severity.
    Audit,
    /// `tab-stops` — the D8 focus-cycle walk; red on traps or an
    /// unclosed cycle. Pairs with `audit` (0065 D3).
    TabStops,
    /// `critique <surface>` — the 0065 D7 ritual input: screenshot +
    /// tree snapshot as one paired capture.
    Critique(String),
    /// `golden <surface> [--theme t] [--update]` — pixel golden
    /// capture/compare (0065 D6). Theme defaults dark on the step
    /// (receipts pin what they mean).
    Golden {
        surface: String,
        theme: String,
        update: bool,
    },
    /// `assert-golden <surface> [--theme t]` — receipt-red on pixel
    /// drift against the committed baseline.
    AssertGolden { surface: String, theme: String },
    /// `baseline <surface> [--update]` — write/refresh the committed
    /// golden (generation scripts only).
    Baseline { surface: String, update: bool },
    /// `assert-baseline <surface>` — structural golden check, red on
    /// drift (0065 D5).
    AssertBaseline(String),
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

const WAIT_LABEL_USAGE: &str = "wait-label needs <substr> (quote spaces), optionally --timeout <secs> and --poke <accesskit_id>";

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

/// `--wait-frame` / `--wait-frame=<ms>` splitter for drive verbs. The
/// atomic form samples the frame counter before the dispatch, which a
/// separate `wait-frame` step can lose to a fast page (#530). Bare
/// `--wait-frame` carries the CLI's default budget (docs/0055 D6).
fn split_wait_frame(rest: &[String]) -> Result<(&[String], Option<u64>), String> {
    match rest.split_last() {
        Some((flag, rest)) if flag == "--wait-frame" => {
            Ok((rest, Some(crate::cli::DEFAULT_WAIT_FRAME_MS)))
        }
        Some((flag, rest)) if flag.starts_with("--wait-frame=") => {
            let ms = flag["--wait-frame=".len()..]
                .parse::<u64>()
                .map_err(|_| format!("--wait-frame=<ms> needs a number, got {flag:?}"))?;
            Ok((rest, Some(ms)))
        }
        _ => Ok((rest, None)),
    }
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
        "click" => {
            let (rest, wait_frame) = split_wait_frame(rest).map_err(|message| err(&message))?;
            let [node] = rest else {
                return Err(err(
                    "click needs exactly one <accesskit_id> [--wait-frame[=ms]]",
                ));
            };
            Ok(StepOp::Click {
                node: node.clone(),
                wait_frame,
            })
        }
        "focus" => {
            let [node] = rest else {
                return Err(err("focus needs exactly one <accesskit_id>"));
            };
            Ok(StepOp::Focus(node.clone()))
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
        "wait-label" => {
            let Some((substr, mut flags)) = rest.split_first() else {
                return Err(err(WAIT_LABEL_USAGE));
            };
            let (mut timeout_secs, mut poke) = (None, None);
            while let Some((flag, tail)) = flags.split_first() {
                let [value, tail @ ..] = tail else {
                    return Err(err(WAIT_LABEL_USAGE));
                };
                match flag.as_str() {
                    "--timeout" if timeout_secs.is_none() => {
                        timeout_secs = Some(
                            value
                                .parse::<u64>()
                                .ok()
                                .filter(|s| (1..=300).contains(s))
                                .ok_or_else(|| {
                                    err("wait-label --timeout needs whole seconds, 1-300")
                                })?,
                        );
                    }
                    "--poke" if poke.is_none() => poke = Some(value.clone()),
                    _ => return Err(err(WAIT_LABEL_USAGE)),
                }
                flags = tail;
            }
            Ok(StepOp::WaitLabel {
                substr: substr.clone(),
                timeout_secs,
                poke,
            })
        }
        "assert-label" | "assert-no-label" => {
            let [substr] = rest else {
                return Err(err(&format!(
                    "{verb} needs exactly one <substr> (quote spaces)"
                )));
            };
            Ok(match verb.as_str() {
                "assert-label" => StepOp::AssertLabel(substr.clone()),
                _ => StepOp::AssertNoLabel(substr.clone()),
            })
        }
        "assert-write" => {
            let (op, pairs) = crate::cli::parse_assert_write(rest).map_err(|m| err(&m))?;
            Ok(StepOp::AssertWrite { op, pairs })
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
        "audit" => {
            if !rest.is_empty() {
                return Err(err("audit takes no arguments here"));
            }
            Ok(StepOp::Audit)
        }
        "tab-stops" => {
            if !rest.is_empty() {
                return Err(err("tab-stops takes no arguments here"));
            }
            Ok(StepOp::TabStops)
        }
        "critique" => {
            let [surface] = rest else {
                return Err(err("critique takes exactly one surface name"));
            };
            if !crate::snapshot::valid_name(surface) {
                return Err(err("bad surface name"));
            }
            Ok(StepOp::Critique(surface.clone()))
        }
        "golden" | "assert-golden" => {
            let assert = verb == "assert-golden";
            let mut surface: Option<String> = None;
            let mut theme = "dark".to_string();
            let mut update = false;
            let mut rest = rest.iter().peekable();
            while let Some(arg) = rest.next() {
                match arg.as_str() {
                    "--theme" => {
                        let Some(value) = rest.next() else {
                            return Err(err("--theme needs dark|light"));
                        };
                        if value != "dark" && value != "light" {
                            return Err(err("unknown theme — dark|light only"));
                        }
                        theme = value.clone();
                    }
                    "--update" => {
                        if assert {
                            return Err(err("assert-golden never updates — drift is red"));
                        }
                        update = true;
                    }
                    other => {
                        if surface.is_none() {
                            surface = Some(other.to_string());
                        } else {
                            return Err(err("unknown golden argument"));
                        }
                    }
                }
            }
            let Some(surface) = surface else {
                return Err(err("golden needs a surface name"));
            };
            if !crate::snapshot::valid_name(&surface) {
                return Err(err("bad surface name"));
            }
            if assert {
                Ok(StepOp::AssertGolden { surface, theme })
            } else {
                Ok(StepOp::Golden {
                    surface,
                    theme,
                    update,
                })
            }
        }
        "baseline" => {
            let update = rest.iter().any(|arg| arg == "--update");
            let positional: Vec<&String> =
                rest.iter().filter(|arg| !arg.starts_with("--")).collect();
            let [surface] = positional.as_slice() else {
                return Err(err("baseline needs exactly one <surface>"));
            };
            if !crate::snapshot::valid_name(surface) {
                return Err(err("bad surface name (a-z0-9-)"));
            }
            Ok(StepOp::Baseline {
                surface: (*surface).clone(),
                update,
            })
        }
        "assert-baseline" => {
            let [surface] = rest else {
                return Err(err("assert-baseline needs exactly one <surface>"));
            };
            if !crate::snapshot::valid_name(surface) {
                return Err(err("bad surface name (a-z0-9-)"));
            }
            Ok(StepOp::AssertBaseline(surface.clone()))
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
    let (rest, wait_frame) = split_wait_frame(rest)?;
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
    Ok(StepOp::Mouse {
        action,
        x,
        y,
        dy,
        wait_frame,
    })
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
            if let Some(digits) = rest.strip_suffix(&format!(".{ext}"))
                && let Ok(n) = digits.parse::<u64>()
            {
                highest = highest.max(n);
            }
        }
    }
    dir.join(format!("{stem}-{}.{}", highest + 1, ext))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every committed receipt must parse — a grammar change that
    /// breaks a shipped script is a regression, not a surprise for the
    /// Linux box (found the hard way: favicon.txt shipped a `tree
    /// --json` step that was never a verb).
    #[test]
    fn parses_visual_golden_steps() {
        let script =
            parse_script("critique home\ngolden home --theme light --update\nassert-golden home\n")
                .unwrap();
        assert_eq!(
            script.steps[0].op,
            StepOp::Critique("home".into()),
            "critique takes the surface name"
        );
        assert_eq!(
            script.steps[1].op,
            StepOp::Golden {
                surface: "home".into(),
                theme: "light".into(),
                update: true
            }
        );
        assert_eq!(
            script.steps[2].op,
            StepOp::AssertGolden {
                surface: "home".into(),
                theme: "dark".into()
            },
            "assert-golden defaults dark — receipts pin what they mean"
        );
    }

    #[test]
    fn assert_golden_never_updates() {
        let err = parse_script("assert-golden home --update\n").unwrap_err();
        assert!(err.contains("never updates"), "got: {err}");
        let err = parse_script("golden home --theme sepia\n").unwrap_err();
        assert!(err.contains("dark|light"), "got: {err}");
    }

    #[test]
    fn committed_receipt_scripts_still_parse() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("verify");
        let mut checked = 0;
        let entries = std::fs::read_dir(&dir)
            .unwrap_or_else(|err| panic!("verify dir {}: {err}", dir.display()));
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("txt") {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let source =
                std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{name}: {err}"));
            parse_script(&source).unwrap_or_else(|err| panic!("{name}: {err}"));
            checked += 1;
        }
        assert!(checked >= 3, "expected the committed receipts to exist");
    }

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
        assert_eq!(
            script.steps[0].op,
            StepOp::Click {
                node: "#1851".into(),
                wait_frame: None
            }
        );
        assert_eq!(
            script.steps[4].op,
            StepOp::Diff("opened".into(), "after-tabs".into())
        );
        assert_eq!(script.steps[5].op, StepOp::AssertLabel("as of".into()));
        assert_eq!(
            parse_script("--session qa\nwait-label \"0 requests\"")
                .unwrap()
                .steps[0]
                .op,
            StepOp::WaitLabel {
                substr: "0 requests".into(),
                timeout_secs: None,
                poke: None
            }
        );
        assert_eq!(
            parse_script("--session qa\nwait-label \"0 requests\" --timeout 90")
                .unwrap()
                .steps[0]
                .op,
            StepOp::WaitLabel {
                substr: "0 requests".into(),
                timeout_secs: Some(90),
                poke: None
            }
        );
        assert_eq!(
            parse_script("--session qa\nwait-label x --poke 42 --timeout 9")
                .unwrap()
                .steps[0]
                .op,
            StepOp::WaitLabel {
                substr: "x".into(),
                timeout_secs: Some(9),
                poke: Some("42".into())
            }
        );
        assert!(parse_script("--session qa\nwait-label x --poke").is_err());
        assert!(parse_script("--session qa\nwait-label x --bogus 1").is_err());
        assert!(parse_script("--session qa\nwait-label x --timeout 0").is_err());
        assert!(parse_script("--session qa\nwait-label x --timeout").is_err());
        assert!(parse_script("--session qa\nwait-label").is_err());
        assert_eq!(script.steps[7].op, StepOp::Screenshot("opened".into()));
    }

    #[test]
    fn mouse_and_assert_arity() {
        assert!(parse_script("mouse down 448 169").is_ok());
        assert!(parse_script("mouse scroll 10 20 -3").is_ok());
        let waited = parse_script("mouse up 448 169 --wait-frame").unwrap();
        assert!(matches!(
            waited.steps[0].op,
            StepOp::Mouse {
                wait_frame: Some(_),
                ..
            }
        ));
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

    #[test]
    fn committed_audit_receipt_covers_every_surface() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/verify/audit.txt");
        let source =
            std::fs::read_to_string(path).expect("crates/debug/verify/audit.txt is committed");
        let script = parse_script(&source).expect("the audit receipt parses");
        let audits = script
            .steps
            .iter()
            .filter(|s| matches!(s.op, StepOp::Audit))
            .count();
        assert!(
            audits >= 5,
            "home, github, usage, hub + the closing home audit — got {audits}"
        );
        // #530: every hop is an id click carrying the atomic --wait-frame
        // (a separate wait-frame step can lose the race to a fast page),
        // and each hop asserts a page-unique label before its audit.
        let hops = script
            .steps
            .iter()
            .filter(|s| matches!(s.op, StepOp::Click { .. }))
            .count();
        assert!(hops >= 4, "github, usage, hub, home — got {hops}");
        assert!(
            script
                .steps
                .iter()
                .filter(|s| matches!(s.op, StepOp::Click { .. }))
                .all(|s| matches!(
                    s.op,
                    StepOp::Click {
                        wait_frame: Some(_),
                        ..
                    }
                )),
            "every click hops atomically — no separate wait-frame step to lose the race"
        );
        assert!(
            script
                .steps
                .iter()
                .any(|s| matches!(s.op, StepOp::TabStops)),
            "tab-order coverage stays paired with the audit (0065 D3)"
        );
        // #535: every audited stop also asserts its structural golden —
        // the walk visits home→usage (Budget, Requests)→hub→home-return→
        // github, and each stop pins the tree shape. The usage stops pin
        // the empty-ledger shape (#700); the seeded one is
        // usage-seeded.txt's.
        let mut baselines: Vec<&str> = script
            .steps
            .iter()
            .filter_map(|s| match &s.op {
                StepOp::AssertBaseline(surface) => Some(surface.as_str()),
                _ => None,
            })
            .collect();
        baselines.sort_unstable();
        assert_eq!(
            baselines,
            [
                "github",
                "home",
                "home-return",
                "hub",
                "usage-budget",
                "usage-requests"
            ],
            "every audit.txt stop asserts its committed golden (0065 D5)"
        );
    }

    #[test]
    fn pending_surfaces_probe_stays_staged() {
        // The settings/favicon hops are staged expected-red (#530,
        // blocked on gpui-kit#3467/#3468): the file must keep parsing and
        // keep its hop+assert+audit shape so the flip into audit.txt is
        // a pure move once upstream lands.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/verify/surfaces-pending.txt");
        let source = std::fs::read_to_string(path)
            .expect("crates/debug/verify/surfaces-pending.txt is committed");
        let script = parse_script(&source).expect("the pending-surfaces probe parses");
        let hops = script
            .steps
            .iter()
            .filter(|s| matches!(s.op, StepOp::Click { .. }))
            .count();
        let asserts = script
            .steps
            .iter()
            .filter(|s| matches!(s.op, StepOp::AssertLabel(_)))
            .count();
        let audits = script
            .steps
            .iter()
            .filter(|s| matches!(s.op, StepOp::Audit))
            .count();
        assert_eq!(hops, 2, "settings + favicon");
        assert_eq!(asserts, 2, "each hop asserts its landing");
        assert_eq!(audits, 2, "each surface audits");
        // #535: goldens ride along — captured from this same walk, so
        // the flip must carry the baselines (re-captured) into audit.txt.
        let mut baselines: Vec<&str> = script
            .steps
            .iter()
            .filter_map(|s| match &s.op {
                StepOp::AssertBaseline(surface) => Some(surface.as_str()),
                _ => None,
            })
            .collect();
        baselines.sort_unstable();
        assert_eq!(
            baselines,
            ["favicon", "settings"],
            "both staged stops assert their goldens too"
        );
    }
}
