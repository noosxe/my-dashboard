# 0039 — Check-run log viewing in the PR checks tab

| | |
|---|---|
| **Issue** | #201 (deferred from docs/0029 §5 Tier 3) |
| **Status** | Design |
| **Depends on** | 0016 (PR detail page / checks tab), 0029 §3 (tier model), 0031 (fetch records / page-owned result convention) |

## 1 Problem

The checks tab (docs/0016) renders pass/fail rows for a PR's checks. A red
check tells the user *that* CI failed; reading *why* — the actual log output —
means leaving the app for github.com. The issue names the frequency: reading
CI logs is the most common reason to open the browser from this app.

Scope from #201: fetch check-run logs via the API, show them in a scrollable
monospace pane when a check run is expanded, and handle large logs without
hanging the UI.

## 2 Upstream facts (verified 2026-09-17, docs.github.com, apiVersion 2022-11-28)

Everything below is quoted or condensed from the pinned docs pages; the
design builds on no undocumented contract.

1. **Job logs endpoint** — `GET /repos/{owner}/{repo}/actions/jobs/{job_id}/logs`:
   *"Gets a redirect URL to download a plain text file of logs for a workflow
   job. This link expires after 1 minute. Look for `Location:` in the response
   header to find the URL for the download."* Status code: `302 Found`. Auth:
   classic OAuth/PAT needs `repo` for private repos (we have it — docs/0005);
   fine-grained tokens need Actions read; public repos need no auth. The
   endpoint takes a **workflow job id**, not a check-run id.
2. **Job listing** — `GET /repos/{owner}/{repo}/actions/runs/{run_id}/jobs?per_page=100`
   (100 is the documented max, default 30; further pages follow by
   `total_count` via `&page=N`, #757; `filter` stays `latest`):
   `200 OK`, `{total_count, jobs: [{id, name, conclusion, …}]}`.
3. **Run discovery by SHA** — `GET /repos/{owner}/{repo}/actions/runs` accepts
   `head_sha`: *"Only returns workflow runs that are associated with the
   specified head_sha"*. Also accepts `check_suite_id`. Response
   `{total_count, workflow_runs: [{id, name, …}]}`.
4. **Check-run listing** — `GET /repos/{owner}/{repo}/commits/{ref}/check-runs`:
   `200 OK`, `check_runs[]` with `id`, `name`, `check_suite_id`, `app`.
4. **Check-run listing** — `GET /repos/{owner}/{repo}/commits/{ref}/check-runs`
   ("List check runs for a Git reference"; `ref` may be a SHA): `200 OK`,
   `check_runs[]` with `name`, `check_suite: {id}`, `app: {slug}`, `status`,
   `conclusion`. Classic OAuth/PAT needs `repo` for private repos; public
   repos need no auth. There is also `check_name` as a server-side name
   filter — useful to pin the match at the API instead of client-side.
   GraphQL check run names its Actions job (community discussion #129314:
   the sanctioned approach is *"fetch the run details, then iterate over the
   jobs until we find the correct one"*). The `details_url` UI URL happens to
   embed `/actions/runs/{run_id}/job/{job_id}`, but that shape is a
   **documented nowhere** UI contract — this design does not parse it (D1).

## 3 Decisions

### D1 — Resolution chain: documented endpoints only, keyed by (head SHA, check name)

Expanding a check row runs a lazy, user-invoked chain of documented REST GETs:

1. `GET /repos/{o}/{r}/commits/{head_sha}/check-runs?check_name={name}` → the
   matching check run (server-side name filter; exact-name match survives
   matrix suffixes since Actions names check runs after their jobs) → take
   its `check_suite.id`. `head_sha` comes from the PR detail we already
   hold (`commits.nodes[0].oid`, the head commit).
2. `GET /repos/{o}/{r}/actions/runs?check_suite_id={id}` → the one workflow
   run for that suite → `run_id`.
3. `GET /repos/{o}/{r}/actions/runs/{run_id}/jobs` → match `name` exactly →
   `job_id`.
4. `GET /repos/{o}/{r}/actions/jobs/{job_id}/logs` → the log text.

Why not fewer calls by parsing `details_url`: the URL shape is undocumented
(§2.5); designing against it re-creates the exact class of breakage the repo
rules forbid ("never design against remembered API details" — same discipline
applies to observed-but-unpromised URL shapes). Four cheap GETs on an explicit
user action are the right price for a chain that is fully documented and
deterministic. Matrix jobs survive: Actions names both the job and the check
run `"build (linux)"` identically, so exact-name matching holds on both the
`check_name` filter and the step-3 job match.

Name-match failure (renamed job, non-Actions check with the same name) is a
degrade, not a crash: the pane shows an inline error with an "open on
github.com" link (D5).

### D2 — One-shot page-owned kind; the freshness system deliberately does not engage

New `Kind::JobLog` following the `PrFiles`/`ThreadDetail` convention
(docs/0031): the page dispatches, **the page parses the 200 body** (plain
text — nothing to `serde_json`), and only non-200 results reach
`GithubState::apply` for the shared auth/rate-limit handling.

No beat lane, no `wants_refresh`, no backoff interplay: a **completed** job's
log is immutable (a CI re-run creates a *new* run with new job ids — D5's
resolution step picks those up naturally). Fetching once per expand and
keeping the text is therefore not staleness-prone, and the freshness system's
core question ("when does this surface re-fetch?") has the degenerate answer
"never". Not etag-gated either — there is no conditional variant of the logs
endpoint.

### D3 — Expander gating: completed Actions-origin check runs only

The log pane's expander renders only when *all* of these hold:

- the row is a `CheckRun` union member (commit statuses — `StatusContext` —
  have no logs; external apps' check runs have no Actions job),
- the check's `status` is `COMPLETED`,
- the check run's suite app is GitHub Actions (fragment widening, below).

`PrCheck` (gql.rs) today normalizes away both the union member and the app.
Two data changes in the GraphQL fetch, **live-verified at impl time** against
the pinned schema before merging (the gql.rs convention of `live-verified`
comments applies):

- retain the union origin as a new `PrCheck.origin` field
  (`Actions` / `OtherCheckRun` / `StatusContext`) — `__typename` is already
  matched in the normalizer (gql.rs:1629), just currently discarded;
- widen the CheckRun fragment with the suite's app slug
  (`checkSuite { app { slug } }`) to distinguish Actions from third-party
  check runs. If live verification shows the field is not fetchable on the
  fragment, the fallback is an optimistic expander on all completed
  CheckRun rows with D5's graceful not-a-job error — behavior stays correct,
  one extra row shows a dead expansion. Record which shape landed in §Shipped.

Every other row (statuses, external checks) renders exactly as today —
`details_url` external link remains the affordance. **Amended by
docs/0076 D1 (#585, 2026-10-16)**: in-progress Actions-origin rows now
expand too — the live watch owns them (official APIs still serve no
growing log; the watch shows live step status and lands the whole tail
when the job completes). docs/0066 `Waiting` rows stay non-expandable,
with the gated-run flow.

### D4 — The pane: inline accordion, monospace, tail-truncated

Expanding a row grows an inline pane under it (accordion; one row expanded at
a time — expanding another collapses the first): a scrollable monospace pane
using the markdown code-block styling (docs/0017) — theme font/knobs via
`dashboard-core`, no ad-hoc `text_size`/`line_height`/colors (0002 D5/D9/D10).

Large logs are handled by **tail truncation**: the pane keeps the **last
512 KiB** of the body (CI conclusions — the failure — live at the end), with a
banner line above it: `Showing the last 512 KiB — open full log` linking
`details_url`. The full body is never held beyond the truncation cut: the
page stores only the kept tail. New debug ids per docs/0010 D10 (`.id()` +
role on every interactive element): `check-log-toggle-{idx}` on the expander
row, `check-log-pane-{idx}` on the pane.

### D5 — Resolution memoization and error surface

- The fetched (truncated) log is cached **per job id** — immutable content
  (D2); re-expanding is instant, no re-fetch.
- The resolution chain re-runs on each expand of an uncached row: it is the
  step that stays correct across CI re-runs (new suite/job ids under the same
  SHA+name) and it is three cheap GETs. Memoizing the *resolution* would go
  stale on re-runs; memoizing the *content* cannot.
- Chain failure (name-match miss, 404/410 job or run gone, empty page) → the
  pane renders an inline error line with a **Retry** button (re-runs the
  chain) and the external link. Auth/rate-limit non-200s forward through
  `apply()` and surface via the existing global handling; a 403 that reaches
  the page (fine-grained token without Actions read — not our token shape,
  but degrade anyway) joins the inline-error path.

### D6 — Architecture placement

- `api.rs`: URL builders for the four endpoints (owner/repo/sha/suite/run/job).
- `service.rs`: `Kind::JobLog` variant + dispatch; apply() gains only the
  non-200 arm (no landing logic — page-owned).
- `pr_page.rs`: `CheckLog` page state (per `(repo, job_id)` cached tails +
  in-flight/error markers), expand/collapse handlers, resolution orchestration
  (sequential `run_specs` steps — the page drives the 4-call chain, each step
  dispatching the next on landing, mirroring how thread ops chain refetches),
  and the render changes in `render_checks`.
- No new dependencies; no `dashboard-core` changes (the pane reuses existing
  markdown code styling tokens). If impl finds a genuinely shared primitive
  (e.g. a `log_viewer` wanting reuse in #202), that is a follow-up extraction
  per the 0037 Q2 ruling — not a blocker here.

## 4 Tests

- **Resolution chain (MockHttp, unit)**: happy path (4 scripted responses →
  job id → log text); name-match miss at each step → inline error state;
  matrix-suffixed names match exactly; re-run scenario (same sha+name, new
  suite/job ids) resolves the new ids.
- **Truncation (unit)**: bodies over the cap keep the tail, banner flag set,
  short bodies pass through untouched.
- **Page-owned plumbing (state test)**: 200 with a text body does not reach
  `apply()` (no landing state changes); 403/404 forward and land in the
  shared handling.
- **Probes (gpui test, debug bounds)**: `check-log-toggle` present only on
  completed Actions rows; pane node exists after expansion; StatusContext and
  in-progress rows have no toggle.
- Existing checks-tab probes stay green unchanged (rows render byte-identical
  when unexpanded).

## 5 Docs

- docs/0029 §3 matrix row moves from "see in browser" to "logs inline";
  §5 Tier-3 checklist entry closed in §Shipped.
- docs/README.md gains the 0039 row.
- This doc's §Shipped fills in the live-verification outcomes (fragment
  fields, D3 fallback if taken).

## 6 Non-goals / Deferred

- **Live log following** for in-progress jobs (D3) — superseded by
  docs/0076's progressive per-job availability (#585): no streaming
  exists officially, but the live watch covers the need. Issue #201's
  tailing idea stays parked.
- Log **search/filtering** within the pane (nice-to-have; unprompted scope).
- Step-level logs (cli/cli#11157's ask) — the API offers only whole-job logs.
- Re-run / cancel job actions — mutation scope, separate issue if wanted.

## 7 Shipped (#249)

- The four-call chain shipped as designed (D1) and was **verified
  end-to-end against live GitHub** on 2026-09-17 before implementation:
  `neovim/neovim` PR #41941, check `old-cmake` — suite `95386065554` →
  run `35221210207` → job `105201539285` → 4,733-line plain-text log.
- D3's fragment was live-verified the same day: `checkSuite { app { slug } }`
  fetches on the CheckRun fragment and Actions runs carry
  `slug: "github-actions"` — **no fallback needed**, the optimistic-expander
  path was not taken.
- **Deviation from D1, recorded**: the head SHA comes from a new
  `headRefOid` field on the PR detail, not `commits.nodes[0].oid` — under
  the commits query's default ordering `nodes[0]` is the *oldest* commit,
  not the head; the dedicated field is unambiguous at any commit count.
- **Addition within D4's letter**: the pane preprocessing folds `\r`
  progress overwrites to the segment a terminal would show and strips
  ANSI/CSI/OSC escape sequences (job logs carry terminal control bytes;
  gpui renders neither) before the tail truncation. Unit-tested.
- **Post-review additions (still within D4's letter)**: the pane opens
  **scrolled to the end** (`ScrollHandle::scroll_to_bottom` on every
  expansion and landing — CI conclusions live at the tail) and carries a
  **vertical scrollbar** (gpui-component's `ScrollableElement`
  `.vertical_scrollbar`, tracking the same handle). A probe pins the
  offset-to-bottom behavior against a 7.5k-px pane.
- Probe surface follows the house convention: gpui-test assertions key on
  `debug_selector` wrappers (`check-log-toggle-{i}`, `check-log-pane-{i}`),
  while the interactive elements keep their 0010-D10 ids for the bridge.
- Tests: +9 (chain happy path and name-miss failure via `MockHttp`,
  expander gating, tail truncation on line boundaries and UTF-8 char
  boundaries, escape/`\r` folding, chain URL builders + percent-encoding,
  chain DTO parsing, `CheckOrigin` mapping incl. suite-less runs).
