//! OAuth device flow + keychain token storage (docs/0005 §3.1, Q3, Q5).
//!
//! Flow: `start_device_flow` → show `user_code`, open `verification_uri` →
//! loop `poll_token` honoring the interval → store the token in the OS
//! keychain. The client ID is public by design (no client secret exists in
//! this flow); scopes are `repo read:org notifications`.

use serde::Deserialize;

use crate::http::{Http, HttpError, HttpResult, Verb};

/// Public application identifier (github.com/settings/developers →
/// "my-dashboard", device flow enabled). Safe to commit — same model as the
/// `gh` CLI.
pub const CLIENT_ID: &str = "Ov23lisWKGYvlnBf2OY3";

pub const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
pub const TOKEN_URL: &str = "https://github.com/login/oauth/access_token";

/// Scopes requested at authorization time (docs/0005 §3.2).
pub const SCOPES: &str = "repo read:org notifications";

/// Keychain coordinates (docs/0005 Q3). Tokens never live in files.
pub const KEYCHAIN_SERVICE: &str = "com.noosxe.my-dashboard";
pub const KEYCHAIN_ACCOUNT: &str = "github";

const DEFAULT_INTERVAL_SECS: u64 = 5;
const DEFAULT_EXPIRES_IN_SECS: u64 = 900;

/// Step-1 response: what the user sees and what we poll with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceFlowStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub interval_secs: u64,
    pub expires_in_secs: u64,
}

#[derive(Debug)]
pub enum AuthError {
    /// Transport/DNS/TLS failure.
    Network(String),
    /// Unexpected status or unparseable payload.
    Malformed(String),
}

impl From<HttpError> for AuthError {
    fn from(HttpError(m): HttpError) -> Self {
        AuthError::Network(m)
    }
}

/// One polling step (docs/0005 §3.1): caller loops, sleeping
/// `interval_secs` between calls, and restarts the whole flow on
/// `Expired`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollOutcome {
    Token(String),
    /// User hasn't approved yet — keep polling at this interval.
    Pending {
        interval_secs: u64,
    },
    /// GitHub demanded a longer interval (+5 s semantics; the response's
    /// interval wins when present).
    SlowDown {
        interval_secs: u64,
    },
    /// 15 minutes elapsed — restart the flow with a fresh code.
    Expired,
    /// User clicked cancel on the verification page.
    Denied,
    Network(String),
    /// Unexpected response — surfaced as a designed error state.
    Malformed(String),
}

// ---------------------------------------------------------------------------
// form encoding — tiny local encoder; the only values we send are the fixed
// grant type URN, the client id and GitHub-issued opaque codes.
// ---------------------------------------------------------------------------

fn form_encode(pairs: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (key, value) in pairs {
        if !out.is_empty() {
            out.push('&');
        }
        out.push_str(&percent_encode(key));
        out.push('=');
        out.push_str(&percent_encode(value));
    }
    out
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Step 1 — request device + user codes.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct StartResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default = "default_expires")]
    expires_in: u64,
    #[serde(default = "default_interval")]
    interval: u64,
}

fn default_interval() -> u64 {
    DEFAULT_INTERVAL_SECS
}

fn default_expires() -> u64 {
    DEFAULT_EXPIRES_IN_SECS
}

pub fn start_device_flow(http: &dyn Http) -> Result<DeviceFlowStart, AuthError> {
    let body = form_encode(&[("client_id", CLIENT_ID), ("scope", SCOPES)]);
    let result = http.request(Verb::Post, DEVICE_CODE_URL, Some(&body), None)?;
    let parsed: StartResponse = parse_json(&result)?;
    if parsed.device_code.is_empty() || parsed.user_code.is_empty() {
        return Err(AuthError::Malformed(
            "device/user code missing in response".into(),
        ));
    }
    Ok(DeviceFlowStart {
        device_code: parsed.device_code,
        user_code: parsed.user_code,
        verification_uri: parsed.verification_uri,
        interval_secs: parsed.interval.max(1),
        expires_in_secs: parsed.expires_in,
    })
}

// ---------------------------------------------------------------------------
// Step 3 (loop) — poll for the access token.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    error: Option<String>,
    /// `slow_down` responses carry the new (longer) interval.
    #[serde(default)]
    interval: Option<u64>,
}

pub fn poll_token(http: &dyn Http, device_code: &str, interval_secs: u64) -> PollOutcome {
    let body = form_encode(&[
        ("client_id", CLIENT_ID),
        ("device_code", device_code),
        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
    ]);
    let result = match http.request(Verb::Post, TOKEN_URL, Some(&body), None) {
        Ok(r) => r,
        Err(HttpError(m)) => return PollOutcome::Network(m),
    };
    let parsed: TokenResponse = match parse_json(&result) {
        Ok(p) => p,
        Err(e) => return PollOutcome::Malformed(format!("{e:?}")),
    };

    match parsed.error.as_deref() {
        None => match parsed.access_token {
            Some(t) if !t.is_empty() => PollOutcome::Token(t),
            _ => PollOutcome::Malformed("200 without access_token".into()),
        },
        Some("authorization_pending") => PollOutcome::Pending { interval_secs },
        Some("slow_down") => PollOutcome::SlowDown {
            interval_secs: parsed.interval.unwrap_or(interval_secs + 5).max(1),
        },
        Some("expired_token") => PollOutcome::Expired,
        Some("access_denied") => PollOutcome::Denied,
        Some(other) => PollOutcome::Malformed(format!("unexpected error: {other}")),
    }
}

fn parse_json<T: for<'de> Deserialize<'de>>(result: &HttpResult) -> Result<T, AuthError> {
    let body = result.body.as_deref().unwrap_or("");
    serde_json::from_str(body)
        .map_err(|e| AuthError::Malformed(format!("status {}: {e}", result.status)))
}

// ---------------------------------------------------------------------------
// Keychain storage (docs/0005 Q3) — trait so tests run in-memory; the real
// store is keyring v3 (Secret Service via zbus on Linux, Security.framework
// on macOS). No file fallback by owner policy.
// ---------------------------------------------------------------------------

pub trait TokenStore: Send + Sync + 'static {
    fn save(&self, token: &str) -> Result<(), String>;
    /// `Ok(None)` = no token stored.
    fn load(&self) -> Result<Option<String>, String>;
    fn delete(&self) -> Result<(), String>;
}

pub struct KeyringTokenStore;

impl TokenStore for KeyringTokenStore {
    fn save(&self, token: &str) -> Result<(), String> {
        entry()?.set_password(token).map_err(|e| e.to_string())
    }

    fn load(&self) -> Result<Option<String>, String> {
        match entry()?.get_password() {
            Ok(t) => Ok(Some(t)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn delete(&self) -> Result<(), String> {
        match entry()?.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

fn entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT).map_err(|e| e.to_string())
}

#[cfg(test)]
pub(crate) struct MockTokenStore {
    pub slot: std::sync::Mutex<Option<String>>,
}

#[cfg(test)]
impl MockTokenStore {
    pub(crate) fn empty() -> Self {
        Self {
            slot: std::sync::Mutex::new(None),
        }
    }

    pub(crate) fn with(token: String) -> Self {
        Self {
            slot: std::sync::Mutex::new(Some(token)),
        }
    }
}
#[cfg(test)]
impl TokenStore for MockTokenStore {
    fn save(&self, token: &str) -> Result<(), String> {
        *self.slot.lock().unwrap() = Some(token.to_string());
        Ok(())
    }
    fn load(&self) -> Result<Option<String>, String> {
        Ok(self.slot.lock().unwrap().clone())
    }
    fn delete(&self) -> Result<(), String> {
        *self.slot.lock().unwrap() = None;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests — the device-flow state machine against a scripted transport.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::mock::MockHttp;

    fn result(status: u16, body: &str) -> Result<HttpResult, HttpError> {
        Ok(HttpResult {
            status,
            etag: None,
            rate: None,
            body: Some(body.into()),
        })
    }

    #[test]
    fn form_encoding_percent_encodes_the_grant_urn() {
        let s = form_encode(&[("grant_type", "urn:ietf:params:oauth:grant-type:device_code")]);
        assert_eq!(
            s,
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"
        );
    }

    #[test]
    fn start_sends_client_id_and_scopes_and_parses_defaults() {
        let http = MockHttp::ok(
            200,
            r#"{"device_code":"dc123","user_code":"WDJB-MJHT",
                "verification_uri":"https://github.com/login/device"}"#,
        );
        let start = start_device_flow(&http).unwrap();
        assert_eq!(start.user_code, "WDJB-MJHT");
        assert_eq!(start.interval_secs, 5);
        assert_eq!(start.expires_in_secs, 900);

        let recorded = http.recorded().remove(0);
        let (verb, url, body) = (recorded.verb, recorded.url, recorded.body);
        assert_eq!(verb, Verb::Post);
        assert_eq!(url, DEVICE_CODE_URL);
        let body = body.unwrap();
        assert!(body.contains(&format!("client_id={CLIENT_ID}")));
        assert!(body.contains("scope=repo%20read%3Aorg%20notifications"));
    }

    #[test]
    fn start_surfaces_http_error_status_as_malformed() {
        let http = MockHttp::ok(404, r#"{"message":"Not Found"}"#);
        assert!(matches!(
            start_device_flow(&http),
            Err(AuthError::Malformed(_))
        ));
    }

    #[test]
    fn start_maps_transport_error_to_network() {
        let http = MockHttp::new(vec![Err(HttpError("dns".into()))]);
        assert!(matches!(
            start_device_flow(&http),
            Err(AuthError::Network(_))
        ));
    }

    #[test]
    fn poll_success_returns_token() {
        let http = MockHttp::ok(200, r#"{"access_token":"gho_x","token_type":"bearer"}"#);
        assert_eq!(
            poll_token(&http, "dc", 5),
            PollOutcome::Token("gho_x".into())
        );
    }

    #[test]
    fn poll_pending_then_slowdown_bumps_interval() {
        let http = MockHttp::new(vec![
            result(200, r#"{"error":"authorization_pending"}"#),
            result(200, r#"{"error":"slow_down","interval":10}"#),
            result(200, r#"{"error":"slow_down"}"#),
        ]);
        assert_eq!(
            poll_token(&http, "dc", 5),
            PollOutcome::Pending { interval_secs: 5 }
        );
        assert_eq!(
            poll_token(&http, "dc", 5),
            PollOutcome::SlowDown { interval_secs: 10 }
        );
        // Without an explicit interval: +5 semantics.
        assert_eq!(
            poll_token(&http, "dc", 5),
            PollOutcome::SlowDown { interval_secs: 10 }
        );
    }

    #[test]
    fn poll_terminal_states() {
        let expired = MockHttp::ok(200, r#"{"error":"expired_token"}"#);
        assert_eq!(poll_token(&expired, "dc", 5), PollOutcome::Expired);

        let denied = MockHttp::ok(200, r#"{"error":"access_denied"}"#);
        assert_eq!(poll_token(&denied, "dc", 5), PollOutcome::Denied);
    }

    #[test]
    fn poll_unexpected_error_is_malformed() {
        let http = MockHttp::ok(200, r#"{"error":"device_flow_disabled"}"#);
        assert!(
            matches!(poll_token(&http, "dc", 5), PollOutcome::Malformed(m) if m.contains("device_flow_disabled"))
        );
    }

    #[test]
    fn poll_network_failure() {
        let http = MockHttp::new(vec![Err(HttpError("timeout".into()))]);
        assert!(matches!(
            poll_token(&http, "dc", 5),
            PollOutcome::Network(_)
        ));
    }

    #[test]
    fn poll_400_with_json_error_body_still_parses() {
        // GitHub returns 4xx with a JSON body carrying `error` — must not
        // blow up as Malformed before the error mapping runs.
        let http = MockHttp::ok(400, r#"{"error":"authorization_pending"}"#);
        assert_eq!(
            poll_token(&http, "dc", 5),
            PollOutcome::Pending { interval_secs: 5 }
        );
    }

    #[test]
    fn token_store_roundtrip() {
        let store = MockTokenStore::empty();
        assert_eq!(store.load().unwrap(), None);
        store.save("gho_secret").unwrap();
        assert_eq!(store.load().unwrap().as_deref(), Some("gho_secret"));
        store.delete().unwrap();
        assert_eq!(store.load().unwrap(), None);
    }
}
