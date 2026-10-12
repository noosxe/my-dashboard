# 0055 — Live Debug Sessions (isolated headless verification environment)

Extends the agent debug bridge (docs/0010) with a scripted, isolated,
**live** verification environment: one command owns a headless
compositor + app instance + discovery wiring, so UI-facing PRs stop
hand-rolling `/tmp` scratch and fighting the host session before they
can fight the UI. Filed as #349; everything below is sourced from real
verification sessions (#342–#344, #347) plus the facts in §2.

## 1. Problem

The bridge is the sanctioned verification path (AGENTS.md), but every
session paid an environment tax first:

1. **Host compositor coupling.** A dev instance on the host Wayland
   session can land on a hidden workspace; compositors suspend frame
   callbacks for non-visible surfaces. Bridge input then *lands* while
   `tree` serializes a stale frame forever — nothing errors, it just
   silently verifies nothing (hit live while verifying #347).
2. **No process isolation by default.** Verification runs risk real
   state; concurrent instances collide on socket/token discovery.
   `DASHBOARD_STATE_DIR` (redirects every `AppPaths` root, including
   the runtime dir that hosts `debug.sock`/`debug.token`) is the
   isolation knob, but nothing propagates or records it — the
   app/CLI pair must be given it consistently by hand.
3. **Recovery is manual archaeology.** Nothing recorded which env an
   instance ran with, so "the app is broken" / "the app isn't
   drawing" / "the CLI is talking to the wrong instance" took an hour
   to tell apart.
4. **Receipts are ad-hoc greps.** "Did my click re-render?" is a
   sleep-then-hope `tree` poll; PR receipts are pasted transcripts —
   unreviewable and unreproducible.

The escape hatch proven repeatedly this quarter — a throwaway
**headless sway** + `DASHBOARD_STATE_DIR` — works and touches nothing
of the owner's session. This doc makes it a first-class workflow:
`dashboard-debug session`, frame-aware waiting, named snapshots and
diffs, screenshots, replayable scripts, a doctor, and scripted
teardown hygiene.

## 2. Non-goals

- **No OS-level input injection** anywhere (xdotool/ydotool/enigo) —
  host-untouched verification is the point (AGENTS.md rule).
- **No app-side a11y exposure work** — #348 (upstream
  `SidebarMenuItem`) and #351 (upstream `DataTable`) stay separate.
- **Not a test framework.** Unit/integration tests stay in-tree;
  this is the interactive/e2e layer. A CI smoke job borrowing the
  script grammar is a *possibility* this enables, deliberately not
  built here (§8).
- **No protocol redesign.** NDJSON request/response (0010 D2) stays;
  the only wire additions are one command (`app.quit`, D11) and
  nothing else — frame numbers, bounds, focus states and merge
  counters already ride `tree.dump`.
- **No multi-window choreography** — one app window per session, like
  the bridge today.

## 3. Verified facts

- **Headless sway works here, GPU-free**: `WLR_BACKENDS=headless
  WLR_LIBINPUT_NO_DEVICES=1 sway -c <conf>` with a config carrying
  `output HEADLESS-1 mode <WxH>` — used for every receipt in
  #342–#344/#347 verification. Frames flow unconditionally (no
  visibility gating), the host desktop is untouched. sway 1.12 and
  grim are in the pinned nixpkgs.
- **The app renders on demand** (gpui semantics): `frame_number`
  advances only when something changes. A frame that never advances
  under input means "input landed, nothing drew" — exactly the
  hidden-workspace signature. This makes the frame number a liveness
  probe, not just a monotone counter.
- **`tree.dump` already carries everything the new tooling needs**:
  the `frame` header (`frame_number`), per-node `bounds`, AT-SPI
  `focusable`/`focused`, and the `a11y_merge` drift counters (0010
  D4). No new dump fields are required.
- **`DASHBOARD_STATE_DIR` redirects all five roots** —
  `{config,data,state,cache,runtime}` land under it (paths.rs), so
  the bridge socket/token discover automatically once the env var is
  set for *both* app and CLI. Proven in every session receipt.
- **The keychain pins us to the host session bus**: the GitHub module
  stores credentials via `keyring` with `sync-secret-service` — the
  Secret Service lives on the session bus. A private per-session
  `dbus-daemon` would hide the credential and quietly break
  authenticated surfaces (and it is precisely the host keyring +
  isolated state dir combo that let verification sessions exercise
  real fetch flows). So the session bus is **shared** with the host,
  and isolation must handle the one thing the bridge leaks onto it:
  the `ScreenReaderEnabled` flip (0010 D5).
- **AT-SPI staleness is real but rare**: after long-lived sessions,
  `click_node`/`focus_node` can start returning `4005`/`4006` while
  a fresh instance enriches everything (observed mid-#342). Falling
  back to coordinates is the pragmatic answer, not bus debugging.
- `at-spi-bus-launcher` may only need starting once per dev shell
  (AGENTS.md connect flow); `session start` ensures it either way.

## 4. Decisions

### D1 — A session is a managed directory plus `session.json`

New CLI concept: a named **debug session**. Root:

```
$XDG_RUNTIME_DIR/dashboard-debug/sessions/<name>/     (fallback: /tmp)
├── session.json      ← the record (below)
├── state/            ← DASHBOARD_STATE_DIR for the app (all AppPaths
│                       roots, incl. the bridge runtime dir)
├── app.log           ← the app's stdout+stderr (impl. note D1)
├── sway.log          ← the compositor's (impl. note D1)
├── a11y.log          ← only when we spawned the a11y bus (D3)
├── screenshots/      ← D10 output
├── snapshots/        ← D7 output
├── transcripts/      ← D12 run logs
```

Implementation note (#357/#356): `screenshots/` sits at the session
root as drawn above; `snapshots/` and `transcripts/` live one level
down under `state/` (they are keyed off the resolved state dir so a
default — session-less — attach gets the same layout under its own
runtime parent).

`session.json` records everything the archaeology of problem 3
wanted, and everything later verbs must reconstruct:

```jsonc
{
  "name": "bridge-verify",
  "created_at": 1739990000,
  "binary": "target/debug/my-dashboard",
  "size": [1920, 1080],
  "app_pid": 4242, "sway_pid": 4241,
  "a11y_launcher_pids": [4240],        // only PIDs we spawned (D3)
  "wayland_display": "wayland-3",      // resolved sway socket name
  "state_dir": "…/sessions/bridge-verify/state",
  "screen_reader_at_start": false,     // D3 restore target
  "frame_at_ready": 41                 // first successful dump
}
```

Implementation notes (sessions core, #355): the record is written
*incrementally* during `start` (each milestone rewrites the file), so a
crashed start is still stoppable — `app_pid`/`sway_pid`/
`wayland_display`/`frame_at_ready` are nullable until their milestone
lands, and `binary` is recorded as an absolute path. The three named
logs above are what makes a failed `start` diagnosable (the failing step
names its log).

Names MUST match `^[a-z0-9][a-z0-9-]{0,31}$` (path-safety, greppable
receipts). `session start` fails if the directory exists — stale
sessions are inspected (`session status <name>`) and removed
(`session stop`), never silently reused. `XDG_RUNTIME_DIR` as root
means reboot cleans up; `session status` with no argument lists every
session with a one-line health summary.

### D2 — Host-compositor decoupling: managed headless sway

Each session runs its own sway with the headless backend:

1. `session start` writes `<session>/sway.conf` — `output HEADLESS-1
   mode <W>x<H>` (default **1920×1080**, `--size WxH` override) and
   nothing else.
2. Spawns `sway -c <session>/sway.conf` with
   `WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1`, inheriting the
   dev shell env (fonts, `LD_LIBRARY_PATH`).
3. Resolves the display socket by diffing `wayland-*` entries in
   `$XDG_RUNTIME_DIR` before/after (wlroots picks the first free
   `wayland-N`; the name is recorded, never assumed), bounded by a
   (implementation note, #355: wlroots drops a `<socket>.lock` sidecar
   next to the socket — the diff counts sockets only, or every start
   would read as two new entries)
   10 s timeout.
4. The app is spawned with `DASHBOARD_STATE_DIR=<session>/state`,
   `WAYLAND_DISPLAY=<recorded socket>` — everything else inherited.

One compositor per session keeps windows independent; killing it in
`stop` cannot affect the host session (sway's own socket is scoped to
its process tree). Multiple concurrent sessions are the expected
case, not an edge.

### D3 — Host session bus stays; the a11y flag is record-and-restore

No private DBus session (§3: the keychain needs the host Secret
Service). The one host-visible side effect of the bridge —
`org.a11y.Status.ScreenReaderEnabled` flipping true on activation
(0010 D5) — is handled transactionally by the session instead:

- `session start` records the flag's current value in
  `session.json` **before** any bridge call can flip it.
- `session stop` restores the recorded value over the session bus
  using the crate's existing zbus stack (no `busctl` dependency).
- Restore targets the *recorded* value, never a hard `false` — an
  owner actually running a screen reader must not have it switched
  off by our teardown.
- Crash-safety: `doctor` (D13) reports drift ("flag is true but this
  session recorded false — run `session stop` or reset manually"),
  which is the honest limit once the process holding the record died.

Kill discipline: `stop` (and `doctor`) only signal PIDs **recorded in
`session.json` as spawned by us** (`app_pid`, `sway_pid`,
`a11y_launcher_pids`). The AT-SPI bus/registry from
`at-spi-bus-launcher` MAY outlive the session (today's documented
dev-shell flow treats it as once-per-shell); only instances we
spawned are ours to kill.

### D4 — Discovery: `--session` on every verb, no env prefixes

A global `--session <name>` flag (or `DASHBOARD_DEBUG_SESSION` env)
resolved **before** connecting: the CLI reads `session.json`, exports
`DASHBOARD_STATE_DIR` in its own process (token/socket discovery then
follows `AppPaths` exactly as today), and verifies the socket exists
under the recorded state dir — a clear "app is down" error instead of
the old silent wrong-instance attach. Precedence: flag > env var >
today's default discovery (attaching to a real run stays fully
supported and unchanged).

`session env <name>` prints the exact `env` prefix for ad-hoc use
(`DASHBOARD_STATE_DIR=… WAYLAND_DISPLAY=…`), for humans driving the
binary directly; scripts and receipts use the flag.

### D5 — Lifecycle verbs

```
dashboard-debug session start [--name n] [--binary path] [--size WxH]
                              [--timeout secs]
dashboard-debug session status [name]
dashboard-debug session stop <name> [--keep]
dashboard-debug session env <name>
```

`start` sequence: validate name → create the session dir → ensure the
a11y bus (`at-spi-bus-launcher --launch-immediately` if
`org.a11y.Bus` is absent; spawned PIDs recorded) → record
`ScreenReaderEnabled` → write `sway.conf`, start sway, resolve the
socket → start the app → poll the bridge (socket + auth +
`window.list`) until the `--timeout` (default 30 s) → one `tree.dump`
to record `frame_at_ready` → print the summary block (state dir,
socket, PIDs, frame) and exit 0. Failure at any step: stop what was
started (D3 order), remove the session dir, exit 1 with the failing
step named.

`stop` sequence: **graceful app exit first** (`app.quit`, D11; wait
≤5 s) → `SIGTERM` to the app (≤3 s) → `SIGKILL` (last resort; the a11y
restore in the next step still covers it) → restore
`ScreenReaderEnabled` to the recorded value → `SIGTERM` sway → delete
the session tree **unless `--keep`**. Default-purge matches the
AGENTS.md scratch rule; receipts belong in the PR body (inline
snapshots/screenshots or `--keep` + commit what matters before
reboot). `stop` never returns while it owns a `logs -f` child (it
spawns none; `doctor` flags strays, D13).

Implementation notes (sessions core, #355): `app.quit` arrives with
step 2 — until then stop is SIGTERM → SIGKILL directly. Before
signalling, every recorded PID is verified against its `/proc/<pid>/cmdline`
(argv[0] equals the recorded absolute binary path; basename match for
sway / at-spi-bus-launcher): a reused PID that no longer looks like ours
is reported and skipped, never signalled. A corrupt or missing
`session.json` refuses to stop (manual removal named) — an interrupted
`start` unwinds itself, so a recordless directory is stale by
construction.

`status` output is the JSON record plus liveness booleans (PIDs
alive, socket present) — the one-glance answer to "which instance was
that".

### D6 — Frame-aware determinism: `wait-frame`, `--wait-frame`

`tree.dump`'s `frame_number` (0010 D4) becomes the synchronization
primitive:

- `dashboard-debug wait-frame [--above N] [--timeout ms]` — polls
  `tree.dump` (default 100 ms interval) until `frame_number` exceeds
  `N` (default: the value from an immediate first dump), then prints
  the new number. Exit 1 on timeout with the last-seen number —
  *input landed but nothing drew* surfaces as a deterministic
  failure, which is precisely the hidden-workspace class.
- `--wait-frame[=ms]` on `click` / `focus` / `key` / `type` /
  `mouse`: dispatch, then wait for a frame above the pre-dispatch
  number (default timeout 2000 ms). Receipts replace sleep-and-hope
  with this flag; the command's output states "frame 41 → 57".

On-demand rendering (§3) is the semantic contract: a frame advances
when the app drew something; a no-op interaction timing out is
*correct* feedback, and the flag's failure mode is the diagnostic.

Implementation notes (#356): defaults — verb timeout 5000 ms, drive-flag
budget 2000 ms, poll interval 100 ms; both timeout paths exit 1 with the
last-seen frame number. The drive flag's pre-dispatch dump failing skips
the receipt (the dispatch still runs) rather than fabricating one.
### D7 — Named snapshots and structural diff on `tree`

```
dashboard-debug --session <n> tree --snapshot <name>
dashboard-debug --session <n> tree --diff <a> <b> [--json]
```

`--snapshot` writes `snapshots/<name>.json`: `{name, frame_number,
taken_at, nodes: [{accesskit_id, role, label, bounds, focusable,
focused}]}` — the merged dump flattened to the fields receipts reason
about. `--diff` compares two snapshots **keyed on `accesskit_id`**
(stable for the run) and prints a human summary first —
`frame 41 → 57 · 2 changed · 1 added · 0 removed` — then per-node
lines: `− #8123 Button "Back"` (removed), `+ #9910 Label "…"` (added),
`~ #7001 Status "8 flights · as of 05:56" → "… 06:12"` (label
changed), with bounds deltas when only geometry moved. `--json`
emits the same as machine-readable pairs for scripting.

Design intent: PR receipts become `snapshot a → interaction →
snapshot b → diff` — mechanical, reviewable, and the frozen-frame
case collapses to "identical frame number across an interaction that
must have drawn" in one command. Bounds compare exactly (fixed-size
headless output makes geometry deterministic); label-only diffs are
the common receipt.

Implementation notes (#356): snapshot files live next to the resolved
state dir — `<parent-of-DASHBOARD_STATE_DIR>/snapshots/<name>.json`, so
`--snapshot` and `--diff` agree without extra state and an unbound run
lands next to the default state dir. Names are `[A-Za-z0-9._-]{1,64}`,
no leading dot, no `..` (traversal stays out of the grammar). Beyond the
spec'd label/bounds kinds, the diff also reports focus-only changes
(`focused=false → focused=true`) — the tab-stop receipt class.
### D8 — `tree --tab-stops`: the focus cycle in one shot

Keyboard is the most robust drive path (mouse needs coordinates;
AT-SPI routing is only sometimes enriched). One command replaces the
tab-tab-tab-tree loop: the CLI walks the cycle — from current focus,
`key tab` → `--wait-frame` → read the focused node from the dump —
numbering each stop (`1. #123 Button "Back"`, …) until focus returns
to the starting node or a 100-stop cap (a focus trap prints as
`cycle did not close after 100 stops — last: …`). Pure CLI-side
composition of existing verbs; no protocol change. The map is also
the natural `run`-script assertion substrate (`assert-focus`).

Implementation notes (#356): a tab that leaves focus on the same node
is a trap (exit 1, named); with no starting focus the walk anchors on
the first focused node. The per-step settle wait follows D6 (a focus
move with no redraw must not abort the walk). Unclosed after 100 stops
exits 1 with the last stop — a finding, not a hang.

Substrate finding (#356): AT-SPI `State::Focused` never fires on this
stack — gpui sets `TreeUpdate::focus` but not `Node::focused`, so the
walk has no focus to enrich. The dump's own `gpui_focus` /
`active_descendant_focus` pointers (node-map keys, upstream debug dump)
are the only focus signal; `merge::mark_pointer_focus` surfaces them as
`focused: true` on the nodes. Some tab stops land on focus handles
whose elements expose no a11y node at all (the docs/0049 #97 class) —
the walk prints them as named gaps (`(focus on an element without an
a11y node)`) and continues; they are follow-up material, not dead ends.
### D9 — `click <id>` falls back to coordinates, announced

When `input.click_node` returns `4005 UNKNOWN_NODE` / `4006
ACTION_UNAVAILABLE` (AT-SPI staleness after long-lived sessions,
§3), the CLI no longer just hints: it takes a fresh `tree.dump`,
resolves the node's merged bounds, dispatches `mouse down`/`up` at
the center, and prints `coordinate fallback: click <id> at (x, y)`.
Receipts stay honest (the fallback line is part of the output), and
the stale-bus failure stops being a dead end. Missing bounds (node
without enrichment) keeps today's error — there is nothing to aim at.

Implementation notes (#356): the fallback covers `click` only; `focus`
keeps the plain error. When the fresh dump has no bounds for the node,
the original 4005/4006 error is kept — there is nothing to aim at.
### D10 — `screenshot`: PNG receipts from the headless compositor

`dashboard-debug --session <n> screenshot [--name stem]` runs `grim`
against the session's `WAYLAND_DISPLAY`, writing
`screenshots/<stem>-<n>.png` (auto-incrementing counter per stem;
default stem `shot`). Prints the path. The owner gets a *picture*
before pulling while still doing the real human check after; the
host session is never captured (grim talks to the session's
compositor only). If `grim` is missing or headless screencopy fails,
the verb exits 1 with the requirement named — `doctor` reports grim
availability as info. (wlr-screencopy-under-headless is the one §3
fact not yet exercised on this machine; the implementation step
verifies it before wiring the rest of D10.)

Implementation note (#357): the §3 risk is retired — wlr-screencopy
works under headless sway on this machine (grim 1.5.0 against sway
1.12: 1280x800 RGB PNGs with real pixel variance), verified before any
of D10 was wired. `grim` resolves through the dev shell's PATH; the
capture inherits `XDG_RUNTIME_DIR` and overrides only
`WAYLAND_DISPLAY`, so the host session is never a capture target.

### D11 — One protocol addition: `app.quit`

The only app-side change: `app.quit {window?}` on the existing
feature-gated server (0010 D1) requests graceful shutdown
(`cx.quit()`); the response `{ok: true}` is written before the app
exits, so `stop` gets its ACK and the normal exit path runs —
including the app's own a11y teardown (0010 D5), making the D3
restore belt-and-braces rather than load-bearing. Not exposed as a
bare CLI verb (avoidable foot-gun); it is `session stop`'s internal
mechanism.

Implementation note (#356): the stop ladder is data (`StopStep`:
quit → term → flag → sway → launchers → purge-when-not-kept), executed
by a `StopActor` — the live one talks to the bridge/signals, the test
one records the order. `app.quit`'s ACK budget is 5 s with a 500 ms
per-read timeout; a failed or absent ACK falls through to SIGTERM
unchanged.
### D12 — `run <script>`: committed, replayable receipts

A tiny line grammar over existing verbs — a receipt script is a
committed artifact, not a pasted transcript:

```
# crates/debug/verify/usage-freeze.txt
--session qa
click #1851                          # usage chip → analytics page
snapshot opened
key tab
key tab
snapshot after-tabs
click #402                           # range → 1h
wait-frame
diff opened after-tabs               # assert-free context print
assert-label "as of"                 # Status unchanged ⇒ same freeze
assert-no-label "Quota placeholder"
screenshot opened
```

Lines: the D6/D7 verbs plus `assert-label <substr>`,
`assert-no-label <substr>`, `assert-focus <accesskit_id>`, `diff <a>
<b>` (prints; `assert-diff-label <a> <b> <substr>` when a receipt
needs "this label *changed* between snapshots"). `click` takes the
CLI's atomic `--wait-frame[=ms]` (#530): the counter is sampled before
the dispatch, which a separate `wait-frame` step can lose to a fast
page — the usage hop rendered in that gap and timed out on a frame
newer than the one it had already missed. `#` comments and
blank lines allowed; a leading `--session` line binds the run
(overridable by the CLI flag). The runner executes line by line,
echoing each with its outcome into
`transcripts/<stem>-<n>.log`, exits 0 only if every assert passed,
and exits 1 at the first failure with the line number and the actual
state. Timing determinism comes from D6: `--wait-frame` on the hop
itself, `wait-frame` lines for bare steps — never sleeps.

Committed scripts live in `crates/debug/verify/*.txt` next to the
tooling that runs them; the first script reproduces the
analytics-freeze receipt (#347's sequence, now a one-liner to
re-run). This is the seed a future CI smoke job would grow from —
deliberately not built here (§2).

Implementation notes (#357, revised by #530): committed scripts target
by **`accesskit_id` clicks with the atomic `--wait-frame`**, not mouse
coordinates. The #357-era rule ("ids are per-run seeds") is
superseded by observation: across every containerized receipt run this
session — fresh boots, different sessions, and a source change to the
audited tree itself — every id held identical (the toggle, cards,
sidebar items, bell). Ids derive from element paths, so an app change
*can* renumber one; that failure is loud (`4005 UNKNOWN_NODE`), and
the id updates in the same PR as the layout change — the same
maintenance contract as a golden. The committed scripts' headers name
their required size.
Arguments may be double-quoted (assert substrings and `type` texts with
spaces). Transcripts land beside the snapshots (`state/transcripts/`),
auto-incrementing per script stem; the runner reuses the verb
implementations verbatim, so a script line's receipt matches the
interactive verb's output exactly. Bare drive steps do not auto-settle
— `click` carries `--wait-frame`, other steps pair with explicit
`wait-frame` lines, per the grammar.

### D13 — `doctor`: the archaeology killer

`dashboard-debug doctor [--session <name>]` (no session: the default
discovery target) prints one line per check, `[ok]` / `[FAIL]` /
`[warn]`, each failure with a one-line next step:

1. session file present and well-formed;
2. recorded PIDs alive (`kill -0`);
3. sway socket file present;
4. bridge reachable: auth + `window.list`;
5. **frames advancing**: dump, 1 px `mouse move` nudge, dump again —
   `frame_number` must move (§3: the hidden-workspace signature
   fails exactly here);
6. `a11y_merge` counters: enrichment ratio reported; `FAIL` when
   nodes exist but `enriched == 0` (bus dead), `[warn]` on
   substantial `unmatched_dump` (the #351 class, app-side);
7. `ScreenReaderEnabled` equals the recorded start value (D3) —
   drift is `[warn]` with the restore command;
8. `grim` on `PATH` (`[info]`, gates D10);
9. no stray `dashboard-debug` processes besides this one (`[warn]`,
   the forgotten `logs -f`).

Output ends with a one-line verdict. This is the command that turns
"silently verified nothing" into a named failure.

**Shipped 2026-09-29 (#400):** the list now opens with a `platform`
line (ten checks): on macOS doctor scopes itself to attach mode —
tree-cluster checks report `[n/a]` with the platform fact instead of
linux session failure classes, and the merge verdict distinguishes
dump-only dumps (walk unavailable, docs/0010 D4) from a dead bus.
grim reports not-applicable on macOS instead of a PATH miss. Linux
output is unchanged for healthy sessions.

**Also 2026-09-29 (#401 interim):** `run` receipts warn on macOS
(degraded: input/logging steps run, tree/screenshot steps fail,
docs/0010 D4 gate policy) and the doctor mac lines link #401.

Implementation notes (#357): the frames nudge is a **`key tab`, not a
mouse move** — gpui redraws only on state change, and a mouse move
that crosses no hover boundary draws nothing (verified live on both a
1 px and a full-screen move; a focus move always draws). The flag
check compares against the recorded start value **only when the
session is gone** — a live bridge legitimately holds
`ScreenReaderEnabled` true (its own activation set it; `stop`
restores). The merge warn threshold is 25% unmatched dump nodes;
doctor exits 1 when any check FAILs, 0 otherwise. Record-dependent
checks report `[n/a]` on a default (session-less) attach instead of
guessing.

### D14 — CLI grammar: keep the hand-rolled parser honest

The global `--session`/`DASHBOARD_DEBUG_SESSION` resolution (D4)
happens before verb parsing; verb grammar additions (`session …`,
`wait-frame`, `tree --snapshot/--diff/--tab-stops`, `--wait-frame`,
`screenshot`, `run`, `doctor`) extend the existing pure
`cli::parse_args` table (0010 D9) — parser, session-record
(rw `session.json`), snapshot format, and diff stay **pure,
unit-tested modules** in the `dashboard-debug` lib; process spawning
and zbus stay in the thin bin layer. No clap (same rationale as
0010: the grammar stays small enough to hand-roll).

### D15 — `audit`: deterministic tree rules (0065 D3)

`dashboard-debug audit [--json] [--window N]` runs the docs/0065 rule
set over the live tree dump: role+label on interactive nodes (FAIL —
0010 D10 made mechanical), degenerate/missing bounds (FAIL/FINDING),
off-scale sibling gaps against the D10 spacing scale (FINDING, ±1px
tolerance, 64px ceiling), sibling overlaps (FINDING, overlay roles and
`overlay-*`/`popover-*` ids exempt), duplicate sibling labels
(FINDING). Exit 1 only on FAIL; findings/incomplete print for the
owner checklist — axe-style: deterministic, zero-false-positive bias,
undecidable results route to humans. Tab-order coverage is not
duplicated here: `tree --tab-stops` already exits nonzero on traps and
unclosed cycles; receipts pair the two (`audit` then `tab-stops`).
Script step: `audit` (receipt-red on FAILs).

### D16 — `baseline`: committed structural goldens (0065 D5)

`dashboard-debug baseline <surface> [--update] [--window N]` compares
the live tree against the committed golden at
`crates/debug/verify/snapshots/<surface>.json` — same flattening and
diff machinery as D7 snapshots, but committed to the repo and
byte-stable (frame/timestamps zeroed). Drift prints the structural
diff and exits 1; `--update` is the deliberate refresh path and the
resulting file change is reviewed like code. Baselines are captured on
the Linux box (the tree cluster is macOS-gated, #405) and only ever
compared in the same environment that captured them. Script steps:
`baseline <surface> [--update]` (generation) and
`assert-baseline <surface>` (receipt-red on drift). Receipt grammar is
guarded by a unit test parsing every committed `verify/*.txt` — a
grammar change that breaks a shipped script fails CI, not the Linux
box (added after two shipped receipts carried a `tree --json` step
that was never a verb).

### D17 — `critique <surface>`: the ritual input, paired by construction

`dashboard-debug critique <surface>` (0065 D7) is one command for the
critique bundle: wait-frame, grim screenshot, tree snapshot — written
as a pair under the session's `critique/` dir with one stem and one
counter (`<surface>-<n>.png` + `<surface>-<n>.tree.json`). The pairing
is the rule, not a convention: the measured reason for the whole
system is that pixel-only critique misses layout structure
(Design2Code), so the tool refuses to produce pixels alone. Session-
bound, Linux-only (grim + a11y). Script step: `critique <surface>`;
before/after pairs are two critiques of the same surface — the counter
keeps them apart.

### D18 — `golden <surface> --theme <t> [--update]`: pixel goldens

`dashboard-debug golden <surface> --theme dark|light [--update]`
(0065 D6) waits one frame, captures the session compositor, and
compares against the committed baseline at
`crates/debug/verify/pixels/<surface>-<WxH>-<theme>.png` (size comes
from the session record — pinned `--size` is the receipt contract;
the theme flag is the operator stating which Aurora theme is active).
The differ is a pure module (`pixels.rs`): per-channel tolerance of
2/255 — above anti-aliasing jitter, far below anything visible — any
pixel over tolerance is drift (exit 1 with count, worst delta, first
offending coordinate). Size mismatch is an error, not a verdict:
recapture, never compare across frame sizes. `--update` overwrites
the baseline and is the only deliberate refresh path, reviewed like
code. The same-environment rule is absolute: baselines live and die
on the Linux runner. Script steps: `golden <surface> [--theme t]
[--update]` (capture) and `assert-golden <surface> [--theme t]`
(receipt-red on drift; refuses `--update`). See `verify/golden.txt`.

## 5. Dev environment

`flake.nix` devShell `packages` gains `sway` and `grim` (both in the
pinned nixpkgs; no GPU, no real input stack needed — §3). The AT-SPI
pair is already present. No app dependencies change; the devshell
grows ~two packages. `session start` refuses to run when `sway` is
absent, naming the missing binary and the devShell — discoverable
failure, not a spawn mystery.

## 6. Testing

- **Pure, in-tree**: `cli::parse_args` tables for every new verb and
  flag; `session.json` (de)serialization round-trips; name
  validation; snapshot serialization; diff output for
  changed/added/removed/label-only/bounds-only fixtures; sway-config
  text fixture; script-grammar parsing (valid lines, unknown verb,
  bad assert arity).
- **Live smoke (dev machine, one command)**:
  `session start → run crates/debug/verify/usage-freeze.txt →
  snapshot/diff pair → screenshot → session stop`, asserting: exit 0;
  `ScreenReaderEnabled` unchanged from its pre-start value; no
  `wayland-*` socket leaked; session dir gone (default purge); host
  pointer/keyboard untouched by construction (headless backend).
- **Negative smoke**: `--keep` retains the dir; a second `session
  start` with the same name fails; `doctor` on a
  `SIGKILL`-orphaned session reports the drift classes (dead PID,
  a11y flag true, socket gone).
- **Docs/AGENTS**: AGENTS.md's "Connect" section shrinks to the
  session flow (the a11y-bus step becomes `session start`'s job);
  the ScreenReaderEnabled footnote points at `stop`/`doctor`.

## 7. Acceptance criteria

- One command yields a live, isolated, host-untouched app instance
  whose discovery needs no env prefixes (`--session` only).
- Drive-then-check is deterministic: every receipt verb can wait on
  a frame number; no verification flow needs `sleep`.
- PR receipts can be produced as snapshot diffs + screenshots, and
  the freeze-class regression (stale frame under input) is caught by
  `doctor`/`wait-frame` in one command, named.
- The analytics-freeze receipt (#347's sequence) exists as a
  committed script and passes against a fresh session.
- Teardown is scripted and hygienic: recorded a11y flag restored,
  only owned PIDs signalled, scratch purged by default.
- Gates green; pure modules table-tested; live smoke recorded in the
  PR body.

## 8. Follow-ups (filed, not built)

- **CI smoke job** running committed verify scripts on an ephemeral
  runner (app + headless sway + asserts, zero display requirements)
  — the D12 grammar is the enabler; costs and runner shape are a
  separate decision.
- #348 / #351 remain the app-side a11y exposure gaps this tooling
  *reports* (`doctor` check 6) but does not fix.
- Possible later verb: `tree --hot-path` (focus+hover state matrix)
  if receipts start needing interaction-state coverage.

## 9. Proposed implementation steps

Three focused issues (split for reviewability; may be squashed by
the owner at filing time):

1. **Sessions core** — D1–D5 + D14 discovery + flake deps (sway,
   grim): start/status/stop/env, record-and-restore, name/dir
   discipline, parser tables, pure-module tests. Smallest PR that
   already kills problems 1–3.
2. **Determinism verbs** — D6–D9 + D11: wait-frame / --wait-frame,
   snapshots + diff, tab-stops, click fallback, `app.quit`.
3. **Receipts** — D10, D12, D13: screenshot, run scripts + the
   committed `usage-freeze.txt`, doctor, AGENTS.md connect-section
   rewrite.
