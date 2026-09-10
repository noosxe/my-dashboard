# 0028 — GitHub Module: Immediate 401 Refresh with Retry Queue

- **Date:** 2026-09-10
- **Depends on:** 0011 (token lifecycle — this doc amends it), 0007 (fetching
  model), 0008 (event catalog), 0013 §D4 (status-bar units), 0003 §3.12
  (unfocused refresh)
- **Amends:** 0011 §4.3 (reactive trigger) and §4.5 (event rows) — from
  merge, **0028 is the authority for the 401/refresh reaction path**; 0011
  remains the authority for credential storage, the refresh grant, rotation
  safety, and the proactive lead.

## 1. Summary

Owner-directed UX fix. Today a 401 only *arms* `pending_refresh`, which the
tick loop drains at the **top of the next tick** — up to 60 s later — and a
successful refresh then triggers a full `needs_bootstrap` refetch instead of
retrying the request that failed. For an interactive dashboard (a click
opens a page, the page fetch 401s) that is seconds-to-a-minute of avoidable
dead time plus the wrong recovery strategy.

New behavior:

- The **first** API request that receives a 401 triggers the token refresh
  **immediately** — same task, no tick involvement.
- That request then joins a **FIFO retry queue as its first item**; every
  request that 401s while the refresh is in flight joins behind it.
- On successful refresh + install, the queue is drained in order: each
  queued request is re-dispatched with the new token and its result flows
  through the normal apply path. Pages get their data as if the 401 never
  happened.
- A **single-flight lock** guarantees concurrent 401s produce exactly one
  refresh call (rotation is single-use; a second concurrent grant with the
  same refresh token would burn the credential).
- Refresh failure surfaces immediately to the queued requests (existing
  designed states — `TokenRejected` for `Reauth`, network-error UX for
  `Transient`). Nothing waits for a tick, ever.

The proactive trigger (0011 §4.3.1) stays, but drains in the **same tick**
it armed in — the old top-of-next-tick drain hop is deleted.

## 2. Problem & evidence

- `service.rs` 401 handler (`apply`): arms `pending_refresh =
  Some(Reactive401)`; the drain block sits at the top of the tick loop, so
  the refresh starts up to one `TICK` (60 s) after the 401, and fetches are
  gated off for that whole window.
- On refresh success, `install_credential` sets `needs_bootstrap = true` —
  a full background refetch on the next tick — while the request that
  actually 401'd (typically a page detail fetch the user is staring at) is
  never retried; the page shows its error state until the user retries by
  hand.
- The proactive drain has the same next-tick hop: `plan_tick` arms, the
  drain block runs at the top of the *following* tick.
- Owner report: unacceptable for an interactive app; the dashboard is used
  in the foreground, not as a background poller.

## 3. Design

### 3.1 Interception point: `run_spec_tracked`

All API traffic already funnels through one function — `run_spec_tracked`
(service.rs; tick loop, `page.rs::run_specs`, and `run_mutation` all go
through it, docs/0013 D4 comment). The 401 gate wraps **there**, on the
completed `SpecResult` (`network.is_none() && status == 401`), not inside
the `Http` transport:

- the transport stays dumb and bearer-blind to auth policy;
- the wrapper holds `&mut AsyncApp`, which the refresh's main-thread
  install step requires (§3.4) — a transport-layer interceptor could not
  reach the state entity;
- the device-flow OAuth POSTs (the only other `Http` callers, `auth.rs`)
  carry no bearer and cannot meaningfully 401 — they are naturally exempt.

One status-bar unit covers the whole run-⟲-refresh-⟲-retry cycle: `begin`
before the first dispatch, `end` after the final result settles (0013 D4
kept; no flicker across the refresh).

Mutations retry safely: a 401 means the request was rejected at the auth
layer — the mutation never executed server-side — so re-dispatch cannot
double-apply.

### 3.2 `AuthGate` — single-flight lock + FIFO queue

New `AuthGate` (in `service.rs`; plain data + `std::sync` locks, fully unit
testable without gpui), created in `lib.rs` beside the token slots, stored
on `GithubState`, threaded into `run_spec_tracked`:

```rust
pub(crate) struct AuthGate {
    st: Mutex<GateState>,      // std Mutex; never held across an await
}
struct GateState {
    generation: u64,           // bumped on every install / sign-out / reset
    inflight: bool,            // a refresh is running (the Lead)
    queue: VecDeque<oneshot::Sender<GateOutcome>>, // 401 waiters, FIFO
    exhausted: bool,           // current credential's chance is spent
}
pub(crate) enum GateEntry {
    Lead,                      // you are first: run the refresh, then complete()
    Wait(oneshot::Receiver<GateOutcome>),
    RetryFresh,                // token rotated while your request was in flight
    Surface,                   // no refresh possible/worth it: return the 401
}
pub(crate) enum GateOutcome { Refreshed, Surface }
```

`on_401(used_gen, round_spent) -> GateEntry`, under the lock, in order:

1. `used_gen != generation` → `RetryFresh` — an install completed after
   this request dispatched; rotation killed its token. Re-dispatch with the
   current token, no refresh, nothing spent.
2. `round_spent` → `Surface` — this run already consumed one refresh round
   and 401'd again on the fresh token (§ Q2).
3. no refresh token in slot, or `exhausted` → `Surface`.
4. `inflight` → push a oneshot onto the queue → `Wait(rx)`.
5. otherwise → `inflight = true`, push own oneshot as **queue item #1** →
   `Lead`.

The **Lead** then (all outside the lock):

1. clones the client + reads the refresh-token slot (main thread, as the
   tick drain does today);
2. runs `auth::refresh` on the background executor;
3. applies the outcome on the main thread (`state.update` →
   `apply_refresh` / `install_credential` — persist-before-publish
   unchanged, docs/0011 §4.4);
4. calls `gate.complete(outcome)`, which under the lock: on `Refreshed`
   bumps `generation`, clears `exhausted`/`inflight`; on `Reauth` sets
   `exhausted`, clears `inflight`; on `Transient` clears `inflight` only
   (chance unburnt); then pops the queue **FIFO**, completing each
   oneshot (`Refreshed` or `Surface`).

Wake order is FIFO, so re-dispatch order is FIFO — the original 401
receiver first, exactly as specified. The executor may interleave the
actual re-dispatches; dispatch order, not completion order, is the
guarantee (Q5).

A Drop guard on the Lead's completion token drains waiters with `Surface`
and clears `inflight` — a cancelled/torn-down Lead cannot hang the queue.
The queue is bounded by the number of in-flight runs; no artificial cap.

`sign_out` calls `gate.reset()` (generation bump, `exhausted = true` until
the next connect, waiters drained with `Surface`) — no token use after the
slots clear, matching today's "sign-out raced the drain" handling.

### 3.3 Wrapper flow (`run_spec_tracked`)

```
status unit begins
gen = gate.generation(); rounds = 0
result = bg(run_spec)
while result is 401:
    match gate.on_401(gen, rounds > 0):
        RetryFresh      => { gen = gate.generation(); result = bg(run_spec); continue }
        Lead | Wait(rx) => match await (Lead: after running §3.2's steps) { rx / own outcome }:
            Refreshed   => { gen = gate.generation(); rounds += 1; result = bg(run_spec); continue }
            Surface     => break
        Surface         => break
status unit ends; return result
```

On `Transient` the Lead's `complete` marks the outcome `Surface` for
waiters **and** rewrites their result into a network-error `SpecResult`
(`network = Some("token refresh unavailable: {m}")`) before returning — a
transient refresh failure must land in the existing network-loss UX
(stale data / page-local "Network error"), never in `TokenRejected`
(0011 Q5 preserved). The gate stays armed: the next request — click or
tick — retries the refresh immediately.

### 3.4 State-machine integration

- `apply`'s 401 handler collapses to: warn + `AuthStatus::TokenRejected`.
  A 401 reaching `apply` now *means* the gate declined (legacy credential,
  spent chance, or per-run round spent). The reactive arming, the
  `pending_refresh`/`Reactive401` machinery on that path, and the state
  field `refresh_attempted` are deleted; attempt accounting lives in the
  gate (`exhausted`), cleared by install, set terminally by `Reauth`.
- Proactive: `plan_tick` arms exactly as today (0011 §4.3.1, before the
  gates, lead 600 s). The tick loop now drains **immediately after
  planning** through the same gate (`gate.refresh_now() -> Lead | Wait |
  Skip`: join an in-flight refresh, lead one, or skip when spent/absent),
  and on success **re-plans in the same tick** — the bootstrap runs now,
  not one tick later. The old top-of-tick drain block is deleted.
- `install_credential` is unchanged (keychain first, slots, expiry, auth,
  `needs_bootstrap = true`): targeted retries fix user-visible latency;
  the bootstrap stays as the background catch-up for tick data.
- Single-writer discipline unchanged: every state write still goes through
  `state.update` on the main thread ending in the §3.12 refresh; HTTP
  still only on the background executor.

### 3.5 Logging (0008 event catalog additions)

| Area | Event | Level |
| --- | --- | --- |
| github auth | 401 → refresh leading (kind, queue depth) | debug |
| github auth | refresh ok — retrying N queued request(s) | info |
| github auth | refresh reauth — surfacing auth failure to N request(s) | warn |
| github auth | refresh transient — surfacing network error to N request(s) | warn |
| github auth | 401 → joined in-flight refresh (queue depth) | debug |

No token material, ever (0011 §4.5 rules carry over).

## 4. Design decisions

Small-Q format per house style; silence rule on merge.

- **Q1 — Gate at `run_spec_tracked`, not inside `Http`.** One choke point
  already exists and is documented as such; the install step needs
  `AsyncApp` for main-thread state writes; the transport stays policy-free.
  The device-flow POSTs are the only `Http` callers outside the choke point
  and carry no bearer.
- **Q2 — Bounds: one refresh round per run, one attempt per credential.**
  A run that 401s again *after* a successful refresh+retry surfaces (no
  per-run loop); a credential gets one attempt until an install (new
  generation, fresh budget) or a terminal `Reauth` (spent until reconnect).
  The only unbounded-looking path — GitHub repeatedly issuing valid grants
  whose tokens then 401 — is a server-side bug, and revocation kills both
  tokens (`Reauth`) long before it could matter.
- **Q3 — Generation check before spending anything.** Rotation kills the
  old access token, so in-flight requests dispatched pre-refresh will 401
  post-refresh. They re-dispatch with the current token (`RetryFresh`)
  instead of triggering a pointless second refresh — which, being
  single-use, would burn the just-installed refresh token.
- **Q4 — `Transient` fails fast, no in-gate retry loop.** Waiters get a
  network-error result immediately (existing stale-data UX), the chance is
  unburnt, and the next interaction retries. No silent waits anywhere —
  the owner's complaint was exactly a silent 60 s wait.
- **Q5 — FIFO dispatch, not FIFO completion.** The queue drains in 401
  order (original receiver first); the executor may interleave executions.
  Serialization would buy nothing — the requests are independent.
- **Q6 — One status-bar unit per logical run.** The unit spans the retry
  cycle; a refresh inside a fetch is not a second unit (0013 D4).
- **Q7 — Proactive drains same-tick.** Same gate, called right after
  planning; success re-plans immediately. Removes the identical next-tick
  hop the old drain had; `Transient` re-arms on the next tick as today.
- **Q8 — `std::sync::Mutex` + `futures` oneshots.** Matches the codebase's
  existing lock usage; critical sections are lock-take-decide-release and
  never span an await; waiters own their oneshot receivers.

## 5. Tests (unit, scripted `MockHttp` + `MockTokenStore` + real `AuthGate`)

1. **Single-flight:** two concurrent 401 runs → exactly one refresh call in
   the recorded script; both runs settle with their retried results.
2. **FIFO + first-item:** three staggered 401s → wake/re-dispatch order
   equals 401 order; the first 401's request is re-dispatched first.
3. **Stale generation:** a 401 landing after an install re-dispatches with
   no refresh call and nothing spent.
4. **Per-run round:** a retried run that 401s again surfaces the 401; no
   second refresh for that run.
5. **`Reauth`:** waiters surface the 401 → `apply` → `TokenRejected`; the
   gate is spent — later 401s surface with no further refresh calls.
6. **`Transient`:** waiters complete as network errors (no
   `TokenRejected`); gate unspent; a following 401 leads a refresh again.
7. **Legacy credential (no refresh token):** 401 surfaces immediately →
   `TokenRejected` (0011 §6.2 updated).
8. **Sign-out mid-refresh:** waiters drained via `Surface`, slots cleared,
   no token use post-clear; gate resets for the next connect.
9. **Mutation retry:** a 401'd mutation re-dispatches after refresh
   (script: 401, refresh-200, mutation-200) and settles.
10. **Status unit:** exactly one begin/end pair per logical run across a
    retry cycle.
11. **Proactive same-tick drain:** armed plan → refresh + re-plan within
    the same tick (no top-of-next-tick hop).
12. **Drop guard:** a Lead dropped before `complete` drains waiters with
    `Surface` and clears `inflight` (no hang, gate reusable).

## 6. Compatibility

| Scenario | Old (0011 §4.3) | New (0028) |
| --- | --- | --- |
| 401 with refresh token | arm flag, drain ≤ next tick, bootstrap refetch | immediate refresh, FIFO retry of the exact 401'd requests |
| 401 while refresh in flight | n/a (no in-flight refresh existed) | join queue, retry after install |
| 401 after rotation (in-flight race) | n/a | re-dispatch with current token, nothing spent |
| Refresh `Reauth` | `TokenRejected` (next tick) | same state, immediate |
| Refresh `Transient` | keep flag, retry next tick, fetches gated | surface network error now, retry on next interaction |
| Legacy / no refresh token | 401 → `TokenRejected` | unchanged |
| Proactive | arm, drain top of next tick | arm, drain same tick, re-plan |

Keychain format, rotation safety (persist-before-publish), secret-handling
rules, and the proactive lead are untouched (0011 remains authority).

Docs updates that land with the **implementation** PR (0011 §7 precedent):
0011 gains an "amended by 0028" banner on §4.3/§4.5; 0008 §4.5 catalog rows
above; 0005 Q9 needs no edit (a surfaced 401 still ends in
`TokenRejected`).

## 7. Non-goals

- No refresh while the app is not running (0011 non-goal, unchanged).
- No UI changes: happy path still silent; failures still land in the
  designed `TokenRejected` / network-error renderings.
- No cross-request coalescing of identical specs — queue items retry
  independently.
- No background retry loop for `Transient` beyond the next interaction.
- No change to the proactive lead, the credential document, or the OAuth
  App configuration.
