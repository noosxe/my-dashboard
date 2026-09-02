# 0011 — GitHub Module: Token Refresh (Expiring Tokens)

- **Date:** 2026-09-02
- **Depends on:** 0005 (§3.1 auth, Q3 keychain, Q5 token lifecycle, Q9
  401 handling), 0007 (fetching model), 0008 (event catalog), 0003 §3.12
  (unfocused refresh)
- **Amends:** 0005 §3.1/Q5/Q9 — from merge, **0011 is the authority for the
  GitHub token lifecycle** (credential storage, expiry, refresh, reconnect).
  0005 remains the authority for everything else (device flow UX, scopes,
  keychain service coordinates, degradation matrix).

## 1. Summary

The registered OAuth App (`Ov23lisWKGYvlnBf2OY3`) has GitHub's optional
**"Expire user authorization tokens"** feature enabled, so every token it
issues expires after **8 hours** and arrives with a single-use **refresh
token**. The app currently stores only the access token and has no refresh
grant, so ~8 h after connect every API call 401s and the module drops to
the reconnect state — the "token expires every few hours" symptom.

This doc specifies the refresh flow: a versioned **credential document**
in the existing keychain entry, a `refresh_token` grant call (no client
secret — device-flow exemption), **proactive** refresh on the tick loop
before expiry, **reactive** refresh as the 401 handler's last step before
the reconnect state, and rotation-safe persistence. No new user-visible
states: the happy path is silent, and every failure lands in the existing
designed states (`TokenRejected`, `KeychainError`).

## 2. Problem & evidence

- Symptom: module stays connected for a few hours, then flips to
  "reconnect required" (the Q9 `TokenRejected` path — `apply` sees 401).
- The app contains no token-expiry logic: the only `expires_in` in the
  codebase is the **device-code window** (900 s), correctly used solely as
  the connect-flow polling deadline.
- Owner enabled expiring tokens when registering the app; the OAuth App
  settings now show no way to disable it. Upstream docs document only
  **Opt-in** for OAuth Apps ("Activating optional features for OAuth
  apps", no opt-out step), and note that enabling affects *newly issued*
  tokens — the way back for this app is implementing refresh, not
  un-ticking a box.
- Conclusion: not an app bug — an app **gap** against a permanent
  GitHub-side property of this OAuth App.

## 3. Verified upstream behavior

Sources fetched and pinned 2026-09-02: docs.github.com *Authorizing OAuth
apps* (§ Device flow, § Expiring access tokens, § Refreshing an access
token with a refresh token), *Token expiration and revocation*,
*Activating optional features for OAuth apps*.

- Both the web flow and the **device flow** support expiring tokens: the
  device-flow token response gains `expires_in` and `refresh_token` when
  the feature is on.
- Access token lifetime: **8 hours** (`expires_in` is always `28800`).
- Refresh token: `ghr_…` prefix, expires after **6 months without use**;
  each use issues a **new** refresh token.
- Refresh grant: `POST https://github.com/login/oauth/access_token` with
  `client_id`, `grant_type=refresh_token`, `refresh_token`.
  `client_secret` is **required only for non-device-flow tokens** — our
  credentials are device-flow, so the public-client model holds (no secret
  ever exists in this app).
- **Rotation is strict:** "Once you use a refresh token, that refresh
  token and the old access token will no longer work." A lost new pair
  (crash between response and persistence) forces full re-auth.
- Refresh failure (`invalid_grant`, expired/revoked refresh token):
  "you must send the user through the … device flow again."
- Token responses may carry `error` with HTTP 200 **or** 4xx + JSON body —
  same parsing contract `poll_token` already handles.

## 4. Design

### 4.1 Credential document (keychain format)

The existing keychain entry (service `com.noosxe.my-dashboard`, account
`github` — unchanged) stores a JSON document instead of a bare token:

```json
{"v":1,
 "access_token":"gho_…",
 "refresh_token":"ghr_…",
 "access_expires_at":1799999999}
```

- `refresh_token` and `access_expires_at` are **optional**: absent for a
  non-expiring token (feature off, or token issued before it was enabled).
- `access_expires_at` = UTC epoch seconds, computed as
  `now + expires_in` at issue/refresh time. `refresh_token_expires_in` is
  **not persisted**: the 6-month window is sliding ("without use") and an
  absolute epoch would be wrong; policy is *attempt refresh, treat
  `invalid_grant` as re-auth*.
- **Legacy compatibility (MUST):** on load, a value not starting with `{`
  is a legacy bare access token — valid, non-expiring, never refreshed.
  Legacy entries are not rewritten (zero benefit, keychain-failure risk).
- One entry (not two) is mandatory: rotation must replace access +
  refresh **atomically**; two entries could leave a half-rotated credential
  after a crash between writes.

`TokenStore` becomes credential-shaped:

```rust
pub struct StoredCredential {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub access_expires_at: Option<u64>, // epoch secs
}
pub trait TokenStore: Send + Sync + 'static {
    fn save(&self, cred: &StoredCredential) -> Result<(), String>;
    fn load(&self) -> Result<Option<StoredCredential>, String>; // None = no entry
    fn delete(&self) -> Result<(), String>;
}
```

`KeyringTokenStore` serializes the JSON document (serde, snake_case);
`MockTokenStore` mirrors it in memory. Parse errors on load surface as the
existing `KeychainError` designed state (Q3), not a crash.

### 4.2 Refresh grant (wire)

```rust
pub enum RefreshOutcome {
    /// New rotated credential, ready to install.
    Refreshed(StoredCredential),
    /// Transport failure — retry on a later tick; nothing is invalidated.
    Transient(String),
    /// Refresh token rejected/revoked/expired, or malformed response —
    /// full re-auth (device flow) required.
    Reauth(String),
}

pub fn refresh(http: &dyn Http, refresh_token: &str) -> RefreshOutcome;
```

Request (via the existing `Http::request` — URL is not `api.github.com`,
so it gets `Accept: application/json` and no bearer, exactly like the
device-flow POSTs):

```
POST https://github.com/login/oauth/access_token
Content-Type: application/x-www-form-urlencoded

client_id=Ov23lisWKGYvlnBf2OY3&grant_type=refresh_token&refresh_token=ghr_…
```

Response handling:

| Response | Outcome |
| --- | --- |
| 200 with `access_token` | `Refreshed` — `access_expires_at = now + expires_in.unwrap_or(28800)`; the response's `refresh_token` MUST replace the stored one (rotation) |
| `error` present (any status; e.g. `invalid_grant`) | `Reauth(reason)` |
| Transport/DNS/TLS error | `Transient(message)` |
| 200 without `access_token`, unparseable body | `Reauth("malformed refresh response …")` (terminal by design — never loop on a malformed body) |

The device-flow `TokenResponse` gains `expires_in: Option<u64>` and
`refresh_token: Option<String>` (serde defaults), and `PollOutcome::Token`
carries a full `StoredCredential` instead of a bare `String` — the pair is
issued by the same response. `DEFAULT_ACCESS_EXPIRES_IN_SECS = 28800` is
the defensive fallback when a refresh token is present but `expires_in`
is not.

### 4.3 State machine integration

Two triggers, **one drain point**:

1. **Proactive** — before planning fetches, each tick: if
   `access_expires_at - LEAD ≤ now` (LEAD = 600 s), set
   `pending_refresh = Some(Proactive)` and plan no fetches this tick
   (they would 401 against a dying token anyway). Covers the running-app
   case; on wake-from-suspend or app-start-past-expiry the immediate
   first beat (0005 Q7) fires it at once. Clock skew backward is harmless
   (late refresh → reactive covers); forward is harmless (early refresh).
2. **Reactive** — `apply`'s 401 handler (Q9): if a refresh token exists
   and this credential hasn't already had its reactive attempt, set
   `pending_refresh = Some(Reactive)` instead of `TokenRejected`. One
   attempt per credential (marker set when the attempt runs, cleared on
   successful install) — no refresh loops on a persistently-401ing token.

The tick-loop orchestration (background executor, existing pattern)
drains `pending_refresh`: call `refresh`, then on the main thread

- `Refreshed(cred)` → `install_credential(cred)` (below) →
  `needs_bootstrap = true` (full data refetch, existing bootstrap walk) →
  log info;
- `Transient(m)` → keep `pending_refresh` (retry next tick ≤ 60 s; fetches
  stay gated, stale data shown — matches existing network-loss behavior);
- `Reauth(m)` → `AuthStatus::TokenRejected`, log warn — the existing Q9
  reconnect UI.

`gated()` additionally returns true while `pending_refresh.is_some()`.

```rust
fn install_credential(&mut self, cred: StoredCredential) -> Result<(), String> {
    self.store.save(&cred)?;            // keychain FIRST (see 4.4)
    *self.shared_token.write() = Some(cred.access_token.clone());
    *self.shared_refresh.write() = cred.refresh_token.clone();
    self.access_expires_at = cred.access_expires_at; // non-secret, lives in state
    self.refresh_attempted = false;
    self.auth = AuthStatus::Connected;
    self.rate_limited_until = None;
    self.needs_bootstrap = true;
    Ok(())
}
```

Secret-handling rules (extends 0005 Q5): the refresh token is treated
exactly like the access token — it lives in the keychain and in a
dedicated `SharedToken`-style slot read only by the refresh call, and
MUST never be cloned into `GithubState`, state files, logs, or error
strings. `access_expires_at` is not secret and may live in state for
scheduling. `sign_out` clears both slots, the expiry, and the keychain
entry (unchanged semantics).

### 4.4 Rotation safety & failure handling

- **Persist before publish:** on `Refreshed`, save the new pair to the
  keychain *before* touching in-memory slots or `AuthStatus`. A crash
  after the keychain write but before state updates loses nothing (next
  boot loads the new pair); a crash before the keychain write loses the
  old (now dead) pair → user re-auths — no worse than today.
- **Keychain save failure mid-refresh:** the old access token is already
  invalidated upstream, so this is terminal for the session: designed
  `KeychainError` state (Q3) + reconnect, mirroring the connect-path save
  failure. In-memory slots keep the dead values; harmless while gated.
- Single-writer discipline: all refresh state writes go through
  `state.update` on the main thread ending in the 0003 §3.12
  `window.refresh()` call; HTTP runs only on the background executor —
  same shape as `start_connect`.

### 4.5 Logging (0008 event catalog additions)

| Area | Event | Level |
| --- | --- | --- |
| github auth | refresh due (expiry ISO, reason proactive/reactive-401) | debug |
| github auth | token refreshed — new expiry ISO | info |
| github auth | refresh failed (reason) — reconnect required | warn |
| github auth | legacy non-expiring token loaded (no refresh) | debug (once per boot) |

No token material, ever (0008 §4 redaction rules; ureq error Display
never embeds request bodies/headers — 0005 Q4 note).

## 5. Design decisions

Small-Q format per house style; silence rule on merge.

- **Q1 — Refresh in-app, not a new OAuth App.** Creating a fresh OAuth App
  without the feature would restore non-expiring tokens but orphans the
  app identity (client ID is committed and doc-pinned), teaches nothing,
  and one accidental Opt-in repeats the problem. Refresh is the durable
  fix and is fully supported for device-flow credentials.
- **Q2 — One JSON credential document in the existing keychain entry.**
  Atomic rotation beats multi-entry convenience; legacy bare-string
  values keep working unchanged (§4.1).
- **Q3 — Refresh triggers: proactive (tick, 10-min lead) + reactive (401,
  single attempt).** Proactive alone misses suspend/clock skew; reactive
  alone makes every expiry user-visible for up to a tick. Together, the
  reactive path is a rarely-taken safety net. One drain point in the tick
  loop keeps the flow pure-plan/spawn/apply testable.
- **Q4 — No `refresh_token_expires_in` tracking.** Sliding 6-month window
  makes a stored epoch a lie; attempt-and-handle-`invalid_grant` is
  simpler and always correct. Cost of the 6-month miss (app unused past
  it): one reconnect — identical to today's revoked-token path.
- **Q5 — `Transient` never escalates, `Reauth` is terminal.** Network
  blips retry on later ticks exactly like fetch errors; only GitHub
  rejecting the grant (or a malformed response) ends the session. No
  infinite loops: `Reauth`/malformed are terminal, and the reactive
  attempt marker bounds the 401 path.
- **Q6 — No UI changes.** Happy path is silent; failures reuse
  `TokenRejected`/`KeychainError` rendering. Surfacing expiry in settings
  is yak-shaving for a personal dashboard — filed as a follow-up idea, not
  built (non-goal).
- **Q7 — LEAD = 600 s.** Two orders of magnitude under the 8-h lifetime,
  comfortably above the 60-s tick, and small enough that a woken machine
  refreshes essentially immediately. Not load-bearing: any error here
  falls into the reactive net.

## 6. Tests (unit, scripted `MockHttp` + `MockTokenStore`)

1. **Issue:** device-flow token response with `expires_in` + `refresh_token`
   → `install_credential` persists the full JSON doc; slots + expiry set;
   keychain value round-trips.
2. **Legacy load:** bare `gho_…` string loads as non-expiring credential;
   never plans refresh; its 401 goes straight to `TokenRejected`.
3. **Proactive scheduling:** refresh planned at `expires_at − 600 s`, not
   before; past-expiry credentials refresh on the first immediate tick;
   fetches gated while pending.
4. **Refresh success:** request body carries `grant_type=refresh_token` +
   `client_id` and no secret; rotated pair replaces the stored one (old
   `ghr_` gone); `needs_bootstrap` set; state `Connected`.
5. **Refresh `invalid_grant`:** → `TokenRejected`; keychain entry still
   deleted only by `sign_out` (reconnect offers device flow).
6. **Refresh transient:** network error → state unchanged, pending kept,
   retried next tick; no attempt-marker burn.
7. **Reactive 401:** with refresh token → refresh runs instead of
  `TokenRejected`; success re-installs and re-bootstraps; second 401 on
   the same credential (attempt marker set) → `TokenRejected`.
8. **Malformed refresh response:** terminal `Reauth`, warn logged.
9. **Keychain save failure on refresh:** `KeychainError`, session ends.
10. **Persist-before-publish:** scripted store failure leaves state/slots
    untouched (order assertion via recorded calls).

## 7. Compatibility & migration

| Keychain value | Behavior |
| --- | --- |
| bare `gho_…` (pre-feature) | works, never refreshed; 401 → reconnect |
| doc without `refresh_token` | works, never refreshed (feature enabled after issue); 401 → reconnect |
| doc with `refresh_token` | full flow: proactive + reactive refresh |

No migration step ships: existing entries are valid legacy credentials by
§4.1; the next device-flow connect writes the new format. Suspend/resume,
and app-closed-past-expiry (< 6 months) both recover via first-tick
refresh; past 6 months the refresh token is dead → reconnect (acceptable;
the app must run at least once per half-year to stay silent).

Docs to update in the implementation PR: 0005 gains an
"amended by 0011" banner on §3.1/Q5/Q9 (pattern of 0007's banner);
0008 event catalog rows (§4.5).

## 8. Non-goals

- No client secret, ever (device-flow public client; verified exemption).
- No web application flow, no GitHub App migration (0005 Q1 stands).
- No use of the OAuth-application revocation REST endpoints.
- No refresh while the app is not running (no daemon/agent).
- No token-status UI in settings (follow-up idea at most; Q6).
