# 0045 — Draft persistence for composers and creators

Part of the GitHub module (docs/0005, 0020/0024 line). Implements #199 —
the follow-up docs/0024 D7 filed when it chose session-scoped draft
retention: losing a draft to an app restart (or to a detail-page
navigation) is no longer acceptable.

## 1. Summary

Text typed into the four write surfaces — the issue comment box, the PR
comment box, the new-issue creator (title + body) and the new-PR creator
(title + body) — is auto-saved to a per-draft file under the module's
data root, trailing-edge debounced at 500 ms. Returning to the same
context (same repo creator, same issue/PR page) restores the text; a
successful submission deletes it; clearing the text discards it. The
existing in-memory retention rules (0024 D7 / 0044 D7) stay as they are;
disk persistence is layered on top, and the 0024 D7 sentence "No disk
persistence" is amended (D6 below).

All persistence primitives already exist in `dashboard_core` — this doc
spends its decisions on layout, keys and lifecycle, not on new
infrastructure.

## 2. House facts (verified against the tree, 2026-09-19)

- **`store::DebouncedSave`** (crates/core/src/store.rs): trailing-edge
  debounced saver — each `schedule(value, cx)` bumps a shared generation
  counter and only the latest task writes after the delay, so at most
  one save per quiet window, always the newest value. The spawned task
  owns the value and the path: it fires **even if the page entity that
  scheduled it has been dropped** — navigation away is covered without
  flush-on-drop hooks. House delay precedent: 500 ms (shell.json,
  settings.json).
- **`store::load_json` / `store::save_json_atomic`**: atomic
  tmp+fsync+rename writes, corrupt-file quarantine, missing = `None`;
  the failure policy is log-and-swallow (0004 §4.7) — a broken disk
  never takes the UI down.
- **`AppPaths::module_data_dir("github")`** → `<data>/modules/github/`,
  created on demand; the per-module slot for module state that is *not*
  re-derivable. The cache root (`<cache>/github/`, `GithubCache`) holds
  snapshots and etags precisely because those *are* re-derivable —
  0024 D7's "the cache holds snapshots, not drafts" stays true.
- **Composer events**: `TextareaState` emits `InputEvent::Change` on
  user edits (same base input machinery as `InputState`), and
  `set_value` suppresses events — so a restore-prefill cannot trigger
  the save subscription it was restored by (no loop).
- **Page lifecycle** (docs/0016/0020 D1): PR and issue pages live in
  single slots (`page.rs` `pr` / `issue`) — opening the sibling drops
  the page, taking its composer text with it today. Creators are
  retained for the session (0024 D7). Both are dropped on app exit —
  the restart-loss case #199 describes.
- **Surfaces and their fields**: issue page `comment: Entity<TextareaState>`;
  PR page `comment` (plus 0030's `reply_composer` / `line_composer`,
  excluded — D7); new-issue `title` + `body`; new-PR `title` + `body`.
- `GithubState::new(paths-injected via GithubCache, …)` has 44 call
  sites (one prod, the rest tests) — a new constructor parameter is
  mechanical churn.

## 3. Design

### D1 — storage: one file per draft under the module data root

Drafts live in `<data>/modules/github/drafts/` (i.e.
`AppPaths::module_data_dir("github").join("drafts")`), one JSON file per
draft context:

```json
{
  "key": "comment@acme/widgets#42",
  "title": null,
  "body": "half-written comment…",
  "updated_ms": 1758000000000
}
```

- **Data root, not cache root.** A draft is user-generated and not
  re-derivable — XDG semantics make `<cache>` deleteable at any time,
  so it must not hold it. `module_data_dir` is the sanctioned per-module
  slot for exactly this (0004 §4.1, `settings.json`/`shell.json`
  precedent for user state).
- **One file per draft, not a shared index.** Each composer's
  `DebouncedSave` writes only its own file — no cross-writer races on a
  shared index (two composers debouncing the same `drafts.json` would
  last-write-win each other's entries). Clear-on-submit is an unlink.
  Atomic write, quarantine and the failure policy come from the core
  store either way.
- `title` is `Some` only for the creators (comment drafts leave it
  `null`). `updated_ms` is written on every save; v1 reads nothing from
  it (pruning is a §9 follow-up), it exists so a later sweep can age out
  stale drafts without a format change.

### D2 — keys and the collision guard

Three context kinds, keyed off what the page already knows at `new()`:

| Context | Key | Fields saved |
|---|---|---|
| New-issue creator | `new-issue@<owner/repo>` | `title`, `body` |
| New-PR creator | `new-pr@<owner/repo>` | `title`, `body` |
| Comment box | `comment@<owner/repo>#<number>` | `body` |

Issues and PRs share one number space per repository on GitHub, so a
single `comment` kind is unambiguous — there is no repo where issue #5
and PR #5 coexist.

The **filename** is the house-sanitized key (`[A-Za-z0-9_-]`, everything
else collapsed to `-` — same rule as `AppPaths::sanitize_id`). Sanitized
names can theoretically collide (`a-b` + `c` vs `a` + `b-c`), so the
**loader verifies `payload.key == requested key`** and treats a mismatch
as no draft: a collision can never restore the wrong text, it just fails
to restore. No hashing dependency, no opaque filenames, airtight.

### D3 — save trigger: 500 ms trailing-edge debounce, per composer

Each surface subscribes to its textarea's `InputEvent::Change` and calls
`DebouncedSave::schedule(payload, cx)` — the shell-state pattern.
Properties that fall out of the existing primitive:

- at most one disk write per 500 ms quiet window, always the newest
  text (typing continuously costs zero writes);
- the pending task owns the final value, so **navigating away (page
  dropped) still lands the last text** — no flush-on-drop hook, which
  gpui's uncancellable tasks would make awkward;
- restore-prefill uses `set_value`, which suppresses `Change` — the
  restore cannot schedule the save it came from.

The issue floats "periodic auto-save, e.g. every 5 seconds of
inactivity". A 500 ms trailing-edge debounce is the same shape with a
5× smaller loss window and matches the house delay (shell.json,
settings.json). Fixed constant, not configurable.

**One core addition — `DebouncedSave::cancel()`**: bumps the generation
counter without spawning a task, so every pending task sees itself
superseded and skips. Needed because a write scheduled *before* a submit
would otherwise fire *after* the submit's unlink and resurrect a draft
for an already-posted comment (D5). Two lines in `store.rs`, one unit
test.

Accepted edge: quitting the app inside the 500 ms window loses the
final keystrokes — the task never runs. Same bound the shell state
already accepts; solving it (flush-on-exit hooks) is not worth the
lifecycle complexity for half a second of text.

### D4 — restore: prefill on page construction, with a visible hint

`IssueDetailPage::new`, `PrDetailPage::new`, `NewIssuePage::new` and
`NewPrPage::new` load their draft synchronously (a small local file —
the same sync-read-at-construction precedent as `GithubState::new`
reading the snapshot cache) and, when a draft exists with non-empty
content, prefill the textarea(s) via `set_value` and set a
`draft_restored` flag that renders a muted hint row under the composer:

> Draft restored — clearing the box discards it.

The hint is honest (silent resurrection of old text is confusing), cheap
(muted text row), and self-documenting: the discard gesture is the
obvious one, and it works (an empty payload clears the file — D5's
discard path). The hint disappears after the first edit — it describes
the *restore*, not a persistent state. Probe ids: `issue-draft-restored`,
`pr-draft-restored`, `new-issue-draft-restored`, `new-pr-draft-restored`.

For the creators, restore composes with the existing session retention:
the entity may still be alive from a previous open (0024 D7) — in that
case its textareas already hold the text, `new()` doesn't run again, and
nothing changes. Disk restore only matters when the entity was dropped
(repo switch, app restart, successful creation elsewhere).

### D5 — clearing: submit deletes, empty discards

- **Successful submission** — comment mutation success arm, and the
  creators' created flow — calls `DraftHandle::discard()`:
  `save.cancel()` (kill pending writes, D3) then `unlink` the file.
  Best-effort: a failed unlink logs and moves on; the stale file's next
  submit-clear or overwrite fixes itself, and a comment draft restored
  after its comment was posted is the one outcome this design must not
  allow (hence cancel-before-unlink, not unlink alone).
- **Cleared text** — when a composer's text transitions to empty, the
  Change handler calls `discard()` instead of scheduling: no empty-body
  residue files, and a pending write from the pre-clear text is killed
  by the same `cancel()`.

### D6 — 0024 D7 amended; session retention untouched

docs/0024 D7 ships with this sentence: *"No disk persistence — the cache
holds snapshots, not drafts; losing a draft to an app restart is
acceptable for v1 and filed as a follow-up if it bites."* #199 is that
follow-up. The amendment:

- The **cache still holds snapshots, not drafts** — drafts go to the
  data root (D1), so the original rationale stands; only the "no disk
  persistence" conclusion is reversed.
- **Repo switch no longer destroys the creator draft**: it parks on
  disk under that repo's key, and reopening the creator for the same
  repo restores it (switching repos keeps each repo's draft separately;
  D2 keys make that free). The 0024 D7 wording "the draft belongs to the
  repo it was opened for" is *kept* — it now survives the switch.
- Cancel/Esc retention, drop-on-successful-creation, and the one-refetch
  flow (D6) are untouched.

### D7 — scope: four surfaces in, composers-of-record out

**In v1** (the surfaces #199 names, plus the 0044 sibling that shares
every mechanism):

1. issue page top comment box (`body`),
2. PR page top comment box (`body`),
3. new-issue creator (`title`, `body`),
4. new-PR creator (`title`, `body`) — the letter of #199 says "new
   issues"; the PR creator is the same form chrome with the same loss
   profile and the same D7 wording in docs/0044, so excluding it would
   be an artificial asymmetry. Flagged here for the owner to strike
   when reviewing this doc.

**Out (§9 follow-up candidates):**

- **PR thread reply composers and Files-tab line composers**: 0030 D4
  *documents* discard-on-switch for the line composer ("opening another
  row's composer discards the previous unsubmitted draft") — persisting
  underneath that behavior would fight the shipped contract; line drafts
  are anchored to diff positions that shift across pushes; and reply
  drafts keyed by thread id need thread-lifetime rules. Real design
  surface, zero urgency — separate doc if it bites.
- **Title/description *edit* editors** (0022/0043 `title_edit` /
  `description_edit`): editing existing content, not drafting — the
  original text is one Esc away and the refusal model keeps one editor
  at a time. Persisting an edit-in-progress would need conflict
  semantics against concurrent upstream edits.
- **Picker selections on the creators** (labels/milestones/draft-flag):
  session UI state; restoring them needs source-fetch arming on open
  (0044 D4's fetches) and is metadata-at-creation territory (#276).

### D8 — plumbing: `drafts.rs`, `GithubState.drafts`, page handles

- **New `crates/modules/github/src/drafts.rs`**: `DraftStore` (root
  `PathBuf`; `new(&AppPaths)`; `#[cfg(test)] at_tmp(tag)` mirroring
  `GithubCache::at_tmp`), pure key builders (`comment_key`,
  `creator_key`), `sanitize_key`, `DraftPayload` (serde), and
  `DraftHandle { path, save: DebouncedSave }` with `load()`,
  `schedule(payload, cx)`, `discard()`. All table-testable without gpui.
- **`GithubState`** gains `pub(crate) drafts: DraftStore`, built by the
  caller next to `cache` (both derive from `AppPaths`; both injectable
  for tests). `GithubState::new` gains one parameter — 44 mechanical
  call-site updates.
- **Pages** hold `draft: DraftHandle` + `draft_restored: bool`, built in
  `new()` from `state.drafts` + the page's key; the Change subscription
  is registered next to the existing ones (issue/pr comment boxes get a
  *new* subscription; the creators' title/body get subscriptions for
  their shared handle). Submit paths call `discard()` in the success
  arm (comment mutation / created flow).
- The debounced write runs on the background executor; the flush is a
  few-KB JSON write — no batching across surfaces (each page writes
  only itself).

## 4. Implementation plan (one PR, after this doc merges)

1. `dashboard_core::store`: add `DebouncedSave::cancel()` + unit test
   (schedule → cancel → deterministic wait → no file).
2. `drafts.rs`: `DraftStore` / `DraftPayload` / key builders /
   `sanitize_key` / `DraftHandle` (+ `at_tmp`), with table tests:
   roundtrip, key-verification collision guard, empty-discard unlink,
   sanitize shape.
3. `service.rs`: `GithubState.drafts` field + constructor parameter;
   update the 44 call sites mechanically.
4. Page wiring: fields + `new()` restore + Change subscriptions +
   submit-arm `discard()` on all four surfaces; muted hint row with
   `.id()`/role/`.debug_selector(...)` (docs/0010 D10).
5. Probes (gpui tests, 0023 §10 harness with `at_tmp` roots): type →
   file lands after the debounce window (deterministic executor
   parking, shell-state test pattern); close + reopen page → prefilled +
   hint visible; submit success → file gone; clear-all → file gone;
   creator repo-switch park/restore per D6.

## 5. Testing

- **Table tests** (drafts.rs): key/filename sanitize and roundtrip;
  `payload.key` mismatch → `load` returns `None`; empty-body payload →
  file removed; `updated_ms` advances across saves.
- **Core test**: `cancel()` invalidates a pending schedule (the
  generation contract that keeps posted comments from resurrecting
  drafts).
- **gpui probes** (one per surface, shared harness): the four flows
  above; the creator probe additionally asserts the D6 park/restore
  across a repo switch and that a live (undropped) creator is untouched
  by restore logic.
- **Gates**: fmt, clippy zero warnings, workspace tests — per AGENTS.md.

## 6. Debug-bridge verification (docs/0010)

The composers sit behind repo → issue/PR → Edit and the creator behind
repo → New issue — the deep-navigation a11y gap (#97) starts one hop
before them, so bridge smoke covers the open/restore hint visuals only
where reachable; the behavioral assertions ride the `debug_bounds`
probes in §4.5. Disclosed in the PR body; no OS-level input injection.

## 7. Acceptance criteria

1. Typing in any of the four surfaces persists within a pause of 500 ms;
   the write is atomic and failure-tolerant (log-and-swallow).
2. Navigating away from an issue/PR mid-comment and returning restores
   the text; restarting the app and returning also restores it.
3. A posted comment / created issue / created PR leaves no draft file
   behind for that context.
4. Clearing a composer's text (or a creator's both fields) discards the
   draft on disk.
5. A draft is never restored into the wrong context (key verification),
   and never resurrected after its submit cleared it (`cancel()`).
6. The hint row shows exactly when a restore happened and yields to the
   first edit.

## 8. Human checks (owner)

- [ ] Type a comment, quit the app (Cmd/Ctrl+Q) within a second, relaunch, reopen the issue — text is there (modulo the last ≤500 ms)
- [ ] Type a comment, navigate to another PR/issue and back — text restored, hint visible; edit → hint gone
- [ ] Post the comment — reopen the page: no restore, no hint, no stale file
- [ ] Select-all + delete in the comment box, navigate away and back — nothing restored
- [ ] New-issue: type title + body, switch repos and back — draft returns per repo; create the issue — draft gone
- [ ] Same on the new-PR creator (if D7's inclusion stands)
- [ ] `DASHBOARD_STATE_DIR` run: files appear under `<dir>/data/modules/github/drafts/`

## 9. Follow-ups (GitHub issues, not this PR)

- Reply-composer and line-composer drafts (0030 D4 tension + thread
  lifetime + diff anchors — needs its own design).
- Draft pruning sweep: age/count cap over `drafts/` reading
  `updated_ms` (format already carries it).
- Metadata-at-creation (#276) would supersede creator picker-selection
  drafts entirely; don't build the latter first.
- Flush-on-quit hook if the ≤500 ms tail loss ever bites (it is the
  same accepted bound as shell state).
