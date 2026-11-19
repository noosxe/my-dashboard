# 0086 — Migrating to the gpui-fast (retained-mode) GPUI backend

- **Date:** 2026-11-19
- **Issue:** #525
- **Trial:** draft PR #943 (not for merge; its commits are reused, §4.1)
- **Amends:** nothing. Older version-pinned "gpui-pre" notes stay as the
  record of what was verified then (§4.6).

## 1. Decision

The app moves from the immediate-mode `gpui-pre` backend to the
retained-mode `gpui-fast` backend that gpui-component (published as
gpui-kit) offers behind its `gpui-fast` feature. The reason is
CPU: a retained-mode view tree re-runs `render` only for views that were
notified, instead of rebuilding every view every frame.

The price is a permanent rendering contract (§3): a missed notify is a
stale UI with no error. This doc makes that contract explicit and
enforceable before the flip ships.

## 2. Evidence (trial #943)

Setup: gpui-component v0.10.0 (rev `f697c31`), `gpui-fast =0.1.16` and
`gpui-fast-platform =0.1.16` through the `gpui-fast` feature, the
manual `impl IntoElement for MentionPopup` dropped, `window.refresh()`
replaced by `cx.notify()` where it was the only repaint trigger.

- **Gates:** fmt, clippy (zero warnings), tests, release check green.
- **UI output:** identical. Critique frames at seven stops have equal
  node trees once ids are stripped; pixel diff at most 41 px of about
  1M; goldens unchanged (the five failing structure-walk steps fail on
  both sides and are already listed as expected red).
- **Software container** (pixman): main-thread CPU about -40% idle and
  -25 to -45% scrolling the docs/0041 §10 PR-page workload; total
  process CPU equal, because software rasterization dominates.
- **Real GPU** (`scripts/verify.sh --gpu`, docs/0071 D10, a Mesa Vulkan
  render node, two runs per side): idle on the PR page, about 25-35%
  less main-thread and total CPU per frame; scrolling, 5-27% less per
  frame (two runs is too few for a tighter number). Home idle draws
  two frames either way, so nothing to save there. The GPU was not the
  bottleneck on either side.
- **Costs:** debug binary +7.3%; cold build 59 s vs 56 s.

## 3. The rendering contract under retained mode

gpui-fast re-runs a cached view's `render` only when that view's
entity (or an entity it observes) is notified. Three rules follow, and
they apply to all new code:

1. **Notify the owner after mutating tracked state.** Any change a view
   shows goes through `cx.notify()` on the entity that owns the view
   (or an entity the view `cx.observe`s). A plain `Cell` or field
   mutated from a paint callback counts: notify the page.
2. **`Window::refresh()` is not a redraw request for cached views.** It
   marks the window dirty; it does not re-run a cached view's render.
   Never use it as the only trigger. Where a notify already precedes
   it, it is redundant.
3. **A time-driven label needs its own render wake**: a timer that
   calls `cx.notify()` on the view at the label's granularity (the
   pattern the home card and module page already use, docs/0052 D6).
   Nothing global repaints on its behalf.

`cx.refresh_windows()` (used by the theme restyle in `settings.rs`) is
the same class of call as rule 2. Whether it re-styles cached views
must be checked on the trial stack in the implementation PR (§4.4); if
not, the theme-change path notifies instead.

### 3.1 Enforcement options

| Option | Catches | Cost | Verdict |
|---|---|---|---|
| `clippy.toml` `disallowed_methods` for `gpui::Window::refresh` | any new direct call, at the gate that already runs | one line, plus a justified `#[allow]` per legitimate site; same mechanism as the token-discipline lints | **Recommended** |
| A test pattern (assert a view re-renders after a mutation) | the specific missed notify | one test per view; GPUI test rigs make this heavy; only covers views someone remembered to test | Use for the three clock labels (§4.3), not as the general gate |
| A debug-bridge check (tree dump before/after a state change) | stale output in a running app | needs a scripted scenario per surface; slow; container-only | Human check and receipts only, not a gate |

The lint is feasible: `Window::refresh` is a plain inherent method
(`pub fn refresh(&mut self)`) in gpui-fast 0.1.16's `window.rs`, and
the dependency is renamed to `gpui`, so the path is
`gpui::Window::refresh`, the same shape as the existing
`gpui::Styled::text_size` entries. The implementation PR confirms the
lint fires on a deliberate probe call before relying on it, and adds
`gpui::App::refresh_windows` to the list only if the §3 check shows it
does not repaint cached views.

The lint is blunt (it cannot know a notify precedes), so the rule for
an `#[allow]` is: a comment naming the notify that precedes it, or the
reason none is needed. Where the refresh is redundant the implementation
removes it instead.

## 4. Implementation scope

### 4.1 The flip

Reuse the trial's commits (`ec04006`, `57c973d`, `561d31e`, `1c9c639`),
rebased on the then-current main and re-run:

- `Cargo.toml`: `gpui` and `gpui_platform` become the `gpui-fast` and
  `gpui-fast-platform` packages (same library names, so `use gpui::…`
  is unchanged), `gpui-component` gains the `gpui-fast` feature,
  `[profile.dev.package]` opt-levels follow the new package names, and
  the explanatory comment block is rewritten.
- `MentionPopup`: remove the manual `impl IntoElement`; gpui-fast's
  blanket impl conflicts with it.
- Notify-after-mutation fixes from the trial: the status tracker's
  begin/end, the favicon page's `wide` cell, the PR timeline anchor
  restore.

The `reqwest_client` dependency stays on `gpui-pre-reqwest-client`
(it implements the http-client crate that gpui-fast also uses).

### 4.2 The 10 s service tick

The end-of-beat `window.refresh()` in the GitHub service tick loop
(`service.rs`) is the one site that did real work under the old
backend: it was a global repaint every 10 s when no result applied
(gated, paused, offline, nothing planned). Under gpui-fast it repaints
nothing cached. It is removed, and the labels that leaned on it get
their own wakes (§4.3). The other eleven sites already follow a notify
(trial audit) and are either removed as redundant or kept with a
justified `#[allow]`.

### 4.3 Clock-label render wakes

Each wake is a timer that notifies the owning view at minute
granularity, like the card's 30 s wake:

- "synced Nm ago" on the issue page and the notification page;
- "synced Nm ago" on the PR header when the PR is not watched (the
  existing 5 s loop only runs while watched);
- relative times in the notification panel (while it is open).

Already safe, unchanged: PR header "checked" label (5 s loop), usage
chip countdown (1 s loop), home and module "synced" labels (30 s).
The usage page reset countdown uses a frozen `as_of` by design.
Each new wake gets a test that advances the clock and asserts a
re-render, using the pinned clock of the fixture mode.

### 4.4 Enforcement

The lint from §3.1, the allows it needs, and a rule in AGENTS.md
under the PR discipline section pointing at this doc's §3. The theme
restyle check from §3 lands here too.

### 4.5 Dependency pins

v0.10.0 pins `gpui-fast =0.1.16`; we stay on that pair. Upstream
gpui-kit main has already moved to 0.1.17 (longbridge/gpui-kit#4027)
but has not released it. We take 0.1.17 with the next gpui-kit
release, as a normal deliberate bump (the Cargo.toml bump checklist).
No git-rev pin to get it early (§6).

### 4.6 Docs

- AGENTS.md: the rendering contract (§3) and the lint's allow rule.
- docs/0010 and docs/0071: no change in behaviour; add one line each
  that the bridge and receipts are re-verified on the gpui-fast stack.
- README (macOS Metal build note, "since the gpui-pre 0.3.7 pin"):
  reword to the current backend once the Metal shader build is
  confirmed unchanged on the new stack.
- The Cargo.toml comment block: rewritten (§4.1).
- Older docs (0001, 0002, 0004, 0032, 0036, 0040, 0041, 0048, 0049,
  0060, 0064, 0070, 0073, 0075) cite gpui-pre sources at specific
  versions as the record of what was verified. They are historical and
  are not rewritten. Where a doc describes behaviour that depends on
  immediate-mode (a repaint every frame or tick), the implementation
  PR adds a short *Amended* note, found by grepping `docs/` for
  `refresh()` and "every frame" at implementation time.

## 5. gpui-pre stays in the graph

gpui-base, gpui-component and gpui-kit-assets depend on `gpui-pre`
unconditionally, so it is compiled even though no first-party code
reaches it (the http-client, scheduler and util crates are shared by
both families). We migrate now and leave it. The cost is compile time
and graph size; the alternative, patching or forking gpui-component to
make it optional, is a maintenance burden larger than the cost.

Upstream tracks making it optional as item 1 of longbridge/gpui-kit#3863.
**Condition for dropping it:** a released gpui-kit whose gpui-base,
gpui-component and gpui-kit-assets compile without `gpui-pre` when the
`gpui-fast` feature is on, confirmed by `cargo tree -i gpui-pre`
showing only the shared http-client/scheduler/util crates (or nothing).
At that bump, the `gpui-pre` entries in `[profile.dev.package]` and
any leftover pins go.

## 6. Non-goals

Each becomes a GitHub issue labelled `deferred` before the
implementation merges (AGENTS.md).

- Dropping `gpui-pre` from the graph (§5).
- A git-rev pin of gpui-kit to get `gpui-fast` 0.1.17 before its
  release (§4.5).

## 7. Rollback

The flip is one commit set confined to `Cargo.toml`/`Cargo.lock`, the
`MentionPopup` impl and the notify fixes. To revert: `git revert` the
flip commit (the `gpui-fast` feature off, packages back to `gpui-pre
=0.3.12`, `MentionPopup` impl restored). The notify additions are
harmless under the old backend and may stay; the lint and the removed
tick refresh are the only things that would need reverting with it.
Because gpui-pre stays in the lockfile resolution graph, the revert
needs no new dependency resolution beyond the lockfile restore.

## 8. Verification plan

1. **Gates:** `cargo fmt --all --check`, `cargo clippy --workspace
   --all-targets` (zero warnings, with the new lint active and a probe
   call proving it fires), `cargo test --workspace`, release check.
2. **Container (docs/0071):** structure walk with `audit` over every
   surface, `tree --tab-stops`, `assert-baseline` against the committed
   goldens (expected: unchanged; any drift explained), critique frames
   for the seven trial stops.
3. **Clock-label tests** from §4.3, plus a theme-change check (§3).
4. **GPU perf receipt:** `scripts/verify.sh --gpu`, the same
   docs/0041 §10 workload, at least three runs per side to tighten the
   scroll figure; recorded in the PR body.
5. **Human check:** run the app; watch the home card, PR header (watched
   and unwatched), issue page, notification page and panel, and the
   usage chip for several minutes while idle, including with the
   network off; scroll a long PR page; resize the diff seam; sign in
   and out, including the discard-drafts confirm; switch theme.
