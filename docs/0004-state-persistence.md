# 0004 — Phase 3: State Management & Persistence (XDG / native dirs)

- **Date:** 2026-08-29
- **Phase:** 3 (Foundations for real modules)

## 1. Scope & goals

Give the app durable state that survives restarts, stored in the
OS-appropriate locations, with a small set of storage primitives that
later modules (0005 GitHub module first) build on.

**In scope (v1):**

1. A `paths` layer in `dashboard-core` resolving config/data/state/cache
   roots per OS (XDG on Linux, native equivalents on macOS; Windows out
   of scope this phase per owner).
2. Atomic, debounced JSON persistence primitives (`store` layer).
3. Persist the sidebar toggle (`Navigator.sidebar_collapsed`) → state dir.
   (The titlebar toggle was removed in review — decoration-driven chrome;
   see §4.3 and the 0002 D7 supersession note.)
4. Extend the module contract with a per-module data directory; wire the
   Notes module to persist its notes → data dir (first real user data).
5. A `DASHBOARD_STATE_DIR` env override for dev/tests/Nix purity.

**Out of scope (recorded, not forgotten):**

- Settings **UI** and a user-facing `settings.json` (config dir is
  reserved; a settings store lands with the first real setting).
- Window geometry persistence (see Q7).
- Secrets storage (tokens for the GitHub module — separate decision,
  0005).
- Windows support (code stays OS-neutral via the `dirs` crate; we simply
  do not build/test/target it).

## 2. Open questions — NEEDS YOUR INPUT

Silence rule as in 0003: if you merge the PR without comment, the
recommendations below are accepted as decisions.

### Q1 — Path-resolution crate: `dirs` 6.0?

**Recommendation: yes.** `dirs` 6.0.0 is already in our `Cargo.lock`
(pulled transitively by zed), and it is exactly what Zed's own
`crates/paths` uses (`dirs.workspace = true`, zed `8cfa848c`
`Cargo.toml:611`, `crates/paths/Cargo.toml:19`). It implements the XDG
spec on Linux and Apple's standard-directory guidelines on macOS.
Alternatives (`directories`, `xdg`) add a second path crate to the tree
for no gain.

### Q2 — macOS: where does each root live? (state has no native slot)

`dirs` returns on macOS (verified, docs.rs 6.0.0 fn pages):

| Root | Linux | macOS |
| --- | --- | --- |
| config | `$XDG_CONFIG_HOME` else `~/.config` | `~/Library/Application Support` |
| data | `$XDG_DATA_HOME` else `~/.local/share` | `~/Library/Application Support` |
| state | `$XDG_STATE_HOME` else `~/.local/state` | **`None`** |
| cache | `$XDG_CACHE_HOME` else `~/.cache` | `~/Library/Caches` |

macOS has no separate state convention (Apple merges everything durable
into Application Support). Zed works around this by using
`~/.local/state/zed` on macOS — a non-native XDG-ism (zed `8cfa848c`
`crates/paths/src/paths.rs:169-192`).

**Recommendation:** follow the owner requirement (native locations on
macOS): state falls back to the **data root** on macOS (i.e. Application
Support), not Zed's `~/.local` deviation. On Linux all four roots are
distinct XDG dirs.

### Q3 — Serialization format: pretty JSON?

**Recommendation: `serde` + `serde_json`**, pretty-printed, one file per
concern (`shell.json`, `modules/notes/notes.json`). Both crates are
already in `Cargo.lock` (serde 1.0.229, serde_json 1.0.151 — no new
downloads). Human-readable/diffable, matches Zed's `settings.json`
choice, and round-trips through the same structs we keep in entities.
TOML would add a hand-edit friendliness we do not need yet (no user
settings file this phase).

### Q4 — Config dir contents in v1: nothing?

**Recommendation:** create nothing under the config root until a real
user-facing setting exists. v1 persists only *state* (shell UI toggles)
and *data* (notes). This keeps "user-editable config" semantically clean
when it appears (first candidate: default titlebar preference, or GitHub
module settings in 0005).

### Q5 — Module contract change: pass a data dir into `init`?

Current: `Module::init(nav, window, cx)`. Modules currently have no
sanctioned place to put durable data.

**Recommendation:** extend to
`init(nav: &Entity<Navigator>, window: &mut Window, paths: &AppPaths, cx: &mut App)`
where `AppPaths::module_data_dir(id: &str)` returns
`<data>/modules/<id>/`. Breaking change to our own modules only (clock,
notes) — both updated in the same PR. The id matches `ModuleMeta::id`
and is filesystem-sanitized by the paths layer.

### Q6 — Save policy: debounced atomic writes

**Recommendation:**

- **Atomicity:** write to `<file>.tmp` then `std::fs::rename` over the
  target (atomic on POSIX for same-filesystem renames — standard
  behavior Zed also relies on with its `settings.json` backup dance).
- **Debounce:** 500 ms trailing-edge per store. The debounce task is
  cancelled/rescheduled on every change (generation supersession — gpui
  tasks cannot be cancelled). **Amended (#283, docs/0045 §9):** the
  pending payload is serialized at schedule time and can be landed
  synchronously — `DebouncedSave::flush` + `flush_pending_on_quit()`,
  wired to `on_app_quit` in `main.rs`. The final 500 ms window is no
  longer lost at quit; the §7 limitation stands only for a hard kill.
- **Corrupt file recovery:** if parse fails at load, rename the file to
  `<name>.invalid-<ts>` and continue with defaults (never crash on bad
  state; log to stderr). Zed ships the same idea (`settings_backup_file`).
- **Load:** synchronous at startup, before entities are built (files are
  tiny; blocking the main thread for <1 ms beats an async
  defaults-then-swap flash for UI state).

### Q7 — Window geometry: persist?

**Recommendation: skip in v1.** On Wayland the compositor (not the app)
assigns position — `WindowBounds` restore is unreliable (X11/macOS
only, effectively), and size-restore fights our responsive grid less
than a wrong position fights the compositor. Revisit with packaging
(roadmap: build/distribution phase).

### Q8 — Env override name and semantics

**Recommendation:** `DASHBOARD_STATE_DIR=<dir>` redirects **all** roots
into `<dir>/{config,data,state,cache}` — same shape as Zed's
`set_custom_data_dir` (`paths.rs:103-120`). Used by tests, smoke runs
and (potentially) the Nix package to isolate the app from the user's
real home. Empty/unset = normal OS locations.

## 3. Verified facts (sources at pinned revs: zed `8cfa848c`, gpui-component `1081d2b2`)

1. **Zed resolves paths via `dirs` 6** — `Cargo.toml:611`
   `dirs = "6.0"`, workspace-inherited by `crates/paths`.
2. **Zed's per-OS mapping** (`crates/paths/src/paths.rs`):
   - `config_dir()`: Linux `$XDG_CONFIG_HOME`/`zed` (honoring Flatpak
     vars); macOS **`~/.config/zed`** (deliberate cross-platform
     deviation — we do NOT copy this; owner wants native macOS).
   - `data_dir()`: macOS `~/Library/Application Support/Zed`;
     Linux `$XDG_DATA_HOME`/`zed`.
   - `state_dir()`: Linux `$XDG_STATE_HOME`/`zed`; macOS
     `~/.local/state/zed` (non-native; rejected for us — Q2).
   - `cache_dir()` via `dirs::cache_dir()`.
3. **XDG spec** (freedesktop basedir-spec, fetched 2026-08-29):
   `$XDG_DATA_HOME` default `$HOME/.local/share`; `$XDG_CONFIG_HOME`
   default `$HOME/.config`; `$XDG_STATE_HOME` default `$HOME/.local/state`
   — and **state is defined as** "current state of the application that
   can be reused on a restart (view, layout, …)" plus history/logs:
   exactly our shell toggles. Config = user configuration; data =
   user-specific data files.
4. **`dirs` 6.0.0 per-OS table** (docs.rs fn pages, fetched 2026-08-29):
   as in Q2 above; `state_dir()` returns `None` on macOS/Windows.
5. **`dirs`/`serde`/`serde_json` already in our `Cargo.lock`**
   (6.0.0 / 1.0.229 / 1.0.151) — declared as direct deps only; no new
   lockfile entries.
6. **gpui async/shutdown reality (0003 §3.12):** detached tasks die at
   shutdown; there is no reliable quit-hook flush — hence Q6's
   "shutdown may drop the last debounce window" limitation.
   *(Amended by #283: gpui-pre does expose `on_app_quit`, and the
   debouncer now parks the serialized payload so the quit hook can
   write it synchronously; the limitation now applies to hard kills
   only.)*
7. **Current shell state to persist:** `Navigator.sidebar_collapsed`
   (`crates/core/src/nav.rs`), `DashboardShell.show_titlebar`
   (`crates/core/src/shell.rs`, OS-dependent default per 0002 D7).


### 3.1 Implementation corrections (added during implementation)

1. **gpui tasks cannot be cancelled** — `Task` has no cancel and dropping
   detaches. Q6's "cancelled/rescheduled" is realized as a generation
   counter (`DebouncedSave`): each schedule bumps it; after the delay
   only the latest schedule writes (superseded timers no-op). Same
   trailing-edge semantics, ≤1 write per quiet window.
2. **`gen` is a reserved keyword** in edition 2024.
3. **`cx.subscribe` callbacks are windowless** (`&mut T`, event,
   `&mut Context`) while `InputState::set_value` requires `&mut Window`
   — Enter-to-add bridges via `AnyWindowHandle::update` captured at
   construction (the 0003 §3.12 clock pattern).
4. **The single-line input at rev `1081d2b2` is `Input` + `InputState`**
   (`gpui_component::input::{Input, InputEvent, InputState}`); the old
   `TextInput` was refactored away. Events via subscribe:
   `InputEvent::{Change, PressEnter{..}, Focus, Blur}`.
5. **`Button` variants (`.primary()`/`.ghost()`) are in `ButtonVariants`**
   (trait import required; icon-only buttons still need `.tooltip(...)`
   per [DG]).
6. `Context<'a, T>` satisfies `AnyWindowHandle::update`'s context bound —
   window access from entity contexts works (refines 0003 §3.12's
   note that `AsyncApp::update` returns `()`).
7. **Window-level key bindings were dead from first launch (0002
   regression surfaced by owner on Hyprland)**: gpui dispatches key
   actions from the focused node UP the dispatch tree; with nothing
   focused, dispatch starts at the tree root and no `on_action` on a
   descendant fires — `ctrl-shift-t` did nothing regardless of window
   focus state. Fix: the shell root is now a focusable node
   (`.id(..).track_focus(&focus_handle)`) and grabs keyboard focus at
   boot (`window.focus(..)` in `DashboardShell::new`); inner elements
   (Notes input) still take focus normally, and bubbling then reaches
   the shell's handlers. Verified end-to-end with synthetic input
   (`wtype` + Hyprland focus cycling): action fires, toggle persists.

   Tooling note for this dev box: `hyprctl dispatch <d> <colon-arg>`
   is broken on this Hyprland 0.56.2 build (Lua IPC parser); argless
   dispatchers (`cyclenext`) still work — cycle until `activewindow`
   matches, then inject keys with `nix shell nixpkgs#wtype`.
8. **Chrome became decoration-driven (owner decision, this PR's review)** —
   the `ctrl-shift-t` toggle, `ToggleTitleBar` action, `show_titlebar`
   persistence and the titlebar-less ghost close button are all removed.
   Rule: `Client` → app draws full title bar (gpui-component's
   `WindowControls` render min/max/close there, capability-gated);
   `Server` → app draws nothing at all. Platform facts verified in gpui
   source: the platform default `window_decorations()` impl returns
   `Server` (macOS → native bar with traffic lights does the work; no
   custom bar needed there); X11 returns `Server` unless the WM supports
   CSD; Wayland compositor answers via `zxdg_toplevel_decoration`
   Configure (gpui requests no preference). Zero boot flash verified on
   Hyprland — the decoration answer rides the initial surface configure,
   before the first frame. `ShellState` shrank accordingly; the boot-focus
   fix from item 7 stays (window.focus at init, keyboard hygiene for
   future bindings).
## 4. Design

### 4.1 `dashboard-core/src/paths.rs`

```rust
pub struct AppPaths { config: PathBuf, data: PathBuf, state: PathBuf, cache: PathBuf }

impl AppPaths {
    /// Resolve OS roots once at startup. `DASHBOARD_STATE_DIR` (if set,
    /// non-empty) redirects all roots under it. Panics only if the OS
    /// cannot answer (no $HOME) — same stance as Zed.
    pub fn init() -> &'static AppPaths;
    pub fn config_dir(&self) -> &Path;   // <config>/my-dashboard
    pub fn data_dir(&self) -> &Path;     // <data>/my-dashboard
    pub fn state_dir(&self) -> &Path;    // <state>/my-dashboard  (macOS: data)
    pub fn cache_dir(&self) -> &Path;    // <cache>/my-dashboard
    /// <data>/my-dashboard/modules/<sanitized-id>/ — created on demand.
    pub fn module_data_dir(&self, id: &str) -> PathBuf;
}
```

- Linux: `dirs::{config_dir, data_local_dir, state_dir, cache_dir}` +
  `my-dashboard` (lowercase, Zed's Linux convention).
- macOS: `dirs::config_dir()`/`data_local_dir()` →
  `…/Application Support/my-dashboard`; state = data root; cache =
  `dirs::cache_dir()`/`my-dashboard`.
- Windows: unreachable by policy (not built/tested); if ever compiled,
  `dirs` still answers sanely.
- `module_data_dir` sanitizes id (`[A-Za-z0-9-]`, else `-`), creates the
  directory (parents included) on first call.

### 4.2 `dashboard-core/src/store.rs` — persistence primitives

```rust
/// Load a JSON file. Ok(None) = missing. Parse failure → rename to
/// `<file>.invalid-<unix-ms>` + Ok(None) (defaults win, never crash).
pub fn load_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>>;

/// Pretty JSON, write `<file>.tmp`, fsync, atomic rename.
pub fn save_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()>;

/// Trailing-edge debouncer. Owns the pending gpui Task; reschedule on
/// every change. Usage from an entity observer:
/// `self.save = paths::save_debounced(path, &self.snapshot(), 500.ms(), cx);`
pub fn save_debounced<T: Serialize>(path: PathBuf, value: T,
    delay: Duration, cx: &App) -> Task<()>;
```

Errors are logged (`eprintln!` until we adopt `tracing`, if ever) and
swallowed — persistence failure must not take the UI down ([DG] spirit:
no dead ends).

### 4.3 Shell state (state root)

`state/my-dashboard/shell.json`:

```json
{ "sidebar_collapsed": false }
```

- Chrome is no longer persisted at all (owner decision in this PR's
  review, superseding 0002 D7: chrome is decoration-driven, resolved
  per render from `window.window_decorations()` — Client → full title
  bar; Server → nothing, not even a close X). `ShellState` shrank to
  the sidebar toggle; older files with a `show_titlebar` field still
  load (serde ignores unknown fields).
- The sidebar toggle handler calls `save_debounced` (500 ms) with the
  snapshot.
- Missing/corrupt file → defaults (sidebar expanded).

### 4.4 Module data (data root) + Notes wiring

- `Module::init` signature change per Q5; `AppPaths` passed in.
- Notes module: `NotesState` (already an entity) gains serde on its
  note structs; every mutation schedules
  `save_debounced(module_data_dir("notes")/notes.json, snapshot, 500 ms)`;
  load happens inside `init` before the entity is created.
- Clock module: nothing to persist (ignores the new arg) — proving the
  contract is opt-in.

### 4.5 What persists where — summary

| Data | Root | File | Trigger |
| --- | --- | --- | --- |
| sidebar toggle | state | `shell.json` | sidebar toggle handler, 500 ms debounce |
| notes content | data | `modules/notes/notes.json` | note mutations, 500 ms debounce |
| (future) user settings | config | `settings.json` | first real setting (0005+) |
| (future) tokens/secrets | TBD | — | 0005 decision |
| (future) HTTP cache | cache | — | GitHub module |

### 4.6 Crate layout delta

```
crates/core/src/paths.rs   (new)
crates/core/src/store.rs   (new)
crates/core/src/module.rs  (init signature)
crates/core/src/shell.rs   (state seed + save hooks)
crates/core/src/nav.rs     (sidebar_collapsed seeding)
crates/modules/notes/…     (serde + load/save)
crates/modules/clock/…     (init signature only)
crates/app/src/main.rs     (AppPaths::init, load shell.json)
```

Workspace deps: `serde` (derive), `serde_json`, `dirs` — all already in
`Cargo.lock`; declared in `dashboard-core` only.

### 4.7 [DG] compliance (unchanged obligations)

No new chrome this phase; the two touched surfaces (sidebar toggle,
titlebar toggle) keep existing tooltips. New rule adopted: **persistence
must never surface a raw error dialog** — degraded silently + stderr
log; a future status surface may show "state not saved" chips.

## 5. Implementation plan

1. `paths.rs` + unit tests (env override, sanitization, macOS fallback
   logic via cfg-independent pure fns where possible).
2. `store.rs` + unit tests (roundtrip, missing→None, corrupt→None +
   quarantine file, atomic replace leaves no `.tmp`).
3. Shell state: seed + debounce saves; smoke-test via
   `DASHBOARD_STATE_DIR=$(mktemp -d)` toggle→restart→assert.
4. Module contract change + Notes persistence (same test shape).
5. Gates (§6).

## 6. Acceptance criteria

Automated gates (CI = the PR check suite, run locally):

- [ ] `cargo build --workspace`
- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace -- -D warnings`
- [ ] `cargo test -p dashboard-core` (new tests green)
- [ ] Smoke run (timeout-exit 124, empty log) with isolated
      `DASHBOARD_STATE_DIR`

Human checks (owner, desktop):

- [ ] Toggle sidebar collapsed + titlebar-less, quit, relaunch: both
      survive (Linux, state under `~/.local/state/my-dashboard/`).
- [ ] Type a note, quit, relaunch: note content survives
      (`~/.local/share/my-dashboard/modules/notes/notes.json`).
- [ ] Corrupt `shell.json` by hand (e.g. `{`) → app launches with
      defaults, old file parked as `.invalid-*`, no crash.
- [ ] Without `DASHBOARD_STATE_DIR`, no files land outside the XDG
      roots (check `~/.config/my-dashboard` absent).

## 7. Risks & deferred

- **Shutdown data loss window** (≤500 ms debounce tail; gpui has no
  quit-flush hook — 0003 §3.12). Accepted for UI state; Notes save
  cadence may tighten later (per-keystroke would be wasteful).
- **Atomic rename** assumes tmp file on the same filesystem — it is
  created next to the target, so always true.
- **Flatpak/sandbox vars** (`FLATPAK_XDG_*`) honored by Zed; we defer —
  not targeting Flatpak yet (packaging phase).
- **Config-dir mutiny risk:** when `settings.json` arrives it must not
  collide with state semantics — Q4 keeps the line bright.
- Windows: untouched by policy.

## 8. References

- XDG Base Directory Specification — freedesktop, fetched 2026-08-29.
- `dirs` 6.0.0 fn pages (config/data_local/state/cache) — docs.rs,
  fetched 2026-08-29.
- Zed `crates/paths/src/paths.rs` @ `8cfa848c` (per-OS mapping,
  custom-dir precedent, backup-file pattern).
- gpui-component Design Guides ([DG]) — repo docs, adopted 0002.
