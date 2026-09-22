//! `dashboard-debug` CLI (docs/0010 D9 + docs/0055): the human/agent
//! front end for the debug bridge. Reads the run token from the app's
//! runtime dir, speaks the NDJSON protocol over the Unix socket, prints
//! results.
//!
//! Verbs and grammar live in [`dashboard_debug::cli`] (pure, tested);
//! session lifecycle I/O in [`session`]; this file is only wiring:
//! connect, auth, request/response, render.

mod session;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use dashboard_debug::cli::{self, Command, MouseAction, TreeView, USAGE};
use dashboard_debug::protocol::error_code;
use dashboard_debug::snapshot;
use serde_json::{Value, json};

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let invocation = match cli::parse_invocation(&args) {
        Ok(invocation) => invocation,
        Err(usage) => {
            eprintln!("{usage}");
            return 2;
        }
    };

    if let Command::Session(cmd) = invocation.command {
        return session::run(cmd, invocation.session);
    }

    // Session binding (docs/0055 D4): --session > DASHBOARD_DEBUG_SESSION
    // > default discovery. `bind` redirects this process's roots before
    // any token/socket lookup, then verifies the socket exists — "app is
    // down", never a silent wrong-instance attach.
    let session_name = invocation.session.or_else(|| {
        std::env::var("DASHBOARD_DEBUG_SESSION")
            .ok()
            .filter(|value| !value.is_empty())
    });
    if let Some(name) = session_name {
        if let Err(code) = session::bind(&name) {
            return code;
        }
    }

    let runtime_dir = dashboard_core::AppPaths::init().runtime_dir().to_path_buf();
    let token_path = runtime_dir.join("debug.token");
    let Ok(token) = std::fs::read_to_string(&token_path).map(|t| t.trim().to_string()) else {
        eprintln!(
            "no debug token at {} — is the dashboard running (debug-tools)?",
            token_path.display()
        );
        return 1;
    };
    let socket_path = runtime_dir.join("debug.sock");
    let Ok(stream) = UnixStream::connect(&socket_path) else {
        eprintln!(
            "cannot connect to {} — is the dashboard running?",
            socket_path.display()
        );
        return 1;
    };

    let conn = Conn::new(stream);
    let mut conn = match conn {
        Ok(conn) => conn,
        Err(code) => return code,
    };
    if let Err(code) = conn.auth(&token) {
        return code;
    }

    match invocation.command {
        Command::Shell => shell(&mut conn),
        other => run_command(&mut conn, &other),
    }
}

/// One authenticated NDJSON connection: request in, one response out.
struct Conn {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
    next_id: u64,
}

impl Conn {
    fn new(stream: UnixStream) -> Result<Self, i32> {
        let reader = BufReader::new(stream.try_clone().expect("clone socket"));
        Ok(Self {
            writer: stream,
            reader,
            next_id: 0,
        })
    }

    /// The auth handshake (first line, per D2). Returns the exit code on
    /// failure.
    fn auth(&mut self, token: &str) -> Result<(), i32> {
        let id = self.next_id;
        self.next_id += 1;
        let response = self.raw(json!({"id": id, "method": "auth", "params": {"token": token}}))?;
        if response.get("result") == Some(&Value::Bool(true)) {
            Ok(())
        } else {
            report(&response);
            Err(1)
        }
    }

    /// One protocol round-trip. Errors are returned as values (the wire
    /// carries them); only I/O failures and malformed responses bubble
    /// up as `Err`.
    fn call(&mut self, method: &str, params: Value) -> Result<Value, i32> {
        let id = self.next_id;
        self.next_id += 1;
        self.raw(json!({"id": id, "method": method, "params": params}))
    }

    fn raw(&mut self, request: Value) -> Result<Value, i32> {
        let line = format!("{}\n", request);
        if self.writer.write_all(line.as_bytes()).is_err() {
            eprintln!("connection closed by the app");
            return Err(1);
        }
        let mut response = String::new();
        match self.reader.read_line(&mut response) {
            Ok(0) | Err(_) => {
                eprintln!("connection closed by the app");
                Err(1)
            }
            Ok(_) => serde_json::from_str(response.trim()).map_err(|err| {
                eprintln!("malformed response: {err}");
                1
            }),
        }
    }
}

/// `error {code}: {message}` on stderr; returns the exit code.
fn report(response: &Value) -> i32 {
    if let Some(error) = response.get("error") {
        eprintln!(
            "error {}: {}",
            error["code"],
            error["message"].as_str().unwrap_or("?")
        );
        return 1;
    }
    0
}

/// Run one parsed command; returns the process exit code.
fn run_command(conn: &mut Conn, command: &Command) -> i32 {
    match command {
        Command::Tree { json, window, view } => match view {
            TreeView::Plain => {
                let result = match tree_dump(conn, window) {
                    Ok(result) => result,
                    Err(code) => return code,
                };
                if *json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&result).expect("serialize result")
                    );
                } else {
                    render_tree(&result);
                }
                0
            }
            TreeView::Snapshot(name) => tree_snapshot(conn, window, name),
            TreeView::Diff(a, b) => tree_diff(a, b, *json),
            TreeView::TabStops => tab_stops(conn, window),
        },
        Command::WaitFrame { above, timeout_ms } => {
            let first = match tree_dump(conn, &None) {
                Ok(result) => result,
                Err(code) => return code,
            };
            let target = above.unwrap_or_else(|| frame_number(&first));
            match wait_for_frame(conn, &None, target, *timeout_ms) {
                Ok(frame) => {
                    println!("{frame}");
                    0
                }
                Err(last) => {
                    eprintln!(
                        "frame still {last} after {timeout_ms} ms — input landed but \
                         nothing drew (hidden workspace?)"
                    );
                    1
                }
            }
        }
        Command::Click {
            node,
            window,
            wait_frame,
        } => {
            let before = pre_dispatch_frame(conn, window, *wait_frame);
            let code = click_with_fallback(conn, node, window);
            if code != 0 {
                return code;
            }
            wait_frame_receipt(conn, window, *wait_frame, before)
        }
        Command::Focus {
            node,
            window,
            wait_frame,
        } => {
            let before = pre_dispatch_frame(conn, window, *wait_frame);
            let code = node_action(conn, "input.focus_node", node, window, "focused");
            if code != 0 {
                return code;
            }
            wait_frame_receipt(conn, window, *wait_frame, before)
        }
        Command::Key {
            keystroke,
            window,
            wait_frame,
        } => {
            let before = pre_dispatch_frame(conn, window, *wait_frame);
            let mut params = json!({});
            if let Some(window) = window {
                params["window"] = json!(window);
            }
            // One full press.
            params["event"] = json!({"type": "key_down", "keystroke": keystroke});
            let down = match conn.call("input.dispatch", params.clone()) {
                Ok(response) => response,
                Err(code) => return code,
            };
            if down.get("result").is_none() {
                return report(&down);
            }
            params["event"] = json!({"type": "key_up", "keystroke": keystroke});
            let up = match conn.call("input.dispatch", params) {
                Ok(response) => response,
                Err(code) => return code,
            };
            if up.get("result").is_none() {
                return report(&up);
            }
            println!(
                "down {} up {}",
                compact(&down["result"]),
                compact(&up["result"])
            );
            wait_frame_receipt(conn, window, *wait_frame, before)
        }
        Command::Type {
            text,
            window,
            wait_frame,
        } => {
            let before = pre_dispatch_frame(conn, window, *wait_frame);
            let mut params = json!({});
            if let Some(window) = window {
                params["window"] = json!(window);
            }
            for ch in text.chars() {
                let keystroke = cli::keystroke_for_char(ch);
                params["event"] = json!({"type": "key_down", "keystroke": keystroke});
                let down = match conn.call("input.dispatch", params.clone()) {
                    Ok(response) => response,
                    Err(code) => return code,
                };
                if down.get("result").is_none() {
                    return report(&down);
                }
                params["event"] = json!({"type": "key_up", "keystroke": keystroke});
                let up = match conn.call("input.dispatch", params.clone()) {
                    Ok(response) => response,
                    Err(code) => return code,
                };
                if up.get("result").is_none() {
                    return report(&up);
                }
            }
            println!("typed {} chars", text.chars().count());
            wait_frame_receipt(conn, window, *wait_frame, before)
        }
        Command::Mouse {
            action,
            x,
            y,
            dy,
            wait_frame,
        } => {
            let before = pre_dispatch_frame(conn, &None, *wait_frame);
            let event = match action {
                MouseAction::Move => json!({"type": "mouse_move", "x": x, "y": y}),
                MouseAction::Down | MouseAction::Up => json!({
                    "type": if action == &MouseAction::Down { "mouse_down" } else { "mouse_up" },
                    "x": x, "y": y,
                }),
                MouseAction::Scroll => {
                    json!({"type": "scroll_wheel", "x": x, "y": y, "dx": 0.0, "dy": dy})
                }
            };
            let response = match conn.call("input.dispatch", json!({"event": event})) {
                Ok(response) => response,
                Err(code) => return code,
            };
            if response.get("result").is_none() {
                return report(&response);
            }
            println!("{}", compact(&response["result"]));
            wait_frame_receipt(conn, &None, *wait_frame, before)
        }
        Command::Logs { follow } => logs(conn, *follow),
        Command::Shell => unreachable!("shell handled in run()"),
        Command::Session(_) => unreachable!("session verbs handled before connecting"),
    }
}

fn node_action(conn: &mut Conn, method: &str, node: &str, window: &Option<u64>, verb: &str) -> i32 {
    let mut params = json!({"node": node});
    if let Some(window) = window {
        params["window"] = json!(window);
    }
    let response = match conn.call(method, params) {
        Ok(response) => response,
        Err(code) => return code,
    };
    if response.get("result").is_none() {
        return report(&response);
    }
    println!("{verb} {node}");
    0
}

/// `tree.dump` round-trip: params in, `result` out. `Err` carries the
/// exit code (I/O failure or wire error, already reported).
fn tree_dump(conn: &mut Conn, window: &Option<u64>) -> Result<Value, i32> {
    let mut params = json!({});
    if let Some(window) = window {
        params["window"] = json!(window);
    }
    let response = conn.call("tree.dump", params)?;
    if response.get("result").is_none() {
        return Err(report(&response));
    }
    Ok(response["result"].clone())
}

/// The dump's frame counter (0 when absent — activation pending).
fn frame_number(result: &Value) -> u64 {
    result["frame"]["frame_number"].as_u64().unwrap_or(0)
}

/// Poll `tree.dump` until `frame_number` exceeds `above` (D6). The dump
/// never forces a draw — on-demand rendering is the contract, so a
/// timeout is the named hidden-workspace failure, not a flake.
fn wait_for_frame(
    conn: &mut Conn,
    window: &Option<u64>,
    above: u64,
    budget_ms: u64,
) -> Result<u64, u64> {
    let deadline = Instant::now() + Duration::from_millis(budget_ms);
    let mut last = above;
    loop {
        match tree_dump(conn, window) {
            Ok(result) => {
                let frame = frame_number(&result);
                if frame > above {
                    return Ok(frame);
                }
                last = frame;
            }
            // A failing dump mid-wait (app gone) is a timeout with the
            // last-seen number — the diagnostic stays honest.
            Err(_) => return Err(last),
        }
        if Instant::now() + Duration::from_millis(100) >= deadline {
            return Err(last);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Frame number before a `--wait-frame` dispatch. `None` when the flag
/// isn't set (or the pre-dump failed — the dispatch still runs, the
/// receipt is simply skipped rather than fabricated).
fn pre_dispatch_frame(
    conn: &mut Conn,
    window: &Option<u64>,
    wait_frame: Option<u64>,
) -> Option<u64> {
    let _budget = wait_frame?;
    tree_dump(conn, window)
        .ok()
        .map(|result| frame_number(&result))
}

/// The `--wait-frame` receipt: `frame 41 → 57`, or the timeout as the
/// named failure (exit 1) with the last-seen number.
fn wait_frame_receipt(
    conn: &mut Conn,
    window: &Option<u64>,
    wait_frame: Option<u64>,
    before: Option<u64>,
) -> i32 {
    let (Some(budget), Some(before)) = (wait_frame, before) else {
        return 0;
    };
    match wait_for_frame(conn, window, before, budget) {
        Ok(after) => {
            println!("frame {before} → {after}");
            0
        }
        Err(last) => {
            eprintln!(
                "frame {before} → {last} — no frame advanced within {budget} ms \
                 (input landed but nothing drew)"
            );
            1
        }
    }
}

// ---- tree views (D7/D8) -----------------------------------------------------

/// Where snapshots live for this invocation: next to the resolved state
/// dir (the bound session's `snapshots/`, 0055 D1).
fn snapshots_root() -> PathBuf {
    snapshot::snapshots_root(dashboard_core::AppPaths::init().state_dir())
}

fn tree_snapshot(conn: &mut Conn, window: &Option<u64>, name: &str) -> i32 {
    if !snapshot::valid_name(name) {
        eprintln!("bad snapshot name {name:?} — want [A-Za-z0-9._-]{{1,64}}, no leading dot");
        return 2;
    }
    let result = match tree_dump(conn, window) {
        Ok(result) => result,
        Err(code) => return code,
    };
    let snap = match snapshot::Snapshot::from_dump(name, &result) {
        Ok(snap) => snap,
        Err(err) => {
            eprintln!("cannot snapshot: {err}");
            return 1;
        }
    };
    let root = snapshots_root();
    if let Err(err) = std::fs::create_dir_all(&root) {
        eprintln!("cannot create {}: {err}", root.display());
        return 1;
    }
    let path = root.join(format!("{name}.json"));
    let body = serde_json::to_string_pretty(&snap).expect("serialize snapshot");
    if let Err(err) = std::fs::write(&path, body) {
        eprintln!("cannot write {}: {err}", path.display());
        return 1;
    }
    println!(
        "snapshot {name}: {} nodes · frame {} · {}",
        snap.nodes.len(),
        snap.frame_number,
        path.display()
    );
    0
}

fn tree_diff(a: &str, b: &str, json_out: bool) -> i32 {
    for name in [a, b] {
        if !snapshot::valid_name(name) {
            eprintln!("bad snapshot name {name:?} — want [A-Za-z0-9._-]{{1,64}}, no leading dot");
            return 2;
        }
    }
    let root = snapshots_root();
    let load = |name: &str| -> Result<snapshot::Snapshot, i32> {
        let path = root.join(format!("{name}.json"));
        let body = std::fs::read_to_string(&path).map_err(|err| {
            eprintln!("cannot read {}: {err}", path.display());
            1
        })?;
        serde_json::from_str(&body).map_err(|err| {
            eprintln!("{path:?} is not a snapshot: {err}");
            1
        })
    };
    let (sa, sb) = (
        match load(a) {
            Ok(s) => s,
            Err(code) => return code,
        },
        match load(b) {
            Ok(s) => s,
            Err(code) => return code,
        },
    );
    let diff = snapshot::diff(&sa, &sb);
    if json_out {
        println!("{}", snapshot::render_json(&diff));
    } else {
        println!("{}", snapshot::render_human(&diff));
    }
    0
}

/// The focused node of a dump: `(accesskit_id, role, label)`.
fn focused_node(dump: &Value) -> Option<(String, String, Option<String>)> {
    dump["nodes"].as_object()?.values().find_map(|node| {
        if !node["focused"].as_bool().unwrap_or(false) {
            return None;
        }
        let id = node["accesskit_id"].as_str()?;
        let role = node["aria"]["role"].as_str().unwrap_or("(no role)");
        Some((
            id.to_string(),
            role.to_string(),
            node["aria"]["label"].as_str().map(str::to_string),
        ))
    })
}

/// `tree --tab-stops` (D8): walk the focus cycle with tab + wait-frame,
/// numbering stops until focus returns to the start. A focus that never
/// moves is a trap; a cycle that never closes hits the 100-stop cap.
fn tab_stops(conn: &mut Conn, window: &Option<u64>) -> i32 {
    let first = match tree_dump(conn, window) {
        Ok(result) => result,
        Err(code) => return code,
    };
    let mut start = focused_node(&first).map(|(id, _, _)| id);
    let mut previous = start.clone();
    let mut moved = false;
    let mut last_stop = None;
    let mut dump = first;
    for stop in 1..=100u32 {
        if let Err(code) = dispatch_tab(conn, window) {
            return code;
        }
        // D8's per-step settle: wait for a frame above the one we last
        // saw, then read focus from the fresh capture — without this the
        // dump can be the pre-focus frame. Advisory: a focus move with no
        // redraw still proceeds (the timeout is not a failure here).
        let _ = wait_for_frame(conn, window, frame_number(&dump), 1000);
        let next = match tree_dump(conn, window) {
            Ok(result) => result,
            Err(code) => return code,
        };
        dump = next;
        let dump = &dump;
        let Some((id, role, label)) = focused_node(dump) else {
            // Focus exists app-side but its element exposes no a11y node
            // (the docs/0049 #97 class): name the gap and keep walking —
            // the next tab moves on. A walk that never sees focus again
            // runs into the 100-stop cap, which names the last stop.
            println!("{stop}. (focus on an element without an a11y node)");
            previous = None;
            moved = true;
            continue;
        };
        let label_text = label
            .as_ref()
            .map(|label| format!(" {label:?}"))
            .unwrap_or_default();
        println!("{stop}. #{id} {role}{label_text}");
        last_stop = Some((id.clone(), role));
        if let Some(start) = &start {
            if id == *start && moved {
                println!("cycle closed after {stop} stops");
                return 0;
            }
        }
        if previous.as_deref() == Some(id.as_str()) {
            eprintln!("focus did not advance past #{id} after tab — focus trap");
            return 1;
        }
        // No starting focus: the first focused node becomes the anchor.
        if start.is_none() {
            start = Some(id.clone());
        }
        previous = Some(id);
        moved = true;
    }
    let (id, role) = last_stop.expect("at least one stop");
    eprintln!("cycle did not close after 100 stops — last: #{id} {role}");
    1
}

/// One full tab press via `input.dispatch`.
fn dispatch_tab(conn: &mut Conn, window: &Option<u64>) -> Result<(), i32> {
    let mut params = json!({});
    if let Some(window) = window {
        params["window"] = json!(window);
    }
    for event in ["key_down", "key_up"] {
        params["event"] = json!({"type": event, "keystroke": "tab"});
        let response = conn.call("input.dispatch", params.clone())?;
        if response.get("result").is_none() {
            return Err(report(&response));
        }
    }
    Ok(())
}

// ---- click fallback (D9) ----------------------------------------------------

/// `click` with the stale-bus fallback: on 4005 UNKNOWN_NODE / 4006
/// ACTION_UNAVAILABLE, aim at the node's merged bounds center and
/// dispatch mouse down/up there — announced, so receipts stay honest.
fn click_with_fallback(conn: &mut Conn, node: &str, window: &Option<u64>) -> i32 {
    let mut params = json!({"node": node});
    if let Some(window) = window {
        params["window"] = json!(window);
    }
    let response = match conn.call("input.click_node", params) {
        Ok(response) => response,
        Err(code) => return code,
    };
    if response.get("result").is_some() {
        println!("clicked {node}");
        return 0;
    }
    let code = response["error"]["code"].as_i64().unwrap_or(0);
    if code != error_code::UNKNOWN_NODE && code != error_code::ACTION_UNAVAILABLE {
        return report(&response);
    }
    let message = response["error"]["message"].as_str().unwrap_or("?");
    let dump = match tree_dump(conn, window) {
        Ok(result) => result,
        Err(exit) => return exit,
    };
    let Some((x, y)) = node_center(&dump, node) else {
        eprintln!("error {code}: {message} — and node {node} has no bounds to fall back to");
        return 1;
    };
    for event in ["mouse_down", "mouse_up"] {
        let click = json!({"event": {"type": event, "x": x, "y": y}});
        let response = match conn.call("input.dispatch", click) {
            Ok(response) => response,
            Err(code) => return code,
        };
        if response.get("result").is_none() {
            return report(&response);
        }
    }
    println!("coordinate fallback: click {node} at ({x}, {y})");
    0
}

/// Center of a node's merged bounds, by `accesskit_id`.
fn node_center(dump: &Value, node: &str) -> Option<(f32, f32)> {
    let found = dump["nodes"]
        .as_object()?
        .values()
        .find(|candidate| candidate["accesskit_id"].as_str() == Some(node))?;
    let bounds = found.get("bounds").filter(|b| !b.is_null())?;
    let (x, y, w, h) = (
        bounds["x"].as_f64()? as f32,
        bounds["y"].as_f64()? as f32,
        bounds["w"].as_f64()? as f32,
        bounds["h"].as_f64()? as f32,
    );
    Some((x + w / 2.0, y + h / 2.0))
}

/// `logs [-f]`: drain the ring from cursor 0; with `-f` keep polling.
fn logs(conn: &mut Conn, follow: bool) -> i32 {
    let mut since = 0u64;
    loop {
        let response = match conn.call("app.logs", json!({"since": since})) {
            Ok(response) => response,
            Err(code) => return code,
        };
        let Some(result) = response.get("result") else {
            return report(&response);
        };
        for line in result["lines"].as_array().unwrap_or(&Vec::new()) {
            println!("{} {} {}", line["level"], line["target"], line["message"]);
        }
        since = result["next_cursor"].as_u64().unwrap_or(since);
        if !follow {
            let got = result["lines"].as_array().map(Vec::len).unwrap_or(0);
            if got == 0 {
                return 0;
            }
            // Keep draining batches until an empty one.
            continue;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

/// The default `tree` render: an indented outline, one node per line —
/// `#<accesskit_id> role "label" (x,y w×h)` with focus markers.
fn render_tree(result: &Value) {
    if let Some(frame) = result.get("frame") {
        println!("frame {}", compact(frame));
    }
    if let Some(merge) = result.get("a11y_merge") {
        println!("a11y_merge {}", compact(merge));
    }
    let nodes = &result["nodes"];
    if let Some(root) = result["root"].as_str() {
        render_node(nodes, root, 0);
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(result).expect("serialize result")
        );
    }
}

fn render_node(nodes: &Value, key: &str, depth: usize) {
    let Some(node) = nodes.get(key) else {
        return;
    };
    let indent = "  ".repeat(depth);
    let id = node["accesskit_id"].as_str().unwrap_or("?");
    let aria = &node["aria"];
    let role = aria["role"].as_str().unwrap_or("(no role)");
    let label = aria["label"].as_str();
    let mut line = format!("{indent}#{id} {role}");
    if let Some(label) = label {
        line.push_str(&format!(" {label:?}"));
    }
    if let Some(bounds) = node.get("bounds").filter(|b| !b.is_null()) {
        line.push_str(&format!(
            " ({},{}) {}×{}",
            bounds["x"], bounds["y"], bounds["w"], bounds["h"]
        ));
    }
    if node["focused"].as_bool().unwrap_or(false) {
        line.push_str(" *focused*");
    } else if node["focusable"].as_bool().unwrap_or(false) {
        line.push_str(" focusable");
    }
    println!("{line}");
    if let Some(children) = node["children"].as_array() {
        for child in children {
            if let Some(key) = child.as_str() {
                render_node(nodes, key, depth + 1);
            }
        }
    }
}

fn compact(value: &Value) -> String {
    serde_json::to_string(value).expect("serialize value")
}

/// The `shell` REPL: read a line, split it, run it through the same
/// parser + dispatcher. `quit`/`exit`/EOF end it.
fn shell(conn: &mut Conn) -> i32 {
    println!("{USAGE}");
    let stdin = std::io::stdin();
    let mut line = String::new();
    loop {
        print!("debug> ");
        if std::io::stdout().flush().is_err() {
            return 1;
        }
        line.clear();
        match stdin.read_line(&mut line) {
            Ok(0) | Err(_) => return 0,
            Ok(_) => {}
        }
        let words = cli::split_line(line.trim());
        match words.first().map(String::as_str) {
            None => continue,
            Some("quit") | Some("exit") => return 0,
            _ => {}
        }
        match cli::parse_args(&words) {
            Ok(Command::Shell) => println!("already in the shell"),
            Ok(command) => {
                run_command(conn, &command);
            }
            Err(usage) => eprintln!("{usage}"),
        }
    }
}
