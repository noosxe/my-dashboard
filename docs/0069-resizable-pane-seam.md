# 0069 — Resizable pane seam: one split mechanism, pane roles, inspector scope

Date: 2026-10-10
Status: Shipped (2026-10-10, #497 — pilot per D3–D5; inspector role pending its first consumer, see the deferred issue)

## 1. Context

The app has no resizable pane system. Side-by-side regions exist exactly
once — the Files tab master–detail split (0018 D1): a **fixed**
`w_75` ≈ 300 px sidebar with `border_r`, plus a `flex_1().min_w_0()`
diff pane. 0018 explicitly deferred "sidebar drag-resize" (its
non-goals). Everywhere else, "detail" means full page replacement
(notification list → detail page via navigation), not a pane, and there
is no inspector concept at all.

Upstream gpui-component ships a resizable system we do not use:
`h_resizable` / `v_resizable` + `resizable_panel()` +
`ResizableState` (an entity holding `Vec<Pixels>` sizes, with
`resize_panel` / `reset_panel` and a `size_range(min..max)` per panel).
It also ships `DockArea` (a full docking system: tabbed panes, floats,
serialization) — a much larger machine than this app needs.

The design guides call for master–detail panes to be resizable with
min/preferred/max and persisted splits, with the primary work area
keeping the surplus. Each future side-by-side surface (PR checks
drill-down, usage drill-down, an inspector) would otherwise invent its
own split answer. The architecture audit (#486, finding for #490)
flagged exactly this: build the seam once, before the second ad-hoc
split appears.

**Owner decision (this PR's review): the inspector pane is in scope** —
the seam must support a right-hand inspector as a named pane role. That
decision is recorded here and drives D2/D6; it does not mean an
inspector ships now.

## 2. Goals and non-goals

**Goals**

- One sanctioned way to split a canvas (upstream resizable), used by
  every side-by-side surface from now on.
- Named pane roles — master/list, primary, inspector — so "which side
  wins the surplus" is decided once, not per surface.
- Pilot on the one existing split: Files tab sidebar becomes resizable
  (min / default / max) with the split persisted.
- Persistence follows the shell-state pattern (0004 §4.3):
  load → clamp on restore.
- Keyboard-operable resize handles (docs/0065 tab-stop coverage).

**Non-goals**

- `DockArea`: tabbed panes, floating panes, drag-to-rearrange, upstream
  dock serialization (D7).
- An inspector *implementation* in the pilot — no consumer surface
  exists yet; the role is defined, the first consumer is filed as its
  own issue (D6).
- Splitting any surface other than the Files tab in this work.
- Sub-pane nesting rules beyond "one axis per group" (v1 keeps groups
  flat; revisit if a real surface needs nesting).

## 3. Pane roles

- **Primary** — the work area. Never sits inside a resizable group as
  the draggable side; it is the `flex` remainder that keeps the surplus.
  Today: the diff pane in Files, the page canvas everywhere.
- **Master / list** — left side, fixed-axis sibling of a primary. The
  user may resize it; the primary absorbs the delta. Today: Files
  sidebar (pilot).
- **Inspector** — right side, secondary tooling bound to the primary
  (checks detail next to the PR, record drill-down next to usage). Same
  resize/persist contract as master, mirrored: the user may resize it;
  the primary absorbs the delta. In scope as a role (owner decision,
  §1); **no consumer ships until a surface needs one** (D6).

Roles are a naming and constraint convention over the same machinery —
not a class hierarchy. A surface declares its role when it joins a
group; the group enforces min/max and persistence.

## 4. Design decisions

### D1 — One seam: upstream resizable only

Every side-by-side region is an `h_resizable(id)` /
`v_resizable(id)` group holding `resizable_panel()`s, backed by an
`Entity<ResizableState>` bound via `.with_state(&state)` (group API:
`child` / `children` / `on_resize`). No new ad-hoc `border_r` +
fixed-px master/detail may be added anywhere; the clippy-reviewable
smell is "fixed-width sibling next to `flex_1` inside the same axis" —
if you wrote that, you needed the seam. Visual: the handle renders with
theme tokens (upstream `resize_handle_appearance()`); the `border_r`
the sidebar carried moves to the handle's resting line (D8 keeps the
framed panel around the whole work area — panes live inside it).

### D2 — Constraints are declared, absolute px

Per panel: `size_range(min..max)` (upstream applies them as
`min_w`/`max_w` and clamps `flex_basis`). The upstream API is absolute
`Pixels` — there is no percentage form — so bounds are named constants
per surface, checked against the default geometry (1280×800, 0002 Q2)
and the smallest supported window (900×600): a max that starves the
primary at min-window is a bug. Defaults: a group with no persisted
state renders at its declared default sizes.

### D3 — Pilot: Files sidebar

0018 D1 is amended by the implementation of this doc (same PR): the
sidebar joins an `h_resizable("files-split")` group as the master pane
— `size_range(px(220.)..px(480.))` (≈40% of the Files tab area at
default geometry), default 300 (today's `w_75`), diff pane remains the
primary remainder. The `border_r` + fixed `w(px)` pair goes away; the
handle replaces it. Sidebar internals (0018 D2) are untouched.

### D4 — Persistence: one number per group, clamped on restore

`state/my-dashboard/shell.json` (0004 §4.3) gains one field per
persisted group — `"files_sidebar_px": 300.0`. `ResizableState` holds
`Vec<Pixels>` and is not `Serialize`; we do not persist its internal
vec. On load: clamp to the group's `[min, max]` (window may have
changed since save — the 0004 restore pattern), then apply to the
entity (`resize_panel(0, px(saved))`) before first render of the group.
`on_resize` writes the master size back (debounced like other shell
state). Inspector groups persist symmetrically when they exist (D6).

### D5 — Handles are keyboard-operable (docs/0065)

Upstream's handle is pointer-only (mouse drag via `on_mouse_event`, no
focus or key handling). A resize affordance is an interactive element:
at pilot time the handle carries an `.id(...)` (e.g. `files-split-handle`),
appears in `tree --tab-stops`, and supports keyboard resize —
`Left`/`Right` step ±16 px against the group axis, `Home`/`End` to min
/ max, clamped by D2 bounds. If upstream cannot express focus/keys on
the handle, we add the thin keyboard wrapper in our layer rather than
shipping mouse-only; the gap is closed in the pilot PR, not filed
away. `tree` shows the handle with role and name ("Resize files
sidebar").

### D6 — Inspector: supported role, no speculative build

The inspector contract, for the surface that eventually uses it: right
pane of an `h_resizable` group, role `Inspector`, a11y name
"inspector" (object + what it inspects, e.g. "checks inspector"),
`size_range(px(280.)..px(480.))`, persistence key
`<page>_inspector_px`, same clamp/restore as D4. It appears only when
its surface has something to inspect (absent ≡ not rendered, not
zero-width). No inspector ships with the pilot: PR checks drill-down is
the first candidate and gets its own issue when the owner wants it.
The scaffold must make adding one a small, boring change — that is the
point of deciding now.

### D7 — No DockArea

Upstream `DockArea` (tabbed panes, floats, drag-rearrange, its own
serialization) is out: no current need, it duplicates our navigation
model (pages are destinations, not dockable tools), and it would own
layout state we cannot reason about in the 0065 pipeline. The inspector
role (D6) covers the foreseeable tool-pane need. Revisit only if a
surface genuinely needs floatable panes — that would be its own design
doc.

## 5. Testing

- Gates as always (fmt / clippy / tests). clippy.toml lints apply to
  any handle styling — no literal colors, no ad-hoc text sizes.
- Bridge receipts (Linux box) for the pilot PR:
  - `tree` on the Files tab: group, both panels, handle with id/role;
    `tree --tab-stops` includes the handle (D5).
  - `mouse move|down|move|up` across the handle: sidebar width changes,
    diff pane keeps ≥ its minimum; handle returns to rest.
  - Keyboard: focus handle, `Right` ×n — width grows by 16 px steps;
    `End` hits max.
  - Restart the session (persisted shell state): sidebar restores at
    the saved width, clamped into bounds.
  - 900×600 window: min-window check — primary never starves (D2).
- Manual (owner): drag feel, double-click-to-default if the upstream
  handle provides it (not required), light/dark handle visibility.

## 6. Supersessions and follow-ups

- **Amends 0018 D1** (fixed sidebar → resizable master); the
  implementation PR updates 0018's text in the same PR.
- **Extends 0004 §4.3** with the `files_sidebar_px` key.
- Implementation issue (files from this PR): pilot per D3–D5.
- Deferred, tagged: first inspector consumer (D6) — file when the owner
  picks the surface; PR checks drill-down is the candidate.
