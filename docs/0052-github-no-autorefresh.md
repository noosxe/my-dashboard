# 0052 — GitHub: no autorefresh; fetch on show, refocus, and manual

- **Date:** 2026-09-21
- **Depends on:** 0031 (fetch record/dedup), 0032 (detail auto-refresh —
  amended), 0035 (list-surface refresh — amended), 0036 (backoff —
  unchanged), 0037 (freshness core — unchanged), 0038 (engaged tier —
  superseded), 0051 (view-driven fetches)
- **Closes the design need of:** #326 (narrowed by the owner to: no
  autorefresh anywhere; introduce fetch on app refocus)

## 1. Context & problem

#325 (docs/0051) made every fetch view-driven and deleted the global
snapshot's 5-minute cycle — but deliberately left the pre-existing
refresh heartbeats in place (docs/0051 D7, verified live post-merge:
an open module page kept refetching its home lane on the engaged
cadence). The owner's direction for #326:

1. **No autorefresh anywhere.** Nothing may fetch on a timer.
2. **Fetch on app refocus.** Regaining window focus is the
   replacement trigger for keeping visible data fresh.

Per-query cost receipts (the other half of #326's original text) are
already recorded in docs/0051 §1 — measured live through
`rateLimit { cost }` — and are not repeated here.

## 2. What is timer-driven today (inventory, receipts)

| # | Path | Code | Fetches on the timer |
|---|---|---|---|
| A | Service tick, 10 s | `service.rs` `TICK` loop → `plan_tick` | **notifications lane** (REST, age-gated `ENGAGED_LIST_MIN_AGE` 10 s / `IDLE_MIN_AGE` 60 s by `focus_engaged`); usage probe (REST `GET /rate_limit`, free, 30 s cooldown); bootstrap arm (only on need) |
| B | Page heartbeat, 10 s | `page.rs` `HEARTBEAT_TICK` loop → `refresh_open_page(engaged, None)` | open PR sub-page lanes (detail+threads GQL 30 s engaged / files REST 10 s), issue detail (GQL), notification thread (REST), repo rows (60 s), module home lane (60 s) |
| C | Activation observer | `page.rs` `observe_window_activation` | **already fetches on focus** — `refresh_open_page(active, Some(FOCUS_MIN_AGE = 15 s))`, module surfaces only; the card's observer (`card.rs`) only maintains `focus_engaged` |

Not timer-driven and out of scope: manual refresh (the freshness
cluster's refresh control, `freshness_row`), view-open/nav-edge
triggers (docs/0051 D4), backoff retries (docs/0036 — error recovery,
gates every trigger), drafts autosave (local disk), the check-run log
pane (fetches on expand, docs/0039 D4).

## 3. Decisions

### D1 — The rule: no timer-driven content fetches

No lane may produce a network request because a timer elapsed. The
service tick survives only for **non-content duties**: clearing an
expired rate-limit pause, proactive token-refresh arming (docs/0028
§3.4 — session mechanics, no content), the usage probe, and the
bootstrap arm (which fires exactly once per connect). The page
heartbeat loop is deleted outright; nothing replaces its wake.

### D2 — The engaged tier is deleted

Docs/0038's cadence machinery goes with the heartbeat:
`auto_refresh(engaged, …)` on the three sub-pages, `lane_min_age`,
`LaneClass`, `ENGAGED_REST_MIN_AGE`, `ENGAGED_GQL_MIN_AGE`,
`ENGAGED_LIST_MIN_AGE`, `IDLE_MIN_AGE`, the `focus_engaged` state
flag (its last reader is the deleted cadence arm), and the
`detail_open` helper. Engagement as a *concept* ends: a surface
refreshes because it was shown, refocused, or the user asked —
never because it stayed open.

### D3 — Sub-pages keep one refresh entry point

`auto_refresh` simplifies to `refresh_if_stale(min_age)` — the same
`wants_refresh` + backoff composition (docs/0037 D4), one floor for
every lane, called with `FOCUS_MIN_AGE` by the refocus trigger. The
REST/GQL lane-class distinction disappears (it only priced the
cadences).

### D4 — Fetch on app refocus (the replacement trigger)

Window activation keeps its existing plumbing (`observe_window_activation`
in the page, extended to the card) and becomes a first-class trigger
for **every visible surface**, always through the same gates (age
floor, backoff, rate-limit pause, docs/0031 dedup):

| Visible surface | On activation |
|---|---|
| App home page (the card) | card totals + notifications, floor 15 s |
| Module home / repo list | home lane or repo rows, floor 15 s |
| Open detail page (PR/issue/notification) | its lanes, floor 15 s |

The 15 s `FOCUS_MIN_AGE` floor is kept from docs/0032 D1 (unchanged):
rapid alt-tabbing must not become a cadence. The page's observer
already implements the module half — the card's observer stops being
flag-only and fires its lanes; both observers may fire the shared
notifications lane, and the ident-keyed dedup absorbs the collision.

### D5 — Notifications: from cadence to triggers

With the tick arm gone (D1) the notifications lane fires on: session
bootstrap (existing), module view-shown (added — fires alongside the
home lane's nav-edge trigger), card view-shown (added, age-gated),
app refocus (D4), manual refresh. REST bucket; one page per fire.

### D6 — The age-label clock survives as a render-only wake

The heartbeat's `cx.notify()` also advanced the relative ages
("synced 2 m ago", #192 / docs/0033 D4). A 30 s **render-only**
timer on the page and the card re-renders — zero specs, zero
network. This is not autorefresh (D1 forbids timer-driven *fetches*,
not repaints).

### D7 — Unchanged on purpose

Backoff (0036), the rate-limit pause, dedup (0031), staleness
display (0034/0037 — the stale chip now simply never heals without a
trigger, which is honest), manual refresh, the usage probe (free
REST reconcile, keeps the chip truthful after pauses — flagging it
as the one deliberate exception to "nothing on a timer"; if unwanted
it is a one-arm deletion later).

## 4. State & plumbing changes

- `service.rs`: delete the notifications cadence arm + list-age
  constants from `plan_tick`; the tick loop, probe, bootstrap arm,
  token arming, and pause clearing stay.
- `page.rs`: delete the heartbeat loop, `detail_open`, engaged
  wiring; the activation observer calls the simplified
  `refresh_open_page(Some(FOCUS_MIN_AGE))`; add the notifications
  spec to the module-open nav-edge batch; add the render-only wake.
- `card.rs`: the activation observer fires card totals +
  notifications (floor 15 s) in addition to the nav-edge triggers;
  add the render-only wake.
- Sub-pages: `auto_refresh` → `refresh_if_stale(min_age)` (same
  gates, no engagement).
- `focus_engaged` deleted from state.

## 5. Testing

- Rewrite the heartbeat/cadence tests (`auto_refresh_policy_over_real_dispatches`,
  notification-page dispatch/backoff tests) against
  `refresh_if_stale` and the activation trigger.
- New: card activation fires totals + notifications;
  module-open fires notifications; idle tick produces no content
  specs (probe/bootstrap only); render-only wake dispatches nothing.
- Backoff, pause, and dedup tests unchanged (their gates survive).

## 6. Acceptance

- Bridge, idle app on the home page 10 min: **zero** content
  fetches (GraphQL lanes silent; REST notifications silent; the free
  probe may fire).
- Idle app with the module open: same silence (the post-#325
  heartbeat refetches are gone).
- Alt-tab away and back: exactly the visible surfaces' lanes fire
  (once — dedup absorbs collisions), respecting the 15 s floor.
- Relative ages keep advancing (render clock).
- Gates: fmt, clippy zero, workspace tests.

## 7. Non-goals

- Per-query cost trims (field/`first:` cap reductions) — receipts
  live in docs/0051 §1; trimming is a follow-up if wanted.
- Refresh-on-focus outside the GitHub module (no other module
  exists; the app-level pattern is exactly this doc's D4).
- Multi-window focus semantics beyond the existing observer.

## 8. Docs amendments (ride with the implementation PR)

- **0032**: heartbeat deleted; the focus trigger (D1/D3) survives as
  the only automatic detail refresh.
- **0035**: list-surface heartbeat deleted; show/refocus/manual only.
- **0038**: superseded — the engaged tier, cadence tables, and the
  notifications cadence arm are gone (D2/D5); a supersession note
  replaces the body's operative sections.
- **0033** D4: the age clock is the render-only wake (D6).
- README row for this doc.
