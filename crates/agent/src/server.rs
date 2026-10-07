//! Unix-socket server (docs/0068 D1/D2/D9): token-gated NDJSON endpoint
//! plus the socket-thread → main-thread job queue — the docs/0010 D2/D3
//! pattern, read-only op surface.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::StreamExt;
use gpui::App;
use serde_json::Value;

use crate::jobs::Job;
use crate::ops::OpsRegistry;
use crate::protocol::{
    AuthParams, OP_AUTH, PROTOCOL_VERSION, Request, Response, RpcError, error_code, parse,
    request_id,
};

const SOCKET_NAME: &str = "agent.sock";
const TOKEN_NAME: &str = "agent.token";
const LOG_TARGET: &str = "dashboard_agent";

/// Everything `start` needs from the filesystem, bound and ready. Split
/// out of `start` so the permission/token contract is testable without
/// a gpui `App`.
struct Endpoint {
    listener: UnixListener,
    token: Arc<String>,
    socket_path: PathBuf,
}

/// Start the bridge. See `crate::start`.
pub fn start(app: &mut App) -> Option<PathBuf> {
    // Job handlers read this global; initialize before anything can fail
    // so the invariant "server up ⇒ registry present" holds.
    app.default_global::<OpsRegistry>();

    let runtime_dir = dashboard_core::AppPaths::init().runtime_dir().to_path_buf();
    let endpoint = bind_endpoint(&runtime_dir)?;
    let socket_path = endpoint.socket_path;

    // Job queue: socket threads push boxed main-thread closures; the
    // executor below runs them on the foreground executor (D9).
    let (job_tx, mut job_rx) = futures::channel::mpsc::unbounded::<Job>();
    app.spawn(async move |cx| {
        while let Some(job) = job_rx.next().await {
            cx.update(|app| job(app));
        }
    })
    .detach();

    let Endpoint {
        listener,
        token,
        socket_path: _,
    } = endpoint;
    std::thread::Builder::new()
        .name("agent-bridge-accept".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let job_tx = job_tx.clone();
                        let token = Arc::clone(&token);
                        let _ = std::thread::Builder::new()
                            .name("agent-bridge-conn".into())
                            .spawn(move || {
                                serve_connection(stream, &token, |request| {
                                    crate::jobs::dispatch(&job_tx, request)
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

/// Bind `agent.sock` in `runtime_dir` and write the per-run token:
/// runtime dir 0700, socket 0600, token file 0600 — the docs/0068 D2
/// trust model, identical to the debug bridge's (docs/0010 D2).
fn bind_endpoint(runtime_dir: &Path) -> Option<Endpoint> {
    let socket_path = runtime_dir.join(SOCKET_NAME);
    let token_path = runtime_dir.join(TOKEN_NAME);

    // The socket lives in a per-user runtime dir; 0700 on our subdir even
    // when a fallback root picked a laxer default.
    if let Err(err) = std::fs::create_dir_all(runtime_dir) {
        log::warn!(target: LOG_TARGET, "runtime dir {} unavailable: {err}", runtime_dir.display());
        return None;
    }
    let _ = std::fs::set_permissions(runtime_dir, std::fs::Permissions::from_mode(0o700));

    // Token: random per run, defense-in-depth behind the same-user socket
    // (D2). The token file doubles as the run marker for the #469 CLI.
    let token = generate_token()?;
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

    Some(Endpoint {
        listener,
        token,
        socket_path,
    })
}

/// Connection lifecycle (D2): first line MUST be `auth` with the run
/// token; after that, one request/response per NDJSON line. Returns when
/// the client disconnects or auth fails.
fn serve_connection<D>(stream: UnixStream, expected_token: &str, dispatch: D)
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
        .op
        .eq(OP_AUTH)
        .then(|| parse::<AuthParams>(&request.params))
        .flatten()
        .map(|params| params.token);
    // Plain compare: the socket is same-user to begin with; the token is
    // defense-in-depth, not a secret channel.
    if token.as_deref() != Some(expected_token) {
        let _ = writer.write_all(
            &Response::err(request_id(&request), RpcError::new(error_code::AUTH, "invalid token"))
                .to_line()
                .into_bytes(),
        );
        return;
    }
    let _ = writer.write_all(&Response::ok(0, Value::Bool(true)).to_line().into_bytes());

    // --- request loop ---
    while let Some(line) = read_line(&mut reader) {
        if line.trim().is_empty() {
            continue;
        }
        let id = parse_request(&line).map(|request| request_id(&request)).unwrap_or(0);
        let response = match parse_request(&line) {
            None => Response::err(0, RpcError::new(error_code::PARSE, "malformed JSON line")),
            Some(request) if request.op == OP_AUTH => Response::err(
                request_id(&request),
                RpcError::new(error_code::BAD_PARAMS, "already authenticated"),
            ),
            Some(request) if matches!(request.v, Some(v) if v != PROTOCOL_VERSION) => {
                Response::err(
                    request_id(&request),
                    RpcError::new(
                        error_code::BAD_PARAMS,
                        format!("unsupported protocol version; server speaks v{PROTOCOL_VERSION}"),
                    ),
                )
            }
            Some(request) => match dispatch(request) {
                Ok(value) => Response::ok(id, value),
                Err(error) => Response::err(id, error),
            },
        };
        if writer.write_all(response.to_line().as_bytes()).is_err() {
            break;
        }
    }
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

fn write_private_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, contents)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};

    fn mode_of(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

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

    #[test]
    fn token_file_is_owner_only() {
        let dir = temp_dir("token-mode");
        let path = dir.join("agent.token");
        write_private_file(&path, b"secret").unwrap();
        assert_eq!(mode_of(&path), 0o600);
        std::fs::remove_dir_all(&dir).ok();
    }

    // -- endpoint binding (the D2 permission contract) ----------------------

    #[test]
    fn bind_endpoint_applies_runtime_dir_socket_and_token_permissions() {
        let dir = temp_dir("bind-perms");
        let endpoint = bind_endpoint(&dir).unwrap();

        assert_eq!(mode_of(&dir), 0o700);
        assert_eq!(mode_of(&dir.join(SOCKET_NAME)), 0o600);
        assert_eq!(mode_of(&dir.join(TOKEN_NAME)), 0o600);
        assert_eq!(endpoint.token.len(), 32);
        assert_eq!(endpoint.socket_path, dir.join(SOCKET_NAME));

        drop(endpoint);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bind_endpoint_removes_a_stale_socket() {
        let dir = temp_dir("bind-stale");
        // First bind leaves a live listener at the path; a second bind must
        // still succeed (the stale socket file is removed, not reused).
        let first = bind_endpoint(&dir).unwrap();
        let second = bind_endpoint(&dir).unwrap();
        drop(first);
        drop(second);
        std::fs::remove_dir_all(&dir).ok();
    }

    // -- connection handling over a loopback socket -------------------------

    /// Bind a listener in a fresh temp dir; the socket path is the return.
    fn loopback() -> (UnixListener, PathBuf) {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = temp_dir(&format!(
            "loopback-{}",
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let path = dir.join("sock");
        let listener = UnixListener::bind(&path).unwrap();
        (listener, dir)
    }

    fn temp_dir(name: &str) -> PathBuf {
        // Keep the total path short: Unix socket paths must fit SUN_LEN
        // (104 bytes on macOS), and the test TMPDIR prefix is already long.
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "da-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Serve exactly one connection with a canned dispatch that mirrors
    /// `jobs::dispatch`'s resolve semantics: the one op a provider serves
    /// echoes, anything else is UNKNOWN_OP. Returns the serving thread.
    fn serve_one(
        listener: UnixListener,
        token: &'static str,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            if let Ok(stream) = listener.incoming().next().unwrap() {
                serve_connection(stream, token, |request| {
                    match request.op.as_str() {
                        "app_status" => Ok(serde_json::json!({"echo": request.op})),
                        op => Err(RpcError::new(
                            error_code::UNKNOWN_OP,
                            format!("no op named {op:?}"),
                        )),
                    }
                });
            }
        })
    }

    fn client(path: &Path) -> (UnixStream, BufReader<UnixStream>) {
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
        let server = serve_one(listener, "secret-token");

        let (mut writer, mut reader) = client(&path);
        let response = roundtrip(
            &mut writer,
            &mut reader,
            r#"{"op":"auth","params":{"token":"secret-token"}}"#,
        );
        assert_eq!(response.id, 0);
        assert_eq!(response.result, Some(serde_json::json!(true)));

        let response = roundtrip(
            &mut writer,
            &mut reader,
            r#"{"v":1,"id":5,"op":"app_status","params":{}}"#,
        );
        assert_eq!(response.id, 5);
        assert!(response.ok);
        assert_eq!(response.result.unwrap()["echo"], "app_status");

        drop(writer);
        drop(reader);
        server.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn wrong_token_gets_auth_error_and_close() {
        let (listener, dir) = loopback();
        let path = dir.join("sock");
        let server = serve_one(listener, "secret-token");

        let (mut writer, mut reader) = client(&path);
        let response = roundtrip(
            &mut writer,
            &mut reader,
            r#"{"op":"auth","params":{"token":"wrong"}}"#,
        );
        assert_eq!(response.error.unwrap().code, error_code::AUTH);

        // Server closed the connection: EOF on the next read.
        let mut trailing = String::new();
        assert_eq!(reader.read_line(&mut trailing).unwrap(), 0);

        drop(writer);
        drop(reader);
        server.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unparseable_auth_line_yields_parse_error_and_close() {
        let (listener, dir) = loopback();
        let path = dir.join("sock");
        let server = serve_one(listener, "t");

        let (mut writer, mut reader) = client(&path);
        let response = roundtrip(&mut writer, &mut reader, "this is not json");
        assert_eq!(response.error.unwrap().code, error_code::PARSE);

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
        let server = serve_one(listener, "t");

        let (mut writer, mut reader) = client(&path);
        roundtrip(
            &mut writer,
            &mut reader,
            r#"{"op":"auth","params":{"token":"t"}}"#,
        );
        let response = roundtrip(&mut writer, &mut reader, "still not json");
        assert_eq!(response.error.unwrap().code, error_code::PARSE);

        drop(writer);
        drop(reader);
        server.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unknown_op_is_rejected() {
        let (listener, dir) = loopback();
        let path = dir.join("sock");
        let server = serve_one(listener, "t");

        let (mut writer, mut reader) = client(&path);
        roundtrip(
            &mut writer,
            &mut reader,
            r#"{"op":"auth","params":{"token":"t"}}"#,
        );
        let response = roundtrip(
            &mut writer,
            &mut reader,
            r#"{"v":1,"id":9,"op":"dom.snapshot","params":{}}"#,
        );
        assert_eq!(response.error.unwrap().code, error_code::UNKNOWN_OP);

        drop(writer);
        drop(reader);
        server.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reauth_is_rejected() {
        let (listener, dir) = loopback();
        let path = dir.join("sock");
        let server = serve_one(listener, "t");

        let (mut writer, mut reader) = client(&path);
        roundtrip(
            &mut writer,
            &mut reader,
            r#"{"op":"auth","params":{"token":"t"}}"#,
        );
        let response = roundtrip(
            &mut writer,
            &mut reader,
            r#"{"op":"auth","params":{"token":"t"}}"#,
        );
        assert_eq!(response.error.unwrap().code, error_code::BAD_PARAMS);

        drop(writer);
        drop(reader);
        server.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unsupported_protocol_version_is_rejected() {
        let (listener, dir) = loopback();
        let path = dir.join("sock");
        let server = serve_one(listener, "t");

        let (mut writer, mut reader) = client(&path);
        roundtrip(
            &mut writer,
            &mut reader,
            r#"{"op":"auth","params":{"token":"t"}}"#,
        );
        let response = roundtrip(
            &mut writer,
            &mut reader,
            r#"{"v":2,"id":1,"op":"app_status","params":{}}"#,
        );
        let error = response.error.unwrap();
        assert_eq!(error.code, error_code::BAD_PARAMS);
        assert!(error.message.contains("v1"));

        drop(writer);
        drop(reader);
        server.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pipelined_requests_keep_order() {
        let (listener, dir) = loopback();
        let path = dir.join("sock");
        let server = serve_one(listener, "t");

        let (mut writer, mut reader) = client(&path);
        roundtrip(
            &mut writer,
            &mut reader,
            r#"{"op":"auth","params":{"token":"t"}}"#,
        );

        // Two requests written before reading anything.
        writer
            .write_all(
                concat!(
                    r#"{"v":1,"id":11,"op":"app_status","params":{}}"#,
                    "\n",
                    r#"{"v":1,"id":12,"op":"app_status","params":{}}"#,
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
