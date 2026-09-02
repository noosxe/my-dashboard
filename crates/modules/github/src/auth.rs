//! OAuth device flow + refresh grant + keychain credential storage
//! (docs/0005 §3.1; docs/0011 is the authority for the token lifecycle).
//!
//! Flow: `start_device_flow` → show `user_code`, open `verification_uri` →
//! loop `poll_token` honoring the interval → store the credential (access
//! token + single-use refresh token + expiry) in the OS keychain as one
//! JSON document. Expiring tokens are kept alive via `refresh` — no
//! client secret exists in either flow (device-flow exemption, verified
//! upstream; docs/0011 §3). The client ID is public by design; scopes are
//! `repo read:org notifications`.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

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
/// Defensive fallback access-token lifetime when a refresh token is
/// present but the response omits `expires_in` (docs/0011 §4.2; upstream
/// always sends 28800 = 8 h).
const DEFAULT_ACCESS_EXPIRES_IN_SECS: u64 = 28800;
/// Keychain credential-document version (docs/0011 §4.1).
const CREDENTIAL_DOC_VERSION: u8 = 1;

// ---------------------------------------------------------------------------
// Stored credential (docs/0011 §4.1) — the keychain value is one JSON
// document so access + refresh rotate atomically. A value not starting
// with `{` is a legacy bare access token: valid, non-expiring, never
// refreshed, never rewritten.
// ---------------------------------------------------------------------------

/// The full GitHub credential as persisted in the keychain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCredential {
    pub access_token: String,
    /// Single-use `ghr_…` token; `None` = non-expiring credential.
    pub refresh_token: Option<String>,
    /// UTC epoch seconds when `access_token` dies; `None` = no expiry.
    pub access_expires_at: Option<u64>,
}

/// Serde shape of the keychain document (snake_case by default).
#[derive(Serialize, Deserialize)]
struct CredentialDoc {
    v: u8,
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    access_expires_at: Option<u64>,
}

fn encode_credential(cred: &StoredCredential) -> String {
    serde_json::to_string(&CredentialDoc {
        v: CREDENTIAL_DOC_VERSION,
        access_token: cred.access_token.clone(),
        refresh_token: cred.refresh_token.clone(),
        access_expires_at: cred.access_expires_at,
    })
    .unwrap_or_default()
}

/// Decode a stored value: JSON document, or legacy bare token.
fn decode_credential(value: &str) -> Result<StoredCredential, String> {
    if !value.starts_with('{') {
        return Ok(StoredCredential {
            access_token: value.to_string(),
            refresh_token: None,
            access_expires_at: None,
        });
    }
    let doc: CredentialDoc =
        serde_json::from_str(value).map_err(|e| format!("corrupt credential: {e}"))?;
    if doc.v != CREDENTIAL_DOC_VERSION {
        return Err(format!("unsupported credential document version {}", doc.v));
    }
    Ok(StoredCredential {
        access_token: doc.access_token,
        refresh_token: doc.refresh_token,
        access_expires_at: doc.access_expires_at,
    })
}

/// Wall-clock UTC epoch seconds (docs/0011 §4.2).
pub(crate) fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

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
    /// Device flow approved — the full credential pair (docs/0011 §4.2).
    Token(StoredCredential),
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
    /// Present for expiring tokens (28800 = 8 h; docs/0011 §3).
    #[serde(default)]
    expires_in: Option<u64>,
    /// Rotated single-use refresh token; absent for non-expiring tokens.
    #[serde(default)]
    refresh_token: Option<String>,
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
            Some(t) if !t.is_empty() => PollOutcome::Token(StoredCredential {
                access_token: t,
                // Expiry must be computed before `refresh_token` is
                // moved out of `parsed` below.
                access_expires_at: match (parsed.expires_in, parsed.refresh_token.as_deref()) {
                    // Expiry rides along whenever upstream sends it;
                    // refresh-token-without-expiry falls back to the 8 h
                    // constant (docs/0011 §4.2); bare pairs never expire.
                    (Some(secs), _) => Some(now_epoch() + secs),
                    (None, Some(rt)) if !rt.is_empty() => {
                        Some(now_epoch() + DEFAULT_ACCESS_EXPIRES_IN_SECS)
                    }
                    (None, _) => None,
                },
                refresh_token: parsed.refresh_token.filter(|r| !r.is_empty()),
            }),
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

// ---------------------------------------------------------------------------
// Refresh grant (docs/0011 §4.2) — keeps expiring tokens alive. No client
// secret: it is required only for non-device-flow tokens, and ours are
// device-flow (verified upstream). Rotation is strict: the response's
// tokens replace the stored pair, and the old pair dies immediately.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// New rotated credential, ready to install.
    Refreshed(StoredCredential),
    /// Transport failure — retry on a later tick; nothing is invalidated.
    Transient(String),
    /// Refresh token rejected/revoked/expired, or malformed response —
    /// full re-auth (device flow) required.
    Reauth(String),
}

pub fn refresh(http: &dyn Http, refresh_token: &str) -> RefreshOutcome {
    let body = form_encode(&[
        ("client_id", CLIENT_ID),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
    ]);
    let result = match http.request(Verb::Post, TOKEN_URL, Some(&body), None) {
        Ok(r) => r,
        Err(HttpError(m)) => return RefreshOutcome::Transient(m),
    };
    let parsed: TokenResponse = match parse_json(&result) {
        Ok(p) => p,
        Err(e) => return RefreshOutcome::Reauth(format!("malformed refresh response: {e:?}")),
    };
    if let Some(err) = parsed.error {
        return RefreshOutcome::Reauth(err);
    }
    match parsed.access_token {
        Some(t) if !t.is_empty() => RefreshOutcome::Refreshed(StoredCredential {
            access_token: t,
            refresh_token: parsed.refresh_token.filter(|r| !r.is_empty()),
            access_expires_at: Some(
                now_epoch() + parsed.expires_in.unwrap_or(DEFAULT_ACCESS_EXPIRES_IN_SECS),
            ),
        }),
        _ => RefreshOutcome::Reauth("malformed refresh response: 200 without access_token".into()),
    }
}
fn parse_json<T: for<'de> Deserialize<'de>>(result: &HttpResult) -> Result<T, AuthError> {
    let body = result.body.as_deref().unwrap_or("");
    serde_json::from_str(body)
        .map_err(|e| AuthError::Malformed(format!("status {}: {e}", result.status)))
}

// ---------------------------------------------------------------------------
// Keychain storage (docs/0005 Q3, docs/0011 §4.1) — trait so tests run
// in-memory; the real store is keyring v3 (Secret Service via zbus on
// Linux, Security.framework on macOS), holding one JSON credential
// document per docs/0011. No file fallback by owner policy.
// ---------------------------------------------------------------------------

pub trait TokenStore: Send + Sync + 'static {
    fn save(&self, cred: &StoredCredential) -> Result<(), String>;
    /// `Ok(None)` = no credential stored.
    fn load(&self) -> Result<Option<StoredCredential>, String>;
    fn delete(&self) -> Result<(), String>;
}

pub struct KeyringTokenStore;

impl TokenStore for KeyringTokenStore {
    fn save(&self, cred: &StoredCredential) -> Result<(), String> {
        entry()?
            .set_password(&encode_credential(cred))
            .map_err(|e| e.to_string())
    }

    fn load(&self) -> Result<Option<StoredCredential>, String> {
        match entry()?.get_password() {
            Ok(value) => decode_credential(&value).map(Some),
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
    pub slot: std::sync::Mutex<Option<StoredCredential>>,
    /// When set, `save` fails — scripts keychain-write failures
    /// (docs/0011 §6.9/6.10).
    pub fail_save: bool,
}

#[cfg(test)]
impl MockTokenStore {
    pub(crate) fn empty() -> Self {
        Self {
            slot: std::sync::Mutex::new(None),
            fail_save: false,
        }
    }

    /// A stored credential as-is.
    pub(crate) fn with_credential(cred: StoredCredential) -> Self {
        Self {
            slot: std::sync::Mutex::new(Some(cred)),
            fail_save: false,
        }
    }

    /// A legacy bare access token (no refresh, no expiry).
    pub(crate) fn with(token: String) -> Self {
        Self::with_credential(StoredCredential {
            access_token: token,
            refresh_token: None,
            access_expires_at: None,
        })
    }

    /// Every save fails.
    pub(crate) fn failing_save() -> Self {
        Self {
            slot: std::sync::Mutex::new(None),
            fail_save: true,
        }
    }
}
#[cfg(test)]
impl TokenStore for MockTokenStore {
    fn save(&self, cred: &StoredCredential) -> Result<(), String> {
        if self.fail_save {
            return Err("keychain unavailable (scripted)".into());
        }
        *self.slot.lock().unwrap() = Some(cred.clone());
        Ok(())
    }
    fn load(&self) -> Result<Option<StoredCredential>, String> {
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
    fn poll_success_returns_bare_credential() {
        let http = MockHttp::ok(200, r#"{"access_token":"gho_x","token_type":"bearer"}"#);
        assert_eq!(
            poll_token(&http, "dc", 5),
            PollOutcome::Token(StoredCredential {
                access_token: "gho_x".into(),
                refresh_token: None,
                access_expires_at: None,
            })
        );
    }

    #[test]
    fn poll_success_carries_refresh_pair_and_expiry() {
        let before = now_epoch();
        let http = MockHttp::ok(
            200,
            r#"{"access_token":"gho_x","expires_in":28800,"refresh_token":"ghr_y"}"#,
        );
        let PollOutcome::Token(cred) = poll_token(&http, "dc", 5) else {
            panic!("expected token")
        };
        assert_eq!(cred.access_token, "gho_x");
        assert_eq!(cred.refresh_token.as_deref(), Some("ghr_y"));
        let at = cred.access_expires_at.unwrap();
        assert!(at >= before + 28800 && at <= now_epoch() + 28800);
    }

    #[test]
    fn poll_refresh_without_expires_falls_back_to_eight_hours() {
        let before = now_epoch();
        let http = MockHttp::ok(200, r#"{"access_token":"gho_x","refresh_token":"ghr_y"}"#);
        let PollOutcome::Token(cred) = poll_token(&http, "dc", 5) else {
            panic!("expected token")
        };
        assert!(
            cred.access_expires_at
                .is_some_and(|at| at >= before + DEFAULT_ACCESS_EXPIRES_IN_SECS)
        );
    }

    #[test]
    fn refresh_rotates_and_sets_expiry() {
        let http = MockHttp::ok(
            200,
            r#"{"access_token":"gho_new","expires_in":28800,"refresh_token":"ghr_new"}"#,
        );
        let before = now_epoch();
        assert!(matches!(
            refresh(&http, "ghr_old"),
            RefreshOutcome::Refreshed(c)
                if c.access_token == "gho_new"
                    && c.refresh_token.as_deref() == Some("ghr_new")
                    && c.access_expires_at.is_some_and(|at| at >= before + 28800)
        ));
        let recorded = http.recorded().remove(0);
        assert_eq!(recorded.url, TOKEN_URL);
        let body = recorded.body.unwrap();
        assert!(body.contains("grant_type=refresh_token"));
        assert!(body.contains(&format!("client_id={CLIENT_ID}")));
        assert!(body.contains("refresh_token=ghr_old"));
        // Public client — the device-flow exemption means no secret (§3).
        assert!(!body.contains("client_secret"));
    }

    #[test]
    fn refresh_invalid_grant_is_reauth_on_any_status() {
        let ok = MockHttp::ok(200, r#"{"error":"invalid_grant"}"#);
        assert!(matches!(
            refresh(&ok, "ghr"),
            RefreshOutcome::Reauth(m) if m == "invalid_grant"
        ));
        let bad = MockHttp::ok(400, r#"{"error":"invalid_grant"}"#);
        assert!(matches!(refresh(&bad, "ghr"), RefreshOutcome::Reauth(_)));
    }

    #[test]
    fn refresh_transport_error_is_transient() {
        let http = MockHttp::new(vec![Err(HttpError("timeout".into()))]);
        assert!(matches!(
            refresh(&http, "ghr"),
            RefreshOutcome::Transient(_)
        ));
    }

    #[test]
    fn refresh_malformed_response_is_terminal_reauth() {
        let no_token = MockHttp::ok(200, r#"{"token_type":"bearer"}"#);
        assert!(matches!(
            refresh(&no_token, "ghr"),
            RefreshOutcome::Reauth(_)
        ));
        let garbage = MockHttp::ok(200, "not json");
        assert!(matches!(
            refresh(&garbage, "ghr"),
            RefreshOutcome::Reauth(_)
        ));
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
    fn credential_doc_roundtrips() {
        let cred = StoredCredential {
            access_token: "gho_a".into(),
            refresh_token: Some("ghr_b".into()),
            access_expires_at: Some(1_799_999_999),
        };
        let encoded = encode_credential(&cred);
        assert!(encoded.starts_with(r#"{"v":1"#));
        assert_eq!(decode_credential(&encoded).unwrap(), cred);
    }

    #[test]
    fn legacy_bare_token_decodes_as_non_expiring() {
        let cred = decode_credential("gho_legacy").unwrap();
        assert_eq!(cred.access_token, "gho_legacy");
        assert_eq!(cred.refresh_token, None);
        assert_eq!(cred.access_expires_at, None);
    }

    #[test]
    fn credential_doc_rejects_unknown_version_and_garbage() {
        assert!(decode_credential(r#"{"v":2,"access_token":"gho"}"#).is_err());
        assert!(decode_credential("{oops").is_err());
    }

    #[test]
    fn token_store_roundtrip() {
        let store = MockTokenStore::empty();
        assert_eq!(store.load().unwrap(), None);
        let cred = StoredCredential {
            access_token: "gho_secret".into(),
            refresh_token: Some("ghr_r".into()),
            access_expires_at: Some(42),
        };
        store.save(&cred).unwrap();
        assert_eq!(store.load().unwrap(), Some(cred));
        store.delete().unwrap();
        assert_eq!(store.load().unwrap(), None);
    }
}
