# 0010 — Agent Debug Bridge (attach, tree dump, non-disruptive input)

Today, debugging the running app means logs and screenshots. What's missing
for AI agents (and humans driving a terminal):

- **Visibility** — the full component tree with sizes, positions, and
  attributes, on demand.
- **Interactivity** — mouse + keyboard simulation that **does not disturb
  the user of the same machine**: no OS cursor movement, no focus stealing,
  no keystrokes leaking into other windows (the reason OS-level injection
  like `enigo`/`xdotool` is disqualified — plus Wayland blocks foreign
  synthetic input by design).

Owner decisions (2026-09-02), recorded:

- **v1 = seven commands** (§3 D6 table): `tree.dump`, `input.dispatch`,
  `input.click_node`, `input.focus_node`, `window.list`, `window.geometry`,
  `app.logs` — plus `app.quit` (0055 D11, the one later addition).
- **MCP bridge deferred** — v1 ships a CLI; MCP is revisited only if the
  CLI cannot provide what agents need.
- **No upstream filings** — upstream gpui gaps are worked around in-repo;
  tracked in #43.
- **Linux (Wayland + X11) first**; macOS in a later version.

## 1. Non-goals

- No `ptrace`/debugger-style attach to an *arbitrary already-running*
  binary. "Attach" = connect to a socket served by an app built with the
  `debug-tools` feature (the Chrome DevTools model: the browser opts in at
  launch). We own the app; this is sufficient.
- No OS-level input injection (`enigo`, `xdotool`, `libei`), no virtual
  displays (`Xvfb`, nested compositors). All input is dispatched inside
  gpui's event pipeline — the core point of the design.
- No MCP server in v1 (#2 above). The protocol is plain JSON; an MCP shim
  can wrap it later without app changes.
- No screenshots in v1 — already solved today; the protocol gains a
  command later if the socket path turns out to be better.
- No mutation commands (no set-state/edit-DOM equivalents) — read +
  input only.

## 2. Verified upstream facts

Everything below was read out of the pinned checkouts; nothing is
remembered. Zed rev `8cfa848c` (Cargo.lock), accesskit_unix `0.21.1`,
accesskit_atspi_common `0.18.1`, atspi `0.29.0` (already in the lockfile
via gpui_linux; zbus `5.13.2` already present via keyring — no new
version pressure).

**Synthetic input — public, ungated:**

- `Window::dispatch_event(PlatformInput, &mut App) -> DispatchEventResult`
  (`crates/gpui/src/window.rs:5026`) — the *same* path OS events take.
  `DispatchEventResult { propagate, default_prevented }` (public) is
  returned to the caller.
- `Window::dispatch_keystroke(Keystroke, &mut App) -> bool`
  (`window.rs:4984`) — KeyDown with `with_simulated_ime()`, so character
  input works.
- `Keystroke::parse("ctrl-s")` — public (`platform/keystroke.rs:120`);
  fills `modifiers`, so modified input needs no global modifier state
  (which would be test-support-gated).
- Upstream's own test-support `simulate_mouse_move` / `TestAppContext::
  simulate_click` are thin wrappers over the same `dispatch_event` — proof
  the path is complete for hover, click, drag, and keys.

**Component tree dump — public, ungated, but incomplete:**

- gpui builds an AccessKit tree every frame: elements with an `.id()` and
  an `.a11y_role()` become nodes with bounds, roles, labels, focus
  mapping (`element.rs:362-383`, `window/a11y.rs`). gpui-component sets
  roles on all its interactive components (Button, Select, List, …), so
  stock UI is covered.
- `Window::debug_a11y_tree_json() -> Option<String>` (`window.rs:6122`)
  serializes the last frame's tree: ephemeral node ids (`a`, `b`, … `aa`),
  `accesskit_id`, `children`, role, label/description/value/placeholder/
  tooltip, selected/expanded/toggled, numeric/table props, and — debug
  builds only — provenance (`element_id`, `view` type name,
  `source_location`). Window-level `gpui_focus` + `active_descendant_focus`
  pointers, frame header (viewport size, scale factor, node count).
- **Gaps**: no per-node `bounds`, `focusable`, `focused` (data exists
  internally in `a11y.node_bounds` but isn't serialized) — #43.
- **Gate**: the tree is only built once an assistive-tech client has
  activated the adapter (`A11y::active_flag`; force-*disable* exists
  (`Application::new_inaccessible`), force-*enable* does not — #43).

**AT-SPI channel (Linux, Wayland + X11 identical):**

- Every non-force-disabled window registers an `accesskit_unix::Adapter`
  at creation (`a11y_init`, both `gpui_linux` wayland/x11 windows) and
  pushes `TreeUpdate`s each frame while active.
- Activation: an AT-SPI client reading the tree triggers the `activation`
  callback (`window.rs:1465-1471`), which sets `active_flag`, forces a
  frame, and from then on `debug_a11y_tree_json()` returns data.
  Deactivation is symmetric.
- The AT-SPI bridge exposes, per node (`accesskit_atspi_common`):
  - `Component` interface: `extents(CoordType)` — **window-relative
    coordinates are directly requestable**, sidestepping the
    Wayland-unknown-window-origin problem.
  - `Action` interface: `do_action` → `ActionRequest{Click}` → upstream
    `handle_a11y_action` (`window.rs:6167`) → synthesizes mouse down/up
    at the node's *internal* bounds center through `dispatch_event`.
  - `grab_focus` → `ActionRequest{Focus}` → `window.focus(handle)` —
    **pure focus, no click** (`window.rs:6187`).
- The `atspi` crate (zbus client) can do all of the above from a
  background thread in our own process.

**Support APIs:**

- `App::windows() -> Vec<AnyWindowHandle>` (`app.rs:1235`),
  `Window::bounds()`, `Window::viewport_size()` — public.
- `VisualTestAppContext::open_offscreen_window` (upstream) — proof of the
  offscreen pattern (`focus: false`, fully rendered) if invisible driving
  is ever wanted.

Prior art shaping the protocol: Chrome DevTools Protocol (`DOM.*`,
`Input.dispatchMouseEvent/Key*` — socket, dot-namespaced methods),
Android `uiautomator` (hierarchy dump + injection). Upstream gpui has no
DevTools (zed#42458 is a discussion, nothing shipped).

## 3. Decisions

### D1 — In-process debug server, feature-gated (the CDP model)

New workspace crate `crates/debug`, package `dashboard-debug`, lib + bin
(§D9). The lib is linked into `crates/app` behind a `debug-tools` cargo
feature (default-on: `cargo run` always serves; release packaging decides
later whether to keep it). The bin is the CLI client.

"Attach" = connect to the socket; the server starts with the app. No
ptrace, no re-launch dance.

### D2 — Transport: Unix socket, NDJSON, token auth

- Socket: `$XDG_RUNTIME_DIR/my-dashboard/debug.sock` (mode 0600), token
  file `debug.token` (0600, random 128-bit, regenerated per run) next to
  it — both under the per-user 0700 runtime dir; paths follow the
  existing `AppPaths` conventions (docs/0004).
- Wire format: newline-delimited JSON, one request/response per line —
  netcat-friendly (`nc -U`), trivially scriptable, pipelining for free.
  Method/response envelope: `{"id": n, "method": "input.dispatch",
  "params": {...}}` / `{"id": n, "result": {...}}` /
  `{"id": n, "error": {"code": c, "message": m}}`.
- Auth: first line MUST be
  `{"id": 0, "method": "auth", "params": {"token": "…"}}`; on mismatch the server
  answers `4001` and closes. The token is defense-in-depth (the
  socket is same-user anyway) and an explicit opt-in handshake.

### D3 — Threading: socket thread → mpsc → main-thread executor

gpui's `Window`/`App` are main-thread-only and not `Send`; the server
runs on its own thread with zero gpui types crossing. Each request is
parsed into a `Box<dyn FnOnce(&mut App) -> serde_json::Value + Send>`
plus a `oneshot` reply channel, sent over a `std::sync::mpsc` consumed by
a long-lived task spawned at startup via `cx.spawn` — the same
main-thread-marshalling pattern our services already use
(`window_handle.update(cx, …)`). AT-SPI calls (D4/D5/D7) run on the
socket thread's async runtime and don't touch gpui types.

### D4 — `tree.dump`: upstream JSON ⊕ AT-SPI enrichment, merged on `accesskit_id`

1. Ensure activation (D5).
2. On the main thread: `cx.windows()` → target window →
   `window.debug_a11y_tree_json()` (structure, semantics, provenance in
   dev builds).
3. On the socket thread: walk the app's AT-SPI tree
   (`CoordType::Window` extents; states incl. `focused`/`focusable`).
4. Merge per node on `accesskit_id`. The identity bridge, verified against
   accesskit_consumer 0.36: the bus object path is
   `/org/a11y/atspi/accessible/{adapter}/{(local_id << 64) | tree_index}`
   where `local_id` is exactly the dump's `accesskit_id`; the merge keys
   on the local id. The tree index (gpui uses a single tree; the adapter
   root's path is nothing but the index) is derived from the bus, never
   assumed. Result nodes carry:
   `id` (ephemeral), `accesskit_id`, `children`, `aria` (upstream),
   `bounds {x, y, w, h}` (px, window-relative, pre-scale-factor; AT-SPI's
   all-(-1) sentinel is reported as absence, not a rect),
   `focusable`, `focused` (AT-SPI states), plus the frame header and an
   `a11y_merge` counts object (`dump_nodes` / `enriched` /
   `unmatched_dump` / `unmatched_walk`) so tree drift between the two

Adapter index = window id (both follow window-creation order; verified
against `accesskit_atspi_common::AppContext::adapter_index`).
Single window in v1 (`params.window` optional; error if absent and
`cx.windows().len() != 1`).

### D5 — Activation: `ScreenReaderEnabled` gate, auto-enabled by the bridge

Verified against accesskit_unix 0.21.1 / zbus 5.13.2: the app's tree is
published **only while `org.a11y.Status.ScreenReaderEnabled` is true on
the session bus** — a client merely *reading* the bus activates nothing.
zbus property streams replay the current value at startup, so a flag
that is already true when the app launches works with zero action from
us.

`ensure_activated()` (first tree/node command) therefore:

1. connects to the session bus and asks `org.a11y.Bus` for the a11y bus
   address (absent → diagnostic `4004` naming the at-spi2 requirement);
2. reads `ScreenReaderEnabled` — **if false, sets it true** (the owner's
   auto-enable decision, #46 review). This is the session-wide "an
   assistive tech is present" signal; on compositors without an
   Orca-style watcher (e.g. Hyprland) it only makes accesskit-based apps
   publish their trees. On GNOME with Orca installed it could wake
   Orca — the known trade-off of auto-enable. A pleasant side effect:
   the dashboard itself becomes screen-reader-accessible while active.
3. connects to the a11y bus, finds our app among the desktop's embedded
   applications (matched on the executable name accesskit publishes
   under), and reads its root children — the touch that proves
   registration happened (bounded retries, 5 × 200 ms).

The flip is **restored to false on graceful app exit** (`app.run`
returning → `dashboard_debug::shutdown()` in `main`). A crash — or
`kill`, which never runs post-`run` code — leaves it true; inert on
this project's target sessions and documented here.

The connection is kept for the process lifetime (needed for D4/D7
anyway); `window.list` uses it opportunistically (`title`) but never
triggers activation — it stays the cheap, always-works command.

Dev environment: the dev shell provides `at-spi2-core` (daemons in
`libexec/`, put on `PATH` via shellHook). The running session bus does
not rescan service dirs at runtime, so the dev flow is:

    at-spi-bus-launcher --launch-immediately &   # once per session
    cargo run

The system-wide alternative stays `services.gnome.at-spi2.enable` +
re-login. No activation at startup — zero cost until first use.

### D6 — The seven commands

| Method | Params | Result |
|---|---|---|
| `tree.dump` | `{window?}` | merged tree JSON (D4) |
| `input.dispatch` | `{window?, event}` (below) | `{propagate, default_prevented}` for pointer/scroll/`key_up` (`DispatchEventResult`); `{handled}` for `key_down` (the `dispatch_keystroke` outcome — its ime wrapper doesn't expose the result struct) |
| `input.click_node` | `{window?, node: accesskit_id, button? = "left", click_count? = 1}` | `{ok: true}` |
| `input.focus_node` | `{window?, node}` | `{ok: true}` |
| `window.list` | `{}` | `[{id, title, bounds, viewport, scale_factor}]` — `title` from the a11y root label once activated, `null` before (gpui has no public title getter; `window.list` itself never triggers activation) |
| `window.geometry` | `{window?}` | one entry of the above |
| `app.logs` | `{since?, limit? = 200}` | `{lines, next_cursor}` (D8) |
| `app.quit` | `{window?}` (accepted, ignored — shutdown is app-wide) | `{ok: true}`, written before the exit: the ACK is queued, then a ~100 ms timer fires `cx.quit()` so the socket thread flushes first. Not a CLI verb; `session stop`'s internal mechanism (0055 D11) |

The seven, in full: `tree.dump`, `input.dispatch`, `input.click_node`,
`input.focus_node`, `window.list`, `window.geometry`, `app.logs`.

`input.dispatch` `event` is a tagged union mapping 1:1 to the
`PlatformInput` subset we need:

```jsonc
{"type": "mouse_move",   "x": 120, "y": 40}
{"type": "mouse_down",   "x": 120, "y": 40, "button": "left",
                         "click_count": 1, "modifiers": ["shift"]}
{"type": "mouse_up",     … same shape …}
{"type": "scroll_wheel", "x": 120, "y": 40, "dx": 0, "dy": -60}
{"type": "key_down",     "keystroke": "ctrl-s"}   // Keystroke::parse
{"type": "key_up",       "keystroke": "ctrl-s"}
```

Text entry = repeated `key_down`/`key_up` per character (ime simulation
kicks in via `dispatch_keystroke`); the CLI provides a `type` verb for
the loop. `input.click_node` and `input.focus_node` go through AT-SPI
`do_action`/`grab_focus` (D7) — coordinate-free, robust to layout
changes, and *purer* than coordinate math: upstream synthesizes the click
at the node's internal bounds center and routes focus through the real
`FocusHandle`. Nodes without an action (plain divs) → use
`input.dispatch` with coordinates from `tree.dump` bounds; that split is
inherent and documented in the CLI help.

### D7 — Node actions ride the AT-SPI channel

`click_node` resolves the AT-SPI object by `accesskit_id` (D4's identity
bridge: `(local_id << 64) | tree_index`), invokes `do_action(0)`;
`focus_node` calls `grab_focus`. Verified against
`accesskit_atspi_common::node::do_action`: **index 0 is always Click** —
the only action slot exposed — so there is no index ambiguity to pin.
The synthesized Click is a single left click; `button`/`click_count`
other than the defaults are rejected with `-32602` pointing at the
`input.dispatch`-with-coordinates fallback (the documented split for
plain elements). Both arrive in-process as `ActionRequest`s and are
handled by upstream gpui exactly as a screen reader would (§2).
Node-path misses map to `4005 UNKNOWN_NODE`; existing nodes without the
needed interface (plain divs) map to `4006 ACTION_UNAVAILABLE` with the
fallback hint.

### D8 — `app.logs`: 0008 extension, in-memory ring writer

`dashboard_core::logging::init` takes an extra-writers parameter
(`Vec<Box<dyn flexi_logger::LogWriter>>`, re-exported from the logging
module; empty by default — no core→debug dependency). Verified against
flexi_logger 0.31.8: extras tee off the primary file writer via
`log_to_file_and_writer` — every record that reaches the file (same
spec, same rotation, same stderr duplication) reaches each extra; the
`add_writer`/special-target mechanism is *not* used (it only sees
records whose target starts with `{name}`). Multiple extras fan out
through a tiny in-core `TeeWriter`.

The debug crate supplies the ring (`logs::RingLog`, last 4096 lines):
created in `main` *before* `logging::init` so the first line ("logging
started…") is captured, stored as structured `{level, target, message}`
— never a pre-formatted line — and served inline on the socket thread,
no main-thread round-trip. `app.logs {since?, limit? = 200}` returns
`{lines: [{cursor, level, target, message}], next_cursor}` where
`next_cursor` is a **continuation cursor** (last returned line's
cursor; unchanged when a batch is empty) — a truncated batch picks up
exactly where it stopped, and lagging past eviction simply loses old
lines. Pull-based in v1; no streaming/notifications.

### D9 — CLI: `dashboard-debug` bin, hand-rolled args

Reads the token file automatically; connects; prints results as
indented tree or JSON. Verbs mirror the protocol:

```
dashboard-debug tree [--json] [--window N]
dashboard-debug click <accesskit_id> [--window N]
dashboard-debug focus <accesskit_id> [--window N]
dashboard-debug key <keystroke> [--window N]  # one press: down+up
dashboard-debug type "hello" [--window N]     # per-char key loop
dashboard-debug mouse move|down|up <x> <y> # raw dispatch
dashboard-debug mouse scroll <x> <y> <dy>
dashboard-debug logs [-f]                  # drain ring; -f polls 500ms
dashboard-debug shell                      # REPL over the same verbs
```

docs/0055 extends this surface with the session + receipt verbs
(`session start|status|stop|env`, `wait-frame`, `tree
--snapshot/--diff/--tab-stops`, `--wait-frame` on drive verbs,
`screenshot`, `run <script>`, `doctor`) — same hand-rolled parser, same
exit-code contract.

No clap (hand parsing keeps the crate dep-free beyond
serde/serde_json + the atspi/zbus pair already in the lockfile).
Exit codes: `0` ok, `1` protocol/IO error (`error {code}: {message}`
on stderr), `2` usage. `type` maps control-ish chars to key names
(`space`/`tab`/`enter`); the tree outline prints `#accesskit_id role
"label" (x,y w×h)` with focus markers, `--json` prints the raw dump.

### D10 — Element hygiene: the dump is only as rich as our roles

Stock gpui-component widgets are covered upstream. Our own custom
elements MUST get `.id(...)` + a role + an **explicit accessible label**
where they're interactive (docs/0049 D1; Engineering rule; review-enforced
like the [DG] obligations — unrole'd interactive elements are invisible to
the dump and to `click_node`, and text children never become a node's
name, so labels are deliberate work). Icon-only buttons never rely on
their tooltip for the accessible name. State flags (`aria_selected`,
`aria_expanded`, `aria_toggled`) carry state where it means something.
`write_a11y_info` (public on `Element`) is the extension point for
module-specific attributes later (state.query follow-up, §7).

**Stale-frame caveat** (observed while verifying #311, formalized in
0049): the accessibility tree is only *built* while an AT-SPI client is
connected, and a one-shot `tree` dump returns the **retained last-built
frame**. Force a fresh build before interpreting a dump — `key tab` is the
cheap way; hover-only mouse moves do not repaint. Node ids also change
across unmount/remount of the same element (ids derive from the element
path), so re-dump before addressing a node that a navigation may have
re-created.

## 4. Risks

| Risk | Mitigation |
|---|---|
| Wayland window-origin confusion in bounds | `CoordType::Window` extents requested directly; human check on both Wayland and X11 (§6) |
| AT-SPI action-index mapping (Click slot) | Resolved: `do_action(0)` is always Click (only slot exposed); verified against accesskit_atspi_common 0.18.1 |
| Node-id ↔ bus-path drift | Resolved: bus id = `(accesskit_id << 64) \| tree_index`, tree index derived from the adapter root path (D4) |
| No at-spi2 bus on minimal sessions | Explicit diagnostic error `4004` (D5) naming the dev-shell launcher or the NixOS enable line; `at-spi2-core` shipped in the dev shell |
| Activation flakiness | Bounded retries in `ensure_activated` (5 × 200 ms discovery) + main-thread dump poll (up to 4 s); #43 tracks the upstream knob that would remove the workaround |
| Token/socket left behind on crash | Best-effort removal at exit; stale files are inert (socket dead, token useless without a listener) |

## 5. Testing

- Protocol serde round-trips for all seven methods (unit).
- Ring writer: capacity eviction, cursor monotonicity, `since` filtering
  (unit; ring is pure).
- Bounds-merge: pure merge function over fixture JSONs (upstream dump ⊕
  AT-SPI walk) — table tests incl. missing node on either side.
- `next_window_selection`-style pure helpers where they arise.
- Live end-to-end (socket → real window) is a human check (§6): needs a
  real session with a11y bus; not reproducible in `cargo test`.

## 6. Acceptance criteria

- Seven commands served over the socket exactly as specified (D2/D6);
  auth enforced; NDJSON pipelining works.
- `tree.dump` on the GitHub page returns the org `Select`, repo `List`
  rows, right-column GroupBoxes with bounds, roles, labels; dev builds
  include `source_location`s.
- `input.dispatch` mouse move triggers hover styling/tooltips **while the
  OS pointer stays where the user left it**; `key`/`type` drive the
  search inputs of Select/List.
- `input.click_node` opens the org Select menu and confirms an option;
  `input.focus_node` moves gpui focus (visible focus ring) without
  clicking.
- `app.logs` tails the 0008 log stream with cursors.
- Gates: fmt, clippy 0 warnings, `cargo test --workspace`.

## 7. Human checks

- [ ] On Wayland: `dashboard-debug tree` shows the full page with sane
      window-relative bounds (spot-check sidebar width, list row heights).
- [ ] On X11: same; bounds consistent with the Wayland numbers.
- [ ] While watching the OS cursor (don't touch mouse/keyboard): run
      `mouse move` sweep + `click` on a button — app reacts (hover,
      click), OS cursor never moves, no focus steal, typing goes nowhere
      else.
- [ ] `click` the org Select trigger → menu opens → `click` a row →
      repo list repopulates (same flow as a real user).
- [ ] `key tab` several times — focus ring walks the page;
      `focus <select-node>` jumps focus there directly, no click side
      effects.
- [ ] `type "rust"` into the repo List search filters rows; `logs -f`
      streams live events; wrong token → connection closed.
- [ ] Activation: with the dev-shell bus up (`at-spi-bus-launcher
      --launch-immediately &`), first `tree.dump` flips
      `busctl --user get-property org.a11y.Bus /org/a11y/bus org.a11y.Status
      ScreenReaderEnabled` to `b true`; closing the app window normally
      flips it back to `b false` (graceful-exit restore; a `kill` leaves
      it true by design — crash path).
- [ ] Without a bus (or before any tree command), `window.list` still
      works and `title` is `null` — no activation side effects.
- [ ] Run without an a11y bus entirely (no launcher, no system module) →
      tree/node commands return the `4004` diagnostic naming both fixes;
      app otherwise unaffected.

## 8. Follow-ups

- MCP bridge (rmcp shim over the socket) — only if CLI falls short
  (owner decision above).
- `state.query` — module-defined entity dumps via `write_a11y_info` /
  a registry (D10 extension point).
- `screenshot` command over the socket.
- Offscreen "shadow window" driving (upstream `VisualTestAppContext`
  pattern) for fully invisible automation.
- macOS: same in-process core; AT-SPI self-connection swaps for
  NSAccessibility (activation + node actions); owner-deferred.
- Upstream knobs that would delete our workarounds — #43.
