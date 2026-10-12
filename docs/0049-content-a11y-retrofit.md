# 0049 — Content A11y Retrofit: ids, roles and labels for every interactive surface

Addresses [#311](https://github.com/noosxe/my-dashboard/issues/311)
(and subsumes [#57](https://github.com/noosxe/my-dashboard/issues/57),
[#95](https://github.com/noosxe/my-dashboard/issues/95)).

> **Status: shipped** in #317 (chrome + list surfaces), #318 (detail
> pages), and the creators/docs PR — implementation split per D6. The two
> upstream name gaps (SidebarMenuItem, ListItem, SidebarToggleButton) are
> filed as [longbridge/gpui-kit#3160](https://github.com/longbridge/gpui-kit/pull/3160);
> a rev bump lands them separately.

## 1. Summary

The debug bridge's `tree` — and any real screen reader — sees **shell chrome
only**. The content area contributes (almost) zero accessibility nodes,
because our own interactive elements are plain `div`s with `.on_click` and no
`.id()`/`.role()`/label, and two upstream widgets we rely on (`SidebarMenuItem`,
`ListItem`) don't expose labels. This retrofit gives **every interactive
element in the app an id, a role, and a human label**, plus state flags where
they carry meaning, and files the small upstream patches needed for the two
remaining widget gaps.

The payoff is two audiences at once: real assistive technology (the point of
a11y), and the debug bridge — `tree` gains labels and bounds for content,
which makes `click`-driven verification (the AGENTS.md-sanctioned path)
possible without blind coordinate sweeps.

## 2. Current state and verified building blocks

Verified against the pinned stack (`gpui-pre =0.3.3`, `gpui-component` @
`94a313a`):

- **gpui's a11y API is complete for our needs** (`StatefulInteractiveElement`,
  requires `.id()`): `role(accesskit::Role)`, `aria_label(…)` (the accessible
  name), `aria_description(…)`, `aria_selected`, `aria_expanded`,
  `aria_toggled`, `aria_value`, `aria_keyshortcuts`, `accessibility_id(…)`
  (author-visible stable id), plus the activedescendant pattern for composite
  widgets. A node is only built for elements that have an id and end up with a
  non-`GenericContainer` role; unrole'd divs are **filtered out** — this is
  why content is invisible today.
- **Names do not derive from text children**: an id'd `Text` child becomes its
  own text node; a parent's accessible name must be set explicitly
  (`aria_label`), or children must use `Text::new_inaccessible`. So "we
  render the PR title inside the row" buys nothing — labels are deliberate.
- **Upstream widget coverage** (pinned rev):
  - `Button` self-roles as `Button` (plus an override builder); it does **not**
    infer a name from `.tooltip()` — icon-only buttons are currently unnamed
    in the tree (our nav-back/nav-forward/collapse included).
  - `Tab`/`TabBar` self-role with a public `aria_label()` builder (falls back
    to the tab's label). ✅
  - `BreadcrumbItem` self-roles as `Link`/`ListItem` but its text child is
    its own text node — **no accessible name lands on the crumb** (the
    first full audit FAILed every link crumb on `role-label`, #522). The
    shell renders its own crumbs (`shell.rs` `ShellCrumb`) with ids,
    roles, and explicit `aria_label`s instead. Upstream gap.
  - `ListItem` self-roles as `ListItem` but has **no label builder** →
    the sidebar repo list items are unnamed (#95). Upstream gap.
  - `SidebarMenuItem` takes a label but sets **no role and no accessible
    name** → module nav items are absent from the tree entirely (#57).
    Upstream gap (the label is already a field — the fix is trivial
    upstream).
  - `SearchableList`, `Select`, `ComboBox`, `Tree`, inputs: self-role as
    containers/widgets; their internal rows/labels vary — spot-checked
    during implementation, not blocking.
- **Our own code already has the seed of the pattern**: the repo-view rows
  (`render_pr_row_inner`, `render_issue_row`, `render_notification_row` in
  `page.rs`) use dynamic ids (`pr-{key}`) and `.role(ListItem)` — but no
  labels, and the live tree showed the sidebar's `SearchableList` items (upstream)
  while home-view cards rendered through the same row helper were not
  consistently observable. The retrofit standardizes this everywhere.
- **Operational fact worth codifying**: the a11y tree is only *built* while an
  AT-SPI client is connected, and a one-shot `tree` dump returns the
  **retained last-built frame** — stale unless a repaint was forced first
  (e.g. `key tab`). docs/0010 gets this caveat.

## 3. Design

### D1 — Conventions (normative)

- **Ids**: every interactive element MUST have a stable `.id(...)`. Kebab-case
  with a dynamic suffix from the entity key where the element repeats:
  `pr-{key}`, `issue-{key}`, `notif-{id}`, `thread-{ix}`, `file-{path-hash}`.
  Ids are the bridge's addressability anchor; keep them stable across
  refactors (they already are for the roled rows).
- **Roles** (taxonomy kept deliberately small):
  - Rows/cards in lists that open something → `accesskit::Role::ListItem`
    (matches existing repo-view rows).
  - Standalone actions → the `Button` widget where visuals allow (it
    self-roles); plain-div actions that can't be a `Button` get
    `.role(Role::Button)`.
  - Binary toggles rendered as plain divs → `.role(Role::Switch)` +
    `aria_toggled` (prefer the upstream `Switch` widget if it fits).
  - Tab strips → upstream `Tab` + `aria_label` (already supported).
  - Panels/cards → `GroupBox` role only where the container is meaningfully
    labeled (existing `…-box` ids mostly cover this).
  - Inline links in our own prose → `.role(Role::Link)` + label (the markdown
    case is D4's exception).
- **Labels**: every interactive element MUST carry `aria_label` — an icon-only
  Button MUST NOT rely on its tooltip for accessibility. Label text follows
  the surface's natural reading order, entity rows use
  `"{kind} {number}: {title} — {repo} · {author} · {state}"`-shaped strings
  (exact wording per surface in the implementation checklists; labels are not
  localized, English only, like the rest of the UI).
- **State flags** where they carry meaning: `aria_selected` on the selected
  repo row and active tab (upstream handles tabs), `aria_expanded` on
  collapsibles (sidebar sections, file-tree folders, thread collapse chevrons),
  `aria_toggled` on switches, `aria_description` for hint text on ambiguous
  controls (e.g. destructive confirm cards).
- **`accessibility_id`** is NOT part of this retrofit: the bridge addresses
  nodes by their accesskit id and the tree dump already surfaces `.id()` — a
  second naming scheme adds churn without a consumer. Revisit if #43 lands
  per-node stable addressing.
- **Decorative text** stays unrole'd. We role *interactive* elements and
  labeled landmarks, not every `div` — the tree must stay small enough to
  scan.

### D2 — Surface inventory (implementation checklist)

125 `on_click`/mouse-handler sites across the app, grouped:

| Surface cluster | Files / fns | What it gets |
|---|---|---|
| Shell chrome | `shell.rs` `render` (6), `nav_menu_items` (3) | `aria_label` on icon buttons (Back/Forward/collapse already tooltip'd — add explicit labels); sidebar items blocked upstream (D3) |
| Home + module views | `page.rs`: `render_home_view`, `render_right_column`, filter toolbars (3), `connect_panel` | ListItem+labels on PR/issue/notification rows (rows partially done); labels on retry/more/filter buttons; Link roles on right-column links |
| Repo view rows | `page.rs`: `render_pr_row_inner`, `render_issue_row`, `render_notification_row` | already ListItem+id — add `aria_label`, `aria_selected` |
| PR detail | `pr_page.rs` (30): tabs, header, checks (+log pane), merge panel, meta pickers, milestone cards, reactions, comment box, inline edit, delete confirm, cap row, files toggle | Tab `aria_label`s; Button-widget labels everywhere; ListItem for row-like actors; `aria_description` on destructive confirms |
| Review threads / files | `files.rs` (22): thread cards, suggestion cards, reply boxes, finish-review bar, resolve picker rows, draft rows, stray bucket | ListItem on thread/suggestion cards with label `"thread @ {file}:{line} — {state}"`; Button labels on actions; `aria_expanded` on collapsed threads |
| Issue detail | `issue_page.rs` (18): title area, sidebar cards, pickers, milestone, state panel, reactions, lock panel, comment box | same pattern as PR detail |
| Creators | `new_issue_page.rs` (5), `new_pr_page.rs` (5) | labels on non-upstream controls; upstream inputs already covered |
| Notifications page | `notification_page.rs` `render_header` (5) | Button labels |
| Settings | core `settings.rs` (1+2), github `settings.rs` (2) | Link role on external URLs, Button labels |
| Small chrome | `home.rs` `card`, `card.rs` (2), `freshness.rs` `freshness_row` | ListItem+label on module cards; Button label on refresh |
| Markdown links | `markdown.rs` `render_runs` | see D4 |

### D3 — Upstream gaps (#57, #95)

Two small patches to `longbridge/gpui-component` (file early, don't block on
them):

1. **`SidebarMenuItem`** (#57): set an accessible name from its existing
   `label` field and a role (`Button` matches its behavior; `TreeItem` would
   match the sidebar's visual tree — pick whichever upstream prefers).
2. **`ListItem`** (#95): public `accessibility_label()` builder so callers can
   name list rows (ours and `SearchableList`'s internal rows).

**Filed:** [longbridge/gpui-kit#3160](https://github.com/longbridge/gpui-kit/pull/3160)
covers both, plus the same gap found in `SidebarToggleButton` during
implementation (icon-only, wraps an unnamed internal Button).

Until a rev bump lands, those two widgets stay partially invisible; our own
surfaces do not depend on them. The fork contingency (#60) remains the
fallback if upstream stalls — not needed for patches this small.

### D4 — Markdown inline links (known hard case)

Link runs render through gpui's `InteractiveText` (upstream, exact-pinned —
we cannot patch it locally without a fork).

**Verified during implementation**: `InteractiveText` emits exactly one
`Label` node per text block whose *value* is the whole text — no `Link`
role, no per-run nodes, no per-run names. Markdown bodies are therefore
readable as text but links are not individually addressable. Per the plan:
documented here, no fork; the per-run gap joins #43's upstream follow-up
list. Surrounding card/row containers still get their own nodes, so links
lose only per-run addressability.

### D5 — Verification

- **Per-surface bridge verification** (recorded in each implementation PR):
  launch the app, force a fresh frame (`key tab` before `tree`), assert the
  expected nodes appear with role+label, and `click` one representative node
  per surface class. This is now practical *because* of the labels.
- **Regression guard** (spike, **dropped with cause**): `VisualTestContext`
  cannot build a11y trees — gpui only builds the tree while an AT client
  is connected (`is_a11y_active`), test contexts have none, and there is no
  force flag in gpui-pre 0.3.3. The D10 review rule stays the guard; the
  docs/0010 amendment records the same operational caveat.
- docs/0010 D10 amendment ships with this work: labels are now part of the
  hygiene rule, plus the stale-frame caveat from §2.

### D6 — Scope

Three implementation issues (split by blast radius, each independently
mergeable and verifiable):

1. **Chrome + list surfaces**: shell chrome labels, home/module views, repo
   view rows, notifications, settings, home cards, freshness. Files upstream
   PRs for #57/#95.
2. **Detail pages**: PR detail + review threads/files + issue detail (the
   30/22/18 clusters).
3. **Creators + markdown link spike + docs amendments** (0010 D10, 0029 §3
   row, this doc's shipped note).

## 4. Non-goals

- **Keyboard navigation** (tab order, arrow-key lists, focus management
  beyond what exists): the tree exposes nodes; making them *focusable* and
  adding key handling is a separate, much larger effort.
- **Full screen-reader UX** (announcements, live regions, traversal tuning):
  this retrofit makes the tree truthful; polishing the AT experience comes
  after, with real AT testing.
- **Localization** of labels.
- **Patching or forking upstream** beyond the two D3 PRs (fork only via #60's
  contingency).
- **`aria_keyshortcuts` sweep** — only Back/Forward buttons get one
  (`Alt+Left`/`Alt+Right`) as a demonstration of the API.

## 5. Acceptance criteria

- Every interactive element in the inventory table exposes an id + role +
  label in `dashboard-debug tree` output.
- A blind bridge operator can open a PR, act on a review thread, and change a
  setting using only `tree` + `click` (no coordinate guessing).
- #57 and #95 upstream PRs are filed (links recorded here when they exist).
- docs/0010 §D10 amended (labels + stale-frame caveat); docs/0029 §3 row.
- Gates green; no behavior change beyond a11y attributes (existing tests
  untouched semantics).
