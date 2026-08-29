# 0005 — GitHub Module (Notifications, Repositories, Pull Requests)

- **Status:** Implemented (owner merged PR [#17](https://github.com/noosxe/my-dashboard/pull/17) `4a34fec` 2026-08-29 — §8 human checks done; review fixes: popover surface token `575a211`, page-root overlay for the org menu `4a34fec`)
- **Date:** 2026-08-29
- **Depends on:** 0003 (module contract, navigator), 0004 (paths, persistence
  primitives), 0002 (shell chrome, D7-as-superseded decoration rule)

## 1. Summary

The first real data module, and the first consumer of everything 0003/0004
built. v1 ships:

- **Auth**: GitHub OAuth App **device flow**, token stored in the **OS
  keychain** (owner policy: any token material lives in keychains, never in
  files). Client ID is public and committed, exactly like `gh` CLI.
- **Home card**: counts only — unread notifications + total open PRs across
  the user's repos.
- **Module page** (owner-specified layout): two columns. Left = fixed-ish
  width, org dropdown (default: the user's personal account) with the repo
  list for the selected org below it, independently scrollable, selectable.
  Right = nothing until a repo is selected; then repo name + description as
  two text rows on top, and a content area of `GroupBox`es per category —
  v1: one **Open PRs** GroupBox and one **repo-scoped Notifications**
  GroupBox. Right column independently scrollable.

## 2. Goals / Non-goals

**Goals (v1)**

1. Connect flow end-to-end on Hyprland (browser handshake, keychain token).
2. Org/repo browsing for the authenticated user (personal + orgs).
3. Open PR list per repo; unread-notification list per repo.
4. Home card with live counts.
5. [DG]-complete states: loading / empty / error / disconnected.

**Non-goals (v1, recorded for later docs)**

- CI/check-run status per PR (one extra call per PR — deferred).
- Mark-as-read / notification actions; issues; releases; GraphQL migration;
  multi-account; GitHub Enterprise.
- Fine-grained least-privilege (impossible: notifications API excludes
  GitHub App / fine-grained tokens — see §3.1).

## 3. Verified upstream facts

Sources fetched and indexed 2026-08-29: docs.github.com *Authorizing OAuth
apps* (device flow), *Scopes for OAuth apps*, *REST API endpoints for
notifications*, *Rate limits for the REST API*. Pinned gpui-component
`1081d2b2`, zed `8cfa848c`.

### 3.1 Auth — device flow (OAuth App)

- Registration: **OAuth App** (not GitHub App). Verified blocker: *"This
  endpoint does not work with GitHub App user access tokens, GitHub App
  installation access tokens, or fine-grained personal access tokens"* —
  both `/notifications` and repo-scoped notifications. Notifications force
  the OAuth App path.
- Device flow must be **enabled** in the app settings (off by default).
  Owner has registered `my-dashboard`, enabled device flow.
- **Client ID (public, committed): `Ov23lisWKGYvlnBf2OY3`**. No client
  secret exists in this flow (verified: *"The `client_secret` is not needed
  for the device flow"*).
- `POST https://github.com/login/device/code` (`client_id`, `scope`) →
  `device_code`, `user_code` (8 chars, `XXXX-XXXX`),
  `verification_uri=https://github.com/login/device`, `interval=5`,
  `expires_in=900`.
- Poll `POST https://github.com/login/oauth/access_token` (`client_id`,
  `device_code`,
  `grant_type=urn:ietf:params:oauth:grant-type:device_code`) no faster than
  `interval`. Errors: `authorization_pending` (keep polling),
  `slow_down` (+5 s to interval, response carries the new interval),
  `expired_token` (15 min — restart), `access_denied` (user cancelled).
  Success → `access_token` (form-encoded body).
- 50 code submissions/hour cap per app (irrelevant at personal scale).

### 3.2 Scopes

Requested at authorization time (not at registration — OAuth Apps have no
permission UI): **`repo read:org notifications`** (space-delimited).
`repo` grants full read/write on public **and private** repos — coarse, but
it is the only way to a notifications-capable token. Documented in the
consent screen the user approves.

### 3.3 API surface (REST v3, `api.github.com`, `Accept: application/vnd.github+json`)

| Purpose | Endpoint |
| --- | --- |
| Identity | `GET /user` |
| Orgs | `GET /user/orgs` |
| Repos (personal **and** orgs, one endpoint) | `GET /user/repos?sort=pushed&per_page=100` (+ `page`) — filter client-side by `owner.login` for the selected org |
| Open PRs | `GET /repos/{owner}/{repo}/pulls?state=open&per_page=100` (+ `page`) |
| Notifications (global) | `GET /notifications?per_page=100` |
| Notifications (repo) | `GET /repos/{owner}/{repo}/notifications?per_page=100` |

- Pagination: `page`/`per_page` (max 100). Follow until a short page.
- Rate limit: 5,000 req/h (personal budget; OAuth app tokens spend it).
  Every response carries `x-ratelimit-limit/remaining/reset` — parse and
  surface in error states.
- Responses carry `ETag`; re-sending `If-None-Match` yields `304` with an
  empty body. We use this to keep polling cheap. (Whether 304s also skip
  rate-limit accounting will be verified empirically at implementation by
  logging `x-ratelimit-used` deltas; the design does not depend on it.)

### 3.4 UI capabilities at pinned rev (gpui-component `1081d2b2`, gpui `8cfa848c`)

- `Select<D: SearchableListDelegate>` — the org dropdown.
- `GroupBox` — the per-category panels.
- `List` + `ListDelegate` (virtualized, loading/separator states) — repo
  list; PR/notification rows are plain `v_flex` rows (small n per repo).
- `scrollable` (crates/ui/src/scroll) — the two independent scroll areas.
- `Badge`, `Avatar`, `Spinner`, `Skeleton`, `Button` — row furniture and
  [DG] states.
- `cx.open_url(&str)` exists (gpui `app.rs:1493`) — opens the verification
  URL and PR/notification links.
- **No GitHub icon** in the bundled icon set at this rev — the app bundles
  its own GitHub-mark SVG via its asset source. *(Post-merge: core grew
  `ModuleIcon::Custom` asset-path icons, so the mark now renders in the
  sidebar too — same bundled SVG.)*

## 4. Design decisions

Small-Q format as 0004; owner pre-approved Q1–Q4 in review conversation,
Q5–Q10 stand per the silence rule on merge.

- **Q1 — App type: OAuth App.** Notifications endpoints exclude GitHub Apps
  and fine-grained PATs (§3.1). Owner registered the OAuth App and enabled
  device flow.
- **Q2 — Scopes: `repo read:org notifications`.** Coarseness accepted
  knowingly (§3.2).
- **Q3 — Token storage: OS keychain via `keyring` v3.** Service
  `com.noosxe.my-dashboard`, account `github`. Linux backend: prefer the
  zbus-based Secret Service backend (pure Rust; `zbus` already in the
  lockfile) over libdbus-based ones — exact feature names verified at
  implementation; NixOS has no system libdbus in the devshell by default.
  macOS: `apple-native` (Security.framework already in the lockfile). If no
  Secret Service is reachable on a Linux box: **designed error state**
  ("no keyring service available — start gnome-keyring or an compatible
  agent"), **no file fallback** (owner policy: tokens never in files).
- **Q4 — HTTP client: `ureq` 2.x** (sync, rustls 0.23 — same major already
  in the lockfile). Called exclusively on `cx.background_executor`; no
  async runtime in the app. `serde`/`serde_json` for payloads (in tree).
  Client injectable via a small `Http` trait so unit tests stub it.
- **Q5 — Token lifecycle:** token read from keychain at module init (lazy,
  once); held inside the client only — never cloned into entities, state
  files, or logs (Authorization headers redacted in any tracing). Sign-out
  = delete keychain entry + drop in-memory token + clear caches. A revoked
  token surfaces as the reconnect state (§6.4), never a raw error dialog.
- **Q6 — Caching:** transient API data is **not** user data → lives under
  the **cache root** (`AppPaths.cache`, 0004): `github/etags.json` (URL →
  ETag) and `github/last-known/` (payload snapshots: notifications, per-repo
  PRs, repo list). Reuses `store::load_json/save_json_atomic` (quarantine on
  corruption, atomic writes). Boot paints last-known data instantly, then
  background refresh corrects it — no skeleton flash for returning users.
- **Q7 — Polling cadence (recommendation):** notifications 60 s; PRs of the
  *selected* repo on selection + 60 s while selected; full repo walk (open
  PR counts for the card) every 5 min using ETags; orgs+repos on connect
  and every 10 min; manual **Refresh** button on card and page. All timers
  follow the clock module's pattern: state writes end with the §3.12
  unfocused-refresh call (`window_handle.update(... refresh())`).
- **Q8 — Unfocused window:** 0003 §3.12 pattern is mandatory here (the
  card counts tick while the dashboard is unfocused — the exact scenario
  that bit us with the clock).
- **Q9 — Rate-limit/HTTP errors [DG]:** `401` → reconnect state (token
  revoked); `403` with `x-ratelimit-remaining: 0` → "rate limited until
  {reset}" banner, polling paused until reset, no request storm; network
  errors → inline error + Retry in the affected surface, stderr log,
  stale last-known data stays visible (degrade, don't blank).
- **Q10 — Personal account in the org dropdown:** `GET /user`'s login is
  prepended as the first entry (owner requirement: personal is the
  default selection), orgs follow in API order.

## 5. Architecture

```
crates/modules/github/
├── Cargo.toml      # deps: dashboard-core, gpui, gpui-component, ureq, keyring, serde, serde_json
└── src/
    ├── lib.rs      # Module impl; init(nav, window, paths, cx) contract v2
    ├── auth.rs     # device flow state machine + keyring get/set/delete
    ├── http.rs     # Http trait + UreqClient (auth header, ETag, rate-limit parse, pagination)
    ├── api.rs      # serde payload structs (§7), endpoint builders
    ├── service.rs  # GithubState entity: caches, poll loops, refresh signals
    ├── card.rs     # home card view (counts)
    └── page.rs     # module page (two-column layout)
```

- `service.rs` owns all state; views are pure observers (`cx.observe`).
  Background fetches: `cx.background_spawn` → send results via entity
  `update` (main thread), then the §3.12 refresh call.
- Boot: init seeds from last-known cache (Q6) and starts poll loops only if
  a token exists; otherwise the module renders the disconnected state.
- No new core crates; `keyring`/`ureq` stay module-local. Core's
  `store`/`paths` are reused as-is.

## 6. UI specification

### 6.1 Home card

- Normal: GitHub mark, `Notifications {n}` row, `Open PRs {m}` row
  (`m` = Σ open PRs across cached repos, capped at the first 100 repos —
  recorded limitation). Click → navigate to the GitHub page.
- Disconnected: one-line pitch + **Connect GitHub** button.
- Loading (first ever run): `Skeleton` rows. Error: inline message + Retry.

### 6.2 Connect flow (shared by card + page)

Dialog-style panel: shows `user_code` (large, monospace) with a copy
button, **Open github.com/login/device** button (`cx.open_url`, auto-fired
once), and a `Spinner` while polling. Terminal states get their own copy:
`access_denied` ("cancelled — try again"), `expired_token` ("code expired —
restart"), keychain-unavailable (Q3), network error + Retry. Success melts
into the normal page.

### 6.3 Module page (owner-specified)

```
┌──────────────┬────────────────────────────────────────────┐
│ [Org ▾]      │  owner/repo                    (row 1)     │
│──────────────│  description (muted)           (row 2)     │
│ ▸ repo A     │────────────────────────────────────────────│
│ ▸ repo B     │ ┌ GroupBox: Open PRs (N) ──────────────── ┐│
│ ▸ repo C     │ │ [Draft] title  #123  @author   2h ago  ││
│   …          │ │ …                                       ││
│  (scrolls)   │ └────────────────────────────────────────┘│
│              │ ┌ GroupBox: Notifications (N) ─────────── ┐│
│              │ │ …                                       ││
│              │ └────────────────────────────────────────┘│
│              │              (column scrolls)              │
└──────────────┴────────────────────────────────────────────┘
```

- Left: fixed 280 px, `Select` for orgs, `List` of repos below (name +
  private badge), independently scrollable; selection persists in memory
  for the session (not persisted to state dir — navigation, not settings).
- Right: empty state (centered, muted: "Select a repository") until a repo
  is chosen; then the two header rows exactly as above and GroupBoxes.
  Right column scrolls independently (header scrolls with content — it is
  part of the column).
- PR row: `[Draft]` badge when `draft`, title (medium weight), `#number`
  (muted), `Avatar` + author login, right-aligned relative age
  (`updated_at`). Row hover highlight; click → `cx.open_url(html_url)`.
- Notification row: subject title, type badge (`pr`/`issue`/…), age. Read
  state v1: list shows unread (`all=false` default) — count and list share
  one source of truth.

### 6.4 States matrix ([DG] — every surface)

| Surface | Loading | Empty | Error |
| --- | --- | --- | --- |
| Card | Skeleton | disconnected pitch + Connect | inline + Retry |
| Org dropdown | Spinner in place | Connect prompt | inline + Retry |
| Repo list | Skeleton rows | "No repositories in {org}" | inline + Retry |
| Right column | — | "Select a repository" | — |
| PR GroupBox | Skeleton rows | "No open PRs" | inline + Retry |
| Notifications GroupBox | Skeleton rows | "No unread notifications" | inline + Retry |
| Auth | code + spinner | — | terminal-state copy (§6.2) |

## 7. Data shapes (serde, minimal fields)

```rust
struct Org        { login: String, avatar_url: String, description: Option<String> }
struct Repo       { id: u64, name: String, full_name: String,
                    owner: Owner, description: Option<String>,
                    private: bool, pushed_at: Option<String> }
struct Owner      { login: String, avatar_url: String }
struct PullRequest{ id: u64, number: u64, title: String, draft: bool,
                    user: Owner, updated_at: String, html_url: String }
struct Notification{ id: String, subject: Subject,
                     repository: RepoRef, updated_at: String, unread: bool }
struct Subject    { title: String, r#type: String, url: String }
struct RepoRef    { full_name: String }
```

Unknown fields ignored (serde default) — forward compatible. GitHub payloads are snake_case (`avatar_url`, `html_url`) — no rename attributes needed; only `subject.type` is renamed (Rust keyword).

## 8. Testing

- **Gate quirk:** clippy on `--all-targets` needs `RUST_MIN_STACK=67108864`
  (the page's gpui builder trees overflow clippy's default stack), and
  `lib.rs` carries `#![recursion_limit = "4096"]` for the same reason.
  The page's two pure-function tests live in `service.rs`'s test module:
  a rustc quirk (recursion error while expanding `#[test]`) triggers when
  they sit in the same file as the deep builder types.

- **Unit (mocked `Http`):** device-flow state machine (pending/slow_down/
  expired/denied/success), pagination follow (short-page stop), ETag
  round-trip + 304 handling, rate-limit header parsing + pause-until-reset,
  payload parsing (fixtures captured from real responses, token-free),
  org grouping incl. personal-account-first ordering.
- **Keychain:** unit tests use keyring's mock credential store; real
  Secret Service covered by human checks.
- **Human checks (owner, Hyprland):** connect end-to-end; `secret-tool
  lookup service com.noosxe.my-dashboard` shows the token; card counts vs
  github.com; repo browsing; PR list matches the repo's open tab; PR click
  opens browser; unfocused card updates (clock regression: does the tick
  survive?); kill keychain agent → designed error, no crash; revoke token
  on github.com → reconnect state; sign-out wipes keychain + caches.

## 9. Security notes

- Token: keychain only (Q3/Q5); HTTPS to `api.github.com`/`github.com`
  only (rustls verification on; no custom certs).
- Cache payloads contain no secrets (public metadata + ETags).
- Device flow has no secret to leak; the user_code is single-use,
  15-minute, entropy owned by GitHub.
- Logs redact `Authorization` unconditionally (helper wrapper, not
  discipline).

## 10. Open issues / limitations (accepted for v1)

1. `repo` scope is read/write and covers private repos — coarser than
   ideal; impossible to avoid while notifications are required (§3.1).
2. Card PR total caps at the first 100 repos (pagination walk stops there;
   logged when hit).
3. Notification counts cap at 100 (`per_page` max) — counts ≥ 100 display
   as `100+`.
4. Whether 304s skip rate-limit accounting — verify empirically at
   implementation (§3.3); design works either way.
5. No keyring on a Linux box is a hard stop (designed error), per the
   tokens-in-keychains-only policy.
