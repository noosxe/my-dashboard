# 0085 — Timeline cross-reference rows (PR and issue pages)

- **Date:** 2026-11-18
- **Issue:** #918
- **Part of:** #730 (PR view fixes)
- **Amends:** docs/0016 (the PR page event rows: "Not yet: cross-reference"),
  docs/0021 (its cross-reference deferral)
- **Upstream pins:** gpui-component `f697c31` (v0.10.0) · GitHub GraphQL
  verified live 2026-11-18

## 1. Context & problem

Both conversation timelines interleave comment cards with muted one-line event
rows: the PR page's shipped in #911 (docs/0016, "PR timeline event rows"), the
issue page's in docs/0021. Both deferred cross-references — the "octo-dev mentioned
this pull request in #42" lines — because they are the only event rows that
point at another object and so are interactive. Without them a PR page hides
the linked issue that it will close, and an issue page hides the PR that fixes
it; github.com shows both prominently. #918 tracks the work.

This doc designs one row, shared by both pages: its wire shape, the query
window it rides, its wording and look, its click target and keyboard behaviour,
and how an unreadable source degrades.

## 2. Scope and non-goals

**In:** `CrossReferencedEvent` rows on the PR page and the issue page, one
shared module, one query shape, fixtures and receipts.

**Out (each gets a `deferred` issue before the implementation merges):**

- `ReferencedEvent` (commit references) — see D1 (Q4).
- Rows for the closer of a `ClosedEvent` ("closed this in #N") and
  `duplicateOf` — a different event, with its own wording.
- Connected/disconnected, sub-issue and project events.
- Opening a source from an untracked repo inside the app (D6).
- Grouping or collapsing bursts of references (D4).

## 3. Verified facts

Live `gh api graphql` introspection (`__type`) and a live read against a public
repository, 2026-11-18.

- `CrossReferencedEvent` fields: `id`, `createdAt DateTime!`, `actor Actor`
  (nullable interface), **`source ReferencedSubject!`**, **`target
  ReferencedSubject!`**, `isCrossRepository Boolean!`, `willCloseTarget
  Boolean!`, `referencedAt DateTime`, `resourcePath URI!`, `url URI`.
  `source` is documented as "issue or pull request that made the reference",
  `target` as the one referenced, `willCloseTarget` as "the target will be
  closed when the source is merged".
- `ReferencedSubject` is a **union of `Issue` and `PullRequest`** only: a
  bare selection on it is an error (the docs/0021 live-API lesson), so every
  field rides an inline fragment.
- Both `IssueTimelineItems` and `PullRequestTimelineItems` contain
  `CrossReferencedEvent`; both item-type enums have `CROSS_REFERENCED_EVENT`
  and `REFERENCED_EVENT`.
- Fields the row needs on the source: `Issue` has `number`, `title`, `state`,
  `stateReason`, `url`, `repository`; `PullRequest` has `number`, `title`,
  `state`, `isDraft`, `merged`, `url`, `repository`; `Repository` has
  `nameWithOwner` and `isPrivate`.
- A live read of a PR's cross-references returned the expected shape for both
  source kinds, with `actor: null` on one node (a deleted user) and
  `willCloseTarget: false` on all.
- **Not verified:** the response for a source the viewer cannot read. `source`
  is non-null, so GitHub's null-propagation rule would null the whole list
  element and add an `errors` entry; no readable fixture of a private source
  was available to a test viewer. D7 is written to survive either shape and the
  implementation tests it with a hand-built envelope.
- The target is always the page's own subject, so the query does not select it.
- The issue page has no GraphQL detail fixtures today (`RepoIssues` only): the
  issue page cannot be captured in fixture mode yet (D9).

## 4. Decisions on the open questions

Decided 2026-11-18, each matching the recommendation first recorded here.

- **Q1 — Source in an untracked repository:** opens in the browser; in-app
  only for repos the module lists (D6, follow-up §12).
- **Q2 — Bursts of references:** only exact repeats (same source, same actor)
  fold into the first; different sources never collapse (D4).
- **Q3 — Unreadable source:** the row is dropped, no stub (D7).
- **Q4 — `ReferencedEvent` (commit mentions):** stays out; filed as a
  `deferred` issue before the implementation merges (D1, §2, §12).

## 5. Design

### D1 — Wire: `CROSS_REFERENCED_EVENT` only

One kind, `CrossReferencedEvent`; `REFERENCED_EVENT` stays out. A commit
reference needs a commit id, a commit repository and a link target the app has
no page for (a commit in an arbitrary repo), and a busy repo produces one per
mentioning commit. Reason: smallest honest row; the commit case is its own
design.

Selection (inline fragments only, like every other event):

```graphql
references: timelineItems(last: 100, itemTypes: [CROSS_REFERENCED_EVENT]) {
  filteredCount
  nodes {
    __typename
    ... on CrossReferencedEvent {
      id createdAt actor { login } isCrossRepository willCloseTarget
      source {
        __typename
        ... on Issue { number title state stateReason url
                       repository { nameWithOwner isPrivate } }
        ... on PullRequest { number title state isDraft merged url
                             repository { nameWithOwner isPrivate } }
      }
    }
  }
}
```

Wire type, shared by both pages in `gql/xref_events.rs`:

```rust
pub struct XrefEvent {
    pub id: String,
    pub actor_login: Option<String>,
    pub created_at: Option<String>,
    pub will_close: bool,
    pub source: XrefSource,
}
pub struct XrefSource {
    pub is_pr: bool,
    pub number: i64,
    pub title: String,
    pub repo: String,          // nameWithOwner
    pub url: String,
    pub state: XrefState,      // Open | Draft | Merged | Closed | NotPlanned
}
pub struct XrefEvents { pub nodes: Vec<XrefEvent>, pub total_count: i64 }
```

`isCrossRepository` is not stored: comparing `source.repo` with the page's repo
is the same answer without trusting a second field. `isPrivate` is not stored
either (D6 decides by tracked-ness, not privacy). Reason: fewer fields, one
source of truth.

### D2 — Its own aliased window, newest 100, on both pages

Cross-references get their own aliased `timelineItems` (`references`), not a
place in `events` or `activity`. Reason: docs/0016's window split exists so a
burst of one kind cannot push another out; references are the burstiest kind on
a popular issue (every mention from every repo), and merged/closed rows must not
fall out because of them. Cost: one more selection, up to 100 small nodes.

Both pages use `last: 100`, so the newest references survive. The issue page's
existing event window is `first: 100`; it is not changed here. `filteredCount`
minus the carried nodes joins the existing cap row's missing count (the issue
page already sums two shortfalls; the PR page's `timeline_events::hidden` gains
a third term). Reason: one honest "N older/more on github.com" row, not a
second one.

### D3 — The row

Same anatomy as the #919 rows: `[glyph] [sentence …] [age]`, `text_sm`, muted,
one line that wraps, age at the trailing edge, actor and subject in the shared
emphasis (medium weight, foreground; no new colour).

- **Glyph** — the *source's* state, reusing `state_pill` (docs/0002 tokens):
  open issue/PR `Open` success; draft PR `Draft` muted; merged PR `Merged`
  info; closed PR `Cross` danger; closed issue (completed) `Check` info;
  closed issue (not planned) `Cross` muted. Reason: github.com colours the
  source's state, and it is the row's one scannable fact.
- **Wording** (page kind is "issue" or "pull request"):

| Case | Sentence |
|---|---|
| source mentions the page | "{actor} mentioned this {issue/pull request} in" |
| `willCloseTarget`, source is a PR, page is an issue | "{actor} linked a pull request that will close this issue" |
| `willCloseTarget`, otherwise | the mention sentence (the flag only means something for PR → issue) |
| actor null | "Someone" replaces `{actor}` |

  then the source title (emphasis) and its reference `#N`, or
  `owner/repo#N` when the source repo differs from the page's, in mono muted.
  Reason: github.com's wording, kept, and the flag is honoured only where it
  is meaningful.
- **Truncation** — the title wraps word by word beside the sentence like a
  renamed title (docs/0016); the reference and the age never break. No
  character cap. Reason: one wrapping rule across event rows.
- **Burst** — see D4.

### D4 — Ordering and repeats

Rows interleave with the cards by time, card first at an equal stamp, the
existing order. A reference with the same source (repo + number) and the same
actor as an earlier one in the window drops: GitHub emits a new event each time
a source edit or comment mentions the target again, and the repeat adds
nothing. Different sources never collapse. Reason: dedup removes noise without
hiding anything a reader could act on.

### D5 — Shared module

`crates/modules/github/src/xref_row/` (one concern: this row), imported by both
pages, with tests next to the code:

- `mod.rs` — `row(ix, id_prefix, page_kind, event, cx) -> AnyElement` and the
  navigation target type.
- `sentence.rs` — the words (table above), pure functions.
- `look.rs` — `XrefState` → `(Tone, Glyph)`.
- `target.rs` — `Target::{InApp{repo, number, is_pr}, Browser(url)}` and the
  tracked-repo decision.

The wire parser lives in `gql/xref_events.rs` next to `pr_events.rs`. The PR
page keeps merging in `pr_page/timeline_events.rs` (a new `Entry::Xref(ix)`),
the issue page in its existing merge, each only a few lines; neither large
file grows by the row itself (AGENTS.md "Focused modules").

### D6 — Click target and navigation

The whole row is one button. Activation:

1. **Source repo is tracked by the module** (listed in the repos the Pulls and
   Issues lanes know): open the in-app PR or issue page, the same hand-off the
   notification page uses (`NotificationPageEvent::OpenSubject`, handled in
   `page.rs` by seeding a minimal row and calling `open_pr` / `open_issue`).
   That handler is lifted into one `open_subject(repo, number, is_pr)` method
   on the host; the notification path and the new rows both call it. It
   records a trail place, so Back returns to the originating page.
2. **Otherwise** `cx.open_url(source.url)` (browser). Reason: an untracked repo
   has no lane, refresh or trail context (Q1); the browser always works, and
   the URL comes from the query, never reconstructed.

The pages emit the request up: `PrPageEvent::OpenSubject{…}` and
`IssuePageEvent::OpenSubject{…}`, as `Leave` and `CloseRequested` already
travel. Leaving a PR page runs the existing leave guard (unsent review drafts,
`LeaveIntent`), so `LeaveIntent` gains a `Subject` variant routed through the
same confirm; the issue page applies whatever guard it has for Esc. Reason:
no navigation path may drop a draft silently.

Self-references (source is the page's own subject) are not possible from
GitHub; if one arrives it drops at parse time.

### D7 — Unreadable and private sources

- **Source null, node null, or a missing number/title** → the row drops
  (tolerant parse, docs/0007 D3), and the carried-versus-`filteredCount`
  difference keeps the cap row honest. The implementation must prove that a
  response with `errors` and a null list element does not fail the whole page
  read (hand-built envelope test); if it does today, the detail read keeps the
  partial data for this alias.
- **Private repo the viewer can see** → shown like any other source with its
  `owner/repo#N`; tracked → in-app, untracked → browser. GitHub only returns it
  because the viewer may read it. Its title is shown: the viewer already sees it
  on github.com.
- **Actor null** → "Someone".

### D8 — Keyboard, accessibility, focus

- Id `pr-event-{ix}` / `issue-event-{ix}` (the existing row key; the row's
  index is its entry index) with role `Button` (docs/0010 D10), **tab stop on**:
  it is the one event row that is interactive, so it is reachable by Tab and
  activated by Enter or Space. Reason: a click-only row would be invisible to
  keyboard users and to the dump.
- Accessible name: the full sentence plus the source's state and reference
  ("octo-dev mentioned this issue in Fix loader #42, open pull request. Opens the
  pull request.").
- Focus: a border in the theme `ring` token on the row's rounded rect, as
  gpui-component's `Select` and `ColorPicker` draw their focus; hover paints
  the theme `accent` token at half opacity, as the mention list does
  (`mentions.rs`). Both tokens exist on `f697c31` (`ThemeColor::ring` in
  `crates/component/src/theme/theme_color.rs`, `ColorTokens::accent`). No data-coloured
  hex, no ad-hoc sizes (docs/0002 D5/D9/D10 lints apply).
- Many rows mean many tab stops; accepted (bounded at 100, and github.com is
  the same). A debug-bridge `tree --tab-stops` receipt covers the order.

## 6. Implementation plan

Two implementation issues, filed after this doc merges (each a focused PR):

1. **Shared row and wire (`xref_row`, `gql/xref_events.rs`) plus the PR page**:
   parser and tests, row, sentence/look/target tables, `open_subject` lift on
   the host, PR-page merge/cap/leave guard, PR fixtures, goldens.
2. **Issue page**: query alias, merge, cap-row term, `IssuePageEvent`, the
   missing `IssueDetail` fixtures (D9), receipts.

Two PRs, not one: the second stacks on the first (shared module) and adds a
fixture family of its own. Docs: each PR updates docs/0016 / docs/0021 text it
touches; this doc is not edited for status.

## 7. Testing

- Parser: both source kinds and all `XrefState` outcomes (draft, merged,
  closed, not planned); null actor; null source / null node / missing title
  drop; duplicates fold; `willCloseTarget` only changes the sentence for
  PR → issue; the document-level tripwire (every fragment carries its fields).
- Tables: sentence per case, `owner/repo#N` versus `#N`, "Someone".
- Target: tracked repo → in-app, untracked → browser, URL from the node.
- Probe: rows render in time order among cards; Tab reaches them; Enter on a
  tracked source opens the page and Back returns; the leave guard fires on a
  PR page with a draft.

## 8. Debug-bridge and receipt plan

Per AGENTS.md, in the containerized env (docs/0071): `audit` over the PR page
and the issue page, `tree --tab-stops` (the new stops), `assert-baseline` for
the goldens below, `critique` before/after with the Appendix A rubric (max 3
iterations), and a new receipt script `crates/debug/verify/timeline-xref.txt`
that opens each scenario, asserts the row labels, tabs to a row and activates
a tracked and an untracked source. The bridge records no browser opens, so
the untracked path is asserted by what it does not do (the page stays) and by
the target unit tests. Anchor (docs/0065 D8): github.com's timeline for the
sentence and glyph rhythm; the #919 rows for density and emphasis.

## 9. Fixtures and goldens

Synthetic scenarios only (docs/0081 §12.1): `fixture-user` owns the tracked
repositories `beacon` and `atlas`, `fixture-org/elsewhere` is the untracked
one, `octo-dev` is the author:

- `PrDetail.101` (populated) gains `references`: an open issue and a draft PR
  in `beacon`, a merged PR in tracked `atlas` (cross-repo reference), a closed
  PR, an untracked repo's source, one with a null actor, one long title.
  Goldens `github-pr-populated.json` (and `github-pr.json` if it dumps the
  rows) change; `pr-page.txt` receipts are re-recorded.
- `PrDetail.102` (overflow) carries `filteredCount` above its nodes, so the
  cap row counts references too; `pr-conversation-caps.txt` changes.
- A hand-built partial-errors envelope covers D7 in a Rust test, not a
  fixture file.
- Issue page: new `IssueDetail.<n>.json` files (an open issue with a linked
  PR that will close it, a closed issue). The issue page gains its first
  structural golden; `docs/0071` capture applies.

No other golden changes: pages without references keep identical trees.

## 10. Acceptance criteria

1. A PR or issue with references shows one row per distinct source, in time
   order, glyph tinted by the source's state, sentence per D3.
2. A PR that will close an issue reads "linked a pull request that will close
   this issue" on that issue.
3. Enter/Space/click opens the in-app page for a tracked repo and the browser
   otherwise; Back returns; a PR page with an unsent review draft asks first.
4. Unreadable sources never produce a row or a page error.
5. More than 100 references show the existing cap row with the larger count.
6. Gates and the 0065 receipts are green; goldens updated, drift explained.

## 11. Human checks

- [ ] A scratch issue mentioned from a PR, from an issue and from another
      repo: three rows, correct wording and glyph colours.
- [ ] A PR using "Fixes #N": the issue page shows the "will close" sentence.
- [ ] Click a row for a tracked repo (opens in-app, Back returns) and an
      untracked one (opens the browser).
- [ ] Tab through a busy timeline: focus ring visible, Enter activates.
- [ ] A reference from a private repo you can read; one you cannot: no
      row, no error.

## 12. Follow-ups (GitHub issues, tagged `deferred`)

- Commit references (`ReferencedEvent`) rows.
- "Closed this in #N" rows from `ClosedEvent.closer` / `duplicateOf`.
- Open untracked-repo sources in-app (Q1).
- Collapse or group long reference bursts (Q2).
