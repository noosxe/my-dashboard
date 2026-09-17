# 0037 — Freshness system: one substrate, two page patterns, zero hand-wiring

| | |
|---|---|
| Status | design |
| Builds on | docs/0031 (`Fetch` record), 0032/0035 (heartbeat + repo-rows beat), 0033 (synced labels), 0034 (cold-start staleness), 0036 (backoff) |
| Related | #192/#193/#224 (closed — their deferral loop is §1), the owner's "consistent systemic approach" direction |

## 1. Problem

Every freshness affordance so far was wired by hand, per page, and the
hand-wiring compounds with each feature:

- **Three copy-pasted header rows** — the `synced_label` + spinner +
  `RotateCw` refresh-button row is hand-built three times
  (pr_page.rs:1760, issue_page.rs:1541, notification_page.rs:529),
  identical in shape, drifting in detail (selectors, tooltips, what the
  refresh button also refetches).
- **Five hand-composed policy gates** — after docs/0036, every lane
  call-site composes `needs_refresh(min_age) && backoff_elapsed()`
  itself. The next policy change (a jitter, a per-lane cap) edits five
  pages.
- **Staleness copy in two more places** — the 0034 chips render
  hand-placed in `page.rs` and `card.rs`.
- **The deferral loop that motivated this** — docs/0033 D1 scoped
  labels to detail pages and explicitly promised the list-level
  indicator would come with #224; docs/0035 then shipped #224 with
  "no UI changes" (D4), and the label fell between two closed issues.
  The repo page today is blind between "fresh" and the 30-minute
  stale chip — precisely where its rows now auto-refresh.

The substrate underneath is already centralized (`Fetch` record,
backoff policy, `synced_label`, `rows_landed`). What is missing is a
**system**: one place where "a refreshable surface" means something,
and two named page patterns that make freshness, backoff and the
synced indicator automatic instead of re-assembled.

## 2. Goals and non-goals

**Goals**

- G1: One module owns freshness: the clock abstraction, the composed
  refresh policy, and the header component. Adding the repo-page
  synced label is a *consumer*, not another hand-wiring.
- G2: Two standard page patterns, documented as the way new pages get
  built: **detail-page** and **list-surface**. A new page that matches
  one declares its clocks and gets policy, backoff, labels, spinner
  and beat wiring for free.
- G3: Policy evolution is a one-file change: the five composed gates
  collapse into the record; the header row is rendered from one
  component.

**Non-goals**

- N1: **Not a page framework.** No lane-registration closures, no
  generic page engine, no trait the pages implement for the beat to
  reflect over. Pages keep their content, their slots, their nav
  wiring; the system standardizes freshness + the header row only.
- N2: The 0034 stale chip keeps its own rendering — its semantics are
  cold-start-specific (disk-seeded cache age, threshold-marked,
  reconnect copy), not lane freshness. It does not join the component.
- N3: No cross-module lift yet: the module lives in the github crate.
  A second consumer module is the trigger to move it to dashboard-core
  (docs/0034 D6 reasoning).
- N4: No visual redesign — the migrated header rows render
  byte-identically (probe tests must stay green unchanged).

## 3. Current mechanics (verified in-tree 2026-09-17)

- `Fetch<T>` (service.rs): `age()`, `is_loading()`, `needs_refresh`,
  `backoff_elapsed()` — the per-record substrate, plus
  `attempted_at`/`fail_streak` (0036).
- `rows_landed: BTree<String, Instant>` (0035 D1) — the list-surface
  clock; in-session only, never persisted. Has **no UI reader**.
- `synced_label(Option<Duration>)` and `relative_age` — `pub(crate)`
  helpers in page.rs, already reused by the 0034 chip.
- The heartbeat host (`refresh_open_page`, page.rs): slot
  mutual-exclusion → per-page `auto_refresh(min_age)` → per-lane
  gates; the repo-rows arm → `repo_rows_refresh_specs`.
- `gated()` — rate-limit/auth/pending silence, checked once per beat.

## 4. Design decisions

**D1 — A `freshness` module in the github crate.** `freshness.rs`
owns everything from this doc. Not `dashboard-core`: today exactly one
module consumes it, and the crate boundary gets crossed when a second
consumer appears (N3), not before.

**D2 — One clock trait, three implementations.**

```rust
/// A refreshable surface's freshness: what the header shows and the
/// beat gates on (docs/0037 D2).
pub(crate) trait Freshness {
    fn age(&self) -> Option<Duration>;
    fn is_loading(&self) -> bool;
    /// The composed dispatch gate (D4): stale-or-failed per the 0032
    /// policy AND past the 0036 backoff. One call-site per lane.
    fn wants_refresh(&self, min_age: Duration) -> bool;
}
```

- `Fetch<T>` implements it directly (`age`/`is_loading` already exist;
  `wants_refresh` composes `needs_refresh && backoff_elapsed` — the
  five hand-composed sites become `lane.wants_refresh(min_age)`).
- `LandedClock(Option<Instant>)` — the list-surface clock: a
  `rows_landed` lookup wrapped up. `is_loading` is always false (rows
  land in bulk, no per-page spinner); `wants_refresh` is the landed
  age past `min_age` (the 0035 gate) — the backoff bookkeeping stays
  state-side as shipped in 0036.
- The 0034 cache-age clock stays **outside** the trait (N2): it is a
  threshold marker, not a refresh gate.

**D3 — `FreshnessRow`: the header component.** One function building
the row all three pages hand-roll today:

```rust
pub(crate) fn freshness_row<F: Freshness>(
    id: &str, clock: &F, cx: &mut Context</* page */>,
    on_refresh: impl Fn(&mut Page, &mut Context<Page>),
) -> AnyElement
```

rendering exactly today's shape — `synced_label(clock.age())` (none →
nothing), `is_loading` → small spinner, then the ghost `RotateCw`
refresh button calling `on_refresh`. Debug selector:
`{id}-synced` / `{id}-refresh` — the existing selectors
(`pr-synced`, …) keep their names so probe tests stay green. The
"what else the refresh button refetches" quirk (PR page refetches
files, D8) lives in the caller's `on_refresh` — behavior preserved,
composition stays at the call-site where it is visible.

**D4 — Policy composition moves into the record** (the G3 change):
`Fetch::wants_refresh` is the only place `needs_refresh` and
`backoff_elapsed` meet. The beat's per-lane arms and the manual
refresh paths keep their existing semantics (manual refresh ignores
backoff — it is user-invoked; unchanged).

**D5 — Two standard page patterns, documented in docs/0029 §4's
successor section.** They are recipes, not code:

- **Detail-page pattern** — the page owns one primary `Fetch<T>`
  (identity payload) plus optional side lanes. To build one: declare
  the records; render `freshness_row("x", &self.detail, …)` in the
  header action row; implement `auto_refresh` as
  `lane.wants_refresh(min_age)` arms calling the fetch methods;
  register the beat entry in `refresh_open_page`'s slot chain. You
  get: the synced label, the spinner, backoff, the rate-limit gate,
  the focus trigger.
- **List-surface pattern** — the page renders rows keyed by ident
  with a landed clock (`rows_landed`) and a beat arm returning specs.
  To build one: bump `rows_landed` where the rows apply; expose a
  `*_refresh_specs(ident, min_age)`; render `freshness_row` from a
  `LandedClock`. You get: the beat refresh, the backoff mirror, the
  synced label.

**D6 — The repo-page label is the pilot adopter** (closes the §1
deferral loop). The module main page header gets a
`freshness_row("repo-rows", &LandedClock(rows_landed.get(selected)))`
rendering `synced Xm ago` from the exact clock the beat already uses.
Honesty rules carry over: no `rows_landed` entry → no label
("nothing confirmed yet" beats a fabricated timestamp — 0033 D2);
cold start shows it as soon as the open or beat lands. The 0034 stale
chip stays in place alongside it (different question: cache vs. this
session's rows).

## 5. Testing

- **Unit**: `LandedClock` policy table (none → never refreshable via
  age gate, label none; fresh → gate closed; stale-at-`>=` → open);
  `Fetch::wants_refresh` equivalence with the composed form (existing
  0036 tables keep pinning the halves).
- **Probe**: the three migrated headers keep their `*-synced` /
  `*-refresh` selectors and rendering (existing tests unchanged =
  the byte-identical proof); new: `repo-rows-synced` appears once
  rows land and advances with the landed age; absent before.
- **Beat**: gating-test phases unchanged — `wants_refresh` is a
  refactor of the composed call, verified by the same dispatch/no-op
  asserts.

## 6. Implementation plan

1. `freshness.rs`: trait, `Fetch` impl, `LandedClock`,
   `wants_refresh`, `freshness_row`; unit tests.
2. Collapse the five lane gates to `wants_refresh`; migrate the three
   headers to `freshness_row` (probe tests must stay green, N4).
3. The repo-page label (D6) + `repo-rows-synced` probe test.
4. docs/0029 §4 pointer + this doc's §7 Shipped + README row land
   with the implementation PR.

## 7. Open questions (owner)

- **Q1**: D6 placement — the label goes in the module main page's
  header area next to the 0034 stale chip (one freshness cluster per
  page) rather than floating near the rows. Right call?
- **Q2**: N3 — the trait/component stay github-crate-local until a
  second module needs them. Comfortable, or do you want the lift to
  dashboard-core in the same PR since the point is systemic reuse?
