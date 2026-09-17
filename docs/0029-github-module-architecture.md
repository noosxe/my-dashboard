# 0029 — GitHub Module Architectural Overview

> Can this app replace day-to-day GitHub usage?

**Short answer:** For issue triage, notification management, and light PR
review — yes, today. For heavy code review, PR authoring, and fast-paced
collaborative workflows — not yet. Below is a map of what's solid, what's
missing, and what to prioritize.

---

## 1  What works well

These areas are production-grade and genuinely better than the GitHub web
UI for a focused workflow:

| Area | Details |
|------|---------|
| **Notification triage** | 60-second polling, auto-mark-as-read on open, mark-as-done, hand-off to native PR/issue pages, jump-to-latest-comment with scroll + highlight. This loop is tight and fast. |
| **Issue management** | Full read/write: comment, edit, close/reopen, labels, assignees, milestones, reactions, locking, create new. Timeline events (assigned, labeled, milestoned, etc.) render inline. |
| **PR overview & merge** | Conversation tab, CI check rollup, merge conflict status, all three merge methods, close/reopen. Approve and Request-changes reviews work. |
| **Offline bootstrap** | Cold start paints immediately from the on-disk cache, then syncs in the background. The app never shows a blank screen. |
| **Optimistic UI** | Mutations (mark-read, reactions, close/reopen) update instantly and revert on failure — the app feels responsive even on slow networks. |
| **Error isolation** | Auth/rate-limit errors are global; mutation errors are scoped to the specific UI surface. A failed label edit doesn't break the page. |
| **Token lifecycle** | 8-hour token refresh is fully automated with immediate 401→retry queue (0028). No manual re-auth unless the refresh token itself expires. |

---

## 2  Data freshness — the polling ceiling

The app has **no push channel** (no webhooks, no SSE, no WebSocket).
Everything is polled:

| Data type | Refresh cadence | Mechanism |
|-----------|----------------|-----------|
| Notifications | **60 seconds** | REST poll, ETag on page 1 only |
| Global snapshot (repos, PRs, issues) | **5 minutes** (`SNAPSHOT_EVERY = 5` × 60 s tick) | GraphQL, no conditional request |
| Repo-scoped detail | **On-demand** (when user opens repo view) | One-shot GraphQL + REST |
| PR / Issue detail page | **On-demand** (page open only, no background refresh) | One-shot GraphQL |

### What this means in practice

- A teammate merges your PR → the PR list still shows it as open for
  **up to 5 minutes**.
- CI finishes on a PR you're viewing → the checks tab **won't update**
  until you navigate away and back.
- Someone comments on an issue you have open → you **won't see it**
  without re-opening the page.

> Detail pages (PR, Issue, Notification) fetch once on open and never
> refresh. In a fast-moving review conversation, the user will fall
> behind unless they manually re-navigate.

### Pagination caps

| Resource | Cap | Impact |
|----------|-----|--------|
| Repositories | 100 per snapshot page, 3 pages max (`SNAPSHOT_MAX_PAGES`) | Users with > 300 repos see slow cursor-based catch-up across multiple 5-min cycles |
| PRs / Issues per repo | 100 each | Repos with > 100 open PRs/issues silently truncate |
| Notifications | 1 000 (`MAX_PAGES = 10` × 100) | Adequate for most users |
| Orgs | 100 | Users in > 100 orgs lose some |

---

## 3  Feature gaps — what sends users to the browser

### Code review (the biggest gap)

The app **cannot** do inline code review — the primary reason most
developers open GitHub:

| Missing | Impact |
|---------|--------|
| **Review threads / line comments** | Cannot comment on specific lines of a diff. `reviewThreads` is not queried in the GraphQL detail query. |
| **Suggested changes** | Cannot propose code edits inline. |
| **Draft reviews** | Cannot batch comments before submitting. |
| **Split diff view** | Only unified diff. |
| **Syntax highlighting** | ~~Diffs are plain text.~~ **Closed by docs/0040 (#202)**: diff rows highlight via a scope→theme-token mapping (syntect tokenizer, no external themes). |
| **Check-run logs** | ~~Can see pass/fail, cannot view the actual CI output.~~ **Closed by docs/0039 (#201)**: completed Actions checks expand to an inline, tail-truncated log pane. |

> Without line-level review, the app cannot serve as a code-review tool.
> Users doing non-trivial reviews will always need the browser.

### PR write operations

| Missing | Impact |
|---------|--------|
| **Create PR** | Must use browser or CLI. |
| **PR metadata editing** (labels, assignees, milestones, reviewers) | Issue page has these pickers; PR page does not. |
| **PR reactions** | Issue comments have reactions; PR comments do not yet. *(→ shipped in #196, docs/0023 §10)* |

### Navigation & discovery

| Missing | Impact |
|---------|--------|
| **No filter / sort on lists** | "Open PRs", "Open Issues", and "Notifications" lists become unusable above ~20 items. *(→ addressed by docs/0042)* |
| **No cross-reference linking** | `#123` and `@username` in markdown body text are not auto-linked (only bare URLs via `linkify`). |
| **No global search** | Cannot search across repos for PRs/issues. |
| **Label / assignee picker caps at 50** | Large repos with many labels require scrolling without type-ahead. |
| **Home card doesn't route to notification page** | The card shows counts but clicking doesn't navigate. |

### Issue creation gaps

| Missing | Impact |
|---------|--------|
| **No issue templates** | `.github/ISSUE_TEMPLATE` is ignored. |
| **No metadata at creation time** | Labels/assignees/milestones only settable after creation. |
| **No draft persistence** | Closing the app loses unsaved drafts. |

---

## 4  Data accuracy & staleness risks

### Cache has no TTL

The on-disk cache (`last-known` JSON blobs) persists indefinitely and is
only cleared on sign-out. On a cold start after prolonged offline or
network issues, the dashboard confidently displays stale data (e.g. PRs
merged hours ago shown as open) for up to 5 minutes until the first
snapshot sync succeeds — with no visual indicator.

### No staleness indicators

The UI has no concept of "last synced at" or "data may be stale." The
user sees the same confident rendering whether the data is 5 seconds or
5 hours old.

### Detail pages are snapshot-in-time

PR and Issue detail pages fetch once and never update. In a
collaborative workflow:

- New comments won't appear.
- Status changes (merged, closed) won't reflect.
- CI status won't update.
- Label/assignee changes by others won't show.

The user has no way to know the page is stale without manually
navigating away and back.

---

## 5  Improvement roadmap

Organized by impact on the "replace GitHub" goal.

### Tier 1 — Stop forcing browser fallbacks

| Improvement | Effort | Why it matters |
|-------------|--------|----------------|
| **Inline code review (review threads + line comments)** | Large | The #1 reason developers open GitHub. Without it, the app is a notification center, not a review tool. |
| **Auto-refresh detail pages** | Medium | A simple periodic re-fetch (or re-fetch on window focus) would keep PR/Issue pages current during active work. |
| **PR metadata editing** (labels, assignees, reviewers) | Medium | Already built for issues (0022); porting to PRs is mostly wiring. |
| **Filter and sort for lists** | Medium | Notification/PR/issue lists become unusable above ~20 items without filtering. |
| **Create PR** | Medium | Common workflow that currently requires leaving the app. |

### Tier 2 — Build trust in the data

| Improvement | Effort | Why it matters |
|-------------|--------|----------------|
| **"Last synced" indicator** | Small | A subtle timestamp or dot showing sync recency lets users gauge freshness at a glance. |
| **Cache TTL / staleness marking** | Small | Flag data older than N minutes with a visual indicator; refuse to paint cache older than a threshold without a disclaimer. |
| **Refresh-on-focus** | Small | Re-fetch when the app window regains focus — catches up after being backgrounded. |
| **Cross-reference autolinks** (`#123`, `@user`) | Small | Makes markdown bodies feel complete rather than missing links GitHub would render. |
| **PR reactions parity** | Small | Already built for issues; extending to PRs is mechanical. *(→ shipped in #196)* |

### Tier 3 — Polish the daily-driver experience

| Improvement | Effort | Why it matters |
|-------------|--------|----------------|
| **Type-ahead in pickers** | Small | Label/assignee pickers currently cap at 50 items with no search. |
| **Issue templates** | Medium | Repos with structured templates expect them in the creation flow. |
| **Draft persistence** | Medium | Losing a half-written issue on app close is painful. |
| **Home card → notification page routing** | Small | The card shows counts but doesn't navigate. |
| **Check-run log viewing** | Large | ~~Seeing CI output without leaving the app.~~ **Shipped — docs/0039 (#201).** |
| **Syntax-highlighted diffs** | Medium | ~~Makes the Files tab usable for reading code.~~ **Designed — docs/0040 (#202).** |
| **Split diff view** | Medium | Preference-dependent but expected by many reviewers. |

---

## Summary

The app is a **strong notification center and issue tracker** today. The
path to a true GitHub replacement runs through **inline code review**
(the single biggest gap) and **data freshness trust** (auto-refresh +
staleness indicators). The architectural bones — optimistic UI, error
isolation, token management, offline bootstrap — are solid. The gaps are
features, not foundations.
