# 0075 — @-mention completion in the PR comment composer

Type `@` in the PR page's comment box and a small popup of context-relevant
people appears anchored at the caret; typing filters it, the keyboard drives
it, accepting inserts `@login ` and returns you to the draft — the GitHub
web-UI interaction, scoped to what this app's pinned stack can actually
express today. GitHub web is the experience anchor (rhythm and hierarchy,
not pixels); the kit's popover family is the widget-level anchor for the
popup surface.

## Background

The composer is an `Entity<TextareaState>` (docs/0016 D4, draft persistence
docs/0045) with an `InputEvent` subscription already in place. Survey of the
pinned stack (gpui-component `67265a3` / gpui-pre `=0.3.9`):

- The kit's completion machinery (`CompletionMenu`, `CaretAnchoredPopup`,
  `editor_popover`) is `pub(crate)` and bound to `EditorState`;
  `TextareaMode::Extras = ()` — a textarea carries **no** completion-provider
  slot. There is no public path to reuse it for the comment box.
- What *is* public on `InputBaseState` is enough to build the interaction
  in-app: `cursor()` (caret offset), `value()`, `add_selection(range)` +
  `replace()` (atomic undo intent), `range_to_bounds()` / `cursor_layout()`
  (caret and range geometry), `focus_handle()`.
- The candidate data for "who can be mentioned here" is **already in memory**
  when a PR detail is open: `assignees(first: 20)`, `reviewRequests(first: 20)`,
  the PR author, and every comment/review author (`author { login avatarUrl }`).

So v1 is an app-owned popup over the existing textarea, with an upstream
follow-up to expose a real textarea completion API (the fork is the
contribution vehicle, per docs/0048-era doctrine; it lands via a pin bump
like the #530 flip — never a fork dependency).

## Decisions

**D1 — In-app popup, kit-styled surface.** The mention list is a small
app-owned module (crates/modules/github, `mentions.rs`) layered over the
composer via GPUI's deferred overlay, painted with the kit's themed popover
surface (`popover_style` semantics — the same surface every kit popup family
uses). One themed popover surface, no invented anchored-surface treatment
(design guides, popup families).

**D2 — Caret-anchored, box-level fallback.** The popup anchors to the
`@`-token's pixel bounds when `range_to_bounds` resolves it, else to
`cursor_layout()`, else to the composer box's top edge. Element→window
coordinate conversion is resolved at implementation; if it proves brittle
for wrapped lines, the box-level fallback is the shipped behavior for that
case — a quiet degradation, never a crash. The popup flips above/below
depending on available room, capped at 10 rows (~240px).

**D3 — Trigger and token rules (GitHub parity).** `@` opens the popup only
at line start or directly after whitespace — `user@example.com` never
triggers. The query is `@` + characters up to the caret; it filters
case-insensitively by substring against login, then name. The popup
dismisses when: the caret leaves the token, whitespace follows the token,
the composer loses focus, the draft is submitted, or the filter matches
nothing (quiet dismissal — no "no results" ghost row in v1). A second `@`
within the token restarts the token fresh.

**D4 — Keyboard contract; focus never leaves the textarea.** While the
popup is open the composer keeps focus (combobox pattern): ↑/↓ move the
selection with wrap-around, Enter and Tab accept, Esc dismisses — and Esc
must not bubble into page-level handlers (takeover close, history back).
Typing filters; mouse hover highlights and click accepts. Bridge-visible
per docs/0010 D10: the popup is named "Mention suggestions", rows carry
`.id()`s and `option` roles inside a `listbox`, and the active row reports
its selected state so `tree` and audits see the whole thing.

**D5 — Insert semantics.** Accepting replaces the token range with
`@login` + a single trailing space, caret after the space, in one undo
step (the state's atomic edit path). The draft debounce-save observes a
normal `InputEvent`, so docs/0045 draft persistence is untouched.

**D6 — Candidate model and ranking.** `MentionCandidate { login, name,
avatar_url }`, deduped by login, derived synchronously from the loaded PR
document at popup open — no fetch, no spinner, no rate cost. Ranking:
review-requested reviewers, then assignees, then the PR author, then
comment/review authors by recency; filtered results keep that order and
cap at 10. (GitHub web additionally searches org-wide people; that needs a
new query and is a non-goal here.)

**D7 — Avatars reuse the remote-image client.** Same gpui `HttpClient`
path as comment avatars (docs/0005 §6.3), standard monogram fallback when
unset or failed.

**D8 — Scope: the main PR comment box only.** The mechanism is
composer-agnostic by construction (it needs the `TextareaState` handle and
a candidate closure); wiring the reply composer, line composers, the
review-thread editor, and the issue-page box is a fast-follow issue reusing
D3–D6 verbatim. Not in this feature's PR.

**D11 — Composer rollout (#578).** The four deferred composers shipped on
the same contracts: one mention slot per page, tagged with a
`MentionComposer` (comment box, line composer, reply, thread edit, issue
box) that routes sync/accept to the owning textarea and lets each render
site gate the shared `mention_wrap` chrome on its own tag — so at most one
popup exists per page and typing into another composer replaces it, which
matches blur-dismiss semantics. The issue page ranks `assignees → author →
comment authors` (no review requests or reviews exist there); still zero
new GraphQL. The issue page's five key bindings are a mirror set scoped to
`GithubIssuePage && Mention && Input`. The PR-side description/timeline
editors and the finish-review summary remain unwired (Non-goals).

**D9 — Verification plan.** The 0065 receipt for the composer grows a
mention drive: focus the box, type `@`, assert popup rows by id/label,
accept one, assert the inserted text; a structural golden of the open-popup
state is captured in the containerized env (docs/0071) in the same PR; the
visual critique ritual (0065 D7) runs before/after against the GitHub-web
anchor. Audit and tab-stops stay green.

**D10 — No new dependencies.** Substring filtering plus the D6 ranking is
the whole matching story in v1; no fuzzy-match crate.

## Non-goals

- ~~Wiring the remaining composers (reply, line, review-thread, issue-page
  box) — fast-follow issue, D8.~~ Shipped by #578 (D11).
- The PR-side description/timeline body editors and the finish-review
  summary textarea (markdown bodies, not chat-style comments — same
  reasoning as the original D8 cut).
- Org-wide / `assignableUsers` lookup beyond the loaded document.
- Emoji picker, `/` commands, `#` issue references — separate features.
- The upstream public textarea-completion API (contribution filed from the
  fork; ships via pin bump when merged and released).
