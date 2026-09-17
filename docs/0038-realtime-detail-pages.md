# 0038 — Realtime detail pages: engaged conditional polling

| | |
|---|---|
| **Issue** | #226 (deferred from docs/0032 §2 Non-goals) |
| **Status** | Design |
| **Depends on** | 0005 §3.3 (ETag transport), 0031 (fetch records / inflight dedup), 0032 D1 (heartbeat), 0036 (backoff), 0037 (freshness contract in core) |

## 1 Context

docs/0032 shipped background refresh for detail pages as a **poll** — the 60s
heartbeat plus a focus trigger — and recorded the ceiling as a decision
(#226): websocket/evented updates were "out of scope at this app's scale and
are not planned". Since then the app grew into its actual shape: a
**monitoring dashboard** (0036's recon called it — a second-screen surface the
owner watches rather than an editor they type into). For a monitoring app the
polling ceiling is felt where the eyes are: an open detail page that updates
only when the user alt-tabs. That is the gap #226 names.

Four more things changed since 0032 deferred this, and they change the answer:

1. **The freshness contract moved to core** (docs/0037): cadence policy is now
   data in one place per lane (`wants_refresh(min_age)`), not a hand-wired
   gate per surface. A second cadence tier is a policy change, not new
   machinery.
2. **Backoff exists** (docs/0036): any faster poll composes with per-lane
   failure delay automatically — the historical fear of "polling harder"
   hammering a broken endpoint is already answered.
3. **The ETag transport exists** (docs/0005 §3.3): conditional GET with
   `If-None-Match`, 304 handling, per-kind etag persistence — but only the two
   notifications *list* kinds (`Notifications`, `RepoNotifications`) actually
   store and revalidate etags today. Every other REST fetch (thread detail,
   PR files) refetches the full 200 every time.
4. **Rate-limit headers are parsed** (`RateLimit` on every `HttpResult`): the
   cost of any new cadence is observable in debug logs, not guessed.

### 1.1 Upstream survey — what "realtime" can mean for this app

Verified against GitHub's docs on 2026-09-17 (not from memory):

- **No push channel exists for a user-token client.** The GraphQL API has no
  subscription type (the schema has none; community discussion #120716 is the
  standing confirmation). Webhooks are a server-to-server mechanism: they
  require a publicly reachable receiver URL and, for repo webhooks, admin on
  the repository — an OAuth **user** token on a desktop app has neither. A
  public relay (smee.io-style) would import third-party infrastructure and a
  webhook-management surface to justify it.
- **GitHub's own guidance for exactly our situation** (REST best practices,
  "Avoid polling"): use webhooks *if you can*; if you cannot and must poll,
  then poll on a fixed schedule, make **authenticated conditional requests**,
  and respect `x-poll-interval` where an endpoint advertises one.
- **The load-bearing guarantee**: *"Making a conditional request does not
  count against your primary rate limit if a `304` response is returned and
  the request was made while correctly authorized."* Conditional polling of a
  REST surface is therefore **free at any cadence we would plausibly run**.
- **The GraphQL budget is separate and point-based**: primary limit 5000
  points/hour, cost derived from query complexity; the secondary-limit point
  table (1 point per read query) is what throttles request *rate*. Our
  per-poll cost is measurable from `x-ratelimit-used`, which the transport
  already surfaces.

### 1.2 Today's cadence surface

| Lane | Kind(s) | Transport | Conditional today? | Cadence today |
|---|---|---|---|---|
| Global notif rows | `Notifications` | REST | **yes** (etag stored, 304 → re-apply last-known) | 60s beat |
| Repo open pair | `RepoSnapshot`, `RepoNotifications` | GraphQL + REST | snapshot no; repo notifs yes | 60s beat, backoff-gated (0035/0036) |
| PR detail | `PrDetail` | GraphQL | never (no upstream path) | 60s heartbeat + focus, backoff-gated |
| PR threads | `PrThreads` | GraphQL | never | rides PR page refresh |
| PR files | `PrFiles` | REST | **no** (transport supports it; state stores no etag) | rides PR page refresh |
| Issue detail | `IssueDetail` | GraphQL | never | 60s heartbeat + focus, backoff-gated |
| Notification thread | `ThreadDetail` | REST | **no** (same) | 60s heartbeat + focus |
| Pickers | `RepoLabels/Assignable/Milestones` | REST | no | on first open, manual refresh |

## 2 Goals

- **G1** — An open detail page in a **focused window** updates itself when
  upstream changes, without user action: new comment/review on the open PR or
  issue, replies on the open notification thread, changed files on the open
  PR.
- **G2** — The REST-backed lanes get there at **zero rate-limit cost**
  (conditional 304s); the GraphQL lanes get there inside a **bounded,
  observable budget** (constants with recorded math).
- **G3** — Every existing gate composes unchanged: per-lane backoff
  (0036), the rate-limit/auth/pending pause (`gated()`), the nav gate, the
  focus trigger, inflight dedup (0031), and the 0037 freshness contract.
- **G4** — No new UI. The engaged tier is invisible except that the synced
  label stays current and new content appears. Zero new interactive elements,
  zero selector changes.

## 3 Non-goals

- **N1 — No webhooks, no relay, no GitHub App server.** Verified impossible
  for a user-token desktop client without new infrastructure (§1.1). If the
  product ever runs a server, that is a different design.
- **N2 — No check-run/CI surface.** The app fetches no checks today
  (no `Kind` for them). "Realtime CI" is the future CI-status feature's
  problem; it will inherit the engaged tier when it exists.
- **N3 — List surfaces stay at the idle cadence.** Card rows and repo rows
  (GraphQL `Snapshot`/`RepoSnapshot`) keep 60s: accelerating the repo-open
  GraphQL pair is expensive for surfaces nobody stares at continuously. The
  free-304 REST list lanes are a cheap future extension — deliberately not in
  this scope (Q1).
- **N4 — No `x-poll-interval` capture yet.** The endpoints this design
  accelerates (`/notifications/threads/{id}`, `/pulls/{n}/files`) do not
  advertise a poll interval; the notifications *list* does (60s, already
  respected by the idle beat). If a future engaged endpoint advertises one,
  the transport should floor its cadence on it — recorded, not built.
- **N5 — No interaction-recency requirement.** Engagement is window focus,
  not keystrokes (D2) — this is a monitoring app; watched-but-idle is the
  product.

## 4 Decisions

### D1 — Mechanism: engaged conditional polling. Push is rejected on evidence.

"Realtime" for this app = **tiered polling**: the surfaces the user is
watching poll faster; everything else is unchanged. The evidence for
rejecting push is in §1.1 and is worth keeping in the doc: the deferral in
0032 was an assumption ("out of scope at this app's scale"); this is now a
verified upstream fact — there is no socket to subscribe to, and GitHub's
sanctioned fallback for exactly our constraints is authenticated conditional
polling.

### D2 — Engagement = window active ∧ detail page open ∧ module owns nav

The engaged tier arms when **all** of:

- the app window is active (`window.is_window_active()` — verified available
  in pinned gpui, docs/0036 recon);
- a detail page is open (`pr` / `issue` / `notification` is `Some`) — the
  module root and repo view stay on the idle tier;
- the github module owns the screen (the existing nav gate in
  `refresh_open_page`).

One window, single-user: focus is a reliable "someone is watching" signal,
and the monitoring use-case (N5) means we do not additionally require recent
input. Losing focus drops the page back to the idle tier automatically —
no separate teardown path.

### D3 — Cadence: re-tick the heartbeat to 10s; per-lane min_age becomes policy

The page heartbeat currently wakes every 60s and passes that same 60s as
`min_age`. It becomes:

```
HEARTBEAT_TICK      = 10s   (the wake; nothing dispatches unless a lane wants)
ENGAGED_REST_MIN_AGE  = 10s  (ThreadDetail, PrFiles — 304s are free, §1.1)
ENGAGED_GQL_MIN_AGE   = 30s  (PrDetail, PrThreads, IssueDetail — paid, D6)
IDLE_MIN_AGE          = 60s  (every lane when disengaged — today's behavior)
```

- The wake ticks at 10s always; **all dispatch decisions stay in the lanes**
  (`wants_refresh(min_age)`): an idle app does six no-op policy checks per
  minute and nothing else. No second timer, no dynamic sleep — the tick is
  the only clock, and every cadence is an exact multiple of it (no
  quantization drift: REST-engaged fires every wake, GraphQL-engaged every
  third, idle every sixth).
- `min_age` selection is a pure function — `lane_min_age(transport_class,
  engaged)` beside the existing constants in `page.rs` — with a table test.
  Cadence is a module data concern (the 0037 Q2 ruling): the core freshness
  contract is untouched; `min_age` was already its cadence input.
- The focus trigger is unchanged (`FOCUS_MIN_AGE = 15s` on window
  activation): it remains the *immediate* path after sleep/away; the engaged
  tier is the *sustained* path. No conflict — activation fires only on the
  inactive→active edge.

### D4 — 304 semantics for page-owned REST kinds: "unchanged landing"

Extending conditional revalidation to `ThreadDetail` and `PrFiles`:

- **Storage**: widen the etag save in `GithubState::apply` from the two list
  kinds to `{Notifications, RepoNotifications, ThreadDetail, PrFiles}` —
  `etag_key(kind, ident)` is already kind-generic; nothing else moves.
- **Landing**: page-owned kinds parse their own 200 bodies
  (`apply_detail_result` & siblings). Today a 304 falls into apply's
  catch-all and is silently dropped — data is unchanged (correct) but the
  lane's freshness clock never advances, so an engaged REST lane would
  re-fetch at full 200 every time the age gate expires. The contract: **a
  304 lands as an unchanged payload** — the lane's `Fetch` record stamps
  `fetched_at = now` and zeroes `fail_streak`, the page does not re-parse or
  re-render, `rows_landed`/backoff clocks are untouched. This is the same
  semantic the list kinds already get from apply's 304 restore arm ("A
  landed sync owns freshness now", docs/0034 D2) — extended to the pages.
- **GraphQL lanes never see a 304** (no conditional path upstream); their
  engaged polls always pay points and always re-land.

### D5 — The gates compose unchanged (explicitly, because "faster polling" is where they'd break)

- **Backoff (0036)**: `wants_refresh` composes `backoff_elapsed` at every
  engaged call-site as before. A failing lane doubles its delay per failure
  to the 10-minute cap *regardless of engagement* — the engaged tier cannot
  hammer a broken endpoint; it just makes healthy endpoints current.
- **The pause**: `gated()` (rate-limit window, auth loss, pending proactive
  refresh) silences the heartbeat at the top of `refresh_open_page` — the
  engaged tier inherits the silence for free.
- **Inflight dedup (0031)**: an engaged dispatch racing a manual refresh (or
  a slow response) skips — the guard exists per lane.
- **First-beat semantics (0032)**: pages fetch on open; the engaged tier only
  changes the *re*-fetch cadence.

### D6 — GraphQL budget: 30s engaged, cost observable, constant is the knob

Worst realistic case: a PR page pinned open in a focused window for an hour —
`PrDetail` + `PrThreads` at 30s = 120 dispatches/hour/lane. Our read queries
are small (the secondary table charges 1 point per read query; the primary
complexity cost is measurable per response from `x-ratelimit-used`, already
parsed into `RateLimit` logs). Even at a pessimistic 8 primary points per
detail pair that is ~1 000 of 5 000 points/hour — under 20% of the budget for
the hottest possible usage, against a 60s-idle cost of ~15% of that. 15s was
considered and rejected: it doubles the burn for a latency improvement
(30s→15s) that no watcher can perceive (Q2 for the owner's taste).

The implementation PR records the *measured* point cost per engaged query in
§8 — if reality surprises, `ENGAGED_GQL_MIN_AGE` is the one-line correction.

## 5 Testing

- **Cadence table test**: `lane_min_age` over `{rest, gql} × {engaged, idle}`
  — constants pinned (D3).
- **Engagement gate test**: page open × window active × nav-ownership matrix;
  disengaging (window inactive) falls back to idle without dispatch churn.
- **304-lands-unchanged**: MockHttp scripted 304 on `ThreadDetail` /
  `PrFiles` — freshness advances (`fetched_at` moves, `fail_streak` stays 0),
  no re-parse, the etag persists to the store and rides the next spec.
- **Backoff beats engagement**: a failing engaged lane's second dispatch is
  suppressed until the doubled delay elapses (extends the 0036 table with the
  engaged flag).
- **Dispatch counting**: the phases technique from #239/#241 — count scripted
  responses so the engaged tier's extra dispatches are pinned, not inferred.
- **No UI delta**: no new selectors; existing probe tests green unchanged
  (G4's proof, the same argument as 0037's byte-identical migration).

## 6 Open questions

- **Q1 — Should the free REST list lanes ride the engaged tier too?**
  (`Notifications`, `RepoNotifications` at 10s — zero rate-limit cost, would
  make the card's notification rows near-live when the window is focused.)
  Cheap and genuinely useful for the monitoring use-case; left out of scope
  only because this design is scoped to the issue's "detail pages". Answer
  "yes" = a follow-up constant, not new machinery.
- **Q2 — GraphQL engaged cadence: 30s (D6's pick) or 15s?** Double the burn
  for double the freshness; the owner's monitoring habits decide.

## 7 Shipped

Lands with the implementation PR (§8 there).
