# 0035 — List-surface auto-refresh (repo, PR, issue, notification rows) (#224)

> **Amended by docs/0052** (no-autorefresh): the list heartbeat is
> deleted — list surfaces refresh on **view-shown**, **app refocus**
> (15 s floor), or **manual refresh** only; the idle cadence lanes are
> gone. The docs/0031 dedup and docs/0036 backoff gates are unchanged.

| | |
|---|---|
| Issue | #224 (deferred from docs/0032 §2 non-goals) |
| Status | design |
| Depends on | 0031/0032 (merged: heartbeat host, per-key in-flight dedup), 0034 (merged: in-session freshness precedent) |

## 1. Context

docs/0032 gave the detail pages a 60-second heartbeat plus focus-triggered
refresh, and deliberately skipped list surfaces: "list surfaces ride the
existing snapshot tick loop … revisit if the snapshot cadence proves too
coarse for list views" (docs/0032 §2). This doc is that revisit.

### What each list surface actually rides today (verified in-tree 2026-09-17)

| Surface | Data source | Refresh story today |
|---|---|---|
| Notification rows (global page) | notifications lane | **60 s** — every tick (service.rs `plan_tick`, docs/0029 §2); plus the mark-settled refetch |
| Repo rows (home lists, org-filtered) | snapshot lane | every **5th tick ≈ 5 min** + on connect |
| PR / issue rows in a repo page | same snapshot lane (per-repo `prs`/`issues` summaries are baked into the snapshot query) | edge-fresh on open (`repo_open_specs`, service.rs:1519) then **≈ 5 min** via the global snapshot |
| Notification rows in a repo page (repo slice GroupBox) | `repo_open_specs`' repo-scoped notifications + the global lane | edge-fresh on open, then **60 s** via the global lane landing |

So the notification rows are already fine. The gap is the repo page:
while the user stares at a repo's PR/issue lists, a row that changes on
GitHub (new PR, PR merged, issue closed) appears **up to five minutes
late**, even though the page is the thing they are looking at. The home
repo rows share the same ceiling but the user is usually *choosing* a
repo there, not monitoring one.

The detail-page refresh design (0032) already answered the general
question — *refresh what the user is looking at, on the beat, gated by
age* — this doc extends that policy to the one surface it skipped.

## 2. Goals and non-goals

**Goals**

- G1: The **open repo page's rows** (PR/issue lists and the repo-slice
  notification GroupBox) refresh on the existing heartbeat, with the
  same focus-triggered tightening the detail pages got.
- G2: Honest per-repo freshness: whichever lane landed a repo's rows
  last (repo-scoped snapshot, global snapshot) counts as "fresh", so
  the heartbeat never re-fetches a repo the global tick just refreshed.
- G3: No new timers, no new fetching machinery — the 0032 heartbeat
  host, `repo_open_specs`, the 0031 in-flight dedup and the existing
  run_specs path carry the whole feature.

**Non-goals**

- N1: **Home repo rows keep the snapshot cadence.** The global snapshot
  is the heaviest query in the module (viewer + orgs + every repo with
  PR/issue data); tightening it to 60 s for browsing rows multiplies
  its cost ~5× for the least-monitored surface. If a specific home
  view needs faster counts, that is a follow-up with its own argument.
- N2: **No row-level staleness labels.** #192's indicator design
  deliberately scoped "synced" labels to the three detail pages; this
  issue is about refresh, not labels. Row labels, if ever wanted, are a
  separate ask.
- N3: **No new threshold constants.** The heartbeat's existing
  `AUTO_REFRESH_INTERVAL` (60 s) / `FOCUS_MIN_AGE` (15 s) pair is the
  policy; lists don't get their own dial (same reasoning as docs/0034
  D6).

## 3. Current mechanics (verified in-tree 2026-09-17)

- **Heartbeat host** (page.rs:229 `refresh_open_page`): gated on the
  nav being on the github module, then mutual-exclusion over the detail
  slots (`self.pr` / `self.issue` / `self.notification`, docs/0020 D1).
  When **no** slot is open the module main page is showing — currently
  the heartbeat does nothing there, even when a repo is selected.
- **Repo open** (page.rs:478): `repo_open_specs(full)` — the repo-scoped
  snapshot (`Kind::RepoSnapshot`, ident = full name) plus the repo-scoped
  notifications slice — dispatched through `run_specs` →
  `run_read_deduped` (0031 dedup on the ident key).
- **Landing**: `apply`'s `RepoSnapshot` arm (service.rs:1760) merges the
  payload into `self.repos` (the entry the repo page renders);
  `apply`'s `Snapshot` arm merges the global snapshot's repos the same
  way. Two writers, one entry — whichever landed last wins, and both
  are legitimate freshness sources for the rows.
- **Freshness substrate**: none per-repo. The 0034 `snapshot_age` is
  disk-cold-start only and is cleared on the first landed apply; it
  says nothing about per-repo in-session freshness.

## 4. Design decisions

**D1 — Per-repo landed instants on the state.** `GithubState` grows

```rust
/// Rows landed at, per repo full name (docs/0035): both lanes that
/// write a repo's rows bump its entry — the repo-scoped snapshot
/// (its ident) and the global snapshot (every repo it carries).
pub(crate) rows_landed: BTreeMap<String, Instant>,
```

- `apply` `RepoSnapshot` arm: `rows_landed.insert(ident, now)`.
- `apply` `Snapshot` arm: `rows_landed.insert(full, now)` for every
  repo the snapshot carries (G2 — the global tick counts as fresh for
  all of them).
- Not persisted, not seeded from disk: it is in-session freshness
  only. A missing entry means "never landed this session" — always
  refreshable, which matches the existing eager-on-open behavior.

**D2 — The heartbeat's else-branch refreshes the selected repo.**
*(Amended by docs/0077 D3/D4: the beat refreshes only the **active
tab's** lane via `repo_lane_refresh_specs` — lane-keyed clocks, per-repo
backoff — no longer the whole pair.)*
`refresh_open_page` gains the missing arm, *after* the detail slots
(preserving docs/0020 D1 precedence — a detail page always wins):

```rust
} else if let Some(full) = self.state.read(cx).selected.clone() {
    // module main page with a repo selected: refresh its rows when
    // stale. The beat is windowless, so dispatch hops through the
    // stored `window: AnyWindowHandle` (page.rs:78), the same hop the
    // connect flow uses.
    if let Some(specs) = self.state.update(cx, |s, _| {
        s.repo_rows_refresh_specs(&full, min_age)
    }) {
        let window = self.window;
        cx.spawn(async move |_this, cx| {
            let _ = window.update(cx, |this, window, cx| {
                this.run_specs(specs, window, cx);
            });
        })
        .detach();
    }
}
```

`repo_rows_refresh_specs(full, min_age) -> Option<Vec<FetchSpec>>` on
the state (service.rs, beside `repo_open_specs`):

- returns `None` when `rows_landed[full]` is younger than `min_age`
  (the same `needs_refresh` boundary semantics as docs/0032: **stale
  at `>=`**, background beats can't race a landing);
- returns the full `repo_open_specs` pair otherwise — rows *and* the
  repo-slice notifications, so the whole page freshens in one
  dispatch, exactly like opening it.

The beat passes `AUTO_REFRESH_INTERVAL` (60 s), window activation
passes `FOCUS_MIN_AGE` (15 s) — identical to the detail pages, so the
repo page effectively re-freshens once per minute while open.

**D3 — Dedup and races are already solved.** `repo_open_specs` is
ident-keyed and dispatched through `run_read_deduped`, so a heartbeat
beat colliding with a manual re-open or a just-fired open dispatch is
skipped, never queued (docs/0031 D1/D2). No new guard work.

**D4 — No UI changes.** No chip, no label, no spinner: the rows just
stay current. (docs/0034 owns cold-start marking; #192 owns labels.
If the owner wants a "rows synced Xm ago" line on the repo page, it is
a small follow-up on top of `rows_landed`.)

**D5 — Notifications page and home lists: untouched.** Recorded
respectively as "already covered by the 60 s lane" and "keeps the
snapshot cadence" (N1) — so the issue's full title is answered: two
surfaces by this design, two by explicit no-change with reasons.

## 5. Testing

- **service**: `rows_landed` bumps — a `RepoSnapshot` apply stamps its
  ident; a `Snapshot` apply stamps every repo it carries (both lanes,
  G2). `repo_rows_refresh_specs`: `None` while fresh (just-landed),
  `Some` past `min_age`, `Some` for a never-landed repo, and the pair
  shape equals `repo_open_specs`.
- **page**: extend `refresh_open_page_gates_on_nav_and_slots` — with a
  repo selected and no detail slot open, a stale repo dispatches the
  pair through the MockHttp; a fresh repo doesn't; with a PR page open
  the repo arm does not run (slot precedence).
- **policy table**: fold the boundary case into the existing
  `needs_refresh_policy_table`-style test (stale-at-`>=`).

## 6. Implementation plan

1. service.rs: `rows_landed` field + bumps in the two `apply` arms +
   `repo_rows_refresh_specs` (+ policy test).
2. page.rs: the else-branch in `refresh_open_page` (+ gating-test
   extension).
3. docs/0035 §8 Shipped + docs/README row (if not already present from
   this PR) land with the implementation PR.

## 7. Open questions (owner)

- Q1: D2's cadence means ~2 requests/min (repo snapshot + repo
  notifications) while a repo page sits open. Acceptable? (The detail
  pages already cost up to 3 lanes/min under the same policy.)
- Q2: N1 — are you comfortable leaving home repo rows on the 5-minute
  snapshot cadence, or should the open module root (no repo selected)
  tighten the *snapshot* lane too? The doc argues no (cost), but it is
  your call.

## 8. Shipped (#235)

D1–D5 as designed; Q1/Q2 accepted with the design (2026-09-17). The
implementation landed exactly the designed surface:

- `rows_landed: BTreeMap<String, Instant>` on the state; the
  `RepoSnapshot` apply stamps its ident, the `Snapshot` apply stamps
  every repo its page carries (G2: a just-ticked repo is never
  re-fetched by the beat).
- `repo_rows_refresh_specs(full, min_age)` beside `repo_open_specs`:
  `None` while younger than `min_age`, the full open pair otherwise —
  stale-at-`>=`, matching docs/0032's boundary.
- `refresh_open_page`'s else-branch dispatches through the stored
  window handle (the connect-flow hop — the beat itself is windowless).

One test-side nuance worth recording: `Duration::ZERO` can never count
as fresh under the stale-at-`>=` boundary (elapsed < 0 is impossible),
so the gating test's "landed rows → beat is a no-op" phase pins with a
real min_age (60 s) — test-only, not a design change.
