# 0054 — Usage analytics page

Reads the pipeline shipped by docs/0053; nothing here changes
collection.

> **Superseded in structure by docs/0082** (usage surfaces redesign,
> two audiences). The Overview, Quota usage and Request latency tabs
> (D3) give way to a **Budget** view and a **Requests** view (0082 D1,
> §9 maps every view below to its new home). Specifically:
>
> - **D2:** range presets become Current window / 1h / 24h / 7d / 30d
>   / 90d (0082 D11).
> - **D4/D5:** cost views pick one budget and never pool REST requests
>   with GraphQL points (0082 D2).
> - **D6:** the donut is removed, and time charts use a proportional
>   axis (0082 D7, D10a).
> - **D11:** runway is replaced by an in-window projection (0082 D6).
> - **D13:** the read bound grows to 90 days (0082 D11).
> - **D15:** the burn-down applies to the current window only, with
>   per-window history beside it (0082 D8).
> - **D17:** no nested table scroll (0082 D20).
>
> D10 (frozen `as_of`) and D13 (one calculator) stand. The sections
> below remain the record of what shipped in 2026-09/10.

## 1. Problem & goals

docs/0053 gave us one immutable record per HTTP flight, in memory. This
doc designs the read side: a shell-level page that answers, from the
record log:

- What do we use our quota **on**? (use case #1 — designed here)
- General session indicators. (Overview tab — designed later, in its own
  round, then appended to this doc)

Design method, per the owner: **step by step, case by case** — each
general use case becomes one tab on the page. The page skeleton and use
case #1 are decided here; later use cases land as new sections before
their implementation.

**Designing for the general case (owner rule).** The page programs
against the record *contract* (schema + sink, docs/0053), never against
the storage behind it: swapping `MemorySink` for the SQLite sink (#333)
requires zero page changes. Ranges that out-run the data (1w/1m while
the app wasn't live) simply contribute fewer rows — an honest empty
window, not a missing feature. Nothing here claims completeness; each
tab shows what the retained records support.

## 2. Placement (decided earlier)

Shell-level page, not a github sub-page:

- `dashboard_core::nav::Page` gains `Page::Usage`; the shell hosts the
  component (same pattern as `Page::Settings`, docs/0006 — **tab
  selection lives inside the component, not in nav state**).
- Sidebar entry + breadcrumb `Home › Usage`; the Navigator gives
  Back/trail/dedup for free.
- The page is cross-module by design (records carry `module`; when
  forgejo/gitea land, their rows join the same views).
- Discoverability hook: clicking the usage chip on the github module
  home navigates here (the chip keeps its hover tooltip, docs/0050 D5; this adds a
  "details" affordance; docs/0082 D13 later makes the chip itself
  the button).

## 3. Page skeleton

```
┌──────────────────────────────────────────────────────┐
│ [1h] [1d] [1w] [1m] ⟳   filters: ⏷transport ⏷module ⚑errors │
│ ┌ Overview ┐ ┌ Quota usage ┐ …(future use cases)     │
│ ┌────────────────────────────────────────────────────┐
│ │  (active tab's content)                            │
└─┴────────────────────────────────────────────────────┘
```

**D1 — Shell hosting.** Component in `dashboard_core`
(`usage_analytics.rs`, sibling of `usage_records.rs`); `Page::Usage`
variant + sidebar row + breadcrumb. Charts render with
`gpui_component::chart` — this is the workspace's first chart consumer.
*(implemented in the step-3 PR — #341: the entity is created on first
navigation, not app start, so the freeze and its one bounded read belong
to first open; re-entry keeps the frozen state)*

**D2 — Global range picker.** A single `Range` control above the tab
strip; it filters **every** tab (it lives in the page header, above the
tabs, both logically and visually):

- Options: **1h · 1d · 1w · 1m** (trailing window from now). Default
  **1d**. Custom start/end is a follow-up, not v1.
- Semantics: `as_of − range <= ts_epoch_ms < as_of`, where `as_of` is
  the page's memoized *now* (D10) — never wall-clock. Nothing else in
  the record participates in windowing.
- **No cycle selector.** Rate-limit cycles are per-transport and
  per-host facts that reset independently (each response carries its
  own reset instant; REST and GraphQL windows rarely align, and
  future modules will diverge further) — a single "cycle" scope
  would be wrong for at least one transport. Cycle context is shown
  *descriptively* where useful
  (per-transport `cycle_reset_epoch` fields), never as the window.
- **Window honesty (v1).** Data is the session's in-memory FIFO
  (`RECORD_CAP = 10_000` flights, docs/0053 D3). The picker is the
  *contract*; the header states what is actually retained and how
  stale the view is — `812 flights · as of 14:32 · oldest retained
  09:41` —
  and 1w/1m simply show the same partial window until persistence
  (#333) lands. The page never pretends to more history than it has.

**D3 — Tabs.** `gpui_component` `TabBar` + `Tab`, one tab per general
use case:

- **Overview** — general indicators; designed in a later round.
- **Quota usage** — use case #1 (§4).
- Future use cases append tabs here as they are designed.

Tab state is component-local; switching tabs keeps the range **and
the frozen `as_of`** (both page-global, not per-tab). v1 ships the strip with the tabs that have
landed designs — an Overview slot is reserved but the tab appears only
with its implementation.

**D10 — Frozen `as_of` view: nothing on this page is live (owner
rule, round 3).** *(implemented in the step-3 PR — #341: the honesty
header is additionally exposed as an a11y `Status` node per the
docs/0049 pattern, so the freeze is observable in the debug bridge and
announced by screen readers)* The page keeps a memoized timestamp `as_of` that
means *now, for the data on screen*. Every window, every tab, every
number is a pure function of records inside `[as_of − range, as_of)` —
data cannot drift under the viewer: records landing after the freeze
have `ts >= as_of` and are invisible until an explicit refresh.
*(Capture clock, docs/0081 §12.1: `as_of` reads
`freshness::clock_epoch_ms`, which is the wall clock except under
`DASHBOARD_FIXED_NOW` in debug builds. There it is the pinned instant
the seeded fixture ledger ends at.)*

- **The refresh button (⟳, beside the range picker) is the only path
  that moves `as_of` or touches the store**: it stamps `as_of = now`,
  re-reads the window, and recalculates everything on the page,
  including range-picker windows. One load also happens on first open
  (`as_of = now`), so the page never opens empty demanding a click.
- **Control changes never re-read the store** — dimension/tab/filter/
  range switches are pure recomputations over the frozen window (and
  re-derive the new window bounds from the same `as_of`).
- **Refresh is not a fetch.** It reads memory (v1) and never involves
  `Http`, specs, origins, or the status spinner; a blocking backend
  (#333) runs the read off-main per D9 and lands the new `as_of` when
  it completes. The button carries a role + id (docs/0010 D10) and is
  keyboard-operable.
- This is the analytics twin of docs/0052: the app does not
  auto-refresh content, and its analytics does not auto-refresh
  analysis. Any future liveness (live tail) would also be
  refresh-driven, never automatic.
- `high_water` stays in the read contract (change/eviction detection,
  tests, future use) but the v1 page does not consult it — consulting
  it would be liveness by the back door.

## 4. Use case #1 — Quota usage analytics

Goal: *see what we use our quota on.* The tab answers it as one
question: **pick dimensions, see a ranking.**

**D4 — Two group-by selectors + a filter row.** The tab's controls are
shaped by the data itself (six categoricals; the meaningful compound
questions are pairs):

- **`Group by`** — primary dimension, default `Surface`. Values:
  `Surface`, `Trigger`, `Operation`, `Target` (`ident`, `—` when
  `None`), `Transport`, `Module`.
- **`Then by`** — optional secondary dimension (default *none*); rows
  nest under the primary. The two designed pairs are
  `Surface ▸ Trigger` (the origin matrix) and `Operation ▸ Target`
  (endpoint drill). Any cross is allowed; none is required.
- **Filter facets** (predicates, not rankings — low-cardinality
  slices set once and forgotten): `Transport`
  (rest/graphql/auth — plus `local` since docs/0064 D9: non-network
  module records, e.g. the favicon generator's export runs),
  `Module`, and an errors-only toggle (`network_error` / failed
  `status`). They apply to charts and table alike.

No third group-by selector, ever: the fact table has no third
dimension worth a pivot — more selectors fragment existing questions
into unreadable cells. Selector count is part of the contract; storage
changes (#333) must not grow it.
*(implemented in the step-4 PR — #342/#347: `Then by` offers `(none)`
plus the five other dimensions; picking a `Group by` equal to the
current `Then by` resets the secondary — a self-nest is not a
question)*

**D5 — The ranking table.** `DataTable` over grouped aggregates, sorted
by Σ cost desc (ties: flights desc). With a `Then by` dimension, rows
nest (primary group header row, indented member rows) — one flat
table, never a pivot cube:

| column | source |
|---|---|
| *(dimension value)* | group key; `ident: None` renders `—` |
| **Σ cost** | `Σ cost.unwrap_or(0)` — **known costs only** |
| share | Σ cost ÷ total known cost, % |
| flights | record count in the window |
| avg | Σ cost ÷ costed flights |
| unknown | flights with `cost: None` (network errors) |

The unknown column keeps the no-guessing rule visible: unattributable
flights are counted, never priced as 0.

**D6 — Charts.** Two charts above the table, both straight from the
verified `gpui_component::chart` inventory (pinned rev `94a313a`):

1. **Horizontal bar chart — the comparison view.**
   `BarChart` + `BarAlignment::Left` (bars grow rightward), one bar per
   group, length = Σ cost, bands labeled by dimension value, sorted like
   the table, top N with an `other` roll-up when groups exceed ~12.
   Horizontal because the values are words (`pr-page`, `owner/repo`)
   and rankings read top-down.
2. **Donut chart — the composition view.** `PieChart` with
   `inner_radius` (donut), same grouping, top 7 + `other`, center label
   = total known cost. Answers "is one thing eating everything?" at a
   glance.

Both charts share the table's data (same aggregate vector); hovering
shows the group's numbers via the chart tooltip (`plot` module).
*(implemented in the step-4 PR — #342/#347: the donut's total rides as
a caption line beneath it rather than inside the hole — the pinned
rev's `PieChart` has no center-text API and a hitbox-safe overlay is
not available in this gpui)*
Time-bucketed series (spend per hour) belong to the Overview tab, not
here. Charts render the same grouping the table shows (compound
groups chart as flat bars per leaf group, labels prefixed by the
primary).

**D7 — Derived view, not per-render aggregation.** Aggregation is a
pure function in core: `rank(records_in_window, dimension) →
Vec<GroupAgg>` (fields: key, cost_sum, flights, unknown, avg), fed by
the read contract (§8) **only on open and refresh** (D10). The page
caches the frozen window and derived aggregates keyed by
`(as_of, range, dimension, filters)`; control changes recompute
synchronously (pure, at most `RECORD_CAP` rows, O(groups) in render) —
background flights cannot touch the frozen set, so there is nothing
to debounce. No fetching of any
kind: the tab runs zero specs, stamps no origins, never trips the
status spinner — the page is a reader, like docs/0053 §5 tests.

**D8 — Accessibility.** Table and Select ride upstream roles; charts
get `.id(...)` plus a text summary in the accessible tree (chart pixels
are invisible to AT — the summary carries the top group and the total,
e.g. "Quota by surface: pr-page 41 of 92 points").
*(implemented in the step-4 PR — #342/#347: Selects do ride upstream
roles (`ComboBox` with our accessibility labels); the upstream
`DataTable` at the pinned rev exposes **no** a11y nodes at all —
rows/cells are invisible to AT and to the debug bridge. The chart
summaries plus the honesty Status carry the AT story meanwhile; the
table exposure is an upstream gap tracked for the rev bump, same class
as the sidebar item gap)* The range picker and
tabs are keyboard-operable like all shell controls (docs/0049 rules).

## 5. Overview tab — quota at a glance (v1)

The first tab: general indicators, partial by design (no completeness
claims). v1 is a card strip (D11) plus a chart row (D15) answering the
quota questions at a glance; more cards/charts append round by round,
and more use-case tabs append beside this one.

**D11 — Three at-a-glance cards (v1).** One strip, all pure functions
of the frozen window `[as_of − range, as_of)` — cards freeze and
refresh with everything else (D10), reusing the D7 derived view (no
store access outside open/refresh):

1. **Points spent** — Σ known cost, **split REST / GraphQL** (separate
   pools with independent cycles — never summed into one number).
   Subtitle: flight count and `N unattributable` (network-error)
   flights excluded from the figure.
2. **Budget & runway** — the latest `remaining_after` snapshot with
   `ts < as_of`, per transport, shown as **remaining % of `limit_after`
   from the same snapshot** (absolute points as secondary text).
   Runway = remaining ÷ (spend ÷ window hours) → "≈ 6 h at current
   pace" — the **only projection on the page**, visually marked `≈`,
   with the snapshot's age shown so staleness is visible. If a
   snapshot lacks `limit_after` (schema-miss), the card degrades to
   absolute-only; it never invents a ceiling.
3. **Top consumer** — #1 surface by Σ cost: `pr-page · 41 pts · 45 %`
   with a mini top-3 beneath; shares are of known cost only.

Cards are composed stat cards (number-first, one-line subtitle,
tooltip detail), each with an id + accessible summary (docs/0010
D10 / docs/0049): the summary repeats the card's numbers, since
pixels are invisible to AT.
*(implemented in the step-5 PR — #343: four cards in a wrapping
strip; runway rides as its own `≈` line per transport so the
projection is never glued to a measured number; each card is a
`Label`-role node carrying title + all numbers in its aria label)*

**Schema enabler (owner decision, round 4): records gain
`limit_after`.** The percentage treatment above rides on one additive
pipeline field — see §8's amendment note (D14).

**Deferred cards** — burn rate, free flights, failed flights:
issue #336 (`deferred`).

**D15 — Overview chart row (round 6): two quota charts.** Beneath the
card strip, both `as_of`-anchored (a stale view stays a coherent
picture), both from use case #1's data:

1. **Budget burn-down** — the trajectory chart: `remaining_after`
   over the window, **one step line per transport** (separate pools),
   rendered with `LineChart`. Snapshots carry the last known value
   forward across flight-less stretches (labeled — that is what
   remaining *was*, as far as any response said). Cycle resets show
   as the natural up-jumps they are; once `limit_after` is recorded
   (D14) each transport gains a dashed ceiling line, making
   "closest approach to the wall" readable and resetting-to-full
   confirmable.
2. **Spend over time** — the activity chart: Σ cost per time bucket
   across the window, one series per transport (never pooled),
   `AreaChart`/`LineChart` over the same ~40-bucket rule as D12's
   trend. Bucket totals sum to the window's Σ cost; a bucket's
   tooltip names its top surface ("14:00–14:36 · 9 pts · mostly
   pr-page") — the "what happened at 14:00" answer at a glance.

Both are calculator outputs (`BurnDown`, `SpendTrend`) riding the
`overview` procedure (D13) — no new read or render machinery.
*(implemented in the step-5 PR — #343: the pinned rev's `LineChart`
is one series per chart, so burn-down and spend render as one panel
per transport rather than two lines in one frame; its x scale is
ordinal, so labels carry second resolution (`HH:MM:SS`, `′` marks on
same-second collisions) and spacing is uniform, not wall-clock
proportional; the dashed ceiling has no stroke API at this rev, so
the ceiling and closest approach ride each panel's caption and a11y
summary instead of a drawn line — all revisited at the rev bump)*

**D16 — Overview health axis (round 7): one line + one card from use
case #2.** Overview gains a latency axis — a sampler of the latency
tab, deliberately condensed (owner pick A + B):

1. **Latency p90 trend (condensed)** — one pooled p90 line per time
   bucket, its own small panel beside the quota charts (shared
   bucketing rule, `LineChart`). Pooled across transports on purpose:
   milliseconds are one currency, and the line answers "is the app
   feeling slow right now", not "which transport" — the tooltip
   splits per transport for whoever asks. The near-duplicate of
   D12's trend is the point: Overview samples each use case's
   headline.
2. **Slowest-op card** — the fourth card in the strip: op name ·
   p90 · worst single flight (`pr-detail · p90 2.3 s · worst 8.1 s`).
   The highest-value single latency fact at a glance. Absorbed from
   #337's deferred strip; that issue's scope shrinks accordingly
   (p50/p90 overall + >1s count remain there).

Both ride the `overview` procedure (`OverviewView` grows `p90_trend`
and `slowest_op`) — still one call, one tab view.
*(implemented in the step-5 PR — #343: the pooled p90 line charts
only buckets with flights; the per-transport splits ride the panel's
a11y summary ("REST up to … · GQL up to …") since the stock tooltip
cannot carry custom per-bucket fields at this rev)*

**D17 — Tab content scrolls under a fixed control surface.** The page
keeps its strips (range, facets, tab bar) fixed; each tab's content
area scrolls vertically within the page slice the strips leave
(`overflow_y_scroll` on the tab root — overview and latency had it
from the start; the quota tab joined late and squeezed its content
until then). Two layout rules make the scroll honest, both learned
the hard way:

1. **Auto-height rows inside a scroll root take `flex_none`.** A
   scroll container does not relax flex-shrink: a squeezable row
   compresses, paints its overflow over the siblings below, and the
   scroll extent lies. Natural-height children + the root's scroll
   is the whole trick.
2. **`Select` slots are bounded by the parent** (`div().w(..)
   .flex_none().child(Select::new(..))` — the page.rs org-picker
   pattern). The upstream Select's outer wrapper is hardcoded
   `size_full()` (select.rs) and style refinements only reach the
   inner trigger: unbounded, the wrapper eats the whole row cell
   (the a11y dump even reports junk `955×134` bounds), flex-wrap
   stacks the controls vertically, and the overflow lands on top of
   whatever follows. Tables inside a scroll root take a fixed height
   slice (`h(px(300.))`), never `flex_1` — a flex slice collapses to
   nothing whenever the content above exceeds the viewport.
*(implemented in the analytics-scroll PR)*

## 6. Use case #2 — Request latency

Goal: *see how slow our requests are, where, and why* — the developer's
and the agent's tab. Every record already carries `duration_ms`
(transport wall time: connect + request + server + response read; it
excludes our queue wait — now recorded separately, D14 — and any
post-processing). The tab states this definition on its face.

Architecturally this tab is use case #1 with a different metric set:
it reuses the dimension selectors, filter row, ranking table and
charts (D4–D8), and adds one control.

**D12 — v1 views.**

1. **Metric selector + ranking table** *(the core)* — metric chooses
   what ranks: `p50 / p90 / max / error rate / count`, default **p90**
   (latency is heavy-tailed; averages and medians hide exactly the
   slowness being hunted). Table per group: count · p50 · p90 · max ·
   error rate. Percentiles are nearest-rank over the group's
   durations in the frozen window; counts sit beside them because a
   p90 of two flights means nothing.
2. **Latency trend** — line/area chart of p50 and p90 per time bucket
   across the window (`LineChart`/`AreaChart`). Auto-bucketing caps at
   ~40 buckets regardless of range (1h → ~90 s buckets, 1d → ~36 min).
   Answers "did the network degrade, or did some pattern start at
   14:00?" — and is `as_of`-anchored like everything else.
3. **Slowest flights** *(the debugging receipt)* — top-20 individual
   flights by `duration_ms` in the window with full context per row:
   time · op · ident · surface/trigger · status · duration (and
   `queue_ms`, once recorded, so self-wait is separable from network
   wait). "What was that 8-second call" answered without log grepping.
   *(implemented in the step-6 PR — #344: all three views ride the
   calculator's `latency_ranking` / `latency_trend` /
   `slowest_flights`; the tab reuses the quota tab's two dimension
   selectors and one-flat-table shape, plus the metric selector
   (`p50 / p90 / max / error rate / count`, default p90) as the one
   added control. The duration definition rides the tab face as a
   muted line. The trend's p50 and p90 render as two panels — the
   one-series-per-chart rev constraint again — with a bucket-width
   caption; the slowest table caps at 20 and shows `queue` in its own
   right-aligned column, `—` when not recorded)*

**Deferred views — landed with #337.** The round-5 deferral, as
amended (its slowest-op item was absorbed into D16): *(implemented
in the #337 PR — #411: the at-a-glance strip renders p50/p90 per
transport plus the >1 s count as the tab's first card row; the
distribution histogram is six fixed bands — `<100 ms`, `100–250`,
`250–500`, `0.5–1 s`, `1–2 s`, `>2 s` — always all six in band order,
empty bands kept, horizontal bars beside the trend; the 304-vs-200
comparison lists only ops with both sides in-window — median per
side, Δ median, and a `≈` bytes-saved estimate from `resp_bytes` —
as a table after the slowest list. All three ride the D13 split
(procedures `latency_glance` / `duration_histogram` /
`revalidation_comparison`, each with its `{window, filters}` query)
and freeze with the page's `as_of`. The payload-size histogram
split and the contention view followed: #410.)*

**Latency enrichments — landed with #410.** The two views the #337
PR left out, both riding the same D13 split (`payload_split` /
`contention`, each with its `{window, filters}` query) and the same
freeze: *(implemented in the #410 PR — #412: the histogram gains
its payload axis as a cross-tab — the six duration bands × five
binary size bands `<1 KB` / `1–10 KB` / `10–100 KB` / `100 KB–1 MB` /
`>1 MB`, all bands in fixed order, empty cells kept — so "slow
because heavy" separates from "slow because far"; flights with no
measured body (304s, network errors) are counted beside the grid,
never guessed into a band. The contention view counts network spans
`[ts − duration, ts)` — half-open, touching spans never overlap, a
zero-duration span occupies nothing — as a peak card (max in flight,
first instant, ops), a queue card (p50/p90 of `queue_ms`, waits
≥ 100 ms), and a per-bucket max-in-flight step line beside the
trend.)*

## 7. The analytics calculator (shared engine)

**D13 — One pure calculator, every tab, thin renderers.** All
computation lives in one core module (`dashboard_core::usage_analytics`),
*(implemented in the step-2 PR — #340: five procedures with serde
round-tripping outputs; the tie-break convention beyond the doc's
"cost desc, flights desc" is key-asc everywhere, and query structs
are serde too, so the future CLI/MCP can carry them verbatim)*
structured so the eventual programmatic exposure (CLI/MCP) consumes
the *same* results the UI renders (owner rule: "we would be golden"):

- **Inputs**: the record rows (the page's single refresh read, D10)
  plus a per-tab query — `QuotaQuery { window, group_by, then_by,
  filters }`, `LatencyQuery { window, group_by, then_by, filters,
  metric }`, `SlowestQuery { window, filters, limit }`,
  `OverviewQuery { window }`. The window derives from
  `(as_of, range)` — the calculator windows and filters rows itself.
- **Outputs**: per-tab, serde-`Serialize` structs (`QuotaRanking`,
  `LatencyRanking`, `LatencyTrend`, `SlowestFlights`,
  `OverviewView { cards, burn_down, spend_trend, p90_trend,
  slowest_op }`)
  built from plain owned types (no gpui types leak into the API).
  **Per-tab procedures** on the module's interface — one call per tab
  view, never "everything for every tab at once" (owner rule).
- **The Overview tab is treated exactly like any other tab** (owner
  decision): `overview(rows, query)` is just another procedure
  returning the tab's full view — D11's three quota cards, D15's two
  quota charts, D16's latency line and slowest-op card.
- **The page is a thin renderer**: components map output structs to
  gpui-component widgets; a future CLI/MCP maps the same structs to
  JSON/text with zero reimplementation. No analytics logic lives above
  the calculator; nothing UI-specific lives inside it.
- **Read discipline (D10 refinement)**: one bounded read per
  open/refresh — `records_between(as_of − 1 month, as_of)`, the widest
  picker option — after which *every* control change is a pure
  calculator call over the fetched rows. Control changes still never
  touch the store; the storage-side window filter survives as the
  month bound. Custom ranges (later) may widen that bound.
- Deterministic given seq-ordered rows; no I/O; no gpui — the whole
  module is plain unit-testable. Serializer versioning waits for the
  exposure round.

## 8. The read contract (writer/reader split)

docs/0053 D3 shipped one trait with both concerns — `record` (writer)
and `snapshot` (reader, but only a §5 *test* affordance). The page
needs a real read contract, and the general-case rule decides its
shape: the page must never force a backend to materialize data it
didn't ask for. *(Unchanged by docs/0063: the app DB generalization
shares the writer thread and file, never this contract — notification
reads ride the same lazy reader and the usage token stays
usage-scoped.)* **This section amends 0053 D3** — `snapshot()`
migrates from `UsageSink` to the new source trait; the amendment
line lands in 0053 with this doc's PR.

**D9 — `UsageSource` beside `UsageSink`.** *(Implemented in the
step-1 PR — #339: the split shipped with `UsageStore` as the injected
object; `MemorySink` gained `high_water` as an atomic bumped per
mutation, eviction included; the §5 records tests read through the
source view.)*

```rust
/// Read side of the usage store. The window filter is storage-side;
/// everything else (grouping, ranking) stays in core's pure
/// functions — one aggregation implementation, every backend.
pub trait UsageSource: Send + Sync {
    /// Records with `from_epoch_ms <= ts_epoch_ms < to_epoch_ms`,
    /// ascending by seq (deterministic downstream tie-breaks). The
    /// upper bound is what makes D10's freeze real — a lower bound
    /// alone lets post-freeze flights slip into re-slices.
    fn records_between(&self, from_epoch_ms: u64, to_epoch_ms: u64)
        -> Vec<UsageRecord>;

    /// Sugar: everything from `from_epoch_ms` onwards.
    fn records_since(&self, from_epoch_ms: u64) -> Vec<UsageRecord> {
        self.records_between(from_epoch_ms, u64::MAX)
    }

    /// Opaque monotonic token; consumers recompute when it moves.
    /// Must change on `record` **and** on cap eviction — eviction
    /// alters the retained set without advancing seq.
    fn high_water(&self) -> u64;

    /// Full retained set — test affordance (`records_since(0)`).
    fn snapshot(&self) -> Vec<UsageRecord>;
}

/// What `init_with` injects: one object, both capabilities.
pub trait UsageStore: UsageSink + UsageSource {}
```

Design choices:

- **Window filter storage-side, aggregation core-side.** Only the
  `ts` predicate is pushed down; `rank()` stays the single
  aggregation implementation (tested once, identical across
  backends). Pushing GROUP BY down would let MemorySink and SQLite
  drift apart.
- **`high_water` is a mutation token, not `max(seq)`** — otherwise a
  1m-window cache goes stale when the 10k cap evicts rows that are
  still inside the window. MemorySink: a counter bumped on push and
  evict; SQLite: a version column.
- **`UsageSink` slims to `record` only** — writers (the `Recorder`,
  `RecordingHttp`) depend on writing alone; the page depends on
  reading alone; `init_with` takes `Arc<dyn UsageStore>`, and
  `sink(cx)`/`source(cx)` hand out the matching views.
- **Reads may block.** The trait is sync. This bullet originally said
  the page calls it off-main via the background executor when the
  backend isn't in-memory (the same discipline as `Http`, docs/0005
  Q4). docs/0057 D8 later chose direct UI-thread reads (owner-accepted),
  and that is what shipped. docs/0082 D11 adds a measurement gate for
  the 90-day reach.
- **Deliberately absent:** an `on_change` callback (liveness would
  arrive with the live tail, deferred. Per D10, the page recomputes
  only on first open, explicit refresh and control changes; it does
  not react to focus or `high_water` movement) and aggregate push-down (a new
  read API *only if* volume ever demands it; the page contract stays
  `Vec<UsageRecord>` until proven insufficient).

Migration cost, checked against the shipped code: the docs/0053 §5
records tests call `sink.snapshot()` through `UsageSink as _` — they switch
to the `UsageSource` import; `MemorySink` gains a mutation counter;
the swap-contract test injects through `Arc<dyn UsageStore>`
unchanged in spirit.

**D14 — Schema amendments riding with this doc's PR (0053 D1
additions, all additive, all `Option`, none retro-filled).**
*(Implemented in the step-1 PR — #339: all three fields stamp in the
`RecordingHttp` draft; `queue_ms` anchors at `run_spec_tracked`'s
dispatch — re-stamped per 401-retry round — with auth/connect flows
stamping from spawn; GQL `limit_after` extends the body parse by one
member, REST rides the header snapshot.)*

1. **`limit_after: Option<u32>`** (owner round 4) — the quota
   ceiling the response itself reported, snapshotted beside
   `remaining_after`: REST from the already-parsed
   `x-ratelimit-limit` header (`RateLimit.limit` — parsed today,
   simply not recorded), GraphQL from the body's `rateLimit.limit`
   (one more member in the existing parse). `None` for network
   errors and auth flights; no smoothing (per-snapshot honesty).
   Unlocks percentage cards (D11 card 2), burn-down ceiling lines,
   clean reset detection (`remaining == limit` after a jump), and
   cross-module comparability once forgejo/gitea arrive.
2. **`queue_ms: Option<u32>`** (owner round 5) — wait from spec
   dispatch to flight start, stamped where `run_spec_tracked` hands
   off to the decorator (auth flows stamp from spawn). Makes
   self-inflicted contention visible and separable from network
   latency in D12's slowest-flights list. `None` when the dispatch
   instant isn't captured.
3. **`resp_bytes: Option<u32>`** — response body length (free: the
   body is already in `HttpResult`). Payload size is the other half
   of "why was this slow", and it enriches the deferred 304-vs-200
   and histogram views (#337). `None` when there is no body.

Landing all three here keeps #333's SQLite schema migration-free;
any `None` degrades gracefully across every consumer.

## 9. Testing

- `limit_after` units: recorded from both transports (header /
  body), `None` on network-error and auth flights, card-2 percentage
  math and its absolute-only fallback.
- `queue_ms`/`resp_bytes` units: dispatch→start gap visible in the
  record, bytes equal body length, `None` cases.
- Calculator units, per procedure: fixture rows → expected structs
  (quota, latency ranking incl. nearest-rank percentiles and the
  metric selector, trend bucketing rule, slowest-flights order and
  context, overview cards, #337 views — band boundaries, per-
  transport glance, revalidation medians and bytes — and #410
  views: size-band boundaries incl. the 1_024 edge and the
  unmeasured/304 case, peak/touching-span/bucket-max contention,
  queue percentiles; determinism on
  seq-ordered input; serde JSON round-trip of every output struct.
- Overview chart units: bucket sums equal the window's Σ cost (per
  transport); burn-down carry-forward holds the last snapshot to
  `as_of`; ceiling line present only when `limit_after` exists;
  pooled p90 line matches per-transport values in tooltips;
  slowest-op card picks the max-p90 group with deterministic
  tie-breaks.
- Freeze units: records landing after `as_of` never appear until
  refresh; refresh picks them up; range windows derive from `as_of`,
  not wall-clock; refresh reads memory only (no spinner, no specs).
- Pure aggregation units (no gpui): window boundaries (exactly at
  `now − range`), ranking + tie-break, share denominators (all-unknown
  → no % column, not divide-by-zero), `—` ident bucket, `other`
  roll-up, unknown-count correctness.
- Read-contract units: `records_between` boundaries (both ends,
  half-open), seq-ascending order, `high_water` moves on push **and**
  eviction, `snapshot` == `records_since(0)`, `records_since` sugar
  matches an open-ended `records_between`.
- `TestAppContext` smoke: page opens on `Page::Usage`, range change
  re-filters, dimension change re-ranks, tab strip switches.
- Bridge verification: `tree` shows the table, picker and tabs with
  roles; exercising the range picker via `click` re-renders the table.

## 10. Non-goals / follow-ups

- Custom start/end range (later, after v1).
- Live/auto-updating analytics — permanently out (D10, owner rule):
  the refresh button is the only clock.
- Cycle selector — permanently rejected (independent transport cycles).
- A third group-by selector — permanently rejected (D4).
- Persistence/depth beyond the session window — #333; the honesty note
  is the v1 answer, and the swap must be page-invisible (general-case
  rule).
- Per-visit correlation — #334.
- Further Overview charts/cards beyond D11+D15 — later rounds (the
  time-bucketing machinery is shared with D12).
- CLI/MCP exposure itself — future work; the calculator (D13) is the
  seam it will consume, including serializer versioning.
- Retry/attempt index field — 401 re-entries already re-emit as
  separate records (0053 D1); linkage is seq adjacency.
- CSV/export of the table.

## 11. Resolved decisions

### Round 1

- **Overview tab**: designed too, in this doc's next round — partial by
  design, no completeness claims anywhere on the page.
- **Chip → page hook**: lands with the Overview tab.
  *(implemented in the step-5 PR — #343: a small icon button beside
  the chip body, `Usage analytics`, navigating to `Page::Usage`; the
  chip's popover and aria label are unchanged)*
- **Bar chart cutoff**: fixed top-12 + `other` for v1.
- **General-case rule**: the page outlives the storage layer; ranges
  without data are honest empty windows, never redesign triggers.

### Round 2

- **Read contract (§8, D9)** — answering "do we have a reader trait?":
  no, only the test-affordance `snapshot()` on the writer.
  `UsageSource`/`UsageStore` become the page's contract; the 0053 D3
  amendment (`snapshot` migrates to the source trait) rides with this
  doc's PR.

### Round 3

- **Nothing live (owner rule)**: memoized `as_of` + explicit refresh
  button (D10); range windows derive from `as_of`, never wall-clock;
  the read contract grows the upper bound (`records_between`, §8).

### Round 4

- **Overview v1 = three cards** (D11): points spent, budget & runway
  (now a percentage card), top consumer. Burn rate / free flights /
  failed flights deferred → issue #336.
- **`limit_after` approved**: added with the current work — one
  additive field, two emission sites (§8/D14); lands
  before #333 freezes a SQLite schema.

### Round 5

- **Use case #2 (request latency) v1**: metric-selector ranking
  table + latency trend + slowest flights (D12); histogram /
  at-a-glance strip / 304-vs-200 deferred → issue #337.
- **More measurement approved**: `queue_ms` and `resp_bytes` join
  `limit_after` in the riding schema amendments (D14).
- **Calculator generalization (D13)**: one pure per-tab-procedure
  module, serde outputs, thin renderers — the programmatic-access
  seam for the whole page; Overview treated exactly like any tab.

### Round 6

- **Overview charts decided**: budget burn-down (per-transport step
  lines + `limit_after` ceilings) and spend over time (per-transport
  bucket series, shared bucketing rule) — D15, both calculator
  outputs on the `overview` procedure.
- **Pending**: which latency-side cards/charts (if any) join the
  Overview strip — candidates tabled with their #336/#337 overlap
  flags.

### Round 7

- **Overview health axis**: condensed p90 trend line (pooled, tooltip
  splits per transport) + slowest-op card (D16) — owner pick A + B;
  failed-flights card stays deferred in #336, the >1s badge and
  p50/p90 strip stay deferred in #337 (scope amended by comment).

## 12. Errata (post-ship fix, 2026-09-27)

**Chart x-axis tick density.** The overview and trend line panels shipped
with upstream `LineChart`'s default `tick_margin` of 1 — one x label per
bucket. Combined with the ~40-bucket rule (§7) and the second-resolution
labels the ordinal point scale forces (§5, `format_hms`), a populated
window drew ~40 eight-character labels into a ~300–450 px panel: unreadable.
The shared panel now thins ticks to at most six (`tick_stride`), preferring
a stride that also lands a tick on the newest sample so "now" stays
readable. Data resolution is unchanged — thinning affects axis labels only.

**0.7.0 note (2026-09-29, #363).** Re-verified at the v0.7.0 pin
`0c830f4`: `tick_margin` semantics are unchanged — the label thinning
still lives in `labeled_items` (`(i + 1) % tick_margin == 0`, chart/
mod.rs:186 at 0.7.0), so `tick_stride` keeps landing a tick on the newest
sample. 0.7.0 also adds a native `x_tick_count(n)` builder (the
`label_count` path in the same helper) — a future simplification could
replace `tick_stride` + `tick_margin` with `.x_tick_count(6)`; deliberately
not folded into the bump PR.

**0.7.1 note (2026-10-06, gpui-kit bump).** Upstream v0.7.1 (`87d10ae`,
gpui-kit #3296/#3308) gives every built-in chart a first-paint draw-in
animation by default — reduced-motion aware, `.appear(false)` opts out,
`.appear_key(...)` replays. The quota bar/donut and latency charts on this
page inherit the default (we set no `appear` flag and no
`interactive(false)`); the one-line opt-out is the documented rollback if
the motion reads wrong. Visual re-check rides the bump PR's human-check
list (Linux-box bridge audit pending).
