# 0066 — GitHub: manually-approved (gated) workflow runs

- **Status:** design PR (docs-only)
- **Tracked by:** noosxe/my-dashboard#447
- **Trigger case:** noosxe/worker-mcp#69 — the automated `nix flake update` PR whose
  CI and Nix-flake runs have sat in `action_required` since 2026-10-05, invisible to
  the app and only approvable in the browser.

## 1. Summary

GitHub can park a workflow run behind a **human approval** before any job starts.
There are two distinct gates, and the module currently sees neither: gated runs
produce **no check runs** pre-approval, and the module's CI model
(docs/0039, docs/0009) is check-run-driven — so a gated run is simply invisible on
the PR page, and approval requires opening github.com.

This design adds three things to the GitHub module:

1. **Discovery** — one REST sweep per PR-page CI pane render surfaces runs waiting
   on approval for the PR's head commit.
2. **Representation** — a distinct *waiting for approval* state (`CheckState::Waiting`),
   rendered as its own section in the CI pane and counted in the CI rollup; never
   conflated with failure or pending.
3. **Action** — approve a gated run in place for both gates; reject for the
   environment gate (the fork gate has **no upstream reject** — §2, D4).

## 2. Facts & constraints

All REST shapes below were **live-verified 2026-10-05** against
`noosxe/worker-mcp` (PR #69's gated runs) and pinned against the official REST
reference (`rest/actions/workflow-runs`, API version `2022-11-28`), fetched the
same day.

### 2.1 How a gated run looks upstream

| Surface | Observation while waiting |
|---|---|
| REST `GET /repos/{o}/{r}/actions/runs?status=action_required` | run objects with `status: "completed"`, `conclusion: "action_required"`, real `check_suite_id`, `head_sha`, `run_number`, `event: "pull_request"`. Note the run *looks completed* — `action_required` is a conclusion, not a status. |
| REST `GET /repos/{o}/{r}/commits/{ref}/check-suites` | check suites exist for the head ref with `status: "completed"`, `conclusion: "action_required"`, `app.slug: "github-actions"` — even though no check runs exist. |
| GraphQL `CheckRun` (what docs/0039 builds on) | **no check-run nodes exist** pre-approval; `gh pr checks` reports *"no checks reported on the branch"*. The existing discovery chain (CheckRun → suite → run → jobs) never starts. |
| GitHub web UI | shows the run with a *Waiting* badge and an *Approve and run* button. |

### 2.2 The two gates

| | Fork/external-contributor gate | Environment protection rule |
|---|---|---|
| What triggers it | Repo Actions setting *"Require approval for all external contributors"* (and first-time contributors); runs triggered by untrusted actors | An environment with *required reviewers* targeted by the workflow |
| Approve | `POST /repos/{o}/{r}/actions/runs/{run_id}/approve` → **201**, empty body | `POST /repos/{o}/{r}/actions/runs/{run_id}/pending_deployments` body `{environment_ids: [..], state: "approved", comment: "…"}` → 200 |
| Reject | **no upstream endpoint** — the web UI has no reject either | same POST with `state: "rejected"`; `comment` is a **required** body parameter |
| Read side | `GET …/runs/{run_id}/approvals` (verified: `[]` on a fork-gated run) | `GET …/runs/{run_id}/pending_deployments` (verified: `[]` on a fork-gated run — the two arrays are the gate discriminator) |
| Token | classic PAT / OAuth: `repo` scope; fine-grained: **Actions (write)** | classic/OAuth: `repo` scope; fine-grained: Environments (write)/Actions (write) |

worker-mcp's runs are **fork-gate**: `pending_deployments: []` and `approvals: []`
while `conclusion: "action_required"`.

### 2.3 Module constraints this must respect

- **No push channel, no autorefresh** (docs/0029 §2, docs/0052): gated state is
  fetched when the CI pane renders or on explicit refresh; approval triggers a
  targeted refetch, nothing polls.
- **Fetch-record + inflight dedup** (docs/0031): the sweep and the gate probe go
  through the standard fetch layer.
- **Freshness/staleness** (docs/0034, docs/0037): new fetches record sync time and
  mark stale like their neighbors on the pane.
- **`CheckState` is module-wide** (`gql.rs`): adding a variant touches every match;
  the compiler will enumerate them (no `#[non_exhaustive]` games).
- **CI pane tail cap** (docs/0039 D4): the pane's summary keeps failures in the
  tail; waiting rows must not push failures out of view.
- **CI runs as conclusions** (docs/0009 D-snapshot, `check_run_state`): GraphQL
  `ACTION_REQUIRED` check-run *conclusion* (post-hoc, e.g. a cancelled gated run)
  currently maps to `Failure`. That mapping stays — it describes a *finished*
  run; the new `Waiting` state describes a *runnable, held* run from REST data.
  Different sources, different phases.

## 3. Design

### D1 — One new state: `CheckState::Waiting`

Add a `Waiting` variant to `CheckState` (doc comment: *run created but held for
manual approval; REST-only signal, never produced by GraphQL data*). Rows built
from the sweep use it when `conclusion == "action_required"`. The CI rollup chip
gains a waiting count (own icon/color slot — waiting is not failure, not pending:
pending means "will run", waiting means "won't run until a human acts").

Every existing `CheckState` match gets reviewed: rollups, badges, tests. GraphQL
mappers (`rollup_state`, `check_run_state`, `status_context_state`) **never**
return `Waiting` — only the REST sweep mints it.

### D2 — Discovery: one sweep per CI-pane render

`GET /repos/{o}/{r}/actions/runs?branch={head_branch}&event=pull_request&status=action_required&per_page=50`
with `head_branch` percent-encoded as a query value (a branch like `fix/#12` must not truncate the query),
filtered client-side by `head_sha == PR head_sha` (the branch name is not unique
across forks; the SHA filter is the correctness guarantee — verified live that the
endpoint honors `branch` + `event` and returns `head_sha` on every row).

Fetch-record key: `(repo, head_sha, "gated-runs")`. Inflight dedup applies; the
sweep refetches on pane refresh and after any approve/reject (D5).

**Fallback** (only if the sweep comes back empty while the PR shows zero
`CheckRun`s *and* zero waiting rows): `GET /repos/{o}/{r}/commits/{head_sha}/check-suites`,
then the existing docs/0039 chain (`workflow_runs_by_suite_url`) per suite with
`conclusion == "action_required"`. This covers any case where `branch=` filtering
misses fork-sourced runs; the code path reuses today's step-2 URL builder and
parse.

### D3 — Rendering: a "Waiting for approval" section in the CI pane

Gated rows render as their own section above the queued/running check rows:

```
◈ Waiting for approval (2)
  ● CI — run #138            [Approve]
  ● Nix flake — run #96      [Approve]
```

- Row content: workflow name, run number, age (`created_at`), gate kind
  ("fork/external contributor" vs environment name — from the gate probe, D4).
- The tail cap (0039 D4) applies **after** the waiting section: failures keep
  their reserved tail; waiting rows prepend.
- Rollup chip: waiting count gets its own slot; a PR whose only CI activity is
  gated shows *waiting*, not *passing* or *no checks*.
- **The conversation rail says the same thing** (rail amendment, review
  round): the merge/close rail's checks summary (docs/0016 D5) shows
  "N waiting for approval" in warning tone instead of the rollup line
  while the sweep holds rows — GitHub's rollup cannot see held runs, so
  "Checks status unknown" would be a lie — and its button reads "Review
  approval", jumping to the checks tab where the actions live. Zero
  waiting delegates to the rollup line untouched.
- Approved-by-someone-else mid-session: next refresh's sweep no longer returns
  the run; the row disappears and normal CheckRun rows take over once jobs
  start. No zombie rows.

### D4 — Gate probe and actions

When the sweep returns ≥ 1 row, the service probes
`GET …/runs/{run_id}/pending_deployments` per gated run (1 GET per run, fetch-record
cached, deduped):

- **non-empty** → environment gate. Row actions: **Approve…** and **Reject…**
  opening a small confirm popover listing the pending environments (all
  preselected) with an optional comment. The POST sends every selected
  `environment_id`, the chosen `state`, and `comment` — the wire comment is
  always non-empty: the UI text if given, else `"Approved via my-dashboard"` /
  `"Rejected via my-dashboard"` (the API requires the field; we never send a
  bare `""`).
- **empty** → fork gate. Row action: **Approve and run** →
  `POST /repos/{o}/{r}/actions/runs/{run_id}/approve` (empty body, expect 201).
  No reject action exists upstream for this gate (§2.2); the UI shows no reject
  button rather than a disabled one, and the row's tooltip says approval is the
  only disposition GitHub offers for this gate. Issue #447's reject requirement
  is therefore scoped to the environment gate.

Both mutations run through the module's standard mutation path (scoped error
isolation per docs/0029 §1, toasts per docs/0058).

### D5 — Optimism and refresh

Approve/reject marks the row optimistically (*approved — starting…* /
*rejected*) and fires a targeted sweep refetch + gate-probe invalidation on
completion. Success: the run leaves `action_required`, the row leaves the
section, CheckRun rows appear as jobs start. Failure: revert + scoped error
toast. No timers, no polling — the same manual-refresh contract as every other
surface (docs/0052).

### D6 — Errors are actionable, never raw

| Upstream | Copy |
|---|---|
| 403 on approve/reject | *"GitHub refused: your token lacks Actions write access (or you're not a required reviewer on this environment)."* |
| 404/422 (run no longer waiting, already reviewed) | *"This run is no longer waiting for approval — refreshing."* + auto refetch |
| network/fetch-layer | standard module error row on the section, retry affordance |

The module cannot know reviewer-writability up front (no cheap upstream
signal), so we rely on the 403 copy instead of pre-disabling — except the
fork-gate reject which never renders (D4).

### D7 — A11y, debug bridge, and the 0065 gate

The section header, every row, and the Approve/Reject buttons get `.id(...)`
accesskit ids + roles + explicit labels (docs/0010 D10) — clickable from
`dashboard-debug click`. The surface joins the 0065 program: `audit` +
`tree --tab-stops` receipts in the implementation PR, a structural golden for
the waiting section captured on the Linux box, and a critique pass against
docs/0065 Appendix A (before/after pair in the PR body). Reference anchor: the
existing PR-page CI pane's rhythm and hierarchy; widget-level anchor stays the
gpui-component `story` gallery. Restraint: two states, one accent — no new
visual vocabulary beyond a waiting badge.

### D8 — Scope guard: PR page only (for now)

The sweep keys off a PR's head SHA and head branch — the natural home is the PR
page CI pane, which is where the trigger case lives. Repo-level surfaces
(commit check sections, future Actions tab) are **non-goals** (§7); the
service-layer functions are shaped so a later surface can reuse the sweep
without rework (owner/repo + head_sha in, waiting runs out).

## 4. Owner decisions (open questions for design review)

1. **Reject UX** (env gate): inline row button + confirm popover with comment
   field (proposed, D4) — or comment-free one-click reject with an undo-less
   toast? The popover matches the "irreversible-ish action" pattern used for
   close/merge.
2. **Section placement**: above queued checks (proposed, D3) vs merged into the
   existing row list ordered by time, waiting rows visually flagged.
3. **Comment default text**: neutral "via my-dashboard" (proposed) vs empty
   string (docs allow `comment: ""`? — unverified; we default to non-empty to
   stay safe).

## 5. Implementation plan

1. **`api.rs`**: URL builders `gated_runs_url`, `run_pending_deployments_url`,
   `approve_run_url`; payload structs `GatedRunsPage`/`GatedRunNode` (id, name,
   head_sha, run_number, event, conclusion, created_at),
   `PendingDeploymentsPage`/`PendingDeploymentNode` (environment id, name,
   `html_url`, `state`), `RunApprovalsPage`. Serde tests against the
   **live-captured JSON** from 2026-10-05 (fork-gated run + a synthetic env-gate
   payload mirroring the reference example).
2. **`gql.rs` / state mappers**: `CheckState::Waiting`; `run_gate_state()`
   mapping REST run rows → `Waiting`; GraphQL mappers untouched.
3. **`service.rs`**: sweep fetch (fetch-record `(repo, head_sha)`), gate-probe
   fetch per run, `approve_run` (fork gate), `review_pending_deployments` (env
   gate) mutations with the standard inflight/error handling.
4. **`pr_page.rs`**: waiting section, rollup waiting slot, approve/reject
   popover, optimistic updates, error rows; CI-pane tests extended with gated
   fixtures (mock transport, pattern of pr_page.rs:7602+).
5. **Debug-verify**: new receipt script `crates/debug/verify/pr-gated-runs.txt`
   (tree assertions for the section, ids, labels); structural golden for the
   CI pane with waiting rows (captured on the Linux box, same PR).

## 6. Testing & human checks

Automated:

- Serde: fork-gate payload (captured), env-gate payload (reference-shaped),
  missing-field tolerance (`#[serde(default)]` style as the module does).
- State mapping: REST `action_required` → `Waiting`; GraphQL paths never mint
  `Waiting`; rollup waiting-count math.
- Filter logic: `head_sha` equality; fork-branch-name collision fixture.
- UI: gate probe empty → approve-only row; non-empty → approve/reject popover;
  optimistic revert on 403.

Human checks (PR body checklist):

- Approve worker-mcp PR #69's gated runs from the app and watch them start
  (the live fixture — both runs are waiting right now).
- If an environment-gated repo is available: approve and reject through the
  popover, including the comment path.
- 403 copy with an account lacking write access.
- Stale-session case: approve in the browser first, refresh the pane, confirm
  no zombie waiting row.

## 7. Non-goals (each gets a `deferred` issue before the implementation PR merges)

- **Repo-wide Actions browser** / dedicated runs tab: browsing and acting on
  *all* `action_required` runs beyond a PR's head SHA.
- **Custom deployment protection rules** (GitHub App-provided review rules,
  `POST …/runs/{run_id}/custom_deployment_protection_rules/…`).
- **Editing** environment protection rules or repo Actions approval settings.
- **Approve-all** batch affordances across multiple waiting runs.
