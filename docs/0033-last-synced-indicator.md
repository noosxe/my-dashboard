# 0033 · Last-synced indicator on detail pages

- **Status:** shipped with the implementation PR (closes #192)
- **Builds on:** docs/0031 (`Fetch` record — `fetched_at`/`age()`), docs/0032 (auto-refresh triggers)
- **Related:** #192 (this), #193 (staleness marking / cold-start), #224 (list surfaces), docs/0029 §4, docs/0013 (status bar)

## 1. Problem

The UI renders a PR, an issue or a notification identically whether the record landed
five seconds or five hours ago. With auto-refresh (docs/0032) the data converges on its
own, but the user still cannot tell *when* the page last confirmed anything — the
confidence problem docs/0029 §4 called out.

The substrate already tracks the answer: every record stamps `fetched_at` on apply
(docs/0031 D4). What is missing is a reader in the UI.

## 2. Decisions

**D1 — Scope: the three detail pages only.** PR, issue and notification pages get the
indicator, placed in each page's header action row next to the refresh button — the
affordance the label explains. Two deliberate exclusions:

- *List surfaces* (repo rows, PR/issue/notification list rows): list rows already show
  GitHub-side `updated_at`; "when did *we* fetch the list" belongs with list-surface
  auto-refresh (#224), which is where a list-level indicator belongs too.
- *Global status bar* (docs/0013): freshness is per-lane now (docs/0031), so one global
  "last synced" would have to pick a lane arbitrarily or aggregate — a bigger design
  than #192 asks for. Deferred until something concrete needs it.

**D2 — Source of truth: the record's `fetched_at` (apply-time).** The label reads
`Fetch::age()` on the page's primary record: `detail` on the PR and issue pages,
`thread` on the notification page. The PR page shows the *detail* lane only — the page's
identity payload; files/threads have their own lanes and their own refresh cadence, and
three labels would be noise. No `age` → no label: an `Idle` record (surface never
fetched) and a `Failed` record that never landed both render nothing — an honest
"nothing confirmed yet" beats a fabricated timestamp. `fetched_at` is apply-time and
session-local (docs/0031 D4); on cold start the page fetches on open, so a label is
always the current session's.

**D3 — Format: bucketed relative, muted.** `synced just now` · `synced Nm ago` ·
`synced Nh ago` · `synced Nd ago` — the same buckets as the existing `relative_age`
("2h"/"3d") but with the "synced" prefix because the timestamp's subject is *our fetch*,
not GitHub's data. Rendered in `muted_foreground` beside the refresh button. Passive
text: no interaction, so no `id`/role (docs/0010 D10) — only a `debug_selector`
(`pr-synced` / `issue-synced` / `notification-synced`) so probes and the bridge can see
it.

**D4 — No new timers: the beat is the clock.** A ticking label needs re-renders; the
auto-refresh loop already wakes every 60s while the page is open and nav-active, and
every trigger funnels through the pages' `auto_refresh(min_age)`. That method therefore
ends with an unconditional `cx.notify()` — even when the policy skipped every lane — so
the label re-renders at most ~60s stale while the page is on screen. The in-app
timestamps never pretend to be clocks; a fresh `land()` also notifies through its apply
path, so user-visible refreshes update the label immediately.

**D5 — Warning badges and cold-start staleness are #193, not this.** The issue's
"optionally warn when older than a threshold" is deliberately out: threshold marking is
exactly #193's design space (which will also want it on cold-start disk-cache data).
This PR ships the honest reading, the marking lands later.

## 3. Test plan

- **Helper table** (`synced_label`): `None` → hidden; <60s → "just now"; minutes/hours/
  days buckets.
- **Probe wiring, pr_page**: after a scripted-OK detail lands, `pr-synced` exists; after
  the record is driven to `Failed` with no prior landing, it does not. (Bucket
  arithmetic is covered by the table; probes pin selector wiring and the hide rule.)
- **Notification wiring**: `notification-synced` appears once the thread record lands
  (the record-swap surface — proves `Fetch<api::Thread>` + label compose).
- **Existing 0032 tests** prove the tick addition does not disturb dispatch policy
  (stale dispatches, fresh skips, failed retries — all asserted over the verb mocks).

## 4. Shipped (this PR)

As designed, D1–D5. Shipped surface:

- `synced_label(Option<Duration>)` in page.rs — the bucketed formatter beside
  `relative_age`; table-tested (`age_buckets`, service.rs tests).
- Header action rows: `pr-synced` (detail lane), `issue-synced` (detail lane),
  `notification-synced` (thread lane) — muted `text_xs`, `debug_selector` only.
- The D4 tick: `auto_refresh` on all three pages ends with an unconditional
  `cx.notify()`, so a policy-skipped beat still advances the label while the
  page is open and nav-active.
- #192's `#[allow(dead_code)]` on `Fetch::fetched_at`/`age` dropped — the
  consumer API now has its reader (docs/0031 D4 closes out for this pair;
  `data_mut`'s allow stays for #193).

Probes: `synced_indicator_follows_the_record` (pr_page — absent on a
never-landed record, present once Ready) and presence/absence asserts folded
into the notification GET-lands / GET-fails tests. The 0032 dispatch tests
double as the tick's regression net (the added notify must not disturb the
policy).
