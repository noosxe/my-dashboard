# 0015 — GitHub module: notification row click (open repo notifications page)

- **Date:** 2026-09-03
- **Depends on:** 0005 (GitHub module), 0012 (per-repo sections, D4 row
  treatment), 0010 (debug bridge, D10 id rule)
- **Closes the design need of:** issue #81 — owner request: "add click
  support for notifications on repo page, should work similar to PRs and
  issues, open the notification page on github"

## 1. Context & problem

The repo page's "Notifications (n)" GroupBox lists the repo's unread
notifications via `render_notification_row` (page.rs). Unlike PR rows
(page.rs:349) and issue rows (page.rs:404 — the doc 0012 D4 "browser
hand-off"), notification rows were a plain non-interactive `div`: no id,
no cursor/hover state, no click handler. Every other row on the pane
opens GitHub in the browser; notification rows dead-end.

## 2. Scope

In:

- Notification rows on the repo page become interactive with the **same
  treatment as PR/issue rows**: `.id(...)`, `cursor_pointer`, accent
  hover (8% opacity), `on_click` → `cx.open_url`.
- The click target is the repo's notifications page on GitHub:
  `https://github.com/{full_name}/notifications`.
- A pure `notification_url` helper (table-tested) + doc + README index
  row, in the same PR (small-feature flow).

Out (non-goals):

- **No per-thread deep links.** The notifications REST payload carries
  no subject URL — deep-linking a row to its exact PR/issue would need a
  resolution fetch per subject. The notifications page needs no API
  change (owner decision, issue #81 Q1).
- **No home-card rows.** `render_notification_row` has exactly one call
  site (the repo page GroupBox); the home card is untouched (owner
  decision, issue #81 Q2).
- No mark-as-read, no unread styling changes, no API surface changes.

## 3. Design

### D1 — Row treatment parity

### D1 — Row treatment parity

The row's modifier chain mirrors `render_issue_row` (doc 0012 D4) and adds
the docs/0010 D10 a11y obligation: `.id("notif-{id}")` **plus a role** —
`.role(accesskit::Role::ListItem)` — so the row exists in the a11y tree at
all. Verified live during implementation: without the role, gpui skips the
node entirely (it logs "has an id but no role"), the row is invisible to
`dashboard-debug tree`, and bridge `click` cannot reach it — while the data
path (fetch → ETag cache → GroupBox rows) was working the whole time. The
row then continues with the shared treatment: `cursor_pointer`, accent
hover (8% opacity), `rounded_sm`, `on_click` → `cx.open_url`.

(PR/issue rows predated this discovery and were id-only — equally invisible.
They gained the same `ListItem` role in the follow-up fix (#82); all three
row families now share the treatment.)

### D2 — URL construction (pure, testable)

```rust
fn notification_url(repo_full_name: &str) -> String {
    format!("https://github.com/{repo_full_name}/notifications")
}
```

Input is `RepoRef::full_name` (`owner/repo`), already on the
notification. Table-tested: normal form and a stray slash-less form
both produce sane URLs (path-bounded, nothing escapes).

[DG]: no new colors, sizes, spacing, or typography — the row inherits
exactly the issue-row visual (accent hover, rounded corners, pointer
cursor).

## 4. Testing

- `notification_url_targets_repo_notifications_page` — table test
  alongside the other pure-fn tests (selective import per the 0009 §4
  no-`use super::*` hazard).
- No new `#[gpui::test]`: the change is one row modifier chain,
  verified via the bridge (§5) and human checks (§6); all existing
  tests pass untouched.

## 5. Debug-bridge verification (docs/0010)

With the app running (`cargo run`, a11y bus up), a repo selected and
unread notifications present:

1. `tree` → the Notifications GroupBox rows now appear with stable
   `notif-*` accesskit ids (they were absent before — the rows were not
   nodes at all).
2. `click notif-<id>` → the default browser opens the repo's
   notifications page; no app state changes, no log noise beyond the
   click.

## 6. Acceptance criteria

1. Clicking a notification row on the repo page opens
   `https://github.com/{owner}/{repo}/notifications` in the browser.
2. Row hover/cursor states match PR and issue rows (no visual delta
   beyond interactivity).
3. Rows are visible in `dashboard-debug tree` with stable `notif-*` ids
   and clickable via the bridge.
4. Home card unchanged; no API or service changes.
5. Gates: `cargo fmt --all --check`, `cargo clippy --workspace
   --all-targets` (zero warnings), `cargo test --workspace`; doc +
   README row merged with the implementation PR.

## 7. Human checks (owner)

- [ ] Click a notification row on the repo page: browser opens the
      repo's notifications page signed in as your account.
- [ ] Hover a row: pointer cursor + faint accent background, matching
      the PR/issue rows above it.
- [ ] Keyboard-only pass: rows participate in tab order like other
      interactive rows (no focus traps).
