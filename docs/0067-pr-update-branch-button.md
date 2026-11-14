# 0067 — PR page: the "Update branch" split button (merge-commit / rebase)

- **Status:** design PR (docs-only)
- **Tracked by:** noosxe/my-dashboard#455
- **Trigger case:** owner request — mirror github.com's "Update branch" split
  button (dropdown: update with a merge commit / update with rebase) on the
  app's PR page.

## 1. Summary

The PR page already *diagnoses* an out-of-date branch: the merge/close rail's
state line maps `mergeStateStatus: BEHIND` to "This branch is out-of-date with
the base branch" (docs/0016 D5) — and stops there. The fix (merge or rebase
the base into the head branch) lives in github.com's "Update branch" split
button; the app offers nothing.

This design adds the missing action:

1. **Gating** — one new GraphQL field (`viewerCanUpdateBranch`) carries both
   *whether the viewer may* update the head ref and *whether it needs it*
   (the schema defines it `false` when the head ref is up to date). No
   client-side guessing.
2. **Representation** — a split button in the rail under the state line,
   mirroring the merge button's established split-button + method-dropdown
   pattern; two menu items: **Update with a merge commit** and **Update with
   rebase**.
3. **Action** — the GraphQL `updatePullRequestBranch` mutation (the documented
   two-method path; the REST twin documents no method choice, §2.1) with an
   `expectedHeadOid` staleness guard; targeted envelope refetch after success,
   no polling (docs/0052).

## 2. Facts & constraints

### 2.1 Upstream API — verified 2026-10-06

Against the public GraphQL schema
(`docs.github.com/public/fpt/schema.docs.graphql`, fetched 2026-10-06),
cross-checked against the REST reference and both dated OpenAPI descriptions
(`2022-11-28`, `2026-03-10`) and cli/cli's implementation:

| Fact | Source |
|---|---|
| `Mutation.updatePullRequestBranch(input: UpdatePullRequestBranchInput!): UpdatePullRequestBranchPayload` | GraphQL schema |
| Input: `pullRequestId: ID!`, `expectedHeadOid: GitObjectID` (optional), `updateMethod: PullRequestBranchUpdateMethod` — "If omitted, defaults to 'MERGE'" | GraphQL schema |
| `enum PullRequestBranchUpdateMethod { MERGE — "Update branch via merge"; REBASE — "Update branch via rebase" }` | GraphQL schema |
| Payload: `pullRequest: PullRequest` (the updated PR) | GraphQL schema |
| `PullRequest.viewerCanUpdateBranch: Boolean!` — "Whether or not the viewer can update the head ref of this PR, by merging or rebasing the base ref. If the head ref is up to date or unable to be updated by this user, this will return false." | GraphQL schema |
| `MergeStateStatus.BEHIND` — "The head ref is out of date." | GraphQL schema |
| REST twin `PUT /repos/{o}/{r}/pulls/{n}/update-branch` (202 Accepted / 403 / 422) documents **only** `expected_head_sha` — no method choice in either dated OpenAPI description | REST reference + `github/rest-api-description` |
| `gh pr update-branch` sends the GraphQL mutation with `{pullRequestId, expectedHeadOid, updateMethod}` | cli/cli `pkg/cmd/pr/update-branch/update_branch.go` (trunk) |

Consequence: **the GraphQL mutation is the only documented path with a method
choice** — the design builds on it. Unlike merge, no repo-level flag gates the
methods here (the schema carries no per-repository allowed-methods field for
updates); both menu items always render once the gate is open. github.com's
own update dropdown behaves the same way (GitHub blog, 2022-02-03).

Mutation failures arrive as prose messages on the GraphQL `errors` array
(conflicts during rebase, `expectedHeadOid` mismatch, permission/protection
rejections). Exact upstream strings are captured live during implementation
(§5, §6) — not guessed here.

### 2.2 What the module already has

- The rail + state line (docs/0016 D5): `merge_state_line()` renders `BEHIND`
  as the warning out-of-date line today — the button slots under it, and the
  line keeps explaining the condition when the button is absent.
- The envelope already selects `headRefOid` (`head_oid` — the docs/0039
  check-chain key) and the PR node `id` (the merge mutation consumes it):
  **both mutation inputs are already in memory**. The only new query field is
  `viewerCanUpdateBranch`.
- The merge button's split-button + method-dropdown (the `MergeMethod` picker:
  `allowed_methods` / `default_method` / `merge_enabled`) is the established
  UI pattern — the update button reuses its rhythm and components rather than
  inventing a new control.
- Rebase rewrites the branch's commits and force-pushes it — the
  irreversible-ish action pattern (confirm popover, as close/merge use)
  applies to that menu item, not to the routine merge-commit update.

### 2.3 Module constraints this must respect

- **No push channel, no autorefresh** (docs/0029 §2, docs/0052): the button
  acts; fresh state arrives via the targeted refetch. Nothing polls for
  behind-ness.
- **Standard paths**: mutation dispatch + scoped errors (docs/0029 §1),
  fetch-record/inflight dedup (docs/0031), toasts (docs/0058).
- **Degraded defaults** (docs/0023 D1 pattern): a payload missing
  `viewerCanUpdateBranch` parses as `false` ⇒ button hidden. We never infer.
- **0010 D10** ids/roles on every new interactive element; **0065 gates**
  (audit, tab stops, structural golden, critique) in the implementation PR.

## 3. Design

### D1 — Surface & interaction: split button in the rail, GitHub-shaped

When gated in (D2), the rail gains a split button under the state line:

```
⚠ This branch is out-of-date with the base branch
[ Update branch ▾ ]
  ├ Update with a merge commit
  └ Update with rebase
```

- Main click = update with a merge commit (GitHub's default); the dropdown
  offers both methods explicitly.
- The button is **hidden, never disabled**, when not applicable (D2).
- Wording pinned as above; the implementation's critique pass re-checks it
  against github.com's current copy and flags any drift in the PR body.
- The rail's other elements (state line, merge/close buttons) are untouched.

*(Amended by docs/0083 D3: the split button moves out of its own row
into the status block's Mergeability gate row ("Out of date with
`main`" or "No conflicts, behind `main`", trailing **Update ▾**), built
on the upstream `DropdownButton` so the main click updates with a
merge commit. The rebase confirm opens directly under that row and the
button gives way to it until Cancel. The D2 gate is unchanged, and
`pr-update-branch.txt` now runs over the committed fixtures.)*

### D2 — Gating: server truth only

`viewer_can_update_branch: bool` on the PR envelope (degraded default
`false`, §2.3). The button renders iff the PR is open **and**
`viewer_can_update_branch`. We deliberately do **not** AND-in
`mergeStateStatus == BEHIND`: the schema defines `viewerCanUpdateBranch` as
`false` when the head is up-to-date *or* the viewer lacks permission — it is
the complete gate, and it stays correct in states where `BEHIND` is absent
while an update is still legitimately offered.

### D3 — Mutation wiring with a staleness guard

`gql.rs` gains `PR_UPDATE_BRANCH_MUTATION`:

```graphql
mutation PrUpdateBranch($input: UpdatePullRequestBranchInput!) {
  updatePullRequestBranch(input: $input) {
    pullRequest { id mergeStateStatus }
  }
}
```

Input assembled from data already held: `pullRequestId` (envelope `id`),
`updateMethod` (**always explicit** — `MERGE` or `REBASE`; explicit beats
relying on the omitted-default for logs and tests), and `expectedHeadOid`
(the rendered envelope's `head_oid`) as the concurrency guard: if the branch
moved since the page rendered, the mutation errors and we refresh (D6)
instead of rewriting a ref the user was not looking at.
The payload selection mirrors `PR_MERGE_MUTATION`'s small confirming field —
it is logged/asserted on, never ingested as state (D5).

`service.rs`: `Mutation::UpdateBranch(UpdateBranchMethod)` through the
standard mutation dispatch; on success the service fires the standard
targeted envelope refetch (the same path a manual refresh takes).

### D4 — Rebase confirms; merge-commit does not

- **Update with a merge commit** (main click and its menu item): one click,
  no confirm — a routine, additive operation.
- **Update with rebase**: a small confirm popover — "This rebases
  `<head>` onto `<base>` and force-pushes the branch; anyone working off it
  must re-pull." with **Rebase branch** / **Cancel**. Same irreversible-ish
  pattern as close/merge.

### D5 — In-flight and refresh: busy mark, then targeted refetch

Click → the button enters its busy state (main click and both menu items
disabled while in flight; the rail's other buttons stay enabled). Success →
targeted envelope refetch; the state line flips from the `BEHIND` warning to
whatever the fresh data says (typically the no-conflicts line). Failure →
busy state reverts + scoped error toast (D6). No timers, no polling
(docs/0052).
*(Amended by docs/0083 D4: any pending write, the update included,
disables the status block's action row; "the state line" is now the
verdict and gate rows.)*

### D6 — Errors are actionable, never raw

| Failure | Copy |
|---|---|
| `expectedHeadOid` mismatch (branch moved since render) | "The branch moved while you were viewing it — refreshing." + auto refetch |
| Conflicts (the update cannot proceed) | "GitHub can't update this branch — the merge/rebase conflicts must be resolved first." |
| Permission / protection rejection | "GitHub refused: your token can't update this branch (repository permissions or branch protection)." |
| Anything else | "GitHub couldn't update the branch: <message>" + retry affordance |

GraphQL error strings are prose; we match stable fragments where verified and
fall through to the generic copy — never raw JSON in a toast (docs/0058).

### D7 — A11y, debug bridge, and the 0065 gate

`pr-update-branch` (button), `pr-update-branch-menu`,
`pr-update-branch-merge`, `pr-update-branch-rebase` (menu items):
`.id(...)` + roles + explicit labels (docs/0010 D10) — clickable from
`dashboard-debug click`. The rail joins the 0065 program in the
implementation PR: `audit` + `tree --tab-stops` receipts, a structural golden
for the rail with the button (captured on the Linux box), and a critique pass
against docs/0065 Appendix A (before/after pair in the PR body). Reference
anchor: the existing merge/close rail's rhythm and hierarchy (docs/0016 D5);
widget-level anchor: the gpui-component `story` gallery's split-button /
dropdown. Restraint: no new visual vocabulary — the update button reuses the
merge button's split-button pattern; the warning tone stays with the state
line.

### D8 — Scope guard: the PR page rail only

The button lives on the PR detail page's rail — where the state line already
speaks. PR list rows (docs/0047), notification pages, and repo CI surfaces
get nothing (§7); the service-layer entry point is shaped so a later surface
could call it without rework (repo/PR identity + method in, result out).

## 4. Owner decisions (open questions for design review)

1. **Rebase confirmation** (D4): confirm popover (proposed) — or one-click
   rebase with only a toast? The popover matches the close/merge
   irreversible-ish pattern.
2. **Main-button default** (D1): always merge-commit (proposed, GitHub
   parity) — or remember the last method used per repo?
3. **Copy** (D1/D6): the pinned wording above — happy with it? (github.com
   parity is re-checked during the critique pass regardless.)

## 5. Implementation plan

1. **`gql.rs`**: `viewerCanUpdateBranch` in the PR detail selection +
   `viewer_can_update_branch` on the envelope (degraded default false);
   `PR_UPDATE_BRANCH_MUTATION`; mutation-name registry entry; parse tests
   with a schema-shaped fixture.
2. **`service.rs`**: `Mutation::UpdateBranch(UpdateBranchMethod)` dispatch
   (input assembly incl. `expectedHeadOid`), success → targeted envelope
   refetch, standard inflight/error handling.
3. **`pr_page.rs`**: rail split button + dropdown + confirm popover + busy
   state + error toasts; mock-transport tests (probe-page pattern): gating
   matrix (open/closed × gate true/false/missing), menu rendering, busy
   lockout, mismatch → auto-refetch.
4. **Debug-verify**: rail receipt additions (ids, labels, menu
   interactions); structural golden for the rail with the button (captured
   on the Linux box, implementation PR).

## 6. Testing & human checks

Automated:

- Input serialization: both methods, `expectedHeadOid` present.
- Gating matrix incl. degraded/missing gate (button hidden, not disabled).
- Busy-state lockout of the control while in flight; mismatch → auto-refetch.
- Error-copy mapping unit tests (verified fragment vs generic fallback).
- Degraded-default parse test (`viewerCanUpdateBranch` absent ⇒ false).

Human checks (implementation PR body):

- A PR genuinely behind its base: both menu items update it; the state line
  clears after the refetch (the worker-mcp flake-update PR is the recurring
  live fixture).
- Push a commit to the branch between render and click → mismatch copy +
  auto-refresh; no wrong-ref update.
- A conflicting head branch → conflict copy; state unchanged.
- An account lacking push access → the button is hidden (gate `false`).

## 7. Non-goals (each gets a `deferred` issue before the implementation PR merges)

- **Automatic branch updates** — GitHub's auto-update setting, or any
  scheduled / on-open updating (docs/0052 contract).
- **Update affordances on other surfaces** — PR list rows, notification
  pages, repo CI views.
- **Local-repo integration** — fetching/rebasing a local checkout; the app
  drives GitHub's server-side update only.
- **Changing the base branch** — a different mutation and a different
  feature.
