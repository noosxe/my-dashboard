# 0084 — PR page freshness: change-probed, focus-scaled refresh

- **Date:** 2026-11-14
- **Epic:** #730 (PR view fixes)
- **Amends:** docs/0052 (D1, D2, D4, §6), docs/0080 (D1), docs/0072
  (D3, D9), docs/0050 (D1's 304 sentence), docs/0016 (§15 and the
  refresh clauses of the detail-query decision), docs/0083 (the "no
  queue polling" rule), docs/0038 (its conditional-polling idea comes
  back in a new form). See §11.
- **Defects found and filed separately:** #871, #872, #873, #874,
  #875, #876 (§4).

## 1. Problem

The user reports that the PR page's tabs are often inaccurate, so they
keep pressing Refresh. That is the most common complaint about the page
today. The page is used while things happen on GitHub: CI runs, review
replies, a teammate pushing, the PR merging. Today almost nothing
updates on its own:

- since docs/0052 no timer fetches content;
- the one automatic trigger, window refocus, is silently disabled for
  the first five minutes after any landing (defect #871, §4).

So in practice a PR page refreshes when it opens, when the user writes,
and when the user presses Refresh.

This doc proposes how the page stays accurate without the Refresh
button. In short:

1. Fix the defects that cause staleness on their own.
2. Use GitHub's free conditional REST requests to detect a change
   before refetching anything.
3. Refetch only the lanes that changed.
4. Scale the cadence with focus, visibility and recent change, inside
   an explicit budget.

### 1.1 Staleness today, per area

The code was traced on main at `5179f02`. "Opening the page" means
`open_pr`, which shows whatever the store holds and revalidates the
detail only when it is cold or older than the 5-minute TTL (docs/0072
D3/D9). "Refocus" means the window-activation trigger (docs/0052 D4).

| Area | Fetched by | What refreshes it today | Missed changes on GitHub, and until when |
|---|---|---|---|
| Header: title, state, "updated" age | `PrDetail` GraphQL, `PR_DETAIL_QUERY` (`gql.rs`); falls back to the list summary | Page open (skipped inside the TTL), Refresh, writes, head change, UNKNOWN re-poll | Merge, close or title edit by others: until Refresh or a reopen after 5 min. Refocus does nothing inside 5 min (#871). |
| Conversation timeline (comments, reviews) | Same query: `comments(last:100)`, `reviews(last:100)` | Same as the header | New comments and reviews by others: same as the header. Your own review batch from Files leaves the review cards stale (#872). |
| Composer and inline editors | Page-local buffers and drafts | Never refetched, by design | A remote edit of a comment you are editing isn't reconciled. Low severity, out of scope (§12). |
| Rail status block: mergeable, merge state, checks summary, review decision, auto-merge, queue | Same query | Same, plus the UNKNOWN mergeable re-poll (3× at 4 s, #752) | CI progress, a base branch moving ("behind"), queue position, an approval by others: until Refresh. `mergeStateStatus: UNKNOWN` doesn't trigger the re-poll. |
| Rail metadata: labels, assignees, reviewers, milestone | Same query | Same; your own picker writes confirm on the store | Edits by others: until Refresh. Edits made on the page never reach the list rows (#873). |
| Commits tab | Same query: `commits(first:100)` | Same | New pushes: until Refresh or a head change seen by another landing. On PRs with more than 100 commits, new commits never show (#876). |
| Checks tab | Same query: `statusCheckRollup.contexts(first:100)`; gated runs over REST (docs/0066) on every detail landing; the expanded check's live log (docs/0076, 10 s while expanded) | Same; the log watch polls only the expanded job's steps | Run progress and new runs: until Refresh. The gated sweep ignores the REST pause (#875). |
| Files tab | REST `GET …/pulls/{n}/files` (`Kind::PrFiles`) | First entry to the tab, Refresh when loaded, refocus when stale, head change (#844) | Re-entering the tab never refetches. A push by others: until Refresh or a detail landing that sees the new head. |
| Review threads | `PrThreads` GraphQL | First Files-tab entry, Refresh when loaded, refocus when stale, thread writes, head change | Replies and resolves by others: until Refresh. |
| PR lists (home, repo) that open the page | `AccountPullRequests` and `RepoPulls` into the store's summary layer | View-shown edge, refocus when no sub-page is open, manual refresh | While a PR page is open the list is never refreshed, and closing the page doesn't refresh it. A merge seen on the page doesn't move the row out of Open (#873). Opening a PR from a notification removes its row (#874). |

**Headline.** One open PR page stays wrong for as long as it stays
open, unless the user acts. The defects make even the user's own
actions (refocus, a review submit, returning to the list) less healing
than the docs say.

## 2. History: why automatic refresh was turned off

| Doc | What it did |
|---|---|
| 0031 | `Fetch<T>` records per lane and in-flight dedup: the substrate everything since builds on |
| 0032 | Detail pages refreshed on a 60 s heartbeat plus a focus trigger (15 s floor) |
| 0034, 0037 | Staleness marking and one freshness contract in core (`wants_refresh(min_age)`, `FreshnessRow`) |
| 0035 | The same heartbeat for list surfaces |
| 0036 | Per-lane failure backoff. It rejected pausing while minimized: gpui can't report minimized state, and "visible but unfocused on a second screen" is a core use of the app |
| 0038 | An "engaged" tier: 10 s REST and 30 s GraphQL while the window was active and a detail page was open, with conditional REST where possible |
| 0051 | Removed the global snapshot query. It cost about 302 points a page and ran every 5 minutes, about 7,200 points an hour, which rate-limited the app against itself within roughly 40 minutes |
| **0052** | **No timer-driven content fetches at all.** It deleted the heartbeat, the engaged tier and the notifications cadence, and kept show, refocus and manual |
| 0072, 0073 | One store for PR facts with stale-while-revalidate reads; lanes became indices over it |
| 0080 | First carve-out: notifications poll every 60 s while the window is inactive |
| #752 / #844 | Second carve-out: UNKNOWN mergeable re-polls 3× at 4 s; a head change refetches the open Files and threads lanes |

**Why 0052 decided what it did** (from #325, #326 and the 0052 text):

1. **A blanket rule, not a measured problem with the PR page.** #325
   recorded "no autorefresh unless explicitly required for a module's
   functionality", with refocus as the replacement (#326). 0052 cites
   no PR-page budget overrun.
2. **Budget, indirectly.** The budget crisis was the global snapshot
   (0051), not detail pages. At 0038's engaged cadence a PR page cost
   about 120 detail reads an hour, roughly 120 to 360 points. That was
   affordable, but it was paid whether or not anything changed, because
   GraphQL has no conditional requests.
3. **Complexity.** The engaged tier needed `LaneClass`, `lane_min_age`,
   cadence tables and an engagement flag. 0052 D2 deleted all of it.
4. **Noise.** An open module page kept refetching while nobody looked
   (0051 D7, seen live). 0052 §6's acceptance became "idle app ⇒ zero
   content fetches".

There is no record of bugs or UX problems caused by the refreshes
themselves.

**Constraints that still hold** and that this design must answer:

- no blanket refetch-everything cadence (also D0 below);
- an idle, unattended app must not spend budget;
- focus alone can't tell "minimized" from "watched on a second screen"
  (0036 D1);
- every fetch carries a receipted `Origin` (Surface × Trigger, docs/0054
  and 0082);
- the per-budget rate-limit pause and backoff gate every trigger (0036,
  0050 D4, 0082 Q10);
- dedup and the outdated-read rule (0031, #739) stay authoritative.

## 3. What GitHub offers (verified 2026-11-14)

The measurements below use live read-only calls with an authenticated
user token against an open PR in a busy public repository, and GitHub's
current docs.

**F1 — conditional REST requests are free when nothing changed.**
GitHub's REST best-practices page says a `304` response "does not count
against your primary rate limit". The table shows the measured headers:
`x-ratelimit-remaining` and `x-ratelimit-used` were unchanged after
every 304, and every 200 cost exactly 1.

| Endpoint | ETag | 304 on `If-None-Match` | 304 costs quota | Notes |
|---|---|---|---|---|
| `GET /repos/{o}/{r}/pulls/{n}` | strong | yes (also `If-Modified-Since`) | no | `Last-Modified` equals the PR's `updated_at` |
| `GET …/pulls/{n}/commits` | weak | yes | no | |
| `GET …/issues/{n}/timeline` | weak | yes | no | |
| `GET …/issues/{n}/comments` | strong | yes | no | |
| `GET …/commits/{sha}/check-runs` | weak | yes | no | |
| `GET …/commits/{sha}/status` | weak | yes | no | combined legacy statuses |
| `GET …/commits/{sha}/check-suites` | weak | yes | no | |
| `GET /repos/{o}/{r}/events` | weak | yes | no | `X-Poll-Interval: 60` |
| `GET /notifications` | yes | yes on ETag; `If-Modified-Since` alone returned 200 once | no | `X-Poll-Interval: 60` |

Gotchas, all measured:

- The ETag depends on the `Accept` header (three media types gave three
  ETags), and `Vary: Authorization` makes it per token. A probe must
  send the same headers every time.
- A 304 echoes the ETag without the `W/` prefix, and the stripped form
  still matches.
- Responses carry `Cache-Control: private, max-age=60`. Whether a
  change can hide behind a 304 for up to 60 s was **not verified**, and
  stage 3 measures it (§9).

**F2 — GraphQL has no conditional requests, and one point is the
floor.** A GraphQL response has no ETag or `Last-Modified`. It is a POST,
and conditional requests don't apply to it. Measured with
`rateLimit(dryRun: true)` and real `rateLimit { cost }`:

| Query | Cost (points) |
|---|---|
| Minimal PR probe (updatedAt, head, state, mergeable, merge state, review decision, auto-merge, queue entry, comment/review/thread/label/assignee/request counts, rollup state and per-state check counts) | 1 |
| The same probe for 10 PRs, aliased in one request | 1 |
| The app's `PR_DETAIL_QUERY` | 1 |
| The app's `PR_THREADS_QUERY` | 1 |
| A deliberately heavy detail shape (timeline 100, reviews 100, threads 50 × comments 20, commits 100 × contexts 100) | 2 |

**Consequence:** for one PR, a GraphQL "probe, then refetch" saves no
points over just refetching the detail. A cheap probe has to be
conditional REST. GraphQL probes pay off only when batched across many
PRs, as list rows would be (§12).

**F3 — `updatedAt` doesn't move on CI.** On the live PR, the newest
check run completed after the PR's `updated_at`. The PR resource (and
its ETag) therefore can't detect CI progress, and the head commit's
check runs and statuses need their own probe. Whether a push, a label
change or a submitted review moves `updated_at` was **not observed
live**. Community knowledge says they do. Stage 3's receipts confirm it
against the fixture-emulated contract and one live check, and the lane
map (D4) is written so a missed bump only delays the update by one
check-runs change or a refocus.

**F4 — the events API is too slow.** The repo, network and org event
endpoints document a latency of "30s to 6h". They are polling-friendly
(ETag, `X-Poll-Interval: 60`) but unusable for a page that someone is
watching.

**F5 — notifications don't cover what goes stale.** The notifications
API is built for `Last-Modified` polling, and docs/0080 already polls it
while the window is unfocused. Its reasons cover mentions, review
requests, comments and assignments. The only CI-related reason,
`ci_activity`, means a workflow run that you triggered finished.
Pushes, others' CI and label edits produce no notification.
Notifications are a useful extra trigger (D2) but not a freshness
source.

**F6 — no push channel for a desktop client.**

- GraphQL has no subscriptions.
- Webhooks need a publicly reachable receiver and, for repository
  hooks, admin rights. GitHub's own testing guide points local
  receivers at relays such as smee.io, which it calls development-only
  and unauthenticated.
- github.com's live-update sockets are undocumented and unsupported.

This confirms docs/0038 §1.1. Push is rejected (§12).

**F7 — secondary limits.** From GitHub's REST rate-limit page:

- at most 100 concurrent requests;
- 900 points a minute on REST and 2,000 on GraphQL, where a GET costs 1
  point and a mutation 5;
- 90 s of CPU per 60 s of real time, at most 60 s of it GraphQL;
- content creation about 80 a minute and 500 an hour.

GitHub asks clients to make requests serially, to honour
`x-poll-interval`, and after a secondary-limit response to wait at least
a minute, then back off exponentially. Secondary limits "are subject to
change without notice".

**F8 — primary limits.** 5,000 REST requests and 5,000 GraphQL points
an hour per user, confirmed by `GET /rate_limit`. They are separate
budgets (docs/0050, 0082 Q10).

**F9 — the app already has the parts.**

- An ETag transport (docs/0005 §3.3). The Files lane already sends
  `If-None-Match` once it holds data.
- The usage recorder on every flight (docs/0050, 0057).
- Per-budget pause (`gated_for`) and backoff.
- The window-active flag (`GithubState.window_active`, docs/0080 D2).
- The head-change fan-out (`pr_page/head_change.rs`).
- The outdated-read rerun (#739).

## 4. Defects found (filed, fixed independently of this design)

These cause staleness on their own. Fixing them is stage 0 and needs
no product decision.

| Issue | Defect | Effect |
|---|---|---|
| #871 | Refocus calls `fetch_detail` with `Trigger::Refocus`, which isn't "forced", so the 5-minute reopen TTL (`detail_fresh`) skips it. A voided `detail_at` is never "stale" to `standard_needs_refresh` | Refocus, the only documented automatic refresh, does nothing for 5 min after any landing, and never heals a voided document |
| #872 | A submitted review batch refetches threads only | Review cards, the review decision and counts stay stale |
| #873 | `land_detail` copies only the rollup into the summary; a late list landing can undo a confirmed state (suspected) | A PR merged by others stays in the Open list; page label edits never reach rows |
| #874 | A notification-subject open overwrites the landed summary with a blank synthesized row | The PR drops out of the lists, and a needless drift read fires |
| #875 | The gated-runs sweep ignores the REST pause and is booked as view-shown | Spends a paused budget; Usage receipts misattribute it |
| #876 | The detail selects `commits(first:100)`, the oldest 100 | New pushes never show on long PRs |

Two smaller findings aren't filed: the header age can read newer than
the rendered detail, and the mergeable re-poll stops quietly when a
poll fails. Both shrink or disappear once D5's age label and the probe
loop land.

## 5. Options considered

The costs are for one open PR page, per hour. "Quiet" means nothing
changes on GitHub. "Busy" means a 20-minute CI run with about 40
check-run transitions, plus 5 comments or reviews and 1 push.

| Option | Quiet cost | Busy cost | Accuracy | Complexity, risk |
|---|---|---|---|---|
| A. Defect fixes and event triggers only | 0 | 0 beyond what the user does | Correct after refocus, tab entry and writes; stale while someone just watches | Low. Not enough: watching CI is the main case |
| B. Timer refetch of every open lane every 30 s (0038-like) | 120 GraphQL points (detail), plus 120 threads points and Files REST pages if open | the same | Good, within 30 s | Low code, but paid when nothing changes. Ruled out by D0 |
| C. GraphQL probe, refetch on change | 120 points | 120 points plus refetches | Good | Saves nothing (F2). Rejected, except where REST can't see a fact (D6) |
| **D. Conditional REST probe, lane refetch on change (recommended)** | **0 quota** (about 360 304s, F1) | about 50 counted REST requests and 50 GraphQL points | Good: within one probe interval for PR facts and CI | Medium: ETag bookkeeping, a lane map, a cadence function |
| E. Events API | 0 (304s) | low | Poor: 30 s to 6 h latency (F4) | Rejected |
| F. Notifications-driven refresh | 0 (already polled) | 0 | Misses pushes, others' CI, labels (F5) | Kept as an extra trigger only (D2) |
| G. Webhooks or push | n/a | n/a | n/a | Not possible for a user-token desktop client (F6) |

## 6. Decisions

### D0 — Settled 2026-11-14: automatic refresh is allowed again, when it is targeted

Automatic refresh is allowed again, and docs/0052 D1's "nothing fetches
on a timer" no longer holds for the PR page. It must be intelligent,
not a blanket "refetch everything every N seconds":

- detect a change before refetching;
- refetch only the lanes that changed;
- scale the cadence with focus, visibility and activity;
- stay inside an explicit budget.

The decisions below are this rule applied to the PR page. §11 records
the amendment to docs/0052 explicitly.

### D1 — Defects first

Stage 0 fixes #871–#876 before any new mechanism. #871 matters most.
Once it is fixed, refocus works as docs/0052 D4 described: one
alt-tab refreshes the open page, with a 15 s floor. The reopen TTL
(docs/0072 D9) goes back to its stated job, deciding only whether a
*reopen* revalidates in the background. A trigger that already passed
its own age floor is never skipped by it.

### D2 — Event-driven refreshes, always on

These triggers are cheap, only fire when the user or GitHub does
something, and stay on whatever D5 decides. Each goes through the
existing gates: age floor, backoff, per-budget pause and dedup.

| Event | What refetches | Floor |
|---|---|---|
| Window refocus (fixed by #871) | The visible PR page's lanes: detail, plus the open tab's own lane if loaded | 15 s (unchanged) |
| Tab entry | That tab's lane if it is loaded and older than the floor: Files and threads; for Conversation, Commits and Checks the detail | 30 s (Q7) |
| Module re-shown with the PR page open | Same as refocus | 15 s |
| A write is confirmed | The lanes the write can change (table below) | none (forced, as today) |
| A notification landing names the open PR | One immediate probe (D3); notifications already land while unfocused (docs/0080) | none |
| Return from the PR page to a list | That list's lane, if older than the floor (closes the list gap in §1.1) | 60 s |

The write-to-lane map replaces today's per-write choices:

| Write | Refetch |
|---|---|
| Comment, reply, reaction, edit or delete on the timeline | detail |
| Review batch, line comment, thread resolve or unresolve | threads and detail (#872) |
| Metadata pickers | detail (store confirmation, as today) |
| Merge, close, reopen, draft flips, auto-merge, queue, delete branch, update branch | detail (store confirmation, as today); update branch also expects a head change |

### D3 — The change probe

While the page is *watched* (D5), the page runs a **probe**: serial
conditional REST GETs, each with the ETag from its last 200.

1. `GET /repos/{o}/{r}/pulls/{n}`: state, merged, draft, title, head
   SHA, labels, requested reviewers, comment and review-comment counts,
   mergeable and `mergeable_state`, auto-merge, `updated_at`.
2. `GET /repos/{o}/{r}/commits/{head}/check-runs?per_page=100`: CI
   progress (F3).
3. `GET /repos/{o}/{r}/commits/{head}/status`, only when the last detail
   showed legacy status contexts.

Rules:

- **ETags live on the page**, keyed by URL, beside the lane records.
  They are never persisted: a reopen starts with a fresh 200. Every
  request sends the same `Accept` header (F1).
- **A 304 is a confirmed "unchanged"**: it stamps the probe's
  `checked_at` and nothing else. No re-parse, no re-render, no lane
  stamp. Lanes keep their own landed age.
- **A 200 is parsed minimally** and compared with the previous 200's
  fields to pick lanes (D4). The first 200 after the page opens only
  seeds the comparison and refetches nothing, because the page just
  landed its data.
- **A new head SHA** moves probe 2 to the new commit (its first request
  is an unconditional 200) and goes through the existing head-change
  fan-out.
- The probe runs **serially** and never overlaps itself or the lanes'
  own reads. While any lane of the page is in flight the tick is skipped
  (F7).
- **Receipts:** probes are `Origin::new(Surface::PrPage,
  Trigger::Watch)`. Refetches a probe caused are `Trigger::Changed`. Both
  are new variants, so the Usage page shows watch cost apart from user
  actions (docs/0054, 0082).

### D4 — Lane map: refetch only what changed

| Probe result | Lanes refetched |
|---|---|
| PR 200, head SHA changed | detail; the head fan-out then refetches Files and threads if loaded (unchanged #844 path) and moves the check-runs probe |
| PR 200, review-comment count changed | detail, and threads if loaded |
| PR 200, anything else (state, labels, reviewers, counts, mergeability, `updated_at` only) | detail |
| Check runs 200 or status 200 | detail (checks live in it) |
| All 304 | none |

- **One coalesced read per probe:** a tick refetches the detail at most
  once, whatever combination changed.
- **Checks-only changes** refetch the detail at most once every 20 s
  (Q4), so a CI matrix finishing job by job doesn't become a stream of
  detail reads. Their next refetch is scheduled, not dropped.
- **The gated-runs sweep** (docs/0066) runs only on page open, Refresh
  and head change, and when the detail's check counts show a newly
  waiting run. It no longer runs on every detail landing. Today every
  re-poll also re-sweeps.

### D5 — Cadence: watched pages only, scaled by focus and recent change

A page is **watched** when all of these hold:

- the PR page is the open sub-page;
- the GitHub module owns the screen (the existing nav gate);
- the session is connected and the REST budget isn't paused.

Nothing outside a watched PR page probes, so no PR is probed in the
background. The probe interval is a pure function of window focus, the
PR's state and how recently something changed:

| Tier | Condition | Probe interval |
|---|---|---|
| Hot | Window active, and a check is running or queued, `mergeable` or `mergeStateStatus` is UNKNOWN, the PR is in a merge queue, or a change landed in the last 2 minutes | 10 s |
| Active | Window active, otherwise | 30 s |
| Shown | Window inactive (the second-screen case, docs/0036 D1) | 60 s, or 30 s while a Hot condition other than focus holds |
| Settled | No change seen for 30 minutes | the tier's interval doubles every 30 quiet minutes, up to 5 min; any change or refocus resets it |
| Unattended | Window inactive for more than 60 minutes | no probe; refocus resumes, and refocus already refetches (D2) |
| Closed PR | State MERGED or CLOSED | no probe; events and Refresh only |

- Closed and merged PRs don't probe because they rarely change. Their
  next event, such as a reopen or a comment, comes back through
  refocus or a notification.
- A minimized window can't be detected (docs/0036 D1), so it runs the
  Shown tier. While nothing changes that costs no quota (F1), only a
  304 a minute, and after an hour it reaches Unattended.
- **One timer per page**, armed after each probe with the next
  interval. It is not a global heartbeat and not a cadence table per
  lane. The page's timer is dropped with the page. If GitHub sends
  `X-Poll-Interval`, it is the floor.
- **On failure:** the probe uses the lanes' backoff (docs/0036 D2). A
  secondary-limit response (403 or 429 with a secondary message, or
  `Retry-After`) waits at least 60 s, then backs off exponentially (F7,
  #751).

### D6 — Facts REST can't see

- **Merge queue position** (`mergeQueueEntry.position`) has no REST
  field. While the PR is queued, every Hot probe whose REST requests all
  came back 304 is followed by one detail refetch, at most every 30 s.
  That is 1 GraphQL point, the same price as any GraphQL probe (F2).
  This amends docs/0083's "no queue polling".
- **The review decision and new reviews** are expected to move
  `updated_at` and the PR ETag (F3, verified in stage 3). If stage 3
  shows they don't, the fallback is to add
  `GET …/pulls/{n}/reviews?per_page=1` to the probe. That is a third
  conditional request and still free when unchanged, though a
  one-per-page ETag only notices new reviews if GitHub's ordering puts
  them on page 1. Stage 3 checks that too.

### D7 — Budget guard

Watch traffic, the `Watch` and `Changed` triggers, has its own ceiling
per open page, on top of the existing pause:

- **Ceiling per rolling hour:** 600 counted REST requests (200s, not
  304s) and 300 GraphQL points (Q1). Reaching it drops the page to the
  Shown interval (60 s). Reaching it twice in an hour stops the probe
  until refocus or Refresh.
- **Usage calculator thresholds** (docs/0082): at **Low** (≤ 25% left)
  of either budget every interval is multiplied by 4. At **Critical**
  (≤ 10%) the probe stops and only events and Refresh refetch. At
  **Exhausted** (paused) nothing on that budget fetches (unchanged).
  #721 and #751 are fixed first or alongside, so Manual, write and
  sweep paths respect the pause too.
- The guard reads the usage book's numbers. It counts nothing of its
  own beyond the page's watch counter.

### D8 — Showing data age honestly

- The header's labelled "updated" age (#847) becomes **"checked N s
  ago"** while the page is watched. It shows the time of the last probe
  that confirmed the data, which is honest even when nothing changed.
- When the probe isn't running, the label keeps today's landed age.
  The page isn't watched when the guard stopped it, the PR is
  closed, or the app is unattended or offline. In the guard-stopped
  case the label adds a reason, such as "auto-refresh paused (budget)".
- Refresh stays, for an immediate full refetch.
- Lanes the probe can't see, such as review thread bodies past the
  Files tab's lazy load, show their own landed age where they have one
  today. No new chips.
- Data that refreshes on its own must not move what the user is doing:
  - the timeline keeps its scroll anchor;
  - text fields, composers, pickers and confirm dialogs are never
    rebuilt;
  - the Files tab keeps its scroll restore (#846).

  Today `apply_detail_result` drops the editor of a node that vanished
  from a landing. Under automatic refresh that would discard typing
  without a click, so the rule changes: a refetch that would remove a
  node the user is editing keeps the editor open with a "deleted on
  GitHub" notice until the user closes it.

### D9 — Unchanged

The following keep their behaviour:

- the notifications unfocused poll (docs/0080);
- the UNKNOWN mergeable re-poll, which the Hot tier absorbs once it
  ships (stage 4 deletes `mergeable_poll.rs`);
- the check-log live watch (docs/0076);
- dedup and the outdated-read rerun;
- skeleton-first loading and the store's stale-while-revalidate reopen.

## 7. Cost estimate

For one watched PR page, per hour:

| Scenario | REST counted (200s) | REST 304s (free) | GraphQL points |
|---|---|---|---|
| Quiet, window active (Active tier) | ~0 | 240 | 0 |
| Quiet, window inactive (Shown tier) | ~0 | 120 | 0 |
| Quiet for an hour, then Settled | ~0 | ≤ 60, falling | 0 |
| Busy: 20 min of CI, 5 comments or reviews, 1 push, Files open | ~50 (40 check-runs 200s, 6 PR 200s, Files pages, sweeps) | ~250 | ~50 (≤ 40 coalesced detail reads, threads on push and review comments) |
| In a merge queue for 30 min | ~5 | ~180 | ≤ 60 |
| Guard ceiling (D7) | 600 | unbounded but free | 300 |

- Idle app, or no PR page open: **zero** PR traffic. Unchanged from
  docs/0080: only the unfocused notifications poll, at most 60 an hour.
- The busy case is about 1% of each hourly budget. Ten PR pages opened
  one after another over an hour are still under 10%, since only one PR
  page is open at a time (docs/0020 D1).

## 8. Fixture mode: simulating a change on GitHub's side

Today the fixture mode (docs/0081 §12.1) re-reads files on every
request. A JSON array file is a sequence, served element n on the n-th
request with the last one repeating. Writes are static, and there are no
ETags. That can't express "unchanged, then changed at a time the
receipt chooses", and it can't exercise 304s. Proposed:

- **Generations.**
  - `FixtureHttp` holds a shared generation counter. Lookup prefers
    `<name>.g<N>.json` for the highest N at or below the current
    generation and falls back to today's names, so existing fixtures
    keep working.
  - A new debug-bridge verb, `fixture bump [N]` (debug builds only),
    advances it.
  - A receipt can open a PR, assert the current state, bump, and
    `wait-label --timeout` for the new comment, check or state. That
    proves the probe found and landed the change with no click.
- **ETags and 304s.**
  - For REST fixtures the ETag is a hash of the served file's bytes.
  - A matching `If-None-Match` answers 304 with an empty body and
    unchanged rate-limit headers.
  - Synthetic `x-ratelimit-*` headers let D7 be exercised; a
    `fixture budget <remaining>` verb sets them.
  - GraphQL fixtures never answer 304 (F2).
- **Counting.** The bridge's existing request log gives the receipt
  its proof of "nothing was fetched while nothing changed": over 60 s
  idle only 304s may appear, and after the bump, exactly the lanes in
  D4.
- The synthetic fixtures (#101–#114) gain a generation 1 for one PR
  with:
  - a new comment;
  - a check run going from in progress to success;
  - a label change;
  - a new head SHA with a changed file list.

## 9. Staged implementation (focused PRs, no code in this PR)

Each stage is one PR, or a small stack where noted, with the gates from
AGENTS.md. UI-facing stages carry the docs/0065 receipts from the
docs/0071 container.

| Stage | Content | Tests and receipts |
|---|---|---|
| 0 | The defects #871–#876, one PR each, any order. #721 and #751 (pause on Manual, write and detail paths) land before stage 4 | Unit tests per issue; #871 adds "refocus at 20 s refetches" and "a voided stamp refetches"; a fixture receipt for #874 (notification open keeps the list row) |
| 1 | Fixture generations, REST ETag/304 emulation, synthetic rate-limit headers, `fixture bump` and `fixture budget` verbs (§8). Tooling only | Unit tests in `fixture.rs`; a receipt that bumps a generation and sees a 200 after 304s |
| 2 | D2 event triggers: the tab-entry floor, the write-to-lane map, list refresh on return, notification-names-open-PR (which arrives as a manual-equivalent refetch until stage 3 lands the probe) | Unit tests with the fake clock; receipt `pr-freshness-events.txt`: switch tabs past the floor and see one refetch, return to the list and see the row updated |
| 3 | D3/D4 probe and lane map, with the hot, active and shown tiers at fixed intervals; the `Watch` and `Changed` triggers; one live measurement on public data of the points §3 leaves unverified (the 60 s cache, whether a review, label or push moves `updated_at`, whether the PR ETag follows mergeable), recorded in this doc's §3 | Receipt `pr-freshness-probe.txt`: open #101, 60 s idle with only 304s, `fixture bump`, `wait-label` for the new comment, check and label within one interval; the Usage page lists the `watch` and `changed` rows |
| 4 | D5 Settled and Unattended tiers, D6 queue and review fallback, D7 guard, removal of `mergeable_poll.rs` | Unit tests over the cadence function (a table test, like 0038's); receipt with `fixture budget` at Low and Critical shows the stretched and stopped probe |
| 5 | D8 "checked N s ago" label and paused reason; anchored updates | 0065 audit and critique on the PR header, before and after; `assert-baseline` for `github-pr-*` goldens; tab stops unchanged |

Stages 3 and 4 can ship as a stack. Stage 2 can land before or after
stage 1, but its receipt needs stage 1.

## 10. Verification and acceptance

- Watched PR, window active, nothing changing for 10 min: no counted
  REST requests and no GraphQL points from the PR page. The request log
  shows only 304s.
- A comment, push, check-run completion, label edit or merge by
  others is visible within one probe interval: ≤ 30 s Active, ≤ 10 s Hot,
  ≤ 60 s Shown. No click.
- Alt-tab back after 20 s refetches (the #871 fix).
- No PR page open, or the window unattended: zero PR traffic.
- At Critical the probe stops, and the label says why.
- Gates: fmt, clippy with zero warnings, workspace tests.

## 11. Amendments to existing docs (ride with the stage that changes the behaviour)

| Doc | Decision | Change | Stage |
|---|---|---|---|
| 0052 | D1 "no timer-driven content fetches" | Amended by D0: automatic refresh is allowed when targeted (change detection first, lane-level refetch, focus/visibility/activity scaling, explicit budget). The PR page's watch loop (this doc's D3–D7) is the first user; other surfaces need their own design under the same rule | 3 |
| 0052 | D2 "the engaged tier is deleted" | The cadence tables stay deleted. The watch tiers here are one function on one page, not a per-lane cadence system | 3 |
| 0052 | D4 refocus | Behaviour restored as written (#871); the reopen TTL never skips a refocus | 0 |
| 0052 | §6 acceptance "idle app ⇒ zero content fetches" | Becomes "zero counted requests while nothing changes": watched PR pages send only conditional requests that come back 304, and unattended apps send nothing | 3 |
| 0080 | D1 "exactly one exception" | No longer the only timer: it stays the only lane polled while unfocused *outside* a watched PR page | 3 |
| 0072 | D9 "SWR staleness for a detail: 5 minutes" | The TTL decides reopen revalidation only; triggers with their own floor bypass it | 0 |
| 0072 | D3 | The catch-up (`Trigger::Drift`) stays; a watched page usually beats it | 3 |
| 0050 | D1 "A 304 counts (it consumes budget upstream)" | Corrected: a correctly authorized 304 doesn't consume primary budget (F1, measured). The book still counts it as a request in session totals | 3 |
| 0016 | Detail-query clause "fetched once on open, on header refresh, and after every mutation"; §15 | Adds probe-driven refetches (D3/D4) and event triggers (D2); §15's UNKNOWN re-poll is absorbed by the Hot tier | 2–4 |
| 0083 | Queue: "no queue polling" | Queued PRs refetch the detail at most every 30 s while Hot (D6) | 4 |
| 0066 | Gated-runs sweep on every detail landing | Only on open, Refresh, head change and newly waiting runs (D4) | 3 |
| 0038 | Superseded note | Gains a line saying its conditional-polling idea returns, scoped, in this doc | 3 |
| 0081 | §12.1 fixture mode | Generations, ETags and the bump and budget verbs (§8) | 1 |

## 12. Non-goals

| Non-goal | Why | Issue |
|---|---|---|
| Push channels: webhooks, relays, github.com sockets | Not possible or not supported for a user-token desktop client (F6) | none (rejected, not deferred) |
| Background watching of PRs that aren't open, such as "tell me when CI on my PR finishes" | A separate feature with its own budget and notification design | #877 |
| Automatic refresh of the PR lists (home, repo), for example a batched 1-point GraphQL probe of visible rows | This doc covers the PR page; lists get event refresh only (D2) | #878 |
| The same watch loop on the issue page and the notification thread page | Same pattern, separate rollout once the PR page proves it | #879 |
| Reconciling a remote edit of a comment you are editing locally | Rare; needs a conflict UX | #880 |
| Persisting probe ETags across restarts | A reopen paying one 200 is cheap | none (no plan to do it) |

## 13. Settled decisions (2026-11-14)

These were proposed as open product questions and settled on
2026-11-14 as recommended. The Q numbers stay, because the decisions
above cite them.

| # | Question | Decision |
|---|---|---|
| Q1 | The watch budget per rolling hour, per page | 600 counted REST requests and 300 GraphQL points (about 12% and 6% of the hourly budgets). The busy case uses about 50 of each, so the ceiling only catches runaway loops |
| Q2 | Idle behaviour: what happens when nobody seems to be there | Probing continues while the window is inactive, at 60 s (second-screen use, docs/0036 D1). It slows down after 30 quiet minutes and stops after 60 minutes unfocused. No input-recency tracking |
| Q3 | Does a visible age label stay? | Yes: "checked N s ago" while watched, with the paused reason when the guard stopped it |
| Q4 | How aggressive CI-progress updates are | Probe every 10 s while a check runs (free when unchanged); refetch the detail at most every 20 s for checks-only changes |
| Q5 | Do closed and merged PRs probe? | No. Refocus, notifications and Refresh cover their rare changes |
| Q6 | Lists | Event refresh on return from a PR page now (D2); list polling deferred (#878) |
| Q7 | Tab-entry floor | 30 s |
| Q8 | Do changes that arrive by themselves get a visual cue (a "new comments" marker, a brief highlight)? | No visual cue: they land silently with the scroll anchored (D8) |
