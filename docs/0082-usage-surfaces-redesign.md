# 0082 — Usage surfaces redesign: two audiences, intent first

- **Issue:** implementation #695–#700 (§11). Review umbrella: #439, whose "visual layer only" rule the
  owner lifted on 2026-11-04 for meaningful improvements.
- **Amends:** docs/0054 §2, D2, D3, D4, D5, D6, D9, D11, D13, D15 and
  D17; docs/0050 D4 (the pause becomes per budget), D5 (the chip shows
  on every page) and its non-goals; docs/0053 §1 (the goal
  questions get metrics); docs/0057 D8 (read reach); docs/0068 D6
  (status only).
- **Keeps:** docs/0050 D1–D4 (verbatim server numbers, buckets, the
  probe, exhaustion); docs/0053 D1–D6 (the record, origins and the
  sink); docs/0054 D10 (the frozen `as_of`) and D13 (one calculator);
  docs/0057 (storage, 90-day retention); docs/0063 (app database).
- **Interacts with:** docs/0049 (content a11y), docs/0052 D7 (the usage
  probe), docs/0058 and 0059 (toasts and the hub), docs/0064 D9 (the
  `local` transport), docs/0065 (UI quality gate), docs/0071 (container
  receipts), docs/0074 and 0078 (bundled theme presets), docs/0080
  G3/D7 (poll receipts), docs/0081 §12.1 (GitHub fixtures).

> **Decision status.** The owner reviewed the mockups on 2026-11-05
> (§2.1) and settled the proposals they covered; those decisions are
> no longer marked. A decision still marked **[P]** was not covered by
> the mockup review and stays a proposal: it states its trade-off in a
> line or two, for correction in review. Unmarked decisions are owner
> decisions (§2, §2.1) or plain consequences of them.

## 1 Summary

The usage surfaces are the footer chip and the Usage page with its
Overview, Quota usage and Request latency tabs. They were built round
by round (docs/0054 §11) and work as specified, but they read badly and
miss part of what the docs set out to answer.

This doc restarts from **intent**. It distils the questions docs
0050/0053/0054/0057/0068/0080 asked these surfaces to answer (§3), then
gives them a structure built around **two audiences** (§4, owner
decision):

- **Budget** is the default view, for the owner as a *user*: how much
  GitHub API budget is left, what spends it, how spending looks across
  past windows, and which spend could be avoided.
- **Requests** is for the owner as the dashboard's *developer*: request
  forensics, meaning latency, errors, contention and a full request log.

Beyond the split, this doc:
- never sums the two budgets;
- replaces the runway projection with an in-window exhaustion estimate;
- draws time on a real time axis;
- reaches the full 90-day retention;
- drops the donut;
- folds the keyboard and a11y fixes into the same work.

The order is fixed by the owner (§2):
1. **This doc.**
2. **Mockups**, rendered as static HTML at the owner's request (§10).
3. **Owner approval.**
4. **Implementation.**

## 2 Owner decisions (2026-11-04)

1. **Two audiences, on purpose.** Some usage surfaces serve the owner
   as a user (budget health) and some serve the owner as the
   dashboard's developer (request forensics). Both stay, and the split
   is explicit in the docs (D1).
2. **#439's "visual layer only" rule is lifted** for meaningful
   improvements. Amending docs/0054 and its relatives is allowed.
3. **Process:** docs → mockups → owner approval → implementation.
4. **Open questions are settled in this doc** as proposed decisions
   ([P]) with trade-offs, for correction in review.
5. **The fixture prerequisites are built separately**, in parallel:
   the seeded ledger (#536 mechanism) and rate-limit data in the GitHub
   fixtures. They are prerequisites for §10, not part of this doc.

### 2.1 Mockup review (2026-11-05)

The owner reviewed the HTML mockups (§10) and the 14 questions they
raised (Q1–Q14; not §3's intent questions Q-a to Q-f). **Owner
decisions:**

| # | Decision | Where |
|---|---|---|
| Q1 | State colour goes on the icon, the bar and chart marks; state words and numbers stay in `foreground` | D5, D13 |
| Q2 | The budget cards stay side by side down to about 640 px of content | D4, D20 |
| Q3 | History auto-scales to the data, with an off-scale limit marker when the limit is above the data range | D8 |
| Q4 | The pace is the drop in `limit − remaining_after` across the current window, not only this app's recorded cost | D6 |
| Q6 | The default budget selection is the budget with the smaller remaining fraction | D7 |
| Q8 | Diagnostics sit above the request log; the log comes last and fills the viewport | D10, D20 |
| Q9 | The chip shows on every page | D13; amends docs/0050 D5 |
| Q10 | Exhaustion is per budget: one `rate_limited_until` per bucket | D5, D13; amends docs/0050 D4 |

**Adopted per mockup recommendation** (the owner accepted the
mockups' recommendations wholesale):

| # | Decision | Where |
|---|---|---|
| Q5 | `Current window` follows the selected budget's window; the cards always show both budgets | D8, D11 |
| Q7 | The `cost` measure is offered only when a single quota transport (REST or GraphQL) is selected | D10 |
| Q11 | Cost tables title the cost column `Cost`, so REST's unit doesn't collide with the request count | D7, D21 |
| Q12 | The avoidable list is accepted as mostly a REST and long-range tool | D9 |
| Q13 | The view switch leads the header on the left; range presets and refresh sit on the right | D3 |
| Q14 | Step lines draw a `chart_1` area fill beneath them | D10a, D14 |

The mockups' own deviations from the first draft of this doc are
folded into the decisions below and adopted the same way.

## 3 Intent: what the docs asked these surfaces to answer

Taken from the design record, de-duplicated:

| # | Question | Source |
|---|---|---|
| Q-a | What is left right now, per budget, verbatim from GitHub? | 0050 §1, D5; 0053 §1 ("what is left") |
| Q-b | What do we spend the quota on: which pages, ops and triggers? | 0053 §1; 0054 §1, §4 |
| Q-c | How much is spent per quota cycle, over time? | 0053 §1, D1/D3 (`cycle_reset_epoch` exists "for per-cycle GROUP BY"); 0057 §1 ("90 days of quota-cycle context") |
| Q-d | Which requests are rarely worth their cost? | 0053 §1 (no metric was ever defined) |
| Q-e | How slow are requests, where, and why? | 0054 §6 ("the developer's and the agent's tab") |
| Q-f | Is specific traffic accounted for (polls, the probe, agents)? | 0080 G3/D7; 0052 D7; 0068 §2 goal 2, D6 |

Constraints carried forward unchanged:
- never invent a number: unknown costs are counted, never priced
  (0050 D1, 0053 D1, 0054 D5);
- be honest about the retained window (0054 §1, D2);
- nothing on the page is live (0054 D10);
- one pure calculator feeds every renderer (0054 D13);
- module-agnostic records (0053 D6).

### 3.1 How the shipped pages fall short of that intent

The containerized captures over the GitHub fixtures and a synthetic
seeded ledger (2026-11-04, docs/0071) found the following:

- **Q-a has no large-format home.** The chip is small muted text that
  appears only on GitHub pages, and warns only at exhaustion (by
  design: 0050 listed "budget warnings" as a non-goal). The page shows
  no reset time anywhere.
- **The runway projection** (0054 D11), remaining ÷ (window spend ÷
  window hours), is wrong for budgets that refill every hour (0050 §2).
  It reads hundreds of hours and changes with the selected range.
- **The Quota tab adds REST requests and GraphQL points** into one Σ
  and one share column whenever the transport facet is All (0054 D5,
  the default). 0054 D11 says they are never summed.
- **Q-c and Q-d have no view.** 0054 D2 permanently rejects a cycle
  selector, and nothing else shows per-cycle history. "Rarely worth
  their cost" never got a metric. The closest thing, the 304-vs-200
  table, sits at the bottom of the latency tab.
- **The page reaches 1 month** (0054 D2 presets, D13 read bound), but
  0057 retains 90 days.
- **Time is not drawn as time.** x scales are ordinal, labels carry
  seconds and no date, and 1w/1m charts can't be read.
- **Every cost breakdown is shown three times**: bar chart, donut and
  table. The donut labels collide.
- **Chart colours collapse to one hue** in the bundled presets
  (docs/0078 ships them verbatim).
- **Keyboard and AT access:**
  - the segmented controls and the tab strip are not Tab stops;
  - the Tab walk on the Quota tab never closes its cycle;
  - the ranking table's cells expose no text, because
    `TableDelegate::cell_label` exists at the pinned gpui-component
    and is not implemented (#351);
  - Select options are unlabelled list items.
- **The latency tab is about six screens long.** It grew additively
  over three rounds (0054 §6), with no hierarchy pass.

## 4 Audiences and page structure

### D1 — Two views on one page, one per audience

`Page::Usage` stays shell-hosted (0054 D1: sidebar entry, breadcrumb,
cross-module). Its body is one of two **views**, picked by a
segmented control at the head of the page:

| View | Audience | Answers | Default |
|---|---|---|---|
| **Budget** | the owner as a user | Q-a, Q-b, Q-c, Q-d | yes |
| **Requests** | the owner as the dashboard's developer | Q-e, Q-f | no |

- The view choice is component-local, like 0054 D3's tab state. It
  keeps the frozen `as_of` and the range.
- The chip (D13) belongs to the Budget audience and navigates to the
  Budget view.
- The Overview, Quota usage and Request latency tabs are retired.
  Their content is redistributed (§9).
- **[P] The view choice is not persisted across restarts.** The page
  always opens on Budget. Trade-off: one extra click for developer
  sessions, but the user-facing default stays predictable.

### D2 — Budgets are never summed

Every cost figure on every surface belongs to **one** budget:
**REST** (requests, the `core` bucket) or **GraphQL** (points). This
resolves the contradiction between 0054 D5 and D11 in D11's favour.

- Cost views carry a `Budget: REST | GraphQL` selector that replaces
  the transport facet for cost (0054 D4).
- `auth` and `local` records have no budget. They appear only in the
  Requests view (D9, D16).
- Trade-off: no single "total spend" number exists. Comparing the two
  budgets is side by side, never pooled. That is correct, because the
  budgets are independent (0050 §2).

### D3 — The shared page header

Fixed above the scrolling view body (0054 D17 stays):
- the view switch, leading the row on the left (Q13): it changes the
  whole page, so it reads first;
- the **range presets** (D11) and the refresh button (0054 D10,
  unchanged), right-aligned on the same row;
- the **honesty line**, which carries dates (D10):
  `1,087 requests · as of Nov 4, 19:21 · history from Oct 25, 19:22`.
  Under `Current window` it also names the window, which differs per
  budget: `45 requests · GraphQL window 14:41–15:41 · as of …`.

Facets that belong to one view live in that view's query bar, not in
the header. At 1024×700 the header row fits on one line (mockups,
§10).

## 5 Budget view (the user audience)

The bands below run top to bottom in one scroll. Every band derives
from the frozen window (0054 D10) through calculator procedures (D15).

### D4 — Band 1: twin budget cards (Q-a)

Two cards with identical anatomy, side by side (stacked only below
the D20 breakpoint, about 640 px of content; Q2), one per budget:

```
REST requests                       GraphQL points           ⚠ Low
3,712 left of 5,000                 1,140 left of 5,000
▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓░░░░░  74%           ▓▓▓▓░░░░░░░░░░░░░░░░  22%
Resets 14:00 (in 13 min)            Resets 13:52 (in 5 min)
On track                            ≈ runs out ~13:50 at this pace
as of 13:47 · snapshot 2 min old    as of 13:47 · snapshot 1 min old
· 153 requests recorded in 24h      · 10,625 points recorded in 24h
```

- **The state badge** (Low, Critical, Exhausted) sits at the right of
  the title: an icon in the D5 token plus the word in `foreground`.
  Normal shows no badge.
- **The meta line** ends with the range's recorded spend for that
  budget ("10,625 points recorded in the last 24h"); this is where the
  retired Overview "points spent" card lands (§9).

- **Numbers** come from the latest `remaining_after` / `limit_after` /
  `cycle_reset_epoch` with `ts < as_of` for that budget. They are
  verbatim (0050 D1). With no snapshot in the window, the card reads
  "No response from GitHub in this range yet". It never shows 0.
- **The bar** is a `Progress`, coloured by the D5 threshold ladder.
- **The reset time** is shown as clock time plus a countdown taken
  from `as_of`. It is the fact the page used to lack.
- **The projection** follows D6 and is marked `≈`. Below D6's guard
  the line reads "Not enough responses in this window to project", so
  the card keeps its shape. An **exhausted** budget replaces the line
  with "GitHub paused GraphQL requests; they resume at the reset" and
  shows an empty bar.
- The snapshot age reads "snapshot under a minute old" below one
  minute, never "0 min old".
- The cards don't move when GitHub traffic lands. The chip does (D12).

### D5 — The threshold ladder

One ladder, shared by the cards, the chip and history columns:

| State | Rule | Token |
|---|---|---|
| Normal | remaining > 25% | `muted_foreground` (chip) / `primary` (bar) |
| Low | remaining ≤ 25% | `warning` |
| Critical | remaining ≤ 10% | `danger` |
| Exhausted | that budget's `rate_limited_until` in the future (0050 D4, per budget since Q10) | `danger` plus the "resumes" copy |

- **The token colours marks, never text** (Q1): the state icon, the
  `Progress` fill, history columns and chart marks. State words and
  numbers stay in `foreground`. The kit tokens can't carry text: in
  Default Light `warning` (yellow-500) is about 2:1 on white and
  `danger` (red-500) about 3.8:1, and the Default Dark
  `warning_foreground` on `warning` pair fails too, so coloured text
  would fail the docs/0065 D4 contrast gate.
- Each state also carries an icon and a word ("Low", "Critical",
  "Exhausted"), so colour is never the only signal (0065 rubric #17).
- **Exhaustion is per budget** (Q10): an exhausted GraphQL budget
  pauses GraphQL requests only; REST keeps fetching and keeps its own
  state.
- **[P] The thresholds are fixed in v1, not user-configurable.**
  Trade-off: a power user can't tune them, but there's no settings
  surface to build or test.
- **[P] Crossing into Critical raises one `Warning` toast per budget
  per window** through the hub (docs/0058, dedupe key
  `usage-<budget>-critical-<reset_epoch>`, source `github`, which the
  docs/0062 per-source mute covers). This lifts 0050's "budget
  warnings" non-goal. Trade-off: one more toast source, but a dry
  budget otherwise stays silent until fetches pause.

### D6 — The in-window projection replaces runway

0054 D11's runway is retired: hourly windows make a cross-window pace
meaningless. Instead, per budget and inside the **current window
only**:

- `spent` = `limit_after − remaining_after` of the latest snapshot
  whose `cycle_reset_epoch` equals the current reset (Q4). This is the
  window's whole drop as GitHub reports it, so it includes other
  clients sharing the token, not only this app's recorded cost.
- `elapsed` = `as_of − (reset − 3600 s)`.
- `pace = spent / elapsed`.
- **Shown only when** at least 5 responses carrying a snapshot exist
  in the window **and** at least 5 minutes have elapsed in it:
  - "≈ runs out ~HH:MM at this pace" when
    `as_of + remaining / pace < reset`;
  - "On track" otherwise.

It is still the page's only projection, and it is always marked `≈`.
The 3600 s window length is GitHub's documented primary window,
re-verified on 2026-11-05 against GitHub's REST and GraphQL rate-limit
pages: both budgets are "per hour", and the reset is UTC epoch seconds.

The drop minus this app's Σ known cost in the same window is the
**external-consumer** spend. D6 doesn't show it; it is the input of
the deferred external-consumer estimate (§11, #705).

Trade-off: early in a window it is noisy, hence the guard; a burst
right after the guard can over-warn. Counting only recorded cost was
the first draft; it would say "On track" next to a Critical badge
whenever another client drains the token. Dropping projection entirely
was the alternative, but "will I run dry before the reset" is the one
forward-looking question users actually have.

### D7 — Band 2: where it went (Q-b)

A query bar plus one chart and one table, both fed by one aggregate.

- **Budget selector:** `Budget: REST | GraphQL` (D2) sits in a
  view-level bar between band 1 and band 2, because bands 2, 3 and 4
  all depend on it. Its default is the budget with the **smaller
  remaining fraction** (Q6), REST on a tie or with no snapshot. The
  cards (band 1) always show both budgets.
- **Band query bar:**
  - `Group by` over Surface, Trigger, Operation, Target or Module;
  - an optional `Then by`.

  0054 D4's two-selector cap stays. Transport leaves the dimension list
  because the budget decides it.
- **Chart:** one horizontal `BarChart` showing the top 8 plus "other",
  with value labels and a value axis. A compound key (`Then by`)
  draws only the top-level groups. Nested members live in the table.
- **Table:** it mirrors the chart. Columns: key · `Cost` · share (with
  an inline share bar) · requests · avg cost · free (304) · errors ·
  unknown cost. The cost column is titled `Cost`, never the unit: for
  REST the unit is "requests", which would collide with the request
  count column (Q11); the band's summary line names the unit. The
  table is sortable (upstream `perform_sort`), sized to its content
  (D20), and each cell exposes its text (D19). Nested rows label their
  own dimension ("operation: notifications") and are all shown.
- **The donut is removed** (amends 0054 D6). It repeated the bar
  chart's data with less precision.
- **[P] Drill-down:** activating a table row offers "Show these
  requests", which switches to the Requests view with that group as a
  filter. Trade-off: a cross-view jump, but the request log lives in
  only one place (D9).

### D8 — Band 3: history across windows (Q-c)

Spend over time, shown **per quota window**, never as a cycle selector
(0054 D2's reason stands: cycles are per budget and independent).

- **Range = Current window:** a step burn-down of `remaining_after`
  across the selected budget's current window (Q5), with the limit
  drawn via `reference_line` (now available at the pin; see 0054
  D15's note), a dashed `as of` marker and the reset marked at the
  right edge. The value axis is the whole budget, 0 to the limit.
- **Range ≤ 7 days:** one column per window of the selected budget.
  Column height = the window's Σ known cost. Each column is coloured
  by the threshold the window reached (D5).
- **Range 30 or 90 days:** one column per day, showing that day's
  **peak** window spend (its closest approach to the limit). The
  tooltip and a11y summary add the day's total.
- **The value axis auto-scales to the data** (Q3). When the data
  reaches about 90% of the limit, the axis shows the whole budget, and
  the limit (`reference_line`) and the **Low and Critical guides** (at
  75% and 90% of the limit spent; for the burn-down, at 25% and 10%
  remaining) are drawn. Otherwise the axis fits the data, draws only
  the guides that fall inside it, and shows an **off-scale limit
  marker** in the right gutter: "↑ limit 5,000 · above scale". REST
  spend is typically 10–20 requests per window, which a 5,000 scale
  draws as nothing.
- The guides give each column's state a position signal as well as a
  colour (0065 rubric #17). This matters in one-hue presets: in
  Catppuccin Mocha a Low column is pale yellow next to white `chart_1`
  columns.

Trade-off: per-day peaks hide the shape inside a day, but 720 hourly
columns can't be read. An auto-scaled axis makes columns from
different ranges or budgets incomparable by height; the summary line
and the off-scale marker keep the absolute scale readable. Per-window
columns aren't stacked by group: D14 keeps categories off colour.

### D9 — Band 4: avoidable spend (Q-d)

This gives 0053's "rarely worth their cost" a metric. For the
selected budget, rank **operations** by avoidable cost:

- **Revalidation gap:** ops that have both 304 and 200 responses in
  the range. Cost paid on 200s by ops that usually come back 304.
- **Timer and refocus spend:** cost from `poll`, `refocus` and `probe`
  triggers, per op. This also covers 0080 G3's "is the poll worth it".
- **Errors:** cost paid on flights that failed (non-2xx/3xx, network
  error).

The categories are **exclusive**, checked in this order: errors, then
revalidation gap, then timer and refocus. One request's cost is never
counted twice.

The band shows a short list (top 5): op, a category tag, a one-line
reason, cost and share of the budget's range spend. "Show these
requests" is the same drill as D7. Its summary line says
"Heuristics, not proof".

Trade-off: these are heuristics, not proof of waste. "Fetched but
never viewed" would be the strongest signal, and it needs visit
records (#334), which are deferred. GraphQL never returns 304, so for
GraphQL the list holds only refocus spend and errors and stays short;
the band is accepted as mostly a REST and long-range tool (Q12).

## 6 Requests view (the developer audience)

### D10 — Structure (Q-e, Q-f)

- **Query bar:**
  - facets: `Transport` (all four, including `auth` and `local`),
    `Module`, `Status class` (2xx, 304, 4xx, 5xx, network);
  - `Group by` and `Then by` (0054 D4's cap);
  - `Measure`: count, cost, p50, p90, max or error rate. Default p90
    (0054 D12 keeps its reasoning). `cost` is offered only when the
    Transport facet is a single quota transport (REST or GraphQL), so
    a trend never pools requests and points (D2, Q7); with any other
    Transport value the option is disabled with a one-line reason.
- **Bands:**
  1. **Glance strip:** one tile per transport (REST, GraphQL, `auth`,
     `local`) with p90, p50 and the request count; a transport with no
     requests in range shows "—". Then a "Slower than 1 s" count (with
     its share) and an error count (4xx, 5xx and network). Six tiles
     in one row from about 900 px of content, three per row below.
  2. **One trend chart** of the selected measure over time. Counts and
     cost are drawn as bars, percentiles as step lines (D10a). Buckets
     are aligned to the clock (whole hours, 6-hour blocks or days).
  3. **Ranking table** mirroring the trend, by group.
  4. **Diagnostics** (a collapsed disclosure): the duration histogram,
     the payload × duration grid, contention, and the 304-vs-200
     comparison (0054 §6, #337/#410), unchanged in content. It sits
     above the log (Q8), so it stays reachable without scrolling
     through the log.
  5. **Request log**, always last: a virtualized, sortable `DataTable`
     of every request in the selection. It fills the scroll viewport
     (Q8): page scroll brings the log to the top of the viewport, then
     the log scrolls its own rows, and nothing sits below it (D20).
     Columns: time · op · target · surface · trigger · status (with a
     plain-language reason, "304 Not modified", "Network error · timed
     out") · cost · duration · queue · size. The cost cell spells its
     unit, because the log mixes transports: "12 points", "1 request",
     "free (304)", "unknown", "no budget" (`auth`, `local`). Times
     carry the date for rows outside the `as_of` day. The default sort
     is newest first, and sorting by duration gives the old "slowest
     requests" view. It replaces the top-20 list (0054 D12.3). It is
     also where 0080's `service-tick × poll` receipts and 0068's agent
     rows are found.

**D10a — Time and axes (both views).**
- Every time chart draws a **proportional time axis**. It needs an
  app-owned chart on the pinned `gpui_base::plot` primitives
  (`ScaleLinear` over epoch ms, `Bar`, `Line`, `PlotAxis`, `Tooltip`;
  the gpui-component story's `stacked_bar_chart.rs` is the worked
  pattern). The stock charts' x scale is ordinal (`ScalePoint`).
- Every chart has value-axis ticks.
- Labels gain the date for ranges over 1 day.
- Natural (spline) curves are not used: they overshoot below zero and
  invent peaks between buckets.
- Step lines draw a `chart_1` area fill beneath them (Q14). In Default
  Light `chart_1` (#93c5fd) is about 1.9:1 on white, too faint as a
  bare line; the area keeps the series legible without a second
  token (D14).
- A single-sample series is a marked point with its value label and a
  drop line (D18).

## 7 Cross-cutting decisions

### D11 — Range presets and reach

- **Presets:** `Current window` (Budget view only), `1h`, `24h`, `7d`,
  `30d`, `90d`. Default: `24h`. This amends 0054 D2. Custom start/end
  stays a follow-up.
- **`Current window` follows the selected budget** (Q5): the REST and
  GraphQL windows reset at different times, so bands 2–4 use the
  window of the budget picked in D7's selector, and the honesty line
  names it (D3). The cards always show both budgets' current windows.
- **The read bound grows to 90 days** to match 0057 D7's retention
  (amends 0054 D13).
- **[P] The read stays on the UI thread** (0057 D8, an owner-accepted
  rule), with a gate: the implementation PR measures a 90-day read over
  the seeded ledger at a heavy-day volume. If it takes more than one
  frame (16 ms), it moves to the background executor with a designed
  loading state (D18), as 0054 D9 first allowed. Trade-off: the UI
  thread stays simple until the measurement says otherwise.

### D12 — Liveness [P]

The page stays frozen (0054 D10): the cards say "as of", and refresh is
the only clock. The **chip** stays the live surface (0050 D5,
unchanged).

Trade-off: the cards can be minutes old while the chip moves, but both
are labelled, and "nothing live on the page" keeps every number on
screen consistent with every other one. A live hero was the
alternative; it would be the page's only live element and would break
the freeze receipt's guarantee.

### D13 — The chip

The chip keeps its verbatim numbers and spoken label (0050 D5). It
changes in these ways:

- **Words, not abbreviations:** `REST 3,712 · GraphQL 4,950`. The
  tooltip gives the limits and resets.
- **The D5 ladder marks each budget independently** (Q1): a budget in
  Low or Critical shows the state icon in the D5 token, then its name,
  number and state word in `foreground` and semibold:
  `REST 3,712 · ⚠ GraphQL 1,140 Low`. Normal budgets stay in
  `muted_foreground` with no icon. The state word is visible, not only
  spoken, so colour is never the only signal (0065 rubric #17); the
  spoken label carries it too.
- **Exhaustion is per budget** (Q10; amends 0050 D4): the paused
  budget reads `GraphQL paused · resumes 15:41` with the pause icon in
  `danger`, while the other budget keeps its live number. The spoken
  label names which budget is paused and when it resumes. This
  replaces the single global `GH rate-limited · resumes 12m` copy.
- **The chip shows on every page** (Q9; amends 0050 D5's "render iff
  `Page::Module("github")`"). Budget health matters wherever the app
  spends it: the home cards and background polls spend it off the
  GitHub pages too.
- **The chip itself becomes the button** that opens the Budget view.
  The separate pie icon is removed. It keeps `.id("github-usage")`,
  its role becomes a button, and its tooltip keeps its content.
- The placeholders `…` / `?` become `REST —` / `GraphQL —`, with a
  tooltip explaining "no response yet".

Trade-off: the chip is wider (about 30 px in normal state, more with a
state word or pause copy). The mockups verified that the status bar's
right region holds every state at 1024 px (§10).

### D14 — Colour never encodes category

The bundled presets (0078) give `chart_1…chart_5` as tints of one hue,
so category colours can't be relied on. Rules:
- at most one data series per chart, in `chart_1`; step lines add a
  `chart_1` area fill beneath them (D10a, Q14);
- categories are encoded by position and label, never by hue (bars
  plus a mirroring table; no donut, no stacked colours);
- the two budgets are distinguished by panel and label;
- state colours come only from the semantic tokens (D5), on marks and
  icons only, and are paired with a position signal where a chart
  shows states (D8's guides).

Trade-off: no multi-series overlays. In exchange, no app-owned palette
bytes (0078) and every preset stays legible. In Catppuccin Mocha
`chart_1` resolves to white (`blue.lighten(0.4)` clamps), which the
mockups checked.

### D15 — One calculator, new procedures [P]

0054 D13 stands. The calculator gains these procedures:
- `budget_cards` (D4/D6);
- `where_it_went` (D7);
- `window_history` (D8);
- `avoidable` (D9);
- `request_trend` (D10);
- `request_log` (D10).

Existing procedures stay where their views survive. The agent op
`api_usage` (0068 §6) currently aggregates on its own
(`agent_ops.rs`) and **should** move onto these procedures, so agents
and the UI get identical numbers; that's a follow-up issue, not this
redesign's scope. Trade-off: two aggregations exist until then.

**D15a — The calculator contract (#695).** The procedures live in
`dashboard_core::usage_analytics::surfaces`, a submodule, because
`usage_analytics.rs` already holds the retiring tabs' procedures and
their page. Each one takes the page's rows plus a serde query and
returns a serde struct (0054 D13):

| Procedure | Query → output |
|---|---|
| `budget_cards` | `BudgetCardsQuery` → `BudgetCards { cards, default_budget }` |
| `where_it_went` | `WhereQuery` → `WhereItWent` |
| `window_history` | `HistoryQuery` → `WindowHistory` |
| `avoidable` | `AvoidableQuery` → `Avoidable` |
| `request_trend` | `RequestTrendQuery` → `RequestTrend` (trend buckets plus the band 3 ranking) |
| `request_log` | `RequestLogQuery` → `RequestLog` |

Budget-view queries carry a `BudgetScope { module, budget, as_of,
preset }`. A budget belongs to one module (`github` today, §12). The
Requests view takes a trailing `Span` only, because `Current window`
is Budget-only (D11).

The implementation settles these mechanical points:
- **Windows.** A snapshot is a response with `remaining_after`. Its
  `cycle_reset_epoch` comes from the same response, so it names the
  record's own window; 0053 D3's carry-forward only fills records
  without one.
  - The current window belongs to the latest snapshot of the last
    hour, while its reset is still ahead of `as_of`. With no open
    window, `Current window` resolves to nothing, and the bands are
    empty rather than guessed.
  - History discovers windows from their snapshots, then places each
    record of the budget by timestamp in `[reset − 1 h, reset)`.
    Records outside every known window are counted as `unwindowed`,
    never placed.
- **Cards.** A snapshot whose window has already reset keeps its
  verbatim numbers, but has no state and projects `WindowClosed`:
  GitHub has refilled the budget since. Exhausted (D5) means either
  the caller-supplied per-budget pause (`rate_limited_until`, Q10) is
  still in the future, or the open window reports 0 left.
  - `WindowDrop` carries `spent`, this app's `recorded_cost` and
    `external = spent − recorded_cost` (for #705). `external` can be
    slightly negative from record timing at the window edge, so
    consumers clamp it.
  - D7's default compares the remaining fraction of an open window;
    a paused budget counts as 0 %; unknown or closed windows count as
    unknown. It goes to REST whenever either fraction is unknown, as
    well as on a tie.
- **Where it went.** The output keeps every group. Groups beyond the
  top 8 also fold into one `other` row.
- **History.**
  - Every column carries the window's Σ known cost (the D8 height)
    and GitHub's deepest drop (`limit − min remaining`). The state it
    reached is the D5 ladder over that minimum, and reaching 0 counts
    as Exhausted.
  - A window belongs to the local day it starts on. The caller passes
    the UTC offset, because the calculator reads no clock. Every day
    of a 30- or 90-day range is present, empty days included.
  - `ScaleInputs` carries the data max, the limit and both guides. For
    columns the guides are 75 % and 90 % of the limit spent; for the
    burn-down they are 25 % and 10 % remaining. The view picks the
    axis.
- **Avoidable.** "Usually comes back 304" means the op has more 304s
  than 200s in range. Entries that cost 0 are not avoidable spend, so
  they never appear.
- **Requests.**
  - Status classes are `Success` (any non-error status other than
    304), `NotModified`, `ClientError`, `ServerError` and `Network`.
  - The 1h range buckets the trend by 5 minutes, because a single
    hour bucket is no trend.
  - The log's cost cell is `Requests(n)`, `Points(n)`, `Free304`,
    `Unknown` or `NoBudget`. Ties under any sort fall back to newest
    first.
- **Read bound (D11).** `UsageSource::records_between` already takes
  any lower bound, so the 90-day reach needs no source change. The
  page's read is #696's wiring.

### D16 — Non-quota records

`auth` and `local` (0064 D9) records never enter Budget-view figures.
They appear in the Requests view with their own transport facet.

### D17 — Agent attribution (0068 D6) [P]

0068 D6 (`Surface::Agent` / `Trigger::Agent`) is not in the shipped
enums. It stays planned and unchanged. When it lands, agent spend
appears in D7, D9 and D10 with no UI change, because group-by values
are data-driven.

Trade-off: the 0068 §2 goal of "attributable on the usage page" stays
unmet until then. The redesign doesn't block on it.

### D18 — Designed states

Every band has the three states (0065 rubric #18, [DG]):

- **Empty:** a band-specific `Empty` with one sentence and the next
  step ("No GitHub responses in this range. Widen the range or open a
  GitHub page."). When the store holds **no records at all** (a fresh
  install), the Budget view shows the no-snapshot cards (D4) and one
  page-level `Empty` in place of bands 2–4, without the Budget
  selector: "No GitHub responses yet. Open a GitHub page; spend,
  history and avoidable cost appear after the first response." Three
  identical band empties would be noise, and "widen the range" is
  wrong advice when no history exists. The Requests view words its
  empty the same way.
- **Loading:** only if D11's gate moves the read off-thread; skeletons
  per band.
- **Degraded store:** when `is_degraded()` (0057 D6, 0063 F4), the
  honesty line adds "History paused: storage unavailable, showing this
  session only": a storage icon in `warning`, the words in
  `foreground` (D5).
- **Single-sample series:** draw as points, never as an empty frame
  (D10a).

### D19 — Keyboard and accessibility

- **Every control is keyboard-reachable.** Segmented controls and the
  view switch are one Tab stop each, with arrow keys inside (or every
  segment a stop, per upstream behaviour). The tab-stop cycle must
  close on every view (`tree --tab-stops` receipt).
- **Tables implement `cell_label`** with their existing cell text
  (closes the app side of #351). Select options carry labels.
- **One a11y summary per chart**, kept as the D8 contract of 0054. It
  is no longer duplicated as both a visible line above and a caption
  below.
- Ids and roles follow docs/0010 D10 and 0049.

### D20 — Layout

- **Content width is capped** (about 1200 px) and centred, so wide
  windows don't stretch the charts.
- **Two columns down to about 640 px of content width** (Q2), one
  below; the breakpoint is measured on the content area, not the
  window. At the app's minimum size (1024×700, about 720 px of
  content) the budget cards stay side by side; stacking them there
  filled the whole first viewport in the mockups.
- **No nested scroll:** the D7 and D10 tables size to their content up
  to a cap. The request log is the one virtualized table, and it is
  the last band, filling the scroll viewport (D10, Q8): page scroll
  hands over to the log's own rows, with nothing below it. Today's
  fixed 300 px tables inside a scrolling page go away.
- At the minimum window width, the header fits on one line (verified
  at 1024×700 in the mockups); if a future control makes it wrap, it
  wraps as one deliberate second row.

### D21 — Vocabulary

- In the UI: "requests", not "flights". The code keeps `flight`.
- "REST requests" and "GraphQL points"; "free (304)"; "unknown cost"
  for unattributable.
- One casing everywhere (`REST`, `GraphQL`, `auth`, `local` as facet
  labels).
- No `Σ`, and no abbreviations outside the chip's tooltip.
- Cost columns are titled `Cost` (D7, Q11); the unit lives in the
  summary line and, in the mixed-transport request log, in each cell.
- Fixed wording: "snapshot under a minute old" (never "0 min old"),
  "Slower than 1 s", and the band titles "Where it went", "Spend per
  window" (≤ 7 days), "Peak window per day" (30 and 90 days), "This
  window" (Current window) and "Avoidable spend".

### D22 — Anchors [P]

- **Linear** stays the visual anchor (docs/references, the
  usage/analytics register).
- GitHub's own billing **Usage** page is named in the mockup PR as a
  *content* reference for "one chart plus a mirroring table". It is not
  a visual anchor, so `docs/references/` doesn't change.

Trade-off: no chart-specific visual anchor; the gpui-component story
gallery remains the widget-level one.

## 8 Contradictions in the existing docs, and how this doc settles them

| # | Contradiction or gap | Settled by |
|---|---|---|
| 1 | 0054 D11 "never summed" vs D5's pooled Σ and share | D2 |
| 2 | 0053 §1 "spend per quota cycle" vs 0054 D2 "no cycle selector" | D8: per-window history, no selector |
| 3 | 0054 D11 runway vs 0050 §2 hourly independent windows | D6 |
| 4 | 0054 D9 "recomputes on … `high_water` movement" vs D10 "nothing live" | D10 rules. The stale D9 sentence is corrected in 0054 |
| 5 | 0057 §1 "90 days of quota-cycle context" vs 0054 D2/D13 1-month reach | D11 |
| 6 | 0054 D11 "the only projection" | D6 keeps one projection, a different one |
| 7 | 0054 §2 "the chip keeps its popover" vs 0050 D5's tooltip | wording corrected in 0054; D13 makes the chip the button |
| 8 | Stale status headers (0053 table, 0054 opening line, 0068 header) | removed (AGENTS.md: docs have no lifecycle) |
| 9 | A truncated duplicate "No cycle selector" line in 0054 D2 | deleted |
| 10 | 0054 D13 "CLI/MCP consume the same results" vs 0068's own `api_usage` aggregation | D15: migration is a follow-up |
| 11 | 0068 D6 decided but not in code | D17 |
| 12 | "Rarely worth their cost" has no metric | D9 |
| 13 | No doc names the pages' user | D1, owner decision |
| 14 | 0054 D9 "reads off-main" vs 0057 D8 "UI-thread reads, owner-accepted" | 0057 D8 rules until the D11 measurement gate. The 0054 D9 bullet is annotated |
| 15 | 0050 D4's global pause vs two independent budgets (0050 §2) | Q10: one `rate_limited_until` per bucket; 0050 D4 amended |
| 16 | D20 "no nested scroll" vs a virtualized log above Diagnostics (first draft of D10) | Q8: Diagnostics above, the log last and viewport-filling |
| 17 | D5/D13 colouring state text vs the kit's `warning`/`danger` contrast | Q1: the tokens colour marks and icons only |

## 9 Where today's content goes

| Today | Goes to |
|---|---|
| Overview: points spent card | Budget band 1 (per-budget spend in each card's sub-line) |
| Overview: budget & runway card | Budget band 1 (D4); runway retired (D6) |
| Overview: top consumer card | Budget band 2 (D7) |
| Overview: slowest-op card | Requests glance strip (D10) |
| Overview: burn-down | Budget band 3, Current window (D8) |
| Overview: spend over time | Budget band 3 (per-window columns) |
| Overview: p90 trend | Requests trend (D10) |
| Quota: selectors, bar chart and table | Budget band 2 (D7) |
| Quota: donut | removed |
| Latency: glance, trend and ranking | Requests bands 1–3 |
| Latency: slowest flights | Requests log, sorted by duration |
| Latency: histogram, payload grid, contention, 304-vs-200 | Requests → Diagnostics |
| Facets: transport | Budget selector (Budget view) or transport facet (Requests view) |
| Facets: module, errors-only | Requests facets (Module, Status class) |

## 10 Verification and the mockup phase

**Prerequisites** (built separately, §2 item 5):
- **A seeded ledger** (#536 mechanism), so populated surfaces have
  receipts and goldens.
- **Rate-limit data in the GitHub fixtures** (docs/0081 §12.1 shipped
  without it). It needs a normal, a Low, a Critical and an exhausted
  variant, so the cards and the chip can be captured in every D5 state.

**Mockups** (step 2 of the process, done 2026-11-05):
- **rendered as static HTML at the owner's request**, because the
  redesigned views don't exist in code yet. Every colour is mapped
  from the app's theme tokens, the data is a port of the seed
  generator over fixture identities, and each frame was rendered to
  PNG at its exact window size with headless Chromium. They replace
  the container captures this section first planned;
- both views, at 1280×800 and 1024×700;
- in Default Light, Default Dark and Catppuccin Mocha (its `chart_*`
  slots are one hue, the stress case for D14);
- every D5 state of the cards and the chip, and the chip on a
  non-usage page (D13);
- the D18 states.

They named Linear as the anchor (D22) and were critiqued against
docs/0065 Appendix A (Claude models judging with their own vision,
AGENTS.md). Two review rounds settled the decisions in §2.1. The
mockups are throwaway artefacts, not merged code, and are not in the
repository.

The **docs/0071 container stays the source of every implementation
receipt**: the HTML mockups don't replace any receipt below.

**Implementation receipts** (step 4, per AGENTS.md):
- `audit`;
- a closed `tree --tab-stops` cycle on both views;
- new structural goldens (`usage-budget`, `usage-requests`, and seeded
  variants) replacing `usage-overview` and `usage-quota`;
- `crates/debug/verify/usage-freeze.txt` rewritten for the new header.
  Its guarantee, that the "as of" line survives a range change, is
  kept.

## 11 Implementation plan (issues filed after this doc merges)

The split is proposed and refined when the issues are filed:

1. **Calculator:** the D15 procedures with unit tests (window maths,
   projection guard, per-day peaks, avoidable ranking).
2. **Page shell:** the D1 view switch, the D3 header, the D11 presets
   and read bound with the measurement gate, D18 and D19 header work.
3. **Budget view:** D4–D9 bands, including the app-owned time-axis
   chart (D10a).
4. **Requests view:** D10, including the virtualized request log.
5. **Chip:** D13 and the D5 ladder, plus the D5 Critical toast, the
   chip on every page, and the per-budget pause (0050 D4 amended).
6. **Receipts:** goldens, the freeze script rewrite, and the
   `cell_label` and Select-label a11y (it may ride with 3 and 4).

Deferred follow-ups, each filed with the `deferred` label before the
implementation merges (AGENTS.md):
- `api_usage` onto the calculator (D15);
- custom date range;
- user-configurable thresholds (D5);
- visit-based waste (#334);
- an external-consumer estimate (other clients draining the shared
  budget: D6's window drop minus this app's recorded cost; #705);
- CSV export.

## 12 Non-goals

- **Changes to collection, the record schema or storage** (docs/0053,
  0057, 0063). The redesign reads existing fields only.
  The per-budget pause (D5, Q10) changes the fetch gate, not what is
  recorded.
- **Live updating of the page** (D12; 0054 D10 stays).
- **Secondary rate limits.** GitHub exposes no queryable state for
  them.
- **Other modules' budgets.** The structure is module-agnostic, but only
  GitHub reports budgets today.
- **The items deferred in §11.**
