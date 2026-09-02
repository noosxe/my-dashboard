//! `dashboard-debug` CLI (docs/0010 D9): the human/agent front end for
//! the debug bridge. Reads the run token from the app's runtime dir,
//! speaks the NDJSON protocol over the Unix socket, prints results.
//!
//! Verbs and grammar live in [`dashboard_debug::cli`] (pure, tested);
//! this file is only I/O: connect, auth, request/response, render.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use dashboard_debug::cli::{self, Command, MouseAction, USAGE};
use serde_json::{Value, json};

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match cli::parse_args(&args) {
        Ok(command) => command,
        Err(usage) => {
            eprintln!("{usage}");
            return 2;
        }
    };

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

    match command {
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
        Command::Tree { json, window } => {
            let mut params = json!({});
            if let Some(window) = window {
                params["window"] = json!(window);
            }
            let response = match conn.call("tree.dump", params) {
                Ok(response) => response,
                Err(code) => return code,
            };
            if response.get("result").is_none() {
                return report(&response);
            }
            if *json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&response["result"]).expect("serialize result")
                );
            } else {
                render_tree(&response["result"]);
            }
            0
        }
        Command::Click { node, window } => {
            node_action(conn, "input.click_node", node, window, "clicked")
        }
        Command::Focus { node, window } => {
            node_action(conn, "input.focus_node", node, window, "focused")
        }
        Command::Key { keystroke, window } => {
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
            0
        }
        Command::Type { text, window } => {
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
            0
        }
        Command::Mouse { action, x, y, dy } => {
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
            0
        }
        Command::Logs { follow } => logs(conn, *follow),
        Command::Shell => unreachable!("shell handled in run()"),
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
