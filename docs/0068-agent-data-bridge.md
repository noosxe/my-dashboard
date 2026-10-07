# 0068 — Agent Data Bridge: programmatic app access for AI agents over MCP

Status: design (docs-only PR) · 2026-10-06

The owner wants AI agents to operate the dashboard the same way the owner can
— **from the data layer, not by navigating views**: enumerate repos, check PR
statuses, analyze GitHub API usage from the analytics pipeline, and generally
"anything I can do programmatically", exposed step-by-step over MCP
(Model Context Protocol) with **local-only transport** (no HTTP server).

This doc designs the in-app **agent data bridge**: a product-feature socket
serving typed, read-only data operations backed by the same module
state the UI reads, plus a thin stdio MCP front-end binary. It deliberately
reuses three proven assets: the debug bridge's socket/token/main-thread
pattern (docs/0010), the module provider seam (docs/0003), and the usage
ledger's origin taxonomy (docs/0053 D2) for agent-attributed API accounting.

## 1. Summary

- **D1** — a new product crate `crates/agent` (not `debug-tools`-gated):
  in-app Unix-socket server on `agent.sock` (token `agent.token`) in the
  existing per-user runtime dir, plus the MCP stdio binary
  `dashboard-agent-mcp`.
- **D2** — trust model mirrors docs/0010 D2: same-user socket (0600) +
  per-run random token, first-line `auth` handshake. File-permission
  kill switch in config; no settings UI in v1.
- **D3** — module-agnostic seam: the bridge is generic; each module
  registers an agent-ops provider (like `SettingsProvider`), so ops live in
  the module that owns the data. Phase 1 = github module only.
- **D4** — the bridge is **read-only, end to end**. Phase 1 is strictly
  held-state reads (zero network flights); on-demand fetches (phase 2)
  ride an explicit `refresh` flag. No write operations exist in this
  design.
- **D5** — every response carries mandatory envelope metadata:
  `freshness {age_s, source}` and `truncation {returned, total_known}` —
  agents must never mistake a 5-minute-old cached snapshot for live truth
  (docs/0029 §2 polling ceiling) or a capped page for the whole set.
- **D6** — agent-originated dispatches are usage-attributed: new
  `Surface::Agent` / `Trigger::Agent` variants in the github module's
  taxonomy (docs/0053 D2), so the usage page can separate agent spend from
  UI spend — and the `api_usage` op can report it.
- **D7** — closed op set; **no raw GraphQL passthrough** (owner decision,
  2026-10-06): new capabilities arrive as curated ops, never as an escape
  hatch.

## 2. Goals

1. An agent connected over MCP can answer, from the running app: *what repos
   do I have, what are my PR statuses, what's unread, how much API budget is
   the app (and any agent) spending?*
2. Agent traffic is **observable and attributable** — on the usage page and
   in the logs — without any new backend plumbing beyond the existing
   ledger seam (docs/0050 D2 counting decorator already sees 100% of
   GitHub-bound traffic).
3. The agent never gets *silent* staleness or truncation: every answer
   states its age and its bounds (D5).
4. The surface is **incremental**: each later phase (on-demand fetches,
   more modules) adds ops inside the owning module without touching the
   bridge core.
5. Local-only: a same-user Unix socket; nothing listens on TCP; nothing
   crosses a trust boundary the user's own processes don't already have.

## 3. Non-goals

- **No HTTP / remote transport.** Unix socket only (Windows named pipes are
  a future work item; the debug bridge is already Unix-only — same stance).
- **No webhooks / push channel** — the polling model (docs/0029 §2) stands;
  agents poll the bridge like the UI polls GitHub.
- **No raw GraphQL/REST passthrough** (D7, owner decision 2026-10-06) — a
  `query` escape hatch would bypass the curated op set and let one agent
  call burn unbounded quota. Decided: none, not deferred.
- **No write operations at all** (owner decision 2026-10-06) — the bridge
  is read-only by design; state entities accept no mutations through it.
  Writes, if ever wanted, need a fresh design doc (draft→apply gating,
  audit log), not an extension of this one.
- **No agent sessions or server-side agent state** — stateless
  request/response; concurrency = concurrent connections (D9).
- **No secrets over the bridge** — tokens never appear in any response.
  `access_expires_at` is already explicitly non-secret (service.rs:1727).
- **No settings UI in v1** — the kill switch is config-file-only, so this
  PR's implementation touches no visual surface (0065 receipts N/A).

## 4. Facts & constraints — verified in the codebase 2026-10-06

| Fact | Source |
|---|---|
| Socket pattern to reuse: runtime dir `0700`, socket `0600`, per-run random token file doubling as run marker, NDJSON one-request-per-line, first line MUST be `auth`; socket threads push boxed closures through an `mpsc` queue executed by `app.spawn` on the foreground executor | `crates/debug/src/server.rs` (`SOCKET_NAME`/`TOKEN_NAME`, `serve_connection`, job queue; docs/0010 D2/D3) |
| Runtime dir resolves once at startup: `AppPaths::init().runtime_dir()` | `crates/core/src/paths.rs:24,135` |
| `GithubState` is the single state entity, created in module init and cloned into the tick loop and every view — a new consumer takes a handle the same way | `crates/modules/github/src/lib.rs:118–135` (`spawn_tick_loop(state.clone(), …)`) |
| Held state available for reads: `repos: Vec<Repo>` (**metadata only** — `prs`/`issues` summaries arrive empty until the lazy repo-scope query on view open), `notifications: Vec<Notification>` (**unread** global), `card_totals`, `snapshot_age: Option<Duration>`, `rate_limited_until`, `access_expires_at` | `crates/modules/github/src/service.rs:1706–1746`; docs/0007 D6, docs/0051 D3 |
| Freshness model: notifications poll 60 s; global snapshot 5 min; repo-scoped and detail data only on view open (one-shot GraphQL/REST); hard pagination caps (300 repos via cursor catch-up, 100 PRs/issues per repo, 1000 notifications) | docs/0029 §2 |
| Every dispatch already declares `origin: Origin` — required, `pub(crate)`, compiler-forced at each dispatch site; stamped into `UsageRecord` as `surface`/`trigger` | docs/0053 D2; `FetchSpec` at `crates/modules/github/src/service.rs:856`; `Surface` at `usage.rs:449`, `Trigger` at `usage.rs:483` |
| `UsageRecord` already carries what usage analytics needs: `op`, `surface`, `trigger`, `ident`, `status`, `etag_304`, `cost`, `remaining_after`, `limit` | `crates/core/src/usage_records.rs:69–94`; docs/0054 D14 |
| Read contract for usage data is already headless-shaped: `UsageSource::{records_between, records_since, high_water, snapshot}` — window filter storage-side, aggregation in core's pure functions (one implementation) | docs/0054 §8 D9 |
| The counting seam wraps the single shared client, so *any* new dispatch path is metered for free | `crates/modules/github/src/lib.rs:110–124` (`usage::new_ledger`, `CountingHttp::wrap`) |
| Token lifecycle (8 h refresh, single-flight 401 gate, FIFO retry) is automatic for any consumer of the state entity — agents inherit it, never see tokens | docs/0028; `service.rs:1727–1736` |

Consequence of the freshness rows: a phase-1 `repo_prs` op is **honest only
if it can return "not held"** — the snapshot inventory deliberately does not
contain PR summaries until a repo view (or phase-2 agent op) triggers the
lazy scope query. The envelope's `freshness.source` values are therefore
`disk` (seeded cache), `cache` (landed session state, with `age_s`), and —
in phase 2 — `live`.

## 5. Design decisions

**D1 — product crate `crates/agent`, own socket; not a debug verb family.**
The debug bridge is `debug-tools`-gated and dev-only; agent access is a
product feature that must run in release builds and deserves its own trust
story. `crates/agent` holds: `protocol.rs` (typed serde request/response),
`server.rs` (in-app listener — the docs/0010 pattern, token `agent.token`),
`ops.rs` (the module-agnostic dispatch seam), and `bin/dashboard-agent-mcp.rs`
(stdio MCP front-end + `--call OP JSON` passthrough for human checks).
Socket: `runtime_dir/agent.sock`, same 0700/0600 discipline, stale-socket
removal, per-run token.

**D2 — trust model: same-user socket + per-run token; file kill switch.**
Identical defense-in-depth shape to docs/0010 D2: the OS same-user socket is
the boundary; the per-run random token (read from `agent.token` by the MCP
binary, 0600) guards against confused-deputy local processes. A
config-file flag (`agent_bridge`, default **on**) disables the listener at
startup; no settings UI in v1 (no 0065 surface). Deliberately *not* a
pairing/approve ceremony in v1: it adds UI, state, and friction without
raising the bar above "your user account can already read the token file".
Revisit if a threat model appears that same-user gating doesn't cover.

**D3 — the bridge is generic; modules own their ops.** `crates/agent`
depends on `dashboard-core` only — no github dependency. Modules register an
ops provider at init (the `SettingsProvider`/card-provider registration
pattern), handing over the entity handles they already clone around
(`lib.rs:118`). Dispatch: bridge validates + routes by op prefix to the
registered provider; the provider's closure runs on the main thread via the
job queue and updates state exactly as a view would. Adding module #2 later
(forgejo/gitea per docs/0053 D6 naming) is additive.

**D4 — phase boundaries are trust boundaries.** Phase 1 performs **zero
network flights**: it reads held state only (inventory, unread
notifications, ledger, auth/quota status, disk cache ages). Security review
of phase 1 is therefore an exercise in "what can this read", not "what can
this trigger". Phase 2 adds explicit-fetch ops (`refresh: true`) which
dispatch through the module's existing one-shot paths with
`Surface::Agent`/`Trigger::Agent` (D6) — quota-metered, attributable.
There is no write phase: mutations are ruled out of scope entirely (owner
decision 2026-10-06) — the bridge never calls a mutating path.

**D5 — mandatory envelope metadata on every response.** Every success
result carries:

```json
"freshness": {"age_s": 214, "source": "cache"},
"truncation": {"returned": 87, "total_known": 87}
```

`age_s` = seconds since the data landed (from `snapshot_age` /
session-record timestamps / `UsageSource.high_water`-adjacent stamps);
`source` ∈ `disk | cache | live`; `truncation.total_known` is `null` when
unknowable (e.g. repos beyond the docs/0029 caps). Ops return structured
"not held" (`repo_prs` with no session record) rather than empty arrays, so
an agent can distinguish *nothing open* from *never fetched*.

**D6 — agent attribution in the usage taxonomy.** Add `Surface::Agent` and
`Trigger::Agent` to the github module enums (`usage.rs:449/:483`). The
compiler's existing forcing of `FetchSpec.origin` (docs/0053 D2) means no
dispatch site can silently borrow a UI identity. Payoff: the usage page's
existing group-bys can show agent spend vs UI spend, `api_usage` (below)
exposes the same cut to agents themselves, and quota-aware behavior (phase
2: refuse fetches while `rate_limited_until` is in the future — the tick
loop's self-gating, extended to agent ops) comes free.

**D7 — closed op set; raw passthrough deferred.** v1 exposes the ops in §6
only. A raw GraphQL escape hatch is a *product* decision about quota abuse
and schema coupling, not an implementation detail — deferred with a bias
toward "never" unless a concrete agent workflow demands it (Open Questions).

**D8 — MCP front-end is a dumb, thin client.** `dashboard-agent-mcp` speaks
stdio MCP (JSON-RPC), connects to `agent.sock`, performs the `auth`
handshake with the token file, and maps one MCP tool per protocol op —
hand-written JSON Schemas unit-tested against the serde types (no new
dependency). Tool descriptions state freshness semantics ("reads the cached
snapshot, up to ~5 min old"). The same binary's `--call OP JSON` mode is
the human-check CLI (agent-bridge answer to `dashboard-debug` verbs), and is
what committed verification scripts will replay. Client wiring is out of
scope (owner decision 2026-10-06): the binary is a standard stdio MCP
server; any conforming client connects it as a plain command, nothing
app-side.

**D9 — stateless request/response; concurrent connections allowed.** No
server-side agent sessions to leak or clean up. Each connection
authenticates independently; the job queue serializes actual state access
onto the main thread, so ordering across clients follows job arrival. Long
ops don't exist in phase 1 (all reads are in-memory); when phase 2 fetches
arrive they resolve through the same background-run → `apply` path pages
use, and the bridge replies when the landed result is applied or times out.

## 6. Phase 1 protocol surface

Envelope: NDJSON, first line `{"op":"auth","params":{"token":"…"}}`, then
`{"v":1,"id":N,"op":…,"params":…}` → `{"id":N,"ok":true,"result":…}` /
`{"id":N,"ok":false,"error":{"code":"…","message":"…"}}`. Error codes
mirror the docs/0010 taxonomy: `PARSE`, `AUTH`, `UNKNOWN_OP`, `BAD_PARAMS`,
`NOT_CONNECTED`, `RATE_LIMITED`, `DISABLED` (kill switch off at startup →
socket never bound, CLI surfaces `DISABLED` with the config path).

| Op | Params | Reads | Notes |
|---|---|---|---|
| `app_status` | — | `auth`, `connected()`, `user`, `snapshot_age`, `rate_limited_until`, `access_expires_at`, ledger `high_water` | the "am I looking at live data?" op; no network |
| `list_repos` | `org?` | `repos` inventory (metadata) + `snapshot_age` | caps apply (0029 §2); `truncation.total_known` carries the cursor-catch-up caveat |
| `notifications_unread` | `repo?` | global (or repo-derived) unread list | honest name: the state holds **unread** only |
| `repo_prs` / `repo_issues` | `repo`, `state?` | session record if landed | else structured `not_held` + hint to use phase-2 `refresh` |
| `api_usage` | `from_ms?`, `to_ms?`, `group_by: op\|surface\|trigger\|status`, `window?` | `UsageSource::records_between` → core pure aggregation | `group_by: "surface"` is how agents report their own spend (D6) |

All five are pure reads of held state — implementable without touching a
single dispatch site, reviewable as "what can this see".

## 7. Phasing / delivery

1. **Phase 1 (this design's implementation PRs):** crate + server + github
   ops provider (§6) + MCP binary + `--call` mode.
2. **Phase 2:** `refresh: true` on `repo_prs`/`repo_issues`; `pr_detail`,
   `pr_checks`, `issue_timeline` via the lazy one-shot paths; `Surface`/
   `Trigger::Agent` variants land here (first flights exist to attribute).
3. **Later/deferred:** second module's ops; Windows transport; agent
   budget guardrails if phase-2 data shows need. No write phase — the
   bridge is read-only (Non-goals).

## 8. Testing & acceptance criteria

- Unit: protocol round-trip, auth failure closes connection, `DISABLED`
  never binds, ops against the module's existing test states
  (`MemorySink`/seeded caches) — freshness and truncation fields asserted
  on every response shape.
- Phase-1 flight-freedom: a test drives all five ops against a connected
  test state and asserts the recorder books **zero** records.
- Integration: app + `dashboard-agent-mcp --call` drive each op; scripted
  under `crates/agent/verify/*.txt` mirroring the debug-bridge receipt
  convention (replayable human checks).
- MCP conformance: the binary completes a standard stdio handshake and
  `tools/list` with an off-the-shelf MCP client; `--call` drives a
  `list_repos` + `api_usage` round-trip.
- **No UI surface** → 0065 audit/critique receipts N/A for phase 1 (stated
  in the implementation PR body); usage-page *rendering* of `Agent` rows
  (phase 2 follow-up) will need its own surface check.
- Gates as always: fmt, clippy (zero warnings), `cargo test --workspace`.

## 9. Open questions

1. **Kill-switch default:** proposal is `agent_bridge = true` by default
   (token-file-gated). Comfortable with on-by-default, or prefer opt-in?

Resolved on the design PR (owner, 2026-10-06): raw GraphQL passthrough —
**none**; mutations — **out of scope entirely**, the bridge is read-only;
client wiring — **out of scope**, any standard MCP client is config-only.

## References

- docs/0010 (debug bridge — socket/token/job-queue pattern reused), §3 protocol
- docs/0003 (module architecture — provider seam), docs/0005 (github module), docs/0029 (module state of the world), docs/0051 (card lanes), docs/0007 (global snapshot + lazy repo scope)
- docs/0050 D2 (counting seam), docs/0053 (usage ledger, Origin taxonomy), docs/0054 §8 (UsageSource read contract), docs/0058 (usage page)
- docs/0028 (token lifecycle), docs/0008 (logging), docs/0004 (state persistence)
