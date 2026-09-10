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
| `PullRequest` | `.../pulls/{n}` → `github.com/{o}/{r}/pull/{n}` — canonical web path is `/pull/N` singular (API segment stays plural, #173) |
| `Discussion` | `.../discussions/{n}` → `github.com/{o}/{r}/discussions/{n}` |
| `Commit` | `.../commits/{sha}` → `github.com/{o}/{r}/commit/{sha}` |
| anything else | no derivation → repo notifications page (0015 target) |

Malformed/missing `subject.url` falls back the same way. `latest_comment_url`
stayed unused this round (filed §7) — superseded by D9, #154.

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

- **Mark as done** (`DELETE /threads/{id}`): GitHub's third inbox action
  — shipped by the #153 follow-up, see D8 below.
- Read-notification browsing (`all=true` list views), subscriptions
  management, notification settings. The repo-view list remains the unread
  list it is today.


### D8 — Mark as done (follow-up #153, amended 2026-09-09)

GitHub's third inbox action (`DELETE /notifications/threads/{id}` →
204, the thread leaves the inbox entirely — §2.3) ships in both places
the inbox is rendered, over the lanes 0026 already reserved:

- **Page-level**: a **Mark as done** button beside Mark as read. Unlike
  the read button it is **always offered, Read included** — done ≠ read
  (owner decision §4.5); hidden-not-disabled does not apply. Disabled
  only while its DELETE is in flight; a failure keeps the page and
  re-arms the button as retry (the D4 failure pattern, no optimistic
  flip — nothing local to revert, the thread simply stays).
- **Row-level**: an icon-only done button (`notif-done-{id}`, check
  circle) on every repo-view row, owner decision §4.5 — the DELETE fires
  straight from the row, no page takeover. A guard div swallows the
  click so the row's open handler never fires (gpui bubbles clicks;
  `Button` only stops propagation while loading). The in-flight round
  disables the row's button (double-click guard); the guard clears on
  settle — on success the refetch replaces the list anyway.
- **Post-done UX (owner decision §4.4)**: a settled DELETE is the
  github.com-like behavior — the open page closes back to the repo view
  (crumb suffix drops) and the host runs `mark_refetch_specs()`
  (RepoNotifications + the global Notifications) so the row leaves the
  unread list and the card count drops without waiting for the tick.
  Row-level settles run the same refetch through the stored window
  handle (the `sync_from_state` pattern — the click's window is out of
  reach by settle time). HTTP failures forward to the shared
  auth/rate-limit handling; network errors stay component-local
  (stderr, degrade in place) — the mark conventions, applied to done.

### D9 — Jump to latest comment (follow-up #154)

GitHub's inbox renders a notification's latest comment with a jump
link; the app's notification page now does the in-app equivalent for
Issue/PR subjects, over the field 0026 carried but left unused (D5,
§7):

- **Parse** — `api::latest_comment_url` carries a REST comment id in a
  pinned upstream shape (github/rest-api-description,
  `thread.subject.latest_comment_url`, verified 2026-09-09):
  `…/repos/{o}/{r}/issues/comments/{id}` — the issue-comment collection
  item, which serves Issue and PR *conversation* comments alike. The
  pure `api::latest_comment_id` (table-tested, same URL-walking
  precedent as `web_url`/`subject_handoff`) yields that id and `None`
  for everything else.
- **Affordance** — a `Jump to latest comment` link under the subject
  title, gated on the D6 hand-off (the subject must be Issue/PR with a
  parseable `subject.url`) **and** a parseable `latest_comment_url`.
  The plain subject-title click keeps opening the conversation from the
  top (`OpenSubject { anchor: None }`); the jump click carries
  `anchor: Some(comment_id)`.
- **Degrade** — PR subjects whose latest comment is a *review* comment
  (`…/pulls/comments/{id}`) get no link: the app renders no review-
  thread surface, so there is nothing to anchor to — silently no
  affordance, the plain hand-off stays. Same for a missing or malformed
  URL (no link, no error).
- **Anchor surface (the native pages, docs/0020/0016)** — the detail
  queries now select `IssueComment.databaseId`, the REST id, matching
  the URL's tail. The pages hold a `CommentAnchor` state machine:
  `Pending(rest_id)` → `Jumping(node_id)` when the landed detail
  contains a comment with that `databaseId` → `Highlighted(node_id)`
  the frame the card actually renders → `Idle`. The frame the jumped
  card renders, the conversation scrolls it to the top and the card
  wears an accent ring (GitHub-like), fading after 3 seconds
  (`ANCHOR_FADE`).
  - The scroll resolves in prepaint: the timeline cards are *direct*
    children of the scroll container (the old single-child wrapper is
    flattened), so `ScrollHandle::scroll_to_top_of_item` can land on
    the card's measured bounds — no next-frame indirection.
  - A card swapped for an inline editor or delete confirm (0019 D4) or
    a non-conversation PR tab defers the jump until the plain card
    really renders; the scroll is one-shot (`jump_done`), so a later
    frame never re-pins the view against the user's scrolling.
  - A miss — deleted comment, beyond the first 100, review comment —
    gives up quietly: no jump, no highlight, no error row; the page
    opens unanchored (log line only).

## 4. Owner decisions (2026-09-07, PR #150 review — first three; #153
   — 4/5, 2026-09-09)

1. **No-unread constraint (D4):** go with what GitHub itself does — the
   page displays read state and offers mark-read only; no local-only
   unread toggle.
2. **Hand-off (D6):** proper notification pages for every kind; Issue/PR
   subjects carry links leading to the in-app PR/issue pages (no
   direct-to-native row routing).
3. **Auto-seen timing (D3):** mark-as-read fires as soon as the page
   starts loading its contents — before content arrives (GitHub's own
   click semantics; no delayed/confirm variant).
4. **Post-done UX (D8, #153):** the github.com-like behavior — a
   settled done closes the open notification page back to the repo view
   and refetches the unread lists; failures retry in place.
5. **Row-level done (D8, #153):** per-row done buttons ship with the
   follow-up, same PR as the page-level button.
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
  Done round trip (D8, #153): `click notif-done-{id}` on a row → the
  row leaves the unread list and the card count drops, no page opened;
  open a notification page → `click notification-mark-done` (present
  while Read too) → the page closes back to the repo view.
  Jump round trip (D9, #154): open an Issue/PR notification whose
  `latest_comment_url` parses → `click notification-latest-comment` →
  the native page opens anchored: the target comment is scrolled into
  view with the accent ring (bridge `tree` shows it in the viewport
  bounds); the ring fades after ~3s; `Open #n` without the jump still
  opens the page at the top.
- Owner: real-account round trip — open an unread PR notification (row
  vanishes from the unread list, card count drops), check github.com shows
  it read, Esc, open a Discussion notification (metadata-only page, ↗ to
  the repo notifications page), open an Issue notification and follow
  `Open #n` into the native page; done: mark a thread done (row button
  and page button) and check github.com's inbox no longer lists it;
  jump: from an Issue/PR notification, `Jump to latest comment` lands on
  the highlighted comment on the native page.

## 7. Follow-ups (GitHub issues, not this PR)

- ~~`MarkDone` (`DELETE /notifications/threads/{id}`) — "done" action on
  the page and optionally on rows~~ → implemented (D8, #153).
- ~~`latest_comment_url` — "jump to latest comment" for Issue/PR
  subjects~~ → implemented (D9, #154). PR *review-comment* targets
  (`…/pulls/comments/{id}`) still have no in-app anchor — a review-thread
  surface would be its own feature (0020/0016 scope).
- Global card: notification rows are counts-only today; if the card ever
  lists notifications, it should reuse this page (noted for whoever writes it).
