# 0014 — GitHub module: selected-repo breadcrumb

- **Date:** 2026-09-03
- **Depends on:** 0003 (Navigator/crumbs contract, §4.3), 0005 (GitHub module),
  0007 (snapshot state model), 0009 (org Select + repo List, selection flow D4/D5),
  0010 (debug bridge, D10 id/role rule)
- **Closes the design need of:** owner request — "currently we show `Home > GitHub`;
  when a repo is chosen it must show `Home > GitHub > <repo-name>`; this will be
  later used to navigate back to the repo from the repo's own subpages"

## 1. Context & problem

The shell's chrome row renders `Breadcrumb` as `Home > <module title> > <module
crumbs…>` (docs/0003 §4.5). Every module gets the first two items for free; module
crumbs come from `Navigator::set_crumbs` and were proven by the throwaway Notes
module (0003 §5.6) — which no longer exists. No real module reports crumbs today.

The GitHub module has exactly the state a crumb needs: `GithubState.selected`
(`owner/repo`, session-only, service.rs:537) is set on repo confirm (0009 D4) and
cleared on org switch (0009 D5) and sign-out. But the chrome ignores it: with a
repo open, the right column shows that repo's PRs/issues/notifications while the
breadcrumb still says `Home > GitHub`. The owner wants the selected repo surfaced
as a third crumb, and wants that crumb to become the back-navigation anchor once
repos grow their own subpages (e.g. an issues view).

Two mechanical gaps make "just call `set_crumbs`" insufficient:

1. **`Navigator::go` clears crumbs on every page switch** (nav.rs:72) — and nothing
   re-reports them. `state.selected` survives leaving the page (it is cleared only
   by org switch / sign-out), so returning to the GitHub page would show the repo
   detail with a bare `Home > GitHub` breadcrumb.
2. **Selection can change while the page is hidden** — sign-out runs from the
   module's settings subpage (settings.rs:88 `sign_out_now`); auth loss
   (`TokenRejected`) swaps the page for the connect panel. Entity observers fire
   regardless of tree visibility, so a state-derived sync covers paths the page's
   own handlers never see.

## 2. Scope

In:

- The GitHub page reports the selected repo as a **label-only crumb**;
  `Home > GitHub > <repo-name>` when a repo is open, `Home > GitHub` otherwise.
- Crumb lifecycle derived from `GithubState` (selection + connectivity + snapshot
  lookup), reconciled against the navigator — survives page re-entry, clears on
  org switch / sign-out / repo disappearance.
- A reconciliation rule that keeps **module-owned crumb prefixes** stable under
  background ticks, designed so a future subpages doc can append suffix crumbs
  without this mechanism fighting it.
- Unit tests for the two new pure functions; doc + README index row.

Out (non-goals):

- **No click behavior on the repo crumb.** There are no repo subpages yet; the
  current-location crumb is non-clickable by breadcrumb convention. The future
  subpages doc adds `on_click` ("back to repo root") and deeper crumbs.
- No core changes: `Crumb`/`set_crumbs`/`crumbs()` (nav.rs) already suffice.
- No org level in the breadcrumb (`Home > GitHub > <org> > <repo>`): the org is a
  filter expressed by the left-column Select, not a navigation destination.
- No persistence: the crumb mirrors session-only `selected`; nothing new enters
  any cache or `shell.json`.
- No truncation/elision work beyond what the chrome row already does.

## 3. Verified facts (our tree, `main` @ 811bed5)

- **`Crumb`** (core/src/nav.rs:50-66): `{ label, on_click: Option<CrumbAction> }`;
  `Crumb::label(..)` is the non-clickable constructor. **`Navigator`**:
  `go()` clears crumbs (nav.rs:72), `set_crumbs(vec, cx)` replaces them and
  notifies (nav.rs:77), `crumbs()` reads them back (nav.rs:82).
- **Shell rendering** (core/src/shell.rs, render): Home item + module-title item +
  `.children(crumbs…)` — clickable when `on_click` is `Some`, plain otherwise
  (the Settings crumb is the label-only precedent). The shell `cx.observe`s the
  navigator, so any `set_crumbs` repaints the chrome row. The row is fixed-height
  (`h_9`, `overflow_hidden`) and the breadcrumb host is `min_w_0 + flex_1 +
  overflow_hidden`: long crumbs clip, the row never wraps or resizes.
- **Selection write paths** (modules/github/src/page.rs): `select_repo` (:216)
  sets `s.selected` then runs fetch specs; the org-switch subscriber (:65-84)
  sets `s.selected = None`, clears `synced_repos`, calls `sync_from_state_in`;
  both funnel through the state observer (:103 `cx.observe(&state, …)` →
  `sync_from_state` → `sync_from_state_in` via the stored window handle).
- **Selection clear paths elsewhere**: `sign_out` (service.rs:1082) wipes all
  data including `selected`; nothing in the tick loop mutates `selected`.
- **Render predicate**: the right column resolves `state.selected` → repo record
  via `repos.iter().find(|r| r.full_name == selected)`; a missing record renders
  the "Select a repository" empty state (page.rs render + `render_right_column`).
  The crumb must use the **same** predicate or it can disagree with the pane
  beside it (e.g. a repo dropped by a background snapshot refresh).
- **Nav access**: `Module::init` already receives `nav: &Entity<Navigator>`
  (core Module trait, 0003 §4.1); github/src/lib.rs:47 currently discards it as
  `_nav` for the page (the card consumes it, card.rs:29). No trait change needed.
- **Observers are not tree-gated**: an `cx.observe` on the state/nav entities
  fires regardless of tree visibility, which makes a state-derived crumb sync
  viable for the sign-out-while-hidden path.
- **Amended during implementation:** the state entity **never called
  `cx.notify()`** — the service applied every fetch/org/auth mutation without
  notifying, so the 0009 D6 observer sync never ran on data arrival (masked by
  `window.refresh()` repaints; construction-time seeding made the list look
  right). This PR adds `cx.notify()` at the mutating orchestration sites
  (§6), which activates the observer-driven sync exactly as designed here —
  and incidentally fixes the 0009 background Select/List staleness.

## 4. Open questions — NEEDS YOUR INPUT

Recommendations in *italics*; silence = follow recommendation.

**Q1 — Crumb label: short `name` or `full_name`?** The repo list rows show `name`
(0009 D2); the right-column header shows `full_name`. *Recommendation: short
`name`* — it matches the owner's `<repo-name>` phrasing, the list rows the user
just clicked, and keeps the chrome compact for the future
`Home > GitHub > repo > Issues` chain. `full_name` is the fallback if you want
owner disambiguation in the chrome.

**Q2 — Click affordance now?** *Recommendation: label-only now.* The repo crumb is
the current location; there is nothing to go back to until subpages exist. The
subpages doc turns the prefix crumb clickable.

## 5. Design

### D1 — One derivation, one writer

The crumb is never set imperatively at interaction sites. Instead the page owns a
single **sync** method that derives the desired crumb from state and reconciles it
against the navigator, invoked from the two places that can make the answer stale:

```rust
// page.rs — new field on GithubPage
nav: Entity<Navigator>,
```

1. **Data changes** — end of `sync_from_state_in` (already on every state tick:
   selection, org switch, sign-out, auth loss, snapshot swaps).
2. **Page activation** — a new `cx.observe(&nav, …)` in `GithubPage::new`; fires
   on every `go`/`set_crumbs`. This is what repairs the cleared-on-navigation
   crumbs when the user comes back to the GitHub page with a repo still open.

Both funnel into:

```rust
impl GithubPage {
    /// Reconcile the navigator's module crumbs with the selected repo.
    /// No-op (no entity writes) unless the prefix actually changed.
    fn sync_repo_crumb(&mut self, cx: &mut Context<Self>) {
        // Gate: never touch crumbs while another page owns the chrome.
        // ("github" literal — card.rs:36 precedent.)
        let active = matches!(
            self.nav.read(cx).page,
            Page::Module(ref id) if id.as_ref() == "github"
        );
        if !active {
            return;
        }
        let derived = {
            let s = self.state.read(cx);
            repo_crumb_label(
                s.selected.as_deref(),
                &s.repos,
                s.auth.connected(),
            )
        };
        let current: Vec<SharedString> =
            self.nav.read(cx).crumbs().iter().map(|c| c.label.clone()).collect();
        if let Some(labels) = reconcile_repo_crumb(derived, &current) {
            self.nav.update(cx, |nav, cx| {
                nav.set_crumbs(labels.into_iter().map(Crumb::label).collect(), cx);
            });
        }
    }
}
```

Properties:

- **Terminating**: a write notifies the navigator, the nav observer re-runs
  `sync_repo_crumb`, the reconcile now returns `None` — one extra pass, no loop.
- **Quiet when idle**: unchanged selection + unchanged crumbs → zero writes on
  every 60 s tick (compare-first, like `synced_orgs`/`synced_repos`).
- **No cross-module leakage**: the gate skips the write unless the GitHub page is
   the current top-level page. While the user sits on Home/Settings, `go` has
   already emptied the crumbs and this sync leaves them empty.

### D2 — Derivation (pure, testable)

```rust
/// The module-level crumb label: the short `name` of the selected repo
/// (Q1), resolved with the same lookup the right column uses — a
/// selected repo that no longer exists in the snapshot is *not* a crumb
/// (the pane beside it shows "Select a repository"; the chrome agrees).
/// Disconnected states never carry a repo crumb (connect panel owns the
/// page).
fn repo_crumb_label(
    selected: Option<&str>,
    repos: &[gql::Repo],
    connected: bool,
) -> Option<SharedString> {
    if !connected {
        return None;
    }
    selected
        .and_then(|f| repos.iter().find(|r| r.full_name == f))
        .map(|r| r.name.clone().into())
}
```

Narrow slice parameters (not `&GithubState`) so tests need no client/store/cache
fixtures.

### D3 — Prefix reconciliation (subpage-proof)

The module owns a **prefix** of the crumb chain; future subpages own the suffix.
`reconcile` decides if a rewrite is needed — returning `None` means "leave the
navigator alone":

```rust
/// Decide whether the navigator's crumbs must be rewritten to carry the
/// repo prefix. `Some(labels)` = write exactly these (prefix only);
/// `None` = no change. Pure — table-tested.
fn reconcile_repo_crumb(
    derived: Option<SharedString>,
    current: &[SharedString],
) -> Option<Vec<SharedString>> {
    match derived {
        None if current.is_empty() => None,             // stable: no repo
        None => Some(Vec::new()),                       // repo lost: clear all
        Some(r) => match current.first() {
            Some(first) if *first == r => None,         // stable: prefix present
            _ => Some(vec![r]),                         // (re)port / changed
        },
    }
}
```

| derived | current crumbs | result | why |
| --- | --- | --- | --- |
| `None` | `[]` | no write | nothing selected |
| `None` | `[repo, …]` | write `[]` | org switch / sign-out / repo vanished — a suffix without its repo is meaningless, drop it |
| `Some(r)` | `[]` | write `[r]` | first report, or re-entry after `go` cleared crumbs |
| `Some(r)` | `[r]` | no write | stable |
| `Some(r)` | `[r, sub, …]` | no write | **subpage suffix is preserved** — background ticks must not clobber it |
| `Some(r)` | `[other, …]` | write `[r]` | repo changed under stale crumbs |

Why a prefix rule and not plain equality: when the subpages doc lands, the chain
will be `[repo (clickable → repo root), subpage, …]`, and this sync runs on every
state tick — an equality rule would flatten `[repo, Issues]` back to `[repo]`
once a minute. The prefix rule keeps this doc's mechanism compatible by
construction. (When a rewrite *does* drop a suffix — repo changed or cleared —
that is correct: the subpage's subject is gone; the subpages doc resets its
screen state on the same transitions.)

Today, with no suffix writers, the table degenerates to exactly the requested
behavior: repo open ⇒ `Home > GitHub > <name>`; nothing open ⇒ `Home > GitHub`.

### D4 — Crumb construction

`Crumb::label(name)` — label-only (Q2): non-clickable, rendered by the shell as
plain text exactly like the Settings crumb; no `on_click`, no disabled styling, no
new component. [DG]: no new colors, sizes, or spacing — the stock
`BreadcrumbItem` styling is the whole visual. Very long repo names clip at the
chrome row's `overflow_hidden` boundary (existing behavior, no wrap/no row
resize); the future subpages doc may add ellipsis if it ever matters.

The subpages follow-up upgrades the prefix crumb to
`Crumb { label, on_click: Some(→ repo root screen) }`; `reconcile`'s label-only
return is unchanged — the builder at the call site grows the action.

### D5 — Wiring (module-local; no core changes)

- `lib.rs`: `init`'s `_nav` → `nav` (the underscore name is stale — the card
  already consumes it); `page::GithubPage::new(state.clone(), nav.clone(),
  window, cx)`.
- `page.rs`: store the `nav` entity; in `new`, after the existing state observer:

  ```rust
  // Crumb re-report on page (re)activation: `go` clears crumbs on every
  // page switch (nav.rs), so returning to this page must re-derive them
  // from state — nothing else will.
  cx.observe(&nav, |this, _, cx| this.sync_repo_crumb(cx)).detach();
  ```

- `sync_from_state_in`: call `self.sync_repo_crumb(cx)` just before its final
  `cx.notify()`.

No changes to core (`nav.rs`, `shell.rs`), the service, or the Module trait.

### D6 — What each lifecycle event produces

| Event | Path | Breadcrumb after |
| --- | --- | --- |
| Repo click / Enter | state observer → sync | `Home > GitHub > <name>` |
| Org switch | `selected = None` → state observer | `Home > GitHub` |
| Sign-out (settings subpage, page hidden) | state observer fires anyway | `Home > GitHub` (already clear after `go`; write is a no-op) |
| Auth loss (`TokenRejected`) on page | derivation gate `connected` | `Home > GitHub` next to the connect panel |
| Repo dropped by background snapshot | lookup fails (same predicate as the right column) | `Home > GitHub` + "Select a repository" |
| Home → GitHub with repo still open | nav observer (activation) | `Home > GitHub > <name>` |
| 60 s tick, unchanged selection | reconcile → no write | unchanged, zero entity writes |
| GitHub module crumb clicked mid-module (issue #100) | `Navigator::go` same-page → `NavEvent::Reentered` → `return_to_root` | `Home > GitHub` (selection + PR page dropped, not just the crumb suffix) |

### D7 — Module-crumb re-entry returns to the module root (amended 2026-09-05, issue #100)

D5's "no core changes" held only for the crumb *reporting* side. The other
half of the crumb contract — the shell-owned module crumb (`GitHub`) — was
a no-op when clicked from inside the module: `Navigator::go` to the current
page changed nothing observable, and the D5 activation observer immediately
re-derived the unchanged chain. Expected behavior (matching the repo crumb's
`close_pr` role): the module crumb returns to the module root — the plain
repo list.

The shell stays dumb; the signal is core-owned and module-answered:

- `Navigator::go` emits `NavEvent::Reentered` **iff the target page is the
  page already current** (page switches, crumb writes, and sidebar toggles
  never emit). Nav state itself does not change on re-entry.
- The GitHub page subscribes (`cx.subscribe_in(&nav, …)`) and answers with
  `return_to_root`: `state.selected = None` plus the org-switch guard reset
  (so the List highlight follows through `sync_from_state_in`) and
  `close_pr`. Unlike `close_pr` alone, the selection does **not** survive —
  the click means "repo list", not "this repo without its PR". Org filter
  and list search are left alone.
- Home → GitHub remains a page switch (no event): the selection survives
  module switches exactly as before.
- Re-entry with nothing open is a no-op; other modules are unaffected (no
  subscriber, no behavior change); `go(Home)`/`go(Settings)` on their own
  page emit to nobody.

## 6. Implementation plan

Branch `feat/0014-repo-breadcrumb`, stacked on this doc PR. Files:

- `crates/modules/github/src/page.rs` — `nav` field + observers (D5),
  `sync_repo_crumb` (D1), `repo_crumb_label` (D2), `reconcile_repo_crumb` (D3),
  imports `dashboard_core::nav::{Crumb, Navigator, Page}`; unit tests (§7).
  `sync_from_state` now defers through `cx.spawn` + the stored window handle
  (the `run_specs` pattern): a notify can land while the page is mid-update
  (repo-List subscriber → state notify), and updating the page synchronously
  from the observer there double-leases it — a hard crash on repo selection
  without the deferral. Also: `cx.notify()` at the selection-mutating sites
  (`select_repo`, org-switch clear, `run_specs` apply).
- `crates/modules/github/src/service.rs` — `cx.notify()` at the remaining
  state-mutating orchestration sites (tick/repo `apply`, `apply_refresh`,
  `start_connect`, `set_connect_phase`, `install_credential`, `sign_out_now`),
  so entity observers actually fire on data arrival (§3 amendment).
- `crates/modules/github/src/lib.rs` — `_nav` → `nav`, pass to `GithubPage::new`.
- Nothing else. `GithubPage::new`'s only caller is `Module::init`; its existing
  `#[gpui::test]` does not construct the page.

## 7. Testing

Pure table tests in page.rs's `#[cfg(test)]` (mind the documented no-
`use super::*` hazard, 0009 §4 — import the two fns selectively):

- `repo_crumb_label`: selected-and-found → short name; selected-but-missing →
  `None`; not-selected → `None`; disconnected → `None` even when selected+found;
  full-name match resolves the repo's `name` (not the full name).
- `reconcile_repo_crumb`: all six table rows of D3, including the
  suffix-preservation row (`Some(r), [r, "Issues"]` → no write) — the regression
  guard for the future subpages doc.

No new `#[gpui::test]`: the wiring is two observer hookups verified via the
bridge and human checks (§8/§9); existing tests (page + service) must pass
untouched.

## 8. Debug-bridge verification (docs/0010)

With the app running (`cargo run`, a11y bus up):

1. `tree` → select a repo (`click` the repo row's ListItem id) → re-dump: the
   breadcrumb region gains a **non-interactive** `ListItem` node for the repo
   name (right of the module-title link); **no** new interactive node — the
   crumb is intentionally non-clickable (D10's id+role rule applies to
   interactive elements only).
2. `logs -f` while stepping through org switch / sign-out / Home-and-back:
   crumb writes log `repo crumb synced: <before> -> <after>` on the state-sync
   and nav paths; a quiet minute of ticks writes nothing.

## 9. Acceptance criteria

1. Repo selected (click or ↑/↓ + Enter) ⇒ breadcrumb shows
   `Home > GitHub > <repo-name>` immediately (next frame, no interaction).
2. Org switch ⇒ repo crumb gone; reselecting a repo restores it.
3. Home → GitHub (sidebar or card) with a repo still selected ⇒ crumb restored,
   matching the repo detail shown in the right column.
4. Sign-out (settings subpage) and auth loss ⇒ no repo crumb on return/next view.
5. Repo removed by a background snapshot ⇒ crumb clears in the same frame the
   right column falls back to "Select a repository".
6. Repo crumb has no click affordance (current location; Q2).
7. Idle ticks (selection unchanged) perform zero `set_crumbs` writes.
8. `reconcile_repo_crumb` suffix-preservation test green (subpage-proofing).
9. Gates: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets`
   (zero warnings), `cargo test --workspace`; this doc + README row merged with
   the implementation PR.

## 10. Human checks (owner)

- [ ] Click a repo in the list: third crumb appears with the repo's short name;
      long names clip gracefully (no row growth, no wrap, no window jitter).
- [ ] Switch org: crumb disappears; the left list resets as before (0009 D5).
- [ ] Open a repo → Home → GitHub: repo still open in the right column **and**
      its crumb back in the breadcrumb.
- [ ] Settings → Disconnect → GitHub: page shows the connect panel with a bare
      `Home > GitHub` breadcrumb.
- [ ] Leave the app idle on an open repo for >1 min: crumb stable, no flicker
      (no per-tick rewrite).

## 11. Follow-ups (GitHub issues, not this PR)

- **Repo subpages** (issues view, PR detail, …): append suffix crumbs
  (`Home > GitHub > repo > Issues`), make the prefix crumb clickable
  (`on_click` → repo root screen), reset subpage screen state on the same
  transitions that drop the prefix here (D3). This doc's reconcile rule is the
  contract that keeps them alive across ticks.
- Crumb ellipsis/truncation polish if long names ever clip badly in practice
  (D4 notes the current clip behavior).
- Other modules adopt the same derive-and-reconcile crumb pattern when they grow
  subpages (no core change needed; this is module-local).
