# 0076 — Live check logs: progressive per-job availability

Design for #585. Extends the docs/0039 checks surface: expanding an
**in-progress** Actions check today fails with `GitHub answered HTTP 404`
(the job-logs blob does not exist yet). This design replaces that dead end
with an on-demand live watch: step-level progress while the job runs, and
the log tail landing automatically the moment the job completes.

**Constraint (owner-set): official APIs only.** The github.com web UI's
live log view runs on internal endpoints; they are out of scope by
decision, which rules true streaming out entirely (research verified
2026-10-16, see §2). What remains — and what we build — is *progressive
per-job availability*.

## 1. Summary

- Expanding a check whose resolved job is not completed enters a **live
  watch**: a poll loop on the page's spawn that re-fetches the run's job
  list (~10 s tick; one GET carries every job's embedded `steps[]`) and
  renders the matched job's step rows live.
- The moment the matched job flips `completed`, the loop fetches its logs
  (the existing step 4) and lands the pane into the exact docs/0039 D4
  tail-truncated view. A 404 there is *materialization pending*, retried
  on the next tick — never surfaced as an error while retries remain.
- **On-demand only**: the loop exists only while the user has the check
  expanded. Collapse, navigating away, sign-out, landing, or a hard tick
  cap each disarm it. No background polling, no cadence change anywhere
  else.
- Officially verified semantics this rides on (§2): logs materialize **per
  job, at job completion** — run completion is not required — and job/step
  status is live in the run-jobs payload.

## 2. Facts & constraints

- **No official streaming.** REST and GraphQL expose no log-tail/SSE/
  WebSocket endpoint for Actions logs. The official ceiling is polling.
- **In-progress job logs are 404** — probed live (2026-10-16,
  `microsoft/vscode`, `astral-sh/uv`): `GET /actions/jobs/{id}/logs`
  answers `404 The specified blob does not exist` while the job runs.
- **Completed-job logs exist immediately, per job, inside a still-running
  run** — probed on `astral-sh/uv` (36-job run): a `completed` job's logs
  answered `200` while sibling jobs were still running; a running job's
  answered `404`. The run-level zip endpoint adds nothing for us.
- **Step status is live and embedded**: `GET /repos/{o}/{r}/actions/runs/
  {run_id}/jobs` returns each job with `status`, `conclusion`,
  `started_at`, `completed_at` and a `steps[]` array (`queued`/
  `in_progress`/`completed` + per-step timestamps). This is exactly the
  `gh run watch` data path.
- **Log materialization can lag job completion briefly** — hence D3's
  404-as-pending with bounded retries rather than instant error.
- **Rate limits are a non-issue at this cadence**: core budget 5,000/h;
  a 10 s tick for a 15-minute watch is ~90 requests. The accordion
  (docs/0039 D4, one row expanded) caps concurrent watches at exactly one.
- **The current chain already reaches the right place**: docs/0039 D1's
  four steps resolve (head SHA + name) → check run → suite → run →
  `run_jobs_url` name-match → `job_logs_url`. Today step 4's 404 fails the
  chain; the watch slots *between* steps 3 and 4.
- **`JobNode` is `{id, name}` today** (api.rs) — the live payload fields
  must be added to it (`#[serde(default)]`, the module's additive-parse
  convention).
- **docs/0039 D5 says cached log content is immutable** — true only for
  completed jobs. The live watch is the explicit carve-out: `Live` state is
  mutable by design until the first `Ready`, after which immutability
  resumes (a completed job's logs never change).

## 3. Design

### D1 — On-demand scope: armed by expand, disarmed by collapse

The watch exists only inside the expanded check-log pane's lifetime. It
arms in `toggle_check_log`'s existing chain start when the resolved job is
not `completed`, and disarms on: collapse (re-toggle or another row
expanding — the accordion allows one), the page dropping (the spawn's
`this.update` fails and the loop exits), sign-out (auth gate checked at
each tick head), landing (D3), and the hard tick cap (D3). Nothing else in
the app polls more often than today; the docs/0072 D10 no-polling stance is
intact because the user's explicit expand *is* the interaction.

### D2 — The watch loop and the `Live` pane state

`CheckLogState` gains a variant (pr_page.rs):

```rust
Live {
    steps: Vec<CheckStepRow>,   // matched job's steps: name, state, started/completed
    updated_at: Instant,        // last successful tick — "updated Xs ago"
}
```

`CheckStepRow` derives from the tick payload; `CheckState` (gql.rs) maps
REST step status/conclusion via a small REST→`CheckState` fn mirroring the
existing `check_run_state`.

Loop shape (page-owned, mirroring docs/0039 D6 "the page drives the
chain"): after step 3 name-matches the job, if `status != "completed"` the
chain does **not** fail into step 4 — it enters the loop:

- every ~10 s (const, D6): `run_jobs_url` re-fetch (one GET); re-match the
  job **by name** (the docs/0039 D1 sanctioned iterate-and-match; matrix
  expansions included); update `Live.steps`; `cx.notify()`.
- a name-match **miss** means the world changed under us (re-run replaced
  job ids, renamed job): fail into the existing D5 inline-error + Retry
  path — Retry re-resolves from step 1 and naturally picks up new ids
  (docs/0039 D5's own rationale).
- `status == "completed"` (any conclusion — cancelled and timed-out jobs
  have logs too): leave the loop, run step 4 (D3), land `Ready`.

### D3 — Progressive landing: 404 is pending, not error

Step 4 inside the watch retries: `404` → next tick (the blob may lag
completion briefly), up to 5 consecutive misses → then, and only then, the
existing inline-error + Retry path. Network errors follow the same
discipline: skip the tick, 3 consecutive → fail into Retry. A hard cap of
90 ticks (~15 min) ends the watch with the inline error naming what
happened ("still running — reopen for live view"); the user re-expands to
re-arm. Landing swaps `Live` → the existing `Ready { text, truncated }`
(512 KiB tail, docs/0039 D4) and keeps the scroll-to-bottom behavior;
`Ready` immutability resumes (D5 carve-out closed).

### D4 — The live pane

Same monospace pane, docs/0017 code-block styling, docs/0039 D4 as the
surface anchor (rhythm and restraint, not a new visual language): a header
line (`Run in progress — logs appear when the job finishes · updated Xs
ago`), then one row per step: state glyph (✓ success, ✗ failure/skipped
with its conclusion, spinner for `in_progress`, dim `queued`), step name,
elapsed (`completed_at − started_at`; the in-progress step counts from
`started_at` to the last tick — no per-second timer; the poll tick is the
clock). On landing, the pane swaps to the log tail and scrolls to bottom as
today. Debug ids per docs/0010 D10: `check-step-{idx}` on rows, pane keeps
`check-log-pane-{idx}`.

### D5 — In-flight discipline

The watch holds the existing `(Kind::JobLog, ident)` in-flight arm for its
whole lifetime (expand → disarm), so a refresh/second expand cannot stack a
second loop on the same check; `in_flight_clear` runs on every disarm path.
No etags: the payload changes every tick, conditional requests would only
add round-trips. The tick runs strictly after the previous tick settles
(no overlapping chains) — the loop is one async task with a sleep between
iterations, not a timer fan-out.

### D6 — Cadence and caps (owner-tunable consts, defaults proposed)

| Knob | Default | Rationale |
|---|---|---|
| Tick interval | 10 s | ~90 requests per 15-min watch of a 5,000/h budget; step granularity users perceive as live |
| Materialization retries (404) | 5 | blob lag is seconds, not minutes; 5 ticks ≈ 50 s of grace |
| Consecutive network failures | 3 | transient blips tolerated, real outages fail fast into Retry |
| Hard watch cap | 90 ticks (~15 min) | our longest CI job is ~15 min; beyond it the watch is a zombie |

### D7 — Architecture placement

- **api.rs**: no new URL builders (`run_jobs_url`, `job_logs_url` exist).
  `JobNode` gains `status`, `conclusion`, `started_at`, `completed_at`,
  `steps` — all `#[serde(default)]`/`Option` (additive parse; older test
  fixtures keep compiling).
- **service.rs**: split `run_check_log_chain` — steps 1–3 stay the
  *resolve* half, returning `(run_id, job_id, JobNode)`; step 4 becomes
  `fetch_job_logs(...)`; the loop itself stays page-owned (docs/0039 D6),
  calling both helpers per tick. Non-200s still forward through
  `apply()` for shared auth/rate-limit handling (docs/0039 D1's rule).
  `Kind::JobLog` and `Trigger::Expand` are reused — same surface, same
  trigger, no vocabulary growth.
- **pr_page.rs**: `CheckLogState::Live`, the loop, the render arm, disarm
  hooks in `toggle_check_log`/page teardown. No `dashboard-core` changes,
  no new dependencies; nothing enters the docs/0072 store (logs stay
  page-local per docs/0039 D5).

### D8 — Non-goals

- **True streaming / log tailing** — impossible via official APIs; the web
  UI's internal endpoints are excluded by owner decision (#585).
- **Log persistence** — docs/0039 D5 keeps logs page-local; docs/0072's
  store is for documents, not 512 KiB tails.
- **Third-party (non-Actions) checks** — no Actions logs exist for them;
  today's "No GitHub Actions check with this name" error stands.
- **Run-level rollup view** (all jobs of the run in one pane) — the pane's
  scope is the expanded check; a run view is a different surface.
- **Per-step log fetching** — the official step-logs endpoint exists, but
  v1 lands whole-job logs; selective retrieval is a follow-up if wanted.
- **Run cancel/re-run or approvals** — docs/0066 owns the gated-run
  interactions.

## 4. Owner decisions

Proposed defaults, to be confirmed (or adjusted) on the design-doc review:
tick **10 s**; 404 materialization retries **5**; consecutive network
failures **3**; hard watch cap **90 ticks**; **no** etag use; **no** log
persistence. The on-demand-only scope (D1) is already owner-set (#585).

## 5. Implementation plan

Issue #585, one PR after this doc merges:

1. api.rs `JobNode` fields + fixtures; service.rs resolve/fetch split with
   the chain's completed-path behavior byte-identical to today.
2. pr_page.rs `Live` variant + loop + disarm hooks; render arm (D4).
3. Tests (§6); bridge receipts + live-pane golden (0065); docs/0039 §7
   cross-note ("in-progress checks: see docs/0076") + this doc's status.

## 6. Testing & human checks

- **Unit** (service/pr_page patterns, mocked JSON): resolve-half returns
  the job for in-progress and lands logs directly for completed (today's
  path unchanged); REST→`CheckState` mapping; 404-retry-then-land;
  name-match miss → Failed; hard cap → Failed; `JobNode` additive parse
  (payload without new fields still parses).
- **UI integration** (pr_page test pattern): `Live` renders step rows and
  header; auto-swap `Live` → `Ready` on landing with scroll-to-bottom;
  collapse disarms (second expand re-arms); accordion keeps one watch.
- **Bridge receipts** (implementation PR, docs/0065/0071): audit over the
  touched surface, `tree --tab-stops`, `critique` on the live-pane state
  (screenshot + tree, max 3 iterations), new structural golden for the
  live pane captured in the containerized env, `assert-baseline` for any
  committed golden of the checks surface.
- **Human checks**: push a scratch branch that runs a multi-minute job,
  expand the in-progress check in-app, watch steps tick and logs land on
  completion; re-run the workflow while watching and confirm the
  name-match-miss error + Retry lands on the new run's logs; confirm a
  completed check expands instantly as today.

## 7. Follow-ups (issues, not this design)

- Per-step log retrieval (official step-logs endpoint) if the whole-job
  tail proves unwieldy on matrix-heavy runs.
- Reactions on the run level (cancel/re-run buttons) — only if the owner
  wants run control in-app; docs/0066 adjacency.
