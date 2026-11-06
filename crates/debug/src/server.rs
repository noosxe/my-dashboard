//! Unix-socket server (docs/0010 D2/D3): token-gated NDJSON endpoint plus
//! the socket-thread → main-thread job queue.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt;
use gpui::App;
use serde_json::Value;

use crate::jobs::{Job, WindowRegistry};
use crate::logs::RingLog;
use crate::protocol::{
    AuthParams, METHOD_APP_LOGS, METHOD_APP_WRITES, METHOD_AUTH, Request, Response, RpcError,
    error_code,
};

const SOCKET_NAME: &str = "debug.sock";
const TOKEN_NAME: &str = "debug.token";
const LOG_TARGET: &str = "dashboard_debug";

/// Start the bridge. See `crate::start`.
pub fn start(app: &mut App, ring: crate::logs::RingLog) -> Option<PathBuf> {
    // Job handlers read this global; initialize before anything can fail
    // so the invariant "server up ⇒ registry present" holds.
    app.default_global::<WindowRegistry>();

    let runtime_dir = dashboard_core::AppPaths::init().runtime_dir().to_path_buf();
    let socket_path = runtime_dir.join(SOCKET_NAME);
    let token_path = runtime_dir.join(TOKEN_NAME);

    // The socket lives in a per-user runtime dir; 0700 on our subdir even
    // when a fallback root picked a laxer default.
    if let Err(err) = std::fs::create_dir_all(&runtime_dir) {
        log::warn!(target: LOG_TARGET, "runtime dir {} unavailable: {err}", runtime_dir.display());
        return None;
    }
    let _ = std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700));

    // Token: random per run, defense-in-depth behind the same-user socket
    // (D2). The token file doubles as the run marker for the CLI (#47).
    let Some(token) = generate_token() else {
        log::warn!(target: LOG_TARGET, "cannot read /dev/urandom — debug bridge disabled");
        return None;
    };
    let token = Arc::new(token);
    if let Err(err) = write_private_file(&token_path, token.as_bytes()) {
        log::warn!(target: LOG_TARGET, "cannot write {}: {err}", token_path.display());
        return None;
    }

    // A socket left behind by a crashed run is inert (nothing accepts);
    // remove it so bind succeeds.
    let _ = std::fs::remove_file(&socket_path);
    let listener = match UnixListener::bind(&socket_path) {
        Ok(listener) => listener,
        Err(err) => {
            log::warn!(target: LOG_TARGET, "cannot bind {}: {err}", socket_path.display());
            return None;
        }
    };
    let _ = std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600));

    // Job queue: socket threads push boxed main-thread closures; the
    // executor below runs them on the foreground executor (D3).
    let (job_tx, mut job_rx) = futures::channel::mpsc::unbounded::<Job>();
    app.spawn(async move |cx| {
        while let Some(job) = job_rx.next().await {
            cx.update(|app| job(app));
        }
    })
    .detach();

    std::thread::Builder::new()
        .name("debug-bridge-accept".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let job_tx = job_tx.clone();
                        let token = Arc::clone(&token);
                        let ring = ring.clone();
                        let _ = std::thread::Builder::new()
                            .name("debug-bridge-conn".into())
                            .spawn(move || {
                                serve_connection(stream, &token, &ring, |request| {
                                    crate::handlers::dispatch(&job_tx, request)
                                });
                            });
                    }
                    Err(err) => {
                        log::warn!(target: LOG_TARGET, "accept failed: {err}");
                    }
                }
            }
        })
        .ok();

    log::info!(target: LOG_TARGET, "listening on {}", socket_path.display());
    Some(socket_path)
}

/// Connection lifecycle (D2): first line MUST be `auth` with the run
/// token; after that, one request/response per NDJSON line. Returns when
/// the client disconnects or auth fails.
fn serve_connection<D>(stream: UnixStream, expected_token: &str, ring: &RingLog, dispatch: D)
where
    D: Fn(Request) -> Result<Value, RpcError>,
{
    let Ok(reader) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(reader);
    let mut writer = stream;

    // --- auth handshake ---
    let Some(first) = read_line(&mut reader) else {
        return;
    };
    let request = match parse_request(&first) {
        Some(request) => request,
        None => {
            let _ = writer.write_all(
                &Response::err(
                    0,
                    RpcError::new(error_code::PARSE, "first line must be auth"),
                )
                .to_line()
                .into_bytes(),
            );
            return;
        }
    };
    let token = request
        .method
        .eq(METHOD_AUTH)
        .then(|| crate::protocol::parse::<AuthParams>(&request.params))
        .flatten()
        .map(|params| params.token);
    // Plain compare: the socket is same-user to begin with; the token is
    // defense-in-depth, not a secret channel.
    if token.as_deref() != Some(expected_token) {
        let _ = writer.write_all(
            &Response::err(
                request.id,
                RpcError::new(error_code::AUTH_FAILED, "invalid token"),
            )
            .to_line()
            .into_bytes(),
        );
        return;
    }
    let _ = writer.write_all(
        &Response::ok(request.id, Value::Bool(true))
            .to_line()
            .into_bytes(),
    );

    // --- request loop ---
    while let Some(line) = read_line(&mut reader) {
        if line.trim().is_empty() {
            continue;
        }
        let response = match parse_request(&line) {
            None => Response::err(0, RpcError::new(error_code::PARSE, "malformed JSON line")),
            Some(request) if request.method == METHOD_AUTH => Response::err(
                request.id,
                RpcError::new(error_code::INVALID_PARAMS, "already authenticated"),
            ),
            Some(request) if request.method == METHOD_APP_LOGS => app_logs(ring, &request),
            Some(request) if request.method == METHOD_APP_WRITES => app_writes(&request),
            Some(request) => {
                let id = request.id;
                match dispatch(request) {
                    Ok(value) => Response::ok(id, value),
                    Err(error) => Response::err(id, error),
                }
            }
        };
        if writer.write_all(&response.to_line().into_bytes()).is_err() {
            break;
        }
    }
}

/// `app.logs` (docs/0010 D8): answered inline on the socket thread from
/// the shared ring — no main-thread round-trip. Cursor semantics in
/// [`RingLog::snapshot`].
fn app_logs(ring: &RingLog, request: &Request) -> Response {
    let params = match crate::protocol::parse::<crate::protocol::LogsParams>(&request.params) {
        Some(params) => params,
        None => {
            return Response::err(
                request.id,
                RpcError::new(
                    error_code::INVALID_PARAMS,
                    "app.logs params: {since?: u64, limit?: usize}",
                ),
            );
        }
    };
    let since = params.since.unwrap_or(0);
    let limit = params.limit.unwrap_or(crate::logs::DEFAULT_LIMIT);
    let (lines, next_cursor) = ring.snapshot(since, limit);
    match serde_json::to_value(&lines) {
        Ok(value) => Response::ok(
            request.id,
            serde_json::json!({
                "lines": value,
                "next_cursor": next_cursor,
            }),
        ),
        Err(err) => Response::err(
            request.id,
            RpcError::new(
                error_code::INTERNAL,
                format!("serializing log lines: {err}"),
            ),
        ),
    }
}

/// `app.writes` (docs/0010 D12): the fixture write record, answered
/// inline from the process-wide log in `dashboard_core::write_log`.
fn app_writes(request: &Request) -> Response {
    let Some(params) = crate::protocol::parse::<crate::protocol::WritesParams>(&request.params)
    else {
        return Response::err(
            request.id,
            RpcError::new(
                error_code::INVALID_PARAMS,
                "app.writes params: {since?: u64}",
            ),
        );
    };
    let (writes, next_cursor) = dashboard_core::write_log::since(params.since.unwrap_or(0));
    Response::ok(
        request.id,
        serde_json::json!({ "writes": writes, "next_cursor": next_cursor }),
    )
}

fn read_line(reader: &mut BufReader<UnixStream>) -> Option<String> {
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line),
    }
}

fn parse_request(line: &str) -> Option<Request> {
    serde_json::from_str(line).ok()
}

fn generate_token() -> Option<String> {
    use std::io::Read;
    let mut urandom = std::fs::File::open("/dev/urandom").ok()?;
    let mut bytes = [0u8; 16];
    urandom.read_exact(&mut bytes).ok()?;
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn write_private_file(path: &PathBuf, contents: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, contents)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};

    // -- token generation ---------------------------------------------------

    #[test]
    fn token_is_32_lowercase_hex_chars() {
        let token = generate_token().unwrap();
        assert_eq!(token.len(), 32);
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[test]
    fn tokens_differ_across_runs() {
        assert_ne!(generate_token().unwrap(), generate_token().unwrap());
    }

    // -- connection handling over a loopback socket -------------------------

    /// Bind a listener in a fresh temp dir; the socket path is the return.
    fn loopback() -> (UnixListener, PathBuf) {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "dash-debug-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sock");
        let listener = UnixListener::bind(&path).unwrap();
        (listener, dir)
    }

    /// Serve exactly one connection with a canned dispatch that echoes the
    /// method back; returns a handle to the serving thread.
    fn serve_one(
        listener: UnixListener,
        token: &'static str,
        ring: RingLog,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            if let Ok(stream) = listener.incoming().next().unwrap() {
                // Mirrors the real dispatcher: implemented methods echo,
                // anything else is METHOD_NOT_FOUND (as jobs::build does).
                serve_connection(stream, token, &ring, |request| {
                    match request.method.as_str() {
                        "window.list" | "window.geometry" | "input.dispatch" => {
                            Ok(serde_json::json!({"echo": request.method, "id": request.id}))
                        }
                        method => Err(RpcError::new(
                            error_code::METHOD_NOT_FOUND,
                            format!("no handler for {method:?}"),
                        )),
                    }
                });
            }
        })
    }

    fn client(path: &std::path::Path) -> (UnixStream, BufReader<UnixStream>) {
        let stream = UnixStream::connect(path).unwrap();
        let reader = BufReader::new(stream.try_clone().unwrap());
        (stream, reader)
    }

    fn roundtrip(
        writer: &mut UnixStream,
        reader: &mut BufReader<UnixStream>,
        line: &str,
    ) -> Response {
        writer.write_all(format!("{line}\n").as_bytes()).unwrap();
        let mut response = String::new();
        reader.read_line(&mut response).unwrap();
        serde_json::from_str(response.trim()).unwrap()
    }

    #[test]
    fn auth_accept_then_dispatch_round_trip() {
        let (listener, dir) = loopback();
        let path = dir.join("sock");
        let server = serve_one(listener, "secret-token", RingLog::new());

        let (mut writer, mut reader) = client(&path);
        let response = roundtrip(
            &mut writer,
            &mut reader,
            r#"{"id":0,"method":"auth","params":{"token":"secret-token"}}"#,
        );
        assert_eq!(response.result, Some(serde_json::json!(true)));

        let response = roundtrip(
            &mut writer,
            &mut reader,
            r#"{"id":5,"method":"window.list","params":{}}"#,
        );
        assert_eq!(response.id, 5);
        assert_eq!(response.result.unwrap()["echo"], "window.list");

        drop(writer);
        drop(reader);
        server.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn wrong_token_gets_error_and_close() {
        let (listener, dir) = loopback();
        let path = dir.join("sock");
        let server = serve_one(listener, "secret-token", RingLog::new());

        let (mut writer, mut reader) = client(&path);
        let response = roundtrip(
            &mut writer,
            &mut reader,
            r#"{"id":0,"method":"auth","params":{"token":"wrong"}}"#,
        );
        assert_eq!(response.error.unwrap().code, error_code::AUTH_FAILED);

        // Server closed the connection: EOF on the next read.
        let mut trailing = String::new();
        assert_eq!(reader.read_line(&mut trailing).unwrap(), 0);

        drop(writer);
        drop(reader);
        server.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn malformed_line_after_auth_yields_parse_error() {
        let (listener, dir) = loopback();
        let path = dir.join("sock");
        let server = serve_one(listener, "t", RingLog::new());

        let (mut writer, mut reader) = client(&path);
        roundtrip(
            &mut writer,
            &mut reader,
            r#"{"id":0,"method":"auth","params":{"token":"t"}}"#,
        );
        let response = roundtrip(&mut writer, &mut reader, "this is not json");
        assert_eq!(response.error.unwrap().code, error_code::PARSE);

        drop(writer);
        drop(reader);
        server.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unknown_method_is_rejected() {
        let (listener, dir) = loopback();
        let path = dir.join("sock");
        let server = serve_one(listener, "t", RingLog::new());

        let (mut writer, mut reader) = client(&path);
        roundtrip(
            &mut writer,
            &mut reader,
            r#"{"id":0,"method":"auth","params":{"token":"t"}}"#,
        );
        let response = roundtrip(
            &mut writer,
            &mut reader,
            r#"{"id":9,"method":"dom.snapshot","params":{}}"#,
        );
        assert_eq!(response.error.unwrap().code, error_code::METHOD_NOT_FOUND);

        drop(writer);
        drop(reader);
        server.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn app_logs_serves_ring_lines_with_cursor_semantics() {
        let ring = RingLog::new();
        {
            let writer = ring.writer();
            let mut now = flexi_logger::DeferredNow::new();
            for (level, msg) in [
                (log::Level::Info, "boot"),
                (log::Level::Warn, "token rejected"),
            ] {
                let owned = msg.to_string();
                let arguments = format_args!("{owned}");
                let record = log::Record::builder()
                    .level(level)
                    .target("dashboard_module_github::service")
                    .args(arguments)
                    .build();
                writer.write(&mut now, &record).unwrap();
            }
        }

        let (listener, dir) = loopback();
        let path = dir.join("sock");
        let server = serve_one(listener, "t", ring);

        let (mut writer, mut reader) = client(&path);
        roundtrip(
            &mut writer,
            &mut reader,
            r#"{"id":0,"method":"auth","params":{"token":"t"}}"#,
        );
        let response = roundtrip(
            &mut writer,
            &mut reader,
            r#"{"id":3,"method":"app.logs","params":{}}"#,
        );
        let result = response.result.unwrap();
        assert_eq!(result["next_cursor"], serde_json::json!(2));
        let lines = result["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["cursor"], serde_json::json!(1));
        assert_eq!(lines[0]["level"], serde_json::json!("INFO"));
        assert_eq!(
            lines[0]["target"],
            serde_json::json!("dashboard_module_github::service")
        );
        assert_eq!(lines[0]["message"], serde_json::json!("boot"));

        // `since` continuation: only the warn line, cursor stays at 2.
        let response = roundtrip(
            &mut writer,
            &mut reader,
            r#"{"id":4,"method":"app.logs","params":{"since":1,"limit":1}}"#,
        );
        let result = response.result.unwrap();
        let lines = result["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["message"], serde_json::json!("token rejected"));
        assert_eq!(result["next_cursor"], serde_json::json!(2));

        // Bad params still get -32602.
        let response = roundtrip(
            &mut writer,
            &mut reader,
            r#"{"id":5,"method":"app.logs","params":{"since":"nope"}}"#,
        );
        assert_eq!(response.error.unwrap().code, error_code::INVALID_PARAMS);

        drop(writer);
        drop(reader);
        server.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pipelined_requests_keep_order() {
        let (listener, dir) = loopback();
        let path = dir.join("sock");
        let server = serve_one(listener, "t", RingLog::new());

        let (mut writer, mut reader) = client(&path);
        roundtrip(
            &mut writer,
            &mut reader,
            r#"{"id":0,"method":"auth","params":{"token":"t"}}"#,
        );

        // Two requests written before reading anything.
        writer
            .write_all(
                concat!(
                    r#"{"id":11,"method":"window.list","params":{}}"#,
                    "\n",
                    r#"{"id":12,"method":"window.geometry","params":{}}"#,
                    "\n",
                )
                .as_bytes(),
            )
            .unwrap();
        let mut first = String::new();
        reader.read_line(&mut first).unwrap();
        let mut second = String::new();
        reader.read_line(&mut second).unwrap();
        let first: Response = serde_json::from_str(first.trim()).unwrap();
        let second: Response = serde_json::from_str(second.trim()).unwrap();
        assert_eq!(first.id, 11);
        assert_eq!(second.id, 12);

        drop(writer);
        drop(reader);
        server.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }
}
