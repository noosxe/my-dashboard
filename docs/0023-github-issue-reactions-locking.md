# 0023 — GitHub Module: Issue Page Reactions + Conversation Locking

- **Status**: design — implementation tracked in #121
- **Prerequisites**: docs/0020 (issue page), docs/0022 (metadata editing —
  editor surfaces, arming gates, rail patterns), docs/0019 (comment rows,
  inline editors), docs/0016 §6.2 (mutation transport), docs/0007 D3
  (degradation), docs/0010 (debug bridge)
- **Follows**: #121 (issue-page follow-up from 0020 §11)

## 1. Summary

Two conversation-surface writes complete the issue page's follow-up list:

- **Reactions** — toggle chips under the issue body and on each comment
  (GitHub's eight contents), plus a static picker for adding the first
  reaction to an entry. Viewer's own state (`viewerHasReacted`) drives the
  chip's armed look and whether a click adds or removes.
- **Conversation locking** — a rail row showing lock state with a
  lock/unlock affordance, and composer gating while locked.

Both ride the existing page machinery: single-flight mutations with
refetch-after-write (0020 D6), hidden-not-disabled gates (0020 D5), the
one-editor rule extended to its sixth surface (0022 D7), hand-parsed wire
structs with per-node degradation (0007 D3).

## 2. Upstream facts (live introspection 2026-09-07, `gh api graphql`)

### 2.1 Reactions

`ReactionContent` enum, canonical order as returned by the API:

| value | glyph | | value | glyph |
| --- | --- | --- | --- | --- |
| `THUMBS_UP` | 👍 | | `CONFUSED` | 😕 |
| `THUMBS_DOWN` | 👎 | | `HEART` | ❤️ |
| `LAUGH` | 😄 | | `ROCKET` | 🚀 |
| `HOORAY` | 🎉 | | `EYES` | 👀 |

`ReactionGroup` fields: `content: ReactionContent!`,
`createdAt: DateTime`, `reactors: ReactorConnection!` (**not `users`** —
the #121 body guessed wrong; corrected here), `subject: Reactable!`,
`viewerHasReacted: Boolean!`. `ReactorConnection.totalCount: Int!` carries
the count; the reactor list itself is not needed.

`Issue` and `IssueComment` **both** carry `reactionGroups` and
`viewerCanReact` (the `Reactable` interface).

**GitHub returns all eight groups even at zero count** — verified on
my-dashboard#118 (eight groups, every `totalCount` 0) and on a
heavily-reacted public issue (mixed counts, same eight-group envelope).
"Rendered chip" must therefore be filtered on `totalCount ≥ 1`, not on
group presence.

Mutations:

| input | fields |
| --- | --- |
| `AddReactionInput` | `subjectId: ID!`, `content: ReactionContent!` |
| `RemoveReactionInput` | `subjectId: ID!`, `content: ReactionContent!` |

Both payloads return `reaction`, `reactionGroups`, `subject` — fresh
groups are available in-band, but per the house rule (0019 D6, 0020 D6:
no optimistic writes, refetch after mutation) we ignore them and refetch
the detail like every other write on this page.

### 2.2 Locking

| fact | value |
| --- | --- |
| `Issue.locked` | `Boolean` — exists |
| `Issue.activeLockReason` | `LockReason` — exists |
| `Issue.viewerCanLock` | **does not exist** (the flag gap #121 suspected — confirmed) |
| `IssueComment.locked` etc. | absent — locking is conversation-level only |
| `viewerCanComment` (either type) | **does not exist** |
| `LockLockableInput` | `lockableId: ID!`, `lockReason: LockReason` (optional) |
| `UnlockLockableInput` | `lockableId: ID!` |
| payloads | `lockedRecord` / `unlockedRecord: Lockable` (ignored; we refetch) |
| `LockReason` | `OFF_TOPIC`, `TOO_HEATED`, `RESOLVED`, `SPAM` |

### 2.3 The lock gate: `Repository.viewerPermission`

With no `viewerCanLock` flag, gating must derive from
`Repository.viewerPermission: RepositoryPermission` — enum values
`ADMIN, MAINTAIN, WRITE, TRIAGE, READ`. GitHub grants lock/unlock to
triage-and-above, so the gate is **permission ∈ {ADMIN, MAINTAIN, WRITE,
TRIAGE}**. Live-verified on our repo: `viewerPermission: "ADMIN"`.
The field can be `null` (no explicit mapping) — null means no lock rights.

## 3. Design

### D1 — Query: extend `ISSUE_DETAIL_QUERY` once more

One document keeps carrying the page (0021 D1). Three additions:

- issue level: `viewerCanReact`,
  `reactionGroups { content viewerHasReacted reactors { totalCount } }`,
  `locked`, `activeLockReason`;
- comment nodes: the same `viewerCanReact` + `reactionGroups { … }`
  selection inside the existing comments connection;
- repository level: `viewerPermission` (next to `issue`; the repository
  node is already the fetch envelope).

No new fetch sources: the reaction picker's candidate set is the eight
enum values — **static**, no `Kind` variant, no repository query (contrast
with 0022's three pickers).

### D2 — Wire structs + parse: one `ReactionGroup` pair, unknown content skips

Hand-parsed per 0007 D3. Shared structs for issue and comments (same
shape):

```rust
pub struct ReactionGroup {           // wire → page, same struct
    pub content: ReactionContent,    // our enum, 8 variants
    pub viewer_has_reacted: bool,
    pub total: u32,
}
```

Parser: unknown `content` string → skip that group alone (degrade per
node, 0007 D3); group with no `content` → skipped; absent `reactionGroups`
→ empty vec (GitHub always sends all eight today, but nothing pins that).
`locked`/`activeLockReason` degrade to `false`/`None`; comment-level
groups attach to the existing comment rows by index.

`ReactionContent` carries the glyph mapping in one place
(`content.glyph() -> &'static str`, `content.label()` for the picker's
text fallback — see D4's emoji risk).

### D3 — Chips row: zero-count filter, canonical order, toggle semantics

- **Where**: a chips row directly under each reactable body — the issue
  body (under the markdown block) and every comment (above its 0019 action
  bar, which stays unchanged). One row per entry, muted small chips:
  `glyph count`, armed (viewer reacted) using the selected/accent token,
  unarmed the standard muted chip token — no literal colors ([DG], 0002
  D5/D9/D10).
- **Which chips**: groups with `totalCount ≥ 1` only (zero groups are
  noise), rendered in the canonical enum order of §2.1 — stable positions,
  not count-sorted (rows don't reshuffle on refetch).
- **The add affordance**: a `+` button ending the row, shown when
  `viewerCanReact` (hidden otherwise, 0020 D5). If the row is empty (no
  reactions yet), the `+` button renders alone so first-reaction is still
  one click away.
- **Click semantics**: chip click → `viewerHasReacted` ? `RemoveReaction`
  : `AddReaction` (single-flight busy op; further clicks ignored while
  busy, per the page's model). `+` click → opens the picker (D4). No
  confirm dialogs.

Probe ids: `issue-reaction-row`, `issue-reaction-row-add`,
`issue-reaction-{CONTENT}` on the description row;
`issue-comment-{ix}-reaction-row`, `issue-comment-{ix}-reaction-{CONTENT}`,
`issue-comment-{ix}-reaction-add` on comments; button role on every chip
(0010 D10).

### D4 — Reaction picker: static eighth-list, sixth editor surface

`+` opens the same picker-card language as 0022's rail cards (title row +
rows + Save/Cancel… minus Save): **rows commit on click** — each picker
row is `glyph label count` and clicking it posts the matching
Add/RemoveReaction immediately and closes the picker (the mutation itself
is the commit; a separate Save would be an empty ritual for a
single-choice action). Cancel/Esc closes without writing.

- Rows: all eight contents, canonical order, with current counts; the
  viewer's reacted content shows armed.
- The picker participates in the one-editor rule (0022 D7): opening it
  closes any other editor surface; `cancel_top_editor` picks it up before
  the page closes (Esc interception order extends to six surfaces).
- Probe ids: `issue-reaction-pick`, `issue-reaction-pick-{CONTENT}`,
  `issue-reaction-pick-cancel`.

**Emoji rendering risk (recorded, checked at impl)**: glyphs are text runs
through the normal font stack; if the platform font lacks emoji the chips
show tofu. Contingency: swap `content.glyph()` to GitHub shortcode text
(`:+1:`, `heart`, …) — one function body, no structure change. Human check
§9 covers it.

### D5 — Locking: rail row, permission-derived gate

- **Rail row** (0022's rail home): `Locked` + reason tag when locked
  (`OFF_TOPIC → "off-topic"` etc.; null reason → just `Locked`),
  `Unlocked` otherwise. The affordance button — "Lock" / "Unlock" — shows
  only when the gate passes (hidden-not-disabled): `viewerPermission ∈
  {ADMIN, MAINTAIN, WRITE, TRIAGE}`; `null` permission → hidden.
- **Mutation ops**: `Mutation::LockIssue` / `Mutation::UnlockIssue` →
  `LOCK_LOCKABLE_MUTATION` / `UNLOCK_LOCKABLE_MUTATION` (`lockableId` =
  the issue node id). v1 omits `lockReason` (input optional; GitHub stores
  null → generic "Locked" tag). A reason picker is a recorded follow-up,
  not v1 scope.
- **Busy op**: `IssueBusyOp::ToggleLock` (the page knows the current
  state; the op's log/busy identity is "changing lock state").
  Refetch-after-mutation; failure copy in the rail slot (`ErrorScope::Rail`,
  per #121).
- Probe ids: `issue-lock-row`, `issue-lock-toggle`, reason tag
  `issue-lock-tag`.

### D6 — Composer gating while locked (derived, honest)

No `viewerCanComment` flag exists (§2.2), so the composer gate derives
from what we can know:

- unlocked → composer per today's rules (0019/0020);
- locked ∧ lock-rights → composer stays (maintainers comment on locked
  threads — GitHub parity);
- locked ∧ ¬lock-rights (incl. null permission) → composer hidden, a
  muted `Conversation locked` notice in its place.

Caveat recorded: a WRITE-collaborator on someone else's repo gets the
hidden composer where GitHub would show them a composer with
commenting disallowed post-lock — the flag to be exact does not exist;
the derivation matches GitHub's behavior for the viewer roles this
dashboard actually uses. Revisit if GitHub ships a real flag (follow-up).

### D7 — No optimistic writes; in-band payload groups ignored

Add/RemoveReaction and lock/unlock payloads all carry fresh state — all
deliberately unused. One write path for the whole page: POST → refetch
`ISSUE_DETAIL_QUERY` → repaint (0019 D6 parity, 0022 unchanged). The
single-flight model means a second reaction toggle waits for the current
op; chips are the only place this could feel slow, and reaction frequency
makes it a non-issue in practice.

### D8 — Token discipline + bridge hygiene

Chips, picker rows, rail lock row: semantic theme tokens only; the armed
chip uses the selected/accent token; no literal color constructors
(clippy gate unchanged). Every interactive element gets `.id(...)` +
role (0010 D10) — the probe-key sets in D3/D4/D5 are the checklist.

## 4. Implementation plan (one PR, after this doc merges)

1. `gql.rs` — extend `ISSUE_DETAIL_QUERY` (D1); `ReactionContent` +
   `ReactionGroup` structs, parse fns, `locked`/`activeLockReason`/
   `viewerPermission` on the detail envelope; two new mutation documents.
2. `service.rs` — `Mutation::{AddReaction, RemoveReaction, LockIssue,
   UnlockIssue}` → the two documents (add/remove share `subjectId` +
   `content` inputs); no new `Kind` variants (D1: no fetch sources).
3. `issue_page.rs` — chips rows + toggle handlers; static reaction picker
   as sixth editor surface (open/close, cancel_top_editor ordering);
   rail lock row + gate; composer gating (D6); `IssueBusyOp::
   {ToggleReaction, ToggleLock}` with the add/remove split decided in the
   handler at click time.
4. Tests (§5) + docs README row.

## 5. Testing

- **gql**: fixture with mixed groups (non-zero, zero, unknown-content
  group skipped, viewer reacted both ways), comment-level groups,
  `locked` + reason, `viewerPermission` present/null; degradation when
  groups absent entirely.
- **service**: transport for the four ops (add/remove carry `subjectId` +
  `content`; lock/unlock carry `lockableId`, unlock without `lockReason`).
- **page**: chips-row arming (`viewerCanReact` false → row + `+` hidden;
  zero-count chip not rendered); toggle posts the correct mutation per
  armed state, single-flight enforced; picker lists eight, click posts +
  closes, Esc closes; one-editor rule across six surfaces; lock row
  hidden at `READ`/null, shown at `TRIAGE`+; toggle posts
  lock/unlock per state; composer hidden only when locked ∧ ¬rights;
  ids/roles probe assertions.
- Probe keys: those listed in D3/D4/D5 plus `issue-locked-notice`.

## 6. Debug-bridge verification (docs/0010)

After implementation, drive through the bridge where the a11y dump
allows: `tree` shows the chips rows and lock row with ids/roles; `click`
toggles a reaction on a scratch issue; `logs -f` correlates click →
mutation → refetch. Bridge-blocked checks fall back to the human list.

## 7. Acceptance criteria

- Chips render under the issue body and every comment, counts and armed
  state truthful after each refetch; toggle round-trips (add → armed,
  click again → removed).
- `+` picker offers all eight contents and commits on click.
- Lock/unlock round-trips on a scratch issue; rail tag and composer
  gating follow §D5/D6 exactly; everything gated hides (never
  disabled-grays) when the viewer lacks the right.
- All gates green; docs updated in the same PR.

## 8. Human checks (owner)

- [ ] Emoji glyphs render on your setup (no tofu) — else we flip D4's
      contingency to shortcodes.
- [ ] React to a scratch issue via chip and via picker; verify both land
      on github.com (web cross-check).
- [ ] Lock the scratch issue, confirm the composer disappears/reappears
      per role, unlock again.
- [ ] Narrow-window pass: chips rows wrap without breaking the layout.

## 9. Follow-ups (GitHub issues, not this PR)

- Lock-reason picker (`lockReason` input is ready; UI + `LockReason`
  mapping deferred).
- PR-page reactions (0016 parity) — same structs, new placement decision.
- Revisit composer gating if GitHub ships `viewerCanComment`.

---

## 10. Shipped (2026-09-07, issue #121)

Implemented as designed in one PR. The introspected facts (§2) held at
implementation; nothing diverged from the design. Notes beyond the text:

- **Probe-key naming**: chip buttons carry the row prefix plus the wire
  content — `issue-reaction-row-THUMBS_UP` on the body,
  `issue-comment-{ix}-reaction-row-{CONTENT}` on comments — and the row
  itself ends in `-row` (`…-reaction-row-row` on comments reads awkward
  but is stable and greppable; kept for the shared prefix rule).
- **Chips render as ghost buttons; armed chips use `outline`** — the
  armed/unarmed distinction rides the button variants' theme tokens, no
  new styling (D8).
- **`status::init` in probe tests**: reaction/lock toggles really spawn
  through `spawn_mutation`, so the probe harness now installs the
  in-flight tracker global (same contract as `main`; `ProbeHttp` fails
  the POSTs, and the tests assert the busy/picker state transitions).
- **`debug_selector` discipline held** (0010 D10): every asserted surface
  (chips, `+`, picker rows, cancel, lock tag, lock toggle, locked notice,
  comment box) carries an explicit selector — plain `.id()`s are
  invisible to `debug_bounds`, which bit the first probe run.

Workspace: 251 tests green (was 243); clippy/fmt clean. Human checks
(§8) ride the PR body.

### 2026-09-07 (owner feedback): the picker is a popup menu

The inline picker card (D4) read heavy — a full column of eight rows
pushed into the timeline flow. It is now gpui-component's `PopupMenu`
attached to the `+` button via `DropdownMenu`: same static eight rows
(glyph, label, count, check on armed rows), same commit-on-click, groups
still frozen at open (the menu builds from the render-time snapshot —
`reaction_direction`, pure and table-tested).

Consequences, all simplifications:

- `reaction_pick` page state, `open_reaction_picker`, `pick_reaction`
  and the card renderer are gone; menu open/dismiss/Esc/keyboard are
  component-owned (native `Cancel` binding restores focus).
- The one-editor rule is back to **five in-page surfaces**; the menu is
  transient, not an editor — `cancel_top_editor` no longer intercepts
  for it.
- Menu rows render via `PopupMenuItem::element` so each row keeps its
  probe key (`issue-reaction-pick-{CONTENT}` — §7 as written; no Cancel
  row — the menu dismisses natively). The toggle direction still rides
  `reaction_direction` (pure, table-tested).
- Item handlers must **defer** their page update (`window.defer`): a
  click dispatches inside the window's update, and any re-entrant
  window update from there fails ("window not found" — silently, under
  `let _`). Deferred to the end of the effect cycle, the same
  update-from-live-context works; the probe test
  `reaction_menu_item_click_commits` pins the whole chain (open → row
  click → deferred post → scoped error applied) with real simulated
  clicks.
