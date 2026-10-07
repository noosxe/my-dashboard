//! The bridge's client half — what `dashboard-agent-mcp` (and its
//! `--call` mode) speaks. Connects to `agent.sock`, reads the per-run
//! token, performs the first-line `auth` handshake (docs/0068 D2), then
//! issues one op per [`BridgeClient::call`]. The server side of this
//! wire is [`crate::server`]; the shapes are [`crate::protocol`].

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::protocol::{self, Request, Response, RpcError};

/// Ops are quick reads; ten seconds covers a busy job queue without
/// wedging a CLI invocation forever.
const IO_TIMEOUT: Duration = Duration::from_secs(10);

const SOCKET_NAME: &str = "agent.sock";
const TOKEN_NAME: &str = "agent.token";

/// A lazily-reconnecting bridge connection. `call` attempts a fresh
/// handshake after any transport failure — the app may have restarted
/// (new token, new socket inode) between MCP tool invocations.
#[derive(Debug)]
pub struct BridgeClient {
    runtime_dir: PathBuf,
    conn: Option<Connection>,
    next_id: u64,
}

#[derive(Debug)]
struct Connection {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl BridgeClient {
    /// Resolve `agent.sock`/`agent.token` under `runtime_dir` (same
    /// layout the server binds: [`crate::server`]) and perform the auth
    /// handshake. Transport failures surface as `NOT_CONNECTED` — from
    /// the MCP tool's point of view "bridge unreachable" is one thing,
    /// whatever the syscall said.
    pub fn connect(runtime_dir: &Path) -> Result<Self, RpcError> {
        let mut client = Self {
            runtime_dir: runtime_dir.to_path_buf(),
            conn: None,
            next_id: 1,
        };
        client.open()?;
        Ok(client)
    }

    fn open(&mut self) -> Result<(), RpcError> {
        let unreachable = |what: &str, err: String| {
            RpcError::new(
                protocol::error_code::NOT_CONNECTED,
                format!("cannot reach the agent bridge ({what}): {err}"),
            )
        };

        let token = std::fs::read_to_string(self.runtime_dir.join(TOKEN_NAME))
            .map_err(|err| unreachable("token file", err.to_string()))?
            .trim()
            .to_owned();
        let stream = UnixStream::connect(self.runtime_dir.join(SOCKET_NAME))
            .map_err(|err| unreachable("socket", err.to_string()))?;
        let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
        let _ = stream.set_write_timeout(Some(IO_TIMEOUT));

        let mut conn = Connection {
            reader: BufReader::new(
                stream
                    .try_clone()
                    .map_err(|err| unreachable("socket", err.to_string()))?,
            ),
            writer: stream,
        };

        // First line is auth; the server answers with id 0 (§6). A
        // token mismatch arrives as an `AUTH` error — surface it
        // verbatim so humans see "wrong token", not "broken pipe".
        let auth = Request {
            v: None,
            id: None,
            op: protocol::OP_AUTH.to_owned(),
            params: serde_json::json!({ "token": token }),
        };
        conn.send(&auth).map_err(|err| unreachable("send", err))?;
        let reply = conn.recv().map_err(|err| unreachable("handshake", err))?;
        if !reply.ok {
            return Err(reply.error.unwrap_or_else(|| {
                RpcError::new(
                    protocol::error_code::AUTH,
                    "bridge rejected the auth handshake",
                )
            }));
        }

        self.conn = Some(conn);
        Ok(())
    }

    /// One op, one reply. `id`s count per client; the server echoes
    /// them verbatim (§6), so matching is exact.
    pub fn call(
        &mut self,
        op: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, RpcError> {
        if self.conn.is_none() {
            self.open()?;
        }
        let id = self.next_id;
        self.next_id += 1;
        let request = Request {
            v: Some(protocol::PROTOCOL_VERSION),
            id: Some(id),
            op: op.to_owned(),
            params: params.clone(),
        };

        let reply = {
            let conn = self.conn.as_mut().ok_or_else(|| {
                RpcError::new(protocol::error_code::NOT_CONNECTED, "bridge unreachable")
            })?;
            match conn.send(&request).and_then(|()| conn.recv()) {
                Ok(reply) if reply.id == id => Ok(reply),
                Ok(mismatched) => {
                    self.conn = None;
                    Err(RpcError::new(
                        protocol::error_code::INTERNAL,
                        format!("bridge replied with mismatched id {}", mismatched.id),
                    ))
                }
                Err(message) => {
                    // The connection is suspect after any transport
                    // error; drop it so the next call re-handshakes.
                    self.conn = None;
                    Err(RpcError::new(
                        protocol::error_code::NOT_CONNECTED,
                        format!("bridge call failed: {message}"),
                    ))
                }
            }
        }?;

        if reply.ok {
            Ok(reply.result.unwrap_or(serde_json::Value::Null))
        } else {
            Err(reply.error.ok_or_else(|| {
                RpcError::new(
                    protocol::error_code::INTERNAL,
                    "bridge error without payload",
                )
            })?)
        }
    }
}

/// A [`BridgeClient`] that tolerates starting before the app does:
/// the socket connect is deferred to the first call and retried after
/// any failure — exactly what a long-lived MCP session needs when the
/// dashboard restarts under it.
pub struct LazyBridge {
    runtime_dir: PathBuf,
    client: Option<BridgeClient>,
}

impl LazyBridge {
    pub fn new(runtime_dir: PathBuf) -> Self {
        Self {
            runtime_dir,
            client: None,
        }
    }
}

impl crate::mcp::Bridge for LazyBridge {
    fn call(
        &mut self,
        op: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, RpcError> {
        if self.client.is_none() {
            self.client = Some(BridgeClient::connect(&self.runtime_dir)?);
        }
        match self.client.as_mut() {
            Some(client) => client.call(op, params),
            None => Err(RpcError::new(
                protocol::error_code::NOT_CONNECTED,
                "no bridge connection",
            )),
        }
    }
}

impl Connection {
    fn send(&mut self, request: &Request) -> Result<(), String> {
        let line = serde_json::to_string(request).map_err(|err| err.to_string())?;
        self.writer
            .write_all(line.as_bytes())
            .and_then(|()| self.writer.write_all(b"\n"))
            .and_then(|()| self.writer.flush())
            .map_err(|err| err.to_string())
    }

    fn recv(&mut self) -> Result<Response, String> {
        let mut line = String::new();
        let read = self
            .reader
            .read_line(&mut line)
            .map_err(|err| err.to_string())?;
        if read == 0 {
            return Err("bridge closed the connection".to_owned());
        }
        serde_json::from_str(&line).map_err(|err| format!("malformed reply: {err}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::error_code;

    /// Bind a real endpoint in a scratch dir and serve one connection
    /// that echoes the op back. Exercises the client against the actual
    /// server framing — auth line first, then `v`/`id`/op replies.
    fn echo_server(dir: &Path) {
        let endpoint = crate::server::bind_endpoint(dir).expect("bind");
        let listener = endpoint.listener;
        let token = endpoint.token;
        std::thread::spawn(move || {
            if let Ok(stream) = listener.incoming().next().unwrap() {
                crate::server::serve_connection(stream, &token, |request| {
                    if request.op == protocol::OP_AUTH {
                        Ok(serde_json::json!({}))
                    } else {
                        Ok(serde_json::json!({ "echo": request.op }))
                    }
                });
            }
        });
    }

    // Scratch dirs must stay short: Unix socket paths cap at SUN_LEN
    // (104 bytes) — the same constraint the server tests document.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("da-mcp-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn handshake_then_roundtrip_against_a_live_server() {
        let dir = scratch("roundtrip");
        echo_server(&dir);
        let mut client = BridgeClient::connect(&dir).expect("connect");
        let result = client.call("list_repos", &serde_json::json!({})).unwrap();
        assert_eq!(result["echo"], serde_json::json!("list_repos"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn wrong_token_surfaces_as_auth_not_transport() {
        let dir = scratch("badtoken");
        echo_server(&dir);
        // The server compares against its in-memory token; overwriting
        // the file the client reads makes the handshake a token
        // mismatch, which must surface as `AUTH`, not a transport error.
        std::fs::write(dir.join(TOKEN_NAME), "definitely-not-it\n").unwrap();
        let error = BridgeClient::connect(&dir).unwrap_err();
        assert_eq!(error.code, error_code::AUTH);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_socket_is_not_connected() {
        let dir = scratch("absent");
        // Token exists, socket doesn't: the transport half fails.
        std::fs::write(dir.join(TOKEN_NAME), "t\n").unwrap();
        let error = BridgeClient::connect(&dir).unwrap_err();
        assert_eq!(error.code, error_code::NOT_CONNECTED);
        std::fs::remove_dir_all(&dir).ok();
    }
}
