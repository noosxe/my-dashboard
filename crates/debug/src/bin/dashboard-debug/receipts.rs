//! The receipt verbs (docs/0055 D10/D12/D13): `screenshot`, `run`,
//! `doctor`. Session-record and process I/O stays here; the grammar and
//! verdict logic are pure modules in the lib (`script`, `doctor`).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use dashboard_debug::cli::{Command, DEFAULT_WAIT_FRAME_TIMEOUT_MS};
use dashboard_debug::doctor::{self, Check, Status};
use dashboard_debug::script::{self, Script, StepOp};
use dashboard_debug::session::{
    SessionDirs, SessionRecord, find_in_path, read_record, sessions_root, valid_name,
};

use crate::{
    Conn, baseline_verb, connect_quiet, focused_node, frame_number, run_command, snapshots_root,
    tab_stops, tree_diff, tree_dump, tree_snapshot,
};

/// `screenshot [--name stem]` (D10): grim against the session's
/// compositor, into `screenshots/<stem>-<n>.png`.
pub fn screenshot(session: Option<&str>, stem: &str) -> i32 {
    let Some(name) = session else {
        eprintln!(
            "screenshot needs a session — pass --session <name> (it captures the session compositor, never the host's)"
        );
        return 2;
    };
    if !valid_name(name) {
        eprintln!("invalid session name {name:?}");
        return 2;
    }
    let dirs = SessionDirs::at(sessions_root(), name);
    let record = match read_record(&dirs.session_json()) {
        Ok(Some(record)) => record,
        Ok(None) => {
            eprintln!("no session {name:?} at {}", dirs.dir.display());
            return 1;
        }
        Err(err) => {
            eprintln!("{}: {err}", dirs.session_json().display());
            return 1;
        }
    };
    run_grim(&record, &dirs.dir, stem)
}

fn grim_path() -> Option<PathBuf> {
    let path_value = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
    find_in_path("grim", &path_value)
}

/// Spawn grim for `record`'s compositor, writing under `<session
/// root>/screenshots/`. Shared by the verb and the script runner's
/// `screenshot` step.
pub(crate) fn run_grim(record: &SessionRecord, session_root: &Path, stem: &str) -> i32 {
    let Some(grim) = grim_path() else {
        eprintln!(
            "grim is not on PATH — screenshots need it (the dev shell ships it: nix develop)"
        );
        return 1;
    };
    let dir = session_root.join("screenshots");
    if let Err(err) = std::fs::create_dir_all(&dir) {
        eprintln!("cannot create {}: {err}", dir.display());
        return 1;
    }
    let path = script::allocate_path(&dir, stem, "png");
    let output = StdCommand::new(&grim)
        .env(
            "WAYLAND_DISPLAY",
            record.wayland_display.as_deref().unwrap_or_default(),
        )
        .arg(&path)
        .stderr(Stdio::piped())
        .output();
    let output = match output {
        Ok(output) => output,
        Err(err) => {
            eprintln!("cannot run grim: {err}");
            return 1;
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        eprintln!(
            "grim failed (exit {:?}) against WAYLAND_DISPLAY={} — headless \
             screencopy refused{stderr_part}",
            output.status.code(),
            record.wayland_display.clone().unwrap_or_default(),
            stderr_part = if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        );
        return 1;
    }
    println!("{}", path.display());
    0
}

/// `doctor [--session <name>]` (D13): ten checks, one verdict — the
/// platform line scopes the rest (a mac attach is not a broken linux
/// session; docs/0010 D4 dump-only mode).
pub fn doctor(session: Option<&str>) -> i32 {
    let mut checks: Vec<Check> = Vec::new();

    // 0: what this platform supports — scopes every later line.
    checks.push(Check::new(
        "platform",
        Status::Info,
        if cfg!(target_os = "macos") {
            "macos attach mode — logs, input.dispatch and quit work; the tree \
             cluster waits on a11y-tree population (#401)"
                .to_string()
        } else {
            "linux — full bridge: sessions, tree, AT-SPI enrichment".to_string()
        },
    ));
    // 1-3: the session record and what it points at. Without a session
    // these have nothing to check — the default attach has no record.
    let mut record = None;
    match session {
        Some(name) => {
            if !valid_name(name) {
                eprintln!("invalid session name {name:?}");
                return 2;
            }
            let dirs = SessionDirs::at(sessions_root(), name);
            match read_record(&dirs.session_json()) {
                Ok(Some(parsed)) => {
                    checks.push(Check::new(
                        "session file",
                        Status::Ok,
                        format!(
                            "{} well-formed (created {})",
                            dirs.session_json().display(),
                            parsed.created_at
                        ),
                    ));
                    record = Some((parsed, dirs));
                }
                Ok(None) => checks.push(Check::new(
                    "session file",
                    Status::Fail,
                    format!(
                        "no record at {} — start one: session start --name {name}",
                        dirs.session_json().display()
                    ),
                )),
                Err(err) => checks.push(Check::new(
                    "session file",
                    Status::Fail,
                    format!(
                        "{} is malformed: {err} — inspect or remove it, then \
                         session start --name {name}",
                        dirs.session_json().display()
                    ),
                )),
            }
        }
        None => checks.push(Check::new(
            "session file",
            Status::Na,
            "no session bound — bind with --session to check the record",
        )),
    }

    let app_pid = record.as_ref().and_then(|(r, _)| r.app_pid);
    let sway_pid = record.as_ref().and_then(|(r, _)| r.sway_pid);
    match (app_pid, sway_pid) {
        (Some(app), Some(sway)) => {
            let app_alive = crate::session::pid_alive(app);
            let sway_alive = crate::session::pid_alive(sway);
            if app_alive && sway_alive {
                checks.push(Check::new(
                    "processes",
                    Status::Ok,
                    format!("app pid {app} and sway pid {sway} alive"),
                ));
            } else {
                let dead = [
                    (!app_alive).then_some(format!("app pid {app}")),
                    (!sway_alive).then_some(format!("sway pid {sway}")),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" and ");
                checks.push(Check::new(
                    "processes",
                    Status::Fail,
                    format!(
                        "{dead} dead — session stop --keep {name} to inspect, then start fresh",
                        name = session.unwrap_or_default()
                    ),
                ));
            }
        }
        _ => checks.push(Check::new(
            "processes",
            Status::Na,
            "no session record to read PIDs from",
        )),
    }

    match &record {
        Some((r, _)) => {
            let runtime =
                std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".into());
            let socket = r
                .wayland_display
                .as_deref()
                .map(|display| PathBuf::from(&runtime).join(display));
            if socket.as_ref().is_some_and(|socket| socket.exists()) {
                checks.push(Check::new(
                    "sway socket",
                    Status::Ok,
                    format!("{} present", socket.unwrap().display()),
                ));
            } else {
                let detail = match socket {
                    Some(socket) => format!(
                        "{} missing — the compositor is gone; stop and start \
                         the session",
                        socket.display()
                    ),
                    None => "record has no wayland_display — the compositor                             socket was never resolved"
                        .to_string(),
                };
                checks.push(Check::new("sway socket", Status::Fail, detail));
            }
        }
        None => checks.push(Check::new(
            "sway socket",
            Status::Na,
            "no session record to resolve a compositor socket",
        )),
    }

    // 4-6: the bridge. Point AppPaths at the session's state dir so the
    // socket/token resolve there (bind would refuse a dead session —
    // doctor must diagnose it instead).
    if let Some((r, _)) = &record {
        crate::session::set_state_dir_env(&r.state_dir);
    }
    // Pre-check the socket so a dead session reports through the
    // check table instead of leaking the connect helper's stderr line.
    let socket = dashboard_core::AppPaths::init()
        .runtime_dir()
        .join("debug.sock");
    let bridge = if socket.exists() {
        connect_quiet().ok()
    } else {
        None
    };
    let mut conn = match bridge {
        Some(mut conn) => {
            let windows = conn
                .call("window.list", json!({}))
                .ok()
                .and_then(|response| {
                    response
                        .get("result")
                        .and_then(|result| result.as_array().cloned())
                });
            match windows {
                Some(windows) => {
                    checks.push(Check::new(
                        "bridge",
                        Status::Ok,
                        format!("auth ok, window.list: {} window(s)", windows.len()),
                    ));
                    Some(conn)
                }
                None => {
                    checks.push(Check::new(
                        "bridge",
                        Status::Fail,
                        "socket accepted but window.list failed — the app is \
                         wedged; session stop then start",
                    ));
                    None
                }
            }
        }
        None => {
            let runtime = dashboard_core::AppPaths::init().runtime_dir().to_path_buf();
            checks.push(Check::new(
                "bridge",
                Status::Fail,
                format!(
                    "cannot connect/auth at {} — the app is down; session \
                     stop then start (or --keep to inspect)",
                    runtime.join("debug.sock").display()
                ),
            ));
            checks.push(Check::new(
                "frames",
                Status::Fail,
                "not checked — bridge unreachable",
            ));
            checks.push(Check::new(
                "a11y merge",
                Status::Fail,
                "not checked — bridge unreachable",
            ));
            None
        }
    };

    // 5-6 need a live connection.
    let mut first_dump = None;
    if let Some(conn) = conn.as_mut() {
        let dump = tree_dump(conn, &None).ok();
        first_dump = dump;
        if let Some(dump) = &first_dump {
            let before = frame_number(dump);
            // Nudge with a focus move, not a mouse move: gpui redraws
            // only on state change, and a mouse move that crosses no
            // hover boundary draws nothing (verified live). Tab always
            // moves focus — and a focus change always draws.
            let nudge = conn
                .call(
                    "input.dispatch",
                    json!({"event": {"type": "key_down", "keystroke": "tab"}}),
                )
                .and_then(|_| {
                    conn.call(
                        "input.dispatch",
                        json!({"event": {"type": "key_up", "keystroke": "tab"}}),
                    )
                });
            let advanced = if nudge.is_ok() {
                wait_for_advance(conn, before, 2000)
            } else {
                None
            };
            match advanced {
                Some(after) => checks.push(Check::new(
                    "frames",
                    Status::Ok,
                    format!("advancing: {before} → {after} after a tab nudge"),
                )),
                None => checks.push(Check::new(
                    "frames",
                    Status::Fail,
                    format!(
                        "stuck at {before} — the hidden-workspace signature; \
                         restart the session"
                    ),
                )),
            }
        } else {
            let (status, detail) = if cfg!(target_os = "macos") {
                (
                    Status::Na,
                    "tree.dump unavailable — macOS does not populate the a11y \
                     tree yet (#401); frames are not observable \
                     until it does",
                )
            } else {
                (
                    Status::Fail,
                    "tree.dump failed — the app is wedged; restart the session",
                )
            };
            checks.push(Check::new("frames", status, detail));
        }
    }

    if let Some(dump) = &first_dump {
        let merge = dump.get("a11y_merge");
        let (nodes, enriched, unmatched) = (
            merge
                .and_then(|m| m.get("dump_nodes"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            merge
                .and_then(|m| m.get("enriched"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            merge
                .and_then(|m| m.get("unmatched_dump"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
        );
        let walk_available = dump.get("walk").is_none();
        let (status, detail) = doctor::merge_verdict(
            nodes as usize,
            enriched as usize,
            unmatched as usize,
            walk_available,
        );
        checks.push(Check::new("a11y merge", status, detail));
    } else if conn.is_some() {
        checks.push(Check::new(
            "a11y merge",
            Status::Na,
            "not checked — tree.dump unavailable on this platform (#401)",
        ));
    }

    // 7: the recorded flag vs the bus. A *live* bridge legitimately
    // holds the flag true (its a11y activation set it; stop restores
    // the recorded value) — drift only matters once the session is
    // gone: a crashed session that never restored is the D3 warn.
    let bridge_alive = conn.is_some();
    match &record {
        Some((r, _)) => match crate::session::read_screen_reader_enabled() {
            Ok(current) if bridge_alive => checks.push(Check::new(
                "a11y flag",
                Status::Ok,
                format!(
                    "ScreenReaderEnabled {current} — the live bridge owns it \
                     (recorded {:?}; stop restores)",
                    r.screen_reader_at_start
                ),
            )),
            Ok(current) if r.screen_reader_at_start.unwrap_or(current) == current => {
                checks.push(Check::new(
                    "a11y flag",
                    Status::Ok,
                    format!(
                        "ScreenReaderEnabled {current} (as recorded; the \
                         session is gone and left it clean)"
                    ),
                ))
            }
            Ok(current) => checks.push(Check::new(
                "a11y flag",
                Status::Warn,
                format!(
                    "ScreenReaderEnabled drifted to {current} (recorded {:?}) — \
                     restore: busctl --user set-property org.a11y.Bus \
                     /org/a11y/bus org.a11y.Status ScreenReaderEnabled b {}",
                    r.screen_reader_at_start, current
                ),
            )),
            Err(err) => checks.push(Check::new(
                "a11y flag",
                Status::Warn,
                format!("cannot read the bus flag: {err}"),
            )),
        },
        None => checks.push(Check::new(
            "a11y flag",
            Status::Na,
            "no session record to compare the flag against",
        )),
    }

    // 8: grim availability gates screenshots.
    checks.push(Check::new(
        "grim",
        Status::Info,
        if grim_path().is_some() {
            "on PATH — screenshots ready (D10)".to_string()
        } else if cfg!(target_os = "macos") {
            "not applicable on macOS (grim is a Wayland tool) — screenshots \
             wait on the in-app frame capture (docs/0055 follow-ups)"
                .to_string()
        } else {
            "not on PATH — screenshots unavailable; the dev shell ships it".to_string()
        },
    ));

    // 9: strays — other dashboard-debug processes (the forgotten logs -f).
    let self_pid = std::process::id();
    let strays = stray_debug_processes(self_pid);
    checks.push(Check::new(
        "strays",
        if strays.is_empty() {
            Status::Ok
        } else {
            Status::Warn
        },
        if strays.is_empty() {
            "no other dashboard-debug processes".to_string()
        } else {
            format!(
                "pids {} — a forgotten logs -f?; kill them before they \
                 confuse the next session",
                strays.join(", ")
            )
        },
    ));

    let text = doctor::render(&checks);
    println!("{text}");
    if checks.iter().any(|c| c.status == Status::Fail) {
        1
    } else {
        0
    }
}

/// Poll `tree.dump` until the frame number exceeds `before`, or `budget`
/// elapses. Returns the new number.
fn wait_for_advance(conn: &mut Conn, before: u64, budget_ms: u64) -> Option<u64> {
    let deadline = Instant::now() + Duration::from_millis(budget_ms);
    loop {
        if let Ok(dump) = tree_dump(conn, &None) {
            let frame = frame_number(&dump);
            if frame > before {
                return Some(frame);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// PIDs of other `dashboard-debug` processes (by /proc comm).
fn stray_debug_processes(self_pid: u32) -> Vec<String> {
    let mut strays = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return strays;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        if pid == self_pid {
            continue;
        }
        if let Ok(comm) = std::fs::read_to_string(entry.path().join("comm")) {
            if comm.trim() == "dashboard-debug" {
                strays.push(pid.to_string());
            }
        }
    }
    strays.sort();
    strays
}

/// `run <script>` (D12): execute the parsed script against `conn`,
/// echoing line + outcome into `transcripts/<stem>-<n>.log`.
pub fn run(conn: &mut Conn, script_path: &Path, parsed: &Script, session: Option<&str>) -> i32 {
    // #401 interim: degraded, not broken — input/logging steps run;
    // tree-backed and screenshot steps will fail. Warn once, per the
    // gate policy (docs/0010 D4); DASHBOARD_DEBUG_A11Y=1 skips it.
    // transcripts/ sits beside snapshots/ — the session root when a
    // session is bound, the resolved state-dir parent otherwise.
    let root = snapshots_root();
    let dir = root
        .parent()
        .map(|parent| parent.to_path_buf())
        .unwrap_or(root)
        .join("transcripts");
    if let Err(err) = std::fs::create_dir_all(&dir) {
        eprintln!("cannot create {}: {err}", dir.display());
        return 1;
    }
    let stem = script_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("run");
    let transcript_path = script::allocate_path(&dir, stem, "log");
    let mut transcript = match std::fs::File::create(&transcript_path) {
        Ok(file) => file,
        Err(err) => {
            eprintln!("cannot write {}: {err}", transcript_path.display());
            return 1;
        }
    };
    let say = |transcript: &mut std::fs::File, line: &str| {
        println!("{line}");
        let _ = writeln!(transcript, "{line}");
    };
    say(
        &mut transcript,
        &format!(
            "# run {} at {} — session {}",
            script_path.display(),
            chrono_now(),
            session.unwrap_or("(default discovery)")
        ),
    );
    println!("transcript: {}", transcript_path.display());
    let _ = writeln!(transcript, "# transcript: {}", transcript_path.display());

    for step in &parsed.steps {
        let outcome = run_step(conn, session, &step.op);
        match outcome {
            StepOutcome::Ok(detail) => {
                say(
                    &mut transcript,
                    &format!("{}> {} — ok{}", step.line_no, step.text, detail),
                );
            }
            StepOutcome::Failed(detail) => {
                say(
                    &mut transcript,
                    &format!("{}> {} — FAILED: {}", step.line_no, step.text, detail),
                );
                say(
                    &mut transcript,
                    &format!(
                        "script failed at line {} — {} step(s) before this passed",
                        step.line_no,
                        parsed
                            .steps
                            .iter()
                            .take_while(|s| s.line_no < step.line_no)
                            .count()
                    ),
                );
                eprintln!(
                    "script failed at line {} ({}): {}",
                    step.line_no,
                    script_path.display(),
                    detail
                );
                return 1;
            }
        }
    }
    say(
        &mut transcript,
        &format!("{} step(s), all passed", parsed.steps.len()),
    );
    0
}

enum StepOutcome {
    Ok(String),
    Failed(String),
}

fn run_step(conn: &mut Conn, session: Option<&str>, op: &StepOp) -> StepOutcome {
    match op {
        // Drive verbs reuse the verb implementations verbatim — the
        // runner is a scripting shell over them, not a parallel parser.
        StepOp::Click(node) => verb(
            conn,
            &Command::Click {
                node: node.clone(),
                window: None,
                wait_frame: None,
            },
        ),
        StepOp::Focus(node) => verb(
            conn,
            &Command::Focus {
                node: node.clone(),
                window: None,
                wait_frame: None,
            },
        ),
        StepOp::Key(keystroke) => verb(
            conn,
            &Command::Key {
                keystroke: keystroke.clone(),
                window: None,
                wait_frame: None,
            },
        ),
        StepOp::Type(text) => verb(
            conn,
            &Command::Type {
                text: text.clone(),
                window: None,
                wait_frame: None,
            },
        ),
        StepOp::Mouse { action, x, y, dy } => verb(
            conn,
            &Command::Mouse {
                action: *action,
                x: *x,
                y: *y,
                dy: *dy,
                wait_frame: None,
            },
        ),
        StepOp::WaitFrame => verb(
            conn,
            &Command::WaitFrame {
                above: None,
                timeout_ms: DEFAULT_WAIT_FRAME_TIMEOUT_MS,
            },
        ),
        StepOp::Snapshot(name) => {
            let code = tree_snapshot(conn, &None, name);
            outcome(code, &format!(" snapshot {name} written"))
        }
        StepOp::Diff(a, b) => {
            let code = tree_diff(a, b, false);
            outcome(code, &format!(" diff {a} {b} printed"))
        }
        StepOp::AssertLabel(substr) => {
            let nodes = match snapshot_nodes(conn) {
                Ok(nodes) => nodes,
                Err(code) => return StepOutcome::Failed(format!("dump failed (exit {code})")),
            };
            match nodes
                .iter()
                .find(|n| n.label.as_deref().is_some_and(|l| l.contains(substr)))
            {
                Some(node) => StepOutcome::Ok(format!(
                    " label {:?} on #{}",
                    node.label.as_deref().unwrap_or_default(),
                    node.accesskit_id
                )),
                None => StepOutcome::Failed(format!(
                    "no label contains {substr:?} ({} nodes scanned)",
                    nodes.len()
                )),
            }
        }
        StepOp::AssertNoLabel(substr) => {
            let nodes = match snapshot_nodes(conn) {
                Ok(nodes) => nodes,
                Err(code) => return StepOutcome::Failed(format!("dump failed (exit {code})")),
            };
            match nodes
                .iter()
                .find(|n| n.label.as_deref().is_some_and(|l| l.contains(substr)))
            {
                Some(node) => StepOutcome::Failed(format!(
                    "label {:?} on #{} contains {substr:?}",
                    node.label.as_deref().unwrap_or_default(),
                    node.accesskit_id
                )),
                None => StepOutcome::Ok(format!(" no label contains {substr:?}")),
            }
        }
        StepOp::AssertFocus(node) => {
            let dump = match tree_dump(conn, &None) {
                Ok(dump) => dump,
                Err(code) => return StepOutcome::Failed(format!("dump failed (exit {code})")),
            };
            match focused_node(&dump) {
                Some((id, _role, label)) if id == *node => StepOutcome::Ok(format!(
                    " focus on #{} {}",
                    id,
                    label.map(|l| format!("\"{l}\"")).unwrap_or_default()
                )),
                Some((id, _, _)) => StepOutcome::Failed(format!("focus is on #{id}, not #{node}")),
                None => StepOutcome::Failed("no focused node in the dump".into()),
            }
        }
        StepOp::AssertDiffLabel { a, b, substr } => {
            let (before, after) = match (read_snapshot(a), read_snapshot(b)) {
                (Ok(before), Ok(after)) => (before, after),
                (Err(err), _) | (_, Err(err)) => {
                    return StepOutcome::Failed(format!("cannot read snapshots: {err}"));
                }
            };
            let diff = dashboard_debug::snapshot::diff(&before, &after);
            let mentions = |label: &Option<String>| {
                label
                    .as_deref()
                    .is_some_and(|l| l.contains(substr.as_str()))
            };
            match diff.entries.iter().find(|e| {
                e.kind == dashboard_debug::snapshot::DiffKind::Changed
                    && (mentions(&e.label)
                        || e.label_change
                            .as_ref()
                            .is_some_and(|(a, b)| mentions(a) || mentions(b)))
            }) {
                Some(entry) => StepOutcome::Ok(format!(
                    " changed line mentions {substr:?} (#{})",
                    entry.accesskit_id
                )),
                None => StepOutcome::Failed(format!(
                    "no *changed* line mentions {substr:?} ({} entries, {} changed)",
                    diff.entries.len(),
                    diff.changed()
                )),
            }
        }
        StepOp::Audit => {
            let dump = match tree_dump(conn, &None) {
                Ok(dump) => dump,
                Err(code) => return StepOutcome::Failed(format!("dump failed (exit {code})")),
            };
            match dashboard_debug::audit::audit_dump(&dump) {
                Ok(report) => {
                    if report.fails() > 0 {
                        StepOutcome::Failed(format!(
                            "audit: {} FAIL findings (of {} nodes) — {}",
                            report.fails(),
                            report.nodes_audited,
                            report
                                .findings
                                .iter()
                                .filter(|f| f.severity == dashboard_debug::audit::Severity::Fail)
                                .map(|f| f.detail.clone())
                                .collect::<Vec<_>>()
                                .join("; ")
                        ))
                    } else {
                        StepOutcome::Ok(format!(
                            " audit clean ({} nodes, {} findings, {} incomplete)",
                            report.nodes_audited,
                            report.findings_count(),
                            report.incomplete()
                        ))
                    }
                }
                Err(err) => StepOutcome::Failed(format!("cannot audit dump: {err}")),
            }
        }
        StepOp::TabStops => {
            let code = tab_stops(conn, &None);
            outcome(code, " tab-stops cycle closed")
        }
        StepOp::Baseline { surface, update } => {
            let code = baseline_verb(conn, surface, *update, &None);
            outcome(
                code,
                &format!(
                    " baseline {surface}{}",
                    if *update { " updated" } else { "" }
                ),
            )
        }
        StepOp::AssertBaseline(surface) => {
            let code = baseline_verb(conn, surface, false, &None);
            outcome(code, &format!(" baseline {surface} clean"))
        }
        StepOp::Screenshot(stem) => {
            let code = screenshot_via_record(session, stem);
            outcome(code, &format!(" screenshot {stem} taken"))
        }
        StepOp::Bind(_) => StepOutcome::Ok(" (binding recorded)".into()),
    }
}

fn verb(conn: &mut Conn, command: &Command) -> StepOutcome {
    let code = run_command(conn, command);
    outcome(code, "")
}

fn outcome(code: i32, ok_detail: &str) -> StepOutcome {
    if code == 0 {
        StepOutcome::Ok(ok_detail.to_string())
    } else {
        StepOutcome::Failed(format!("verb exited {code}"))
    }
}

/// The `screenshot` step needs the session record, not the bridge —
/// resolve it from the run's binding.
fn screenshot_via_record(session: Option<&str>, stem: &str) -> i32 {
    let Some(name) = session else {
        return 1;
    };
    if !valid_name(name) {
        return 1;
    }
    let dirs = SessionDirs::at(sessions_root(), name);
    match read_record(&dirs.session_json()) {
        Ok(Some(record)) => run_grim(&record, &dirs.dir, stem),
        _ => 1,
    }
}

fn snapshot_nodes(conn: &mut Conn) -> Result<Vec<dashboard_debug::snapshot::SnapshotNode>, i32> {
    let dump = tree_dump(conn, &None)?;
    let snapshot =
        dashboard_debug::snapshot::Snapshot::from_dump("assert", &dump).map_err(|err| {
            eprintln!("cannot flatten dump: {err}");
            1
        })?;
    Ok(snapshot.nodes)
}

fn read_snapshot(name: &str) -> Result<dashboard_debug::snapshot::Snapshot, String> {
    let path = snapshots_root().join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).map_err(|err| format!("{name}: {err}"))?;
    serde_json::from_str(&text).map_err(|err| format!("{name}: {err}"))
}

fn chrono_now() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "?".into())
}
