# 0026 — GitHub Notification Detail Page (in-app thread view, auto-seen, read state)

Part of the GitHub module's page family (0016/0020/0024): clicking a
notification row in the repo view opens an in-app notification page instead
of handing off to the browser. The page shows the notification's content
(subject, kind, reason, freshness, read state), **automatically marks the
notification as seen when opened**, and exposes the read state — within
the upstream constraint that GitHub's API has no "mark as unread"
(§2.3), resolved by owner decision in §4: do what GitHub itself does.

## 1. Summary

- Notification rows in the repo view (the `Notifications (N)` group box,
  docs/0012 D4 + 0015) open the **notification detail page** — the
  PR/issue-page takeover pattern (full-content sub-page, repo crumb gains a
  suffix, Esc closes, focus moves). The row's browser hand-off (0015 D2)
  moves into the page header's ↗ button, like the PR/issue rows before it.
- The page renders from the row it was opened with (title, kind, age, unread)
  and upgrades itself with a `GET /notifications/threads/{id}` fetch
  (reason, subject URLs, last-read timestamp).
- On open the page fires `PATCH /notifications/threads/{id}` — GitHub's
  "mark as read", their docs describe it as "equivalent to clicking a
  notification in your inbox". Optimistic local state; failure degrades
  in place (per-file stderr rule, no dialogs).
- After the mark settles, the module refetches the repo notification list
  (refetch-after-mutation): the row leaves the unread list, exactly like
  GitHub's inbox behaves.

## 2. Upstream facts (live introspection 2026-09-07)

Sources: `@octokit/openapi-types@23.0.1` `components.schemas.thread` +
`operations["activity/*"]` (generated from `github/rest-api-description`),
GitHub REST docs (Notifications, apiVersion 2022-11-28).

1. **Thread shape** — list items and the single-thread GET return the same
   `thread` schema:
   `id`, `repository` (minimal-repository), `subject {title, url,
   latest_comment_url, type}`, `reason` (string), `unread` (bool),
   `updated_at`, `last_read_at` (string | null), `url`, `subscription_url`.
2. **The list payload carries `subject.url` and `latest_comment_url`.**
   docs/0015 D2's claim "the notifications REST payload carries no subject
   URL" is wrong — the app's minimal `api::Subject` struct (title + kind
   only) simply drops them. Correction recorded here; 0015's design
   (repo-level browser target) stands as history, and this doc supersedes
   the row behavior.
3. **Per-thread write surface is exactly three things**: mark as read
   (`PATCH` → **205 Reset Content**), mark as done (`DELETE` → 204,
   removes the notification from the inbox), subscribe/unsubscribe
   (`PUT`/`DELETE .../subscription`). **There is no mark-as-unread
   endpoint** — not per-thread, and the global sweeps
   (`PUT /notifications`, `PUT /repos/{o}/{r}/notifications`) are
   all-at-once operations. GitHub's own inbox has no unread toggle either.
4. `GET /notifications/threads/{thread_id}` → 200 (`thread`) / 304 / 401 /
   403. Thread id = the `id` string the list already carries (the app keys
   rows by it).
5. `subject.type` values include at least `PullRequest`, `Issue`,
   `Discussion`, `Release`, `Commit`, `CheckSuite`, `WorkflowRun`,
   `RepositoryInvitation`, `SecurityAlert` — the page MUST NOT assume the
   subject maps to an in-app page or a web URL.
6. `subject.url` is an **API** URL (`https://api.github.com/repos/{o}/{r}/
   issues/{n}` etc.). Web URLs are derivable per kind (§3 D5); some kinds
   (Release by id, CheckSuite) have no faithful web equivalent.
7. Notifications remain REST-only upstream — no GraphQL API (community
   discussion #13056, as recorded in docs/0005 §3.3 and `api.rs`).

## 3. Design

### D1 — Row click opens the notification page (takeover pattern)

`open_notification(n: api::Notification)` in `page.rs`, the
`open_pr`/`open_issue` clone: builds `NotificationDetailPage::new(state,
n, window, cx)`, subscribes for its `CloseRequested` event (Esc), enforces
the single-sub-page rule (docs/0020 D1) across the now-quad family
{pr, issue, notification} + creator hidden, moves focus, `cx.notify()`.
The repo crumb gains the suffix `· notification` (0014 chain; no number —
thread ids are huge REST ids, not human labels).

The row click handler (`render_notification_row`) swaps `cx.open_url` for
`self.open_notification(n.clone())`. Nothing else about the row changes
(badge, title, age, hover).

### D2 — Seed-first render, thread fetch upgrades

The page renders immediately from the carried row: kind badge
(`subject.kind`), subject title, repo `full_name`, `updated_at` (relative),
unread state. It then fetches `GET /notifications/threads/{id}` (new
`Kind::ThreadDetail`, page-owned parse per the `IssueDetail` convention:
200 body parsed page-side, non-200 forwarded to `GithubState::apply` for
shared auth/rate-limit handling) and upgrades: `reason` badge
("mentioned", "comment", "review_requested", … rendered raw), unread
re-confirmation, `subject.url`/`latest_comment_url` (for the §D5
hand-off). Fetch failure keeps the seed-rendered page and logs to stderr —
degrade, no dialog ([DG] error states).

`api.rs` gains the dropped thread fields: `Subject {title, kind, url:
Option<String>, latest_comment_url: Option<String>}` and `Notification`
gains `reason: Option<String>` (serde defaults; the list payload provides
them today — §2.2). The repo-view list keeps rendering exactly as before.

### D3 — Auto mark-as-seen on open (the feature's core)

The instant the page starts loading its contents (owner decision, §4.3) it
fires `PATCH /notifications/threads/{id}` — before the thread GET returns,
the two requests in parallel — and optimistically flips its local `unread`
to `false` (the read state is rendered from the page's own field, not from
the list).
New `Kind::ThreadMutation(ThreadOp)`
with `ThreadOp::MarkRead` (and `MarkDone` reserved — §7): the transport
extension is a REST lane with a method, since `FetchSpec` is GET-only and
every existing mutation is GraphQL. 205 and 204 are success; non-2xx is a
failed mutation — page state reverts to unread, stderr logs, the page stays.

On success the module refetches (refetch-after-mutation, the 0015 rule):
`Kind::RepoNotifications` for the open repo (the row leaves the unread
list) and the global `Kind::Notifications` so the card count catches up
without waiting for the tick.

### D4 — Read state on the page: display + explicit read, no unread

The page shows the current state (dot/badge: Unread / Read). The explicit
action offered is **"Mark as read"** (idempotent `PATCH`), which doubles as
the retry when the auto-mark failed. **There is no "mark as unread".**
Rationale: upstream has no such endpoint (§2.3) and GitHub's inbox doesn't
either; a local-only unread flag would desync from github.com and lie to
the user. The owner asked for a read/unread toggle — this is the one place
the feature as stated collides with the API. Owner decision (§4.1): do
what GitHub itself does — display + mark-read only.
### D5 — The ↗ browser button: kind-aware web URL, safe fallback

Header ↗ (0016 pattern) derives the web URL from `subject.url` per kind —
pure, table-tested:

| subject.type | API → web mapping |
| --- | --- |
| `Issue` | `/repos/{o}/{r}/issues/{n}` → `github.com/{o}/{r}/issues/{n}` |
| `PullRequest` | `.../pulls/{n}` → `github.com/{o}/{r}/pulls/{n}` |
| `Discussion` | `.../discussions/{n}` → `github.com/{o}/{r}/discussions/{n}` |
| `Commit` | `.../commits/{sha}` → `github.com/{o}/{r}/commit/{sha}` |
| anything else | no derivation → repo notifications page (0015 target) |

Malformed/missing `subject.url` falls back the same way. `latest_comment_url`
stays unused this round (filed §7).

### D6 — Issue/PR subjects: hand-off into the native pages

When the subject is `Issue`/`PullRequest` and the number parses out of
`subject.url`, the page shows an **`Open #n`** button that closes the
`subject.url`, the page links to the native page — an **`Open #n`** link
(the subject title links the same way, owner decision §4.2) that closes
the notification page and opens the native detail page via the existing
`{full_name}#{n}`, number, title; author `None`, age from the thread) —
both pages already fetch full detail on open, so the seed is display-only.
Other kinds get no in-app hand-off (§2.5): metadata + ↗ is the whole page.

### D7 — What deliberately stays out

- **Mark as done** (`DELETE /threads/{id}`): GitHub's third inbox action,
  useful, but not part of this ask — reserved in `ThreadOp`, filed as a
  follow-up (§7).
- Read-notification browsing (`all=true` list views), subscriptions
  management, notification settings. The repo-view list remains the unread
  list it is today.

## 4. Owner decisions (2026-09-07, PR #150 review — all three resolved)

1. **No-unread constraint (D4):** go with what GitHub itself does — the
   page displays read state and offers mark-read only; no local-only
   unread toggle.
2. **Hand-off (D6):** proper notification pages for every kind; Issue/PR
   subjects carry links leading to the in-app PR/issue pages (no
   direct-to-native row routing).
3. **Auto-seen timing (D3):** mark-as-read fires as soon as the page
   starts loading its contents — before content arrives (GitHub's own
   click semantics; no delayed/confirm variant).
## 5. Implementation plan (one PR, after this doc merges)

1. `api.rs`: extend `Subject`/`Notification` (serde defaults); add
   `ThreadNotification` struct (full thread); URL builder
   `thread_url(id)` / `thread_patch_url(id)`; keep existing tests green.
2. `service.rs`: `Kind::ThreadDetail`, `Kind::ThreadMutation(ThreadOp)`;
   REST lane with method (PATCH/DELETE, no body); result convention per
   `IssueDetail`; module refetch helper after a settled mark
   (RepoNotifications + Notifications).
3. `notification_page.rs`: the page (seed render, thread upgrade, auto
   PATCH, mark-read button, ↗, `Open #n`, Esc event); `web_url(subject)`
   pure fn; crumb suffix plumbing in `page.rs` (`· notification`).
4. Tests (gpui::test + pure tables): mark-read fires on open and flips the
   page state; failure reverts; refetch after settle; `web_url` table per
   §D5 incl. fallback rows; crumb suffix; synthesized-row hand-off; seed
   render before the thread fetch lands.
5. Docs: this doc's implementation note + docs/0015 follow-up strike
   (browser hand-off superseded by 0026), docs/README index/roadmap rows.

## 6. Testing & human checks

- Gates as always: fmt, clippy zero warnings, full test workspace.
- Debug bridge (`docs/0010`): open repo view → `tree` shows notification
  rows; `click` a row → page takeover, crumb `· notification`; verify the
  unread dot flips and the row leaves the list on close; `key tab`/`enter`
  through mark-read; ↗ button presence per subject kind; Esc returns.
- Owner: real-account round trip — open an unread PR notification (row
  vanishes from the unread list, card count drops), check github.com shows
  it read, Esc, open a Discussion notification (metadata-only page, ↗ to
  the repo notifications page), open an Issue notification and follow
  `Open #n` into the native page.

## 7. Follow-ups (GitHub issues, not this PR)

- `MarkDone` (`DELETE /notifications/threads/{id}`) — "done" action on the
  page and optionally on rows.
- `latest_comment_url` — "jump to latest comment" for Issue/PR subjects.
- Global card: notification rows are counts-only today; if the card ever
  lists notifications, it should reuse this page (noted for whoever writes it).
