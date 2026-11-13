//! Module page (docs/0005 §6.3, amended by 0009): two-column layout —
//! searchable org `Select` + virtualized searchable repo `List` (left),
//! repo header + GroupBoxes (right, independently scrollable) — plus the
//! connect panel (§6.2) for every non-connected state.
//!
//! Both left-column components are gpui-component entities constructed
//! once here (0009 D3): their item-replacement and selection APIs need
//! `&mut Window`, so background data sync goes through the stored window
//! handle (D6) while user interaction arrives via `subscribe_in`.

use std::sync::Arc;

use std::time::Duration;

use dashboard_core::ui::{page_canvas, page_title};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, IndexPath, Selectable as _, Sizable as _,
    avatar::Avatar,
    badge::Badge,
    button::{Button, ButtonVariants},
    group_box::GroupBox,
    h_flex,
    input::{Input, InputEvent, InputState},
    list::{List, ListDelegate, ListEvent, ListItem, ListState},
    menu::{DropdownMenu as _, PopupMenuItem},
    searchable_list::SearchableVec,
    select::{Select, SelectEvent, SelectState},
    skeleton::Skeleton,
    spinner::Spinner,
    tab::{Tab, TabBar},
    tooltip::Tooltip,
    v_flex,
};

use dashboard_core::freshness::{freshness_row, relative_age, synced_label};

/// The repo view's active tab (docs/0077 D1): session-local, resets to
/// [`RepoTab::Pulls`] on every repo selection, survives detail-page
/// round trips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum RepoTab {
    #[default]
    Pulls,
    Issues,
    Notifications,
    /// docs/0081 D1 — appended last (Q5) so the existing indices and
    /// trail keys stay stable.
    Commits,
}

impl RepoTab {
    /// The fetch lane this tab renders (docs/0077 D3).
    fn lane(self) -> RepoLane {
        match self {
            RepoTab::Pulls => RepoLane::Pulls,
            RepoTab::Issues => RepoLane::Issues,
            RepoTab::Notifications => RepoLane::Notifications,
            RepoTab::Commits => RepoLane::Commits,
        }
    }

    fn index(self) -> usize {
        match self {
            RepoTab::Pulls => 0,
            RepoTab::Issues => 1,
            RepoTab::Notifications => 2,
            RepoTab::Commits => 3,
        }
    }

    fn from_index(ix: usize) -> Option<Self> {
        match ix {
            0 => Some(RepoTab::Pulls),
            1 => Some(RepoTab::Issues),
            2 => Some(RepoTab::Notifications),
            3 => Some(RepoTab::Commits),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            RepoTab::Pulls => "Open PRs",
            RepoTab::Issues => "Open issues",
            RepoTab::Notifications => "Notifications",
            RepoTab::Commits => "Commits",
        }
    }
    /// The trail-key tag (docs/0048 D4 amended): the visit key is
    /// `repo:<name>:<tag>`, human-readable for debugging.
    fn key_tag(self) -> &'static str {
        match self {
            RepoTab::Pulls => "pulls",
            RepoTab::Issues => "issues",
            RepoTab::Notifications => "notifs",
            RepoTab::Commits => "commits",
        }
    }
}
use dashboard_core::module::ModuleNav;
use dashboard_core::nav::{Crumb, NavEvent, Navigator, Page, Route};

use crate::api;
use crate::card::{REPO_LOCK, github_mark};
use crate::commits::{self, CommitsDelegate, RefItem, RefSearchDelegate, RefSource};
use crate::filters;
use crate::gql;
use crate::issue_page;
use crate::new_issue_page::{NewIssuePage, NewIssuePageEvent};
use crate::new_pr_page::{NewPrPage, NewPrPageEvent};
use crate::notification_page::{self, NotificationPageEvent};
use crate::pr_page;
use crate::pr_page::leave_guard::LeaveIntent;
use crate::service::{
    AuthStatus, CommitFilter, CommitsKey, ConnectPhase, FetchSpec, GithubState, HomeScope, Kind,
    LandedClock, LaneView, RefKey, RepoLane, ThreadOp, run_read_deduped, run_spec_tracked,
    start_connect,
};
use crate::usage::{Origin, Surface, Trigger};

/// The reconcile view's fixed left column (0002 D10).
const RECONCILE_COL_W: f32 = 280.0;
/// The connect panel's blurb max width (0002 D10): keeps the sign-in
/// text measure readable on wide windows.
const CONNECT_BLURB_MAX_W: f32 = 420.0;

/// docs/0052 D6: the age-label clock — a render-only wake. Repaints
/// advance the relative ages ("synced 2 m ago", #192 / docs/0033 D4);
/// it never dispatches a spec (D1 forbids timer-driven fetches, not
/// repaints).
pub(crate) const RENDER_WAKE: Duration = Duration::from_secs(30);

/// The home view's truncation hint (docs/0047 D4): `None` when the
/// snapshot is complete; otherwise "first N of M", or the cap copy once
/// the shown rows hit GitHub's 1,000-result search ceiling (verified
/// live — the server ends the chain cleanly there, docs/0047 §2).
fn home_prs_hint(shown: usize, total: i64) -> Option<String> {
    if shown >= gql::SEARCH_RESULT_CAP && total > shown as i64 {
        Some(format!("first {shown} of {total} — search caps at 1,000"))
    } else if total > shown as i64 {
        Some(format!("first {shown} of {total} open pull requests"))
    } else {
        None
    }
}

/// The home view's render inputs (docs/0047 D2/D6): the account's landed
/// snapshot when the search lane has landed, else the D6 union of the repo
/// snapshot's PR nodes with its `partial` honesty flag; the freshness
/// clock (the lane's own, or the union's repo-snapshot provenance); and
/// the lane's last failure for the Retry state. Owned values — the state
/// borrow drops before the element tree builds.
struct HomeSource {
    /// docs/0073 D3 — the derived open list + lane books; `None` = the
    /// lane never landed (the view owns skeletons).
    view: Option<LaneView>,
    clock: LandedClock,
    error: Option<String>,
}

pub(crate) struct GithubPage {
    state: Entity<GithubState>,
    /// Org login chosen in the Select (`None` = the first org, i.e. the
    /// personal account — Q10; resolved on orgs arrival, 0009 D6).
    org: Option<SharedString>,
    org_select: Entity<SelectState<SearchableVec<SharedString>>>,
    repo_list: Entity<ListState<RepoListDelegate>>,
    /// The open PR sub-page (docs/0016 D1): `Some` = the PR detail page
    /// owns the module content (full takeover — org Select + repo list
    /// unmount; they remount with selection intact on close).
    pr: Option<Entity<pr_page::PrDetailPage>>,
    /// The open issue sub-page (docs/0020 D1) — the PR page's sibling;
    /// mutually exclusive with `pr` (opening one closes the other), same
    /// takeover and close semantics.
    issue: Option<Entity<issue_page::IssueDetailPage>>,
    /// The open notification sub-page (docs/0026 D1) — the third
    /// takeover sibling, mutually exclusive with `pr`/`issue`/creator.
    notification: Option<Entity<notification_page::NotificationDetailPage>>,
    /// The issue creator (docs/0024 D3/D7): `creator` stays alive once
    /// opened so Cancel/Esc retain the draft for the session;
    /// `creator_open` is the takeover visibility. Dropped on successful
    /// creation and on repo switch; mutually exclusive with `pr`/`issue`
    /// (opening one hides the other).
    creator: Option<Entity<NewIssuePage>>,
    creator_open: bool,
    /// The PR creator (docs/0044 D3/D7): the 0024 creator's sibling —
    /// same retention and mutual-exclusion rules, own takeover slot.
    creator_pr: Option<Entity<NewPrPage>>,
    creator_pr_open: bool,
    /// Repo-view filter/sort state (docs/0042 D2): session-scoped; the
    /// label choices reset on repo switch (names are repo-local), the
    /// rest survives switches but not app restarts.
    filters: filters::RepoViewFilters,
    /// The active repo-view tab (docs/0077 D1): resets on repo
    /// selection, survives detail-page round trips.
    tab: RepoTab,
    /// The shared quick-filter input (docs/0042 D5) — one query narrows
    /// all three boxes at once.
    row_search: Entity<InputState>,
    /// The Commits tab's virtual list (docs/0081 D5): its delegate holds
    /// a filtered copy of the active book, synced at render. Built in the
    /// constructor's deferred step, never inline in `new`: an entity
    /// built there would shift every later element-path accesskit id the
    /// committed receipts click by (the #610 re-key). `None` only until
    /// that step runs.
    commits_list: Option<Entity<ListState<CommitsDelegate>>>,
    /// The book the commits list last synced — a different repo or ref
    /// resets its scroll and selection.
    commits_synced: Option<(String, CommitsKey)>,
    /// docs/0081 D7 — the Commits tab's selected ref: session-local,
    /// reset to the default branch on every repo selection (like the
    /// tab itself, docs/0077 D1).
    commits_ref: RefKey,
    /// docs/0081 §14 — the "mine" toggle: session-local, kept across
    /// branch picks, reset with `commits_ref` on every repo selection.
    /// Effective only once the viewer's node id is known
    /// ([`Self::commits_key`]).
    commits_mine: bool,
    /// docs/0081 D7 — the branch selector, built beside the commits
    /// list (same deferred boot step, same id-stability reason).
    ref_select: Option<Entity<SelectState<RefSearchDelegate>>>,
    /// The selector's closed-state focus handle: `SelectState` hands out
    /// its menu's handle while open, so a different handle means open —
    /// the open edge that fetches the branch list (no open event
    /// upstream).
    ref_select_trigger: Option<FocusHandle>,
    ref_select_open: bool,
    /// docs/0081 D7 — the selector's branch list (page-owned, lazy).
    ref_source: RefSource,
    /// The items and selection last pushed into the selector
    /// (compare-first, so a render doesn't rebuild the menu).
    ref_synced: Option<(Vec<RefItem>, Option<SharedString>)>,
    /// Window access for observer-time entity updates (0009 D6).
    window: AnyWindowHandle,
    /// View-shown edge tracking (docs/0051 D4): transitions into the
    /// github module fire the home lane once per show.
    last_module_active: bool,
    /// Last-synced item keys — compare-first background sync so untouched
    /// ticks cost two vec comparisons and no entity writes (0009 D6).
    synced_orgs: Vec<SharedString>,
    synced_repos: Vec<String>,
    /// Navigator handle — the page derives and re-reports its breadcrumb
    /// prefix from state (docs/0014 D1/D5).
    /// Row-level done writes in flight (docs/0026 D8, #153): thread
    /// ids with a DELETE pending — their row button disables for the
    /// round (double-click guard); cleared on settle, when the
    /// post-write refetch replaces the list anyway.
    row_done_inflight: std::collections::HashSet<String>,
    nav: Entity<Navigator>,
}

/// One place in the module's page tree (docs/0048): the trail payload a
/// visit hands to the navigator and a restore hands back. Deep places
/// carry everything `open_*` needs — the click-time snapshot objects —
/// so restoring is exactly re-clicking, with no refetch to rebuild
/// context (the detail pages refresh themselves per docs/0032).
#[derive(Clone)]
pub(crate) enum GithubPlace {
    /// The repo view at a tab (docs/0048 D4 amended): the tab rides the
    /// place so a switch is its own visit and a restore re-enters the
    /// exact lane.
    Repo(String, RepoTab),
    /// Identity only — the row copy lives in the store (docs/0073
    /// D4); restore re-opens from the entry.
    Pr(String, i64),
    Issue(String, gql::Issue),
    NewIssue(gql::Repo),
    NewPr(gql::Repo),
}

impl GithubPlace {
    fn key(&self) -> String {
        match self {
            Self::Repo(name, tab) => format!("repo:{name}:{}", tab.key_tag()),
            Self::Pr(repo, number) => format!("pr:{repo}#{number}"),
            Self::Issue(repo, issue) => format!("issue:{repo}#{}", issue.number),
            Self::NewIssue(repo) => format!("new-issue:{}", repo.full_name),
            Self::NewPr(repo) => format!("new-pr:{}", repo.full_name),
        }
    }
}

/// History support (docs/0048 D3): restore re-clicks, collapse is the
/// existing return-to-root. A payload this build can't downcast (a
/// stale/foreign route) is ignored — never a panic.
pub(crate) struct GithubNav {
    pub(crate) page: WeakEntity<GithubPage>,
}

impl ModuleNav for GithubNav {
    fn restore(
        &self,
        payload: &Arc<dyn std::any::Any + Send + Sync>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(place) = payload.downcast_ref::<GithubPlace>() else {
            return;
        };
        let Some(page) = self.page.upgrade() else {
            return;
        };
        page.update(cx, |this, cx| this.restore_place(place, window, cx));
    }

    fn collapse(&self, window: &mut Window, cx: &mut App) {
        let Some(page) = self.page.upgrade() else {
            return;
        };
        page.update(cx, |this, cx| this.return_to_root(window, cx));
    }
}

impl GithubPage {
    pub fn new(
        state: Entity<GithubState>,
        nav: Entity<Navigator>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let org_select = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(Vec::<SharedString>::new()),
                None,
                window,
                cx,
            )
            .searchable(true)
        });
        let repo_list =
            cx.new(|cx| ListState::new(RepoListDelegate::default(), window, cx).searchable(true));

        // Org switch (0009 D5): adopt the login, drop the repo selection
        // (it belongs to the old org), repopulate, reset the search.
        cx.subscribe_in(
            &org_select,
            window,
            |this, _, event: &SelectEvent<SearchableVec<SharedString>>, window, cx| {
                let SelectEvent::Confirm(Some(login)) = event else {
                    return;
                };
                this.org = Some(login.clone());
                // docs/0047 D3/D9: the home lane follows the account — the
                // switch refetches (the docs/0031 dedup absorbs a
                // collision with a just-fired fetch).
                this.fetch_home_prs(
                    Origin::new(Surface::ModuleHome, Trigger::ViewShown),
                    window,
                    cx,
                );
                // Label choices are repo-local (docs/0042 D2) — a new
                // org drops them with the selection.
                this.filters.prs.label = None;
                this.filters.issues.label = None;
                this.state.update(cx, |s, cx| {
                    s.selected = None;
                    cx.notify();
                });
                this.synced_repos.clear();
                this.sync_from_state_in(window, cx);
                this.repo_list.update(cx, |list, cx| {
                    list.set_query("", window, cx);
                });
                cx.notify();
            },
        )
        .detach();

        // Repo selection (0009 D4): click / Enter = confirm → the same
        // fetch path the manual rows used.
        cx.subscribe_in(
            &repo_list,
            window,
            |this, _, event: &ListEvent, window, cx| {
                if let ListEvent::Confirm(ix) = event {
                    let full_name = this.repo_list.read(cx).delegate().repo_full_name(*ix);
                    if let Some(full_name) = full_name {
                        this.select_repo(full_name, window, cx);
                    }
                }
            },
        )
        .detach();

        // Background data arrival (0009 D6): orgs/repos land on the state
        // entity asynchronously; this observer shuttles them into the
        // Select/List entities through the stored window handle.
        cx.observe(&state, |this, _, cx| this.sync_from_state(cx))
            .detach();

        // Crumb re-report on page (re)activation (docs/0014 D5): `go`
        // clears crumbs on every page switch (nav.rs), so returning to
        // this page must re-derive them from state — nothing else will.
        // The same observer carries the docs/0051 D4 view-shown fetch:
        // an edge into the github module fires the home lane.
        cx.observe(&nav, |this, nav, cx| {
            this.sync_repo_crumb(cx);
            let active = matches!(
                nav.read(cx).page,
                Page::Module(ref id) if id.as_ref() == "github"
            );
            let edge = active && !this.last_module_active;
            this.last_module_active = active;
            if edge {
                this.fetch_home_prs_shown(cx);
                // docs/0052 D5: module view-shown fires the bell's
                // lane too (age-gated — a fresh one skips).
                this.fetch_notifications_in(
                    Origin::new(Surface::ModuleHome, Trigger::ViewShown),
                    cx,
                );
            }
        })
        .detach();

        // Module-crumb re-entry (docs/0014 §5 D7, issue #100): a `go`
        // landing on the already-active github page emits `Reentered`;
        // the module answers by returning to its root — repo list, no
        // selection, no open PR page.
        cx.subscribe_in(&nav, window, |this, _, _: &NavEvent, window, cx| {
            // #744: unsent review drafts on an open PR page ask first.
            if this.pr_may_leave(LeaveIntent::ModuleRoot, window, cx) {
                this.reenter_root(window, cx);
            }
        })
        .detach();

        // docs/0052 D4 — fetch on app refocus: the window-activation
        // observer is the (only) automatic refresh trigger left after the
        // heartbeat's deletion (D1). It funnels into `refresh_open_page`,
        // which is nav-gated so nothing refetches while another module
        // owns the screen — pages stay alive behind other modules, which
        // is exactly why the gate lives there.
        cx.observe_window_activation(window, |this, window, cx| {
            // docs/0080 D2: the poll gate tracks focus both ways — the
            // state-side arm (docs/0080 D3) reads this flag per tick.
            this.state
                .update(cx, |s, _| s.window_active = window.is_window_active());
            if window.is_window_active() {
                this.refresh_open_page(cx);
            }
        })
        .detach();
        // docs/0052 D6 — the render-only wake: relative ages advance
        // without a fetch. The loop dies with the module root.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(RENDER_WAKE).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();
        // Shared quick filter (docs/0042 D5): one input narrows all
        // three boxes; every keystroke is a plain in-memory rescan.
        let row_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter rows… (#number or text)"));
        cx.subscribe_in(&row_search, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.apply_row_search(cx);
            }
        })
        .detach();
        // docs/0051 D4: computed before the literal — `nav` moves into it.
        let module_active = matches!(
            nav.read(cx).page,
            Page::Module(ref id) if id.as_ref() == "github"
        );
        let page = Self {
            state,
            nav,
            org: None,
            org_select,
            repo_list,
            pr: None,
            issue: None,
            creator: None,
            notification: None,
            creator_open: false,
            creator_pr: None,
            creator_pr_open: false,
            window: window.window_handle(),
            last_module_active: module_active,
            synced_orgs: Vec::new(),
            synced_repos: Vec::new(),
            row_done_inflight: std::collections::HashSet::new(),
            filters: filters::RepoViewFilters::default(),
            tab: RepoTab::default(),
            row_search,
            commits_list: None,
            commits_synced: None,
            commits_ref: RefKey::Default,
            commits_mine: false,
            ref_select: None,
            ref_select_trigger: None,
            ref_select_open: false,
            ref_source: RefSource::default(),
            ref_synced: None,
        };
        // Seed from whatever the state already holds (last-known
        // caches) — but NOT here: Module::init runs inside
        // `cx.open_window`'s build closure, before the window handle is
        // registered (gpui `App::open_window` inserts it only after
        // `build_root_view` returns), so `window.update` would fail
        // here and the failure would be invisible. `defer_in` runs at
        // the end of the current effect cycle, window in hand.
        cx.defer_in(window, |this, window, cx| {
            this.sync_from_state_in(window, cx);
            // docs/0081 §12: the Commits list is built here, after the
            // window's build closure (so the shell's boot-time entities
            // keep their ids) but at a fixed point in boot (so the list's
            // own element ids are stable across runs — a list built on
            // first show would get a timing-dependent entity id).
            this.ensure_commits_list(window, cx);
        });
        page
    }

    /// docs/0052 D4: the refocus entry point — refresh whichever page is
    /// open, only while the github module owns the screen, every lane
    /// at the `FOCUS_MIN_AGE` floor (rapid alt-tabbing must not become a
    /// cadence). This is the only automatic refresh trigger left (D1).
    fn refresh_open_page(&mut self, cx: &mut Context<Self>) {
        if !matches!(
            self.nav.read(cx).page,
            Page::Module(ref id) if id.as_ref() == "github"
        ) {
            return;
        }
        // Auth loss, a pending proactive refresh or both budgets paused
        // silence the whole trigger — the same gate the tick loop obeys
        // (docs/0036 D4). With one budget paused, each lane below checks
        // its own (docs/0082 Q10): a refocus never feeds an exhausted
        // budget.
        if self.state.read(cx).fully_gated() {
            return;
        }
        // Mutual exclusion (docs/0020 D1) — at most one slot is Some.
        // The sub-pages refresh their lanes behind one floor (docs/0052
        // D3): `refresh_if_stale(FOCUS_MIN_AGE)`.
        if let Some(page) = self.pr.as_ref() {
            page.update(cx, |p, cx| {
                p.refresh_if_stale(crate::service::FOCUS_MIN_AGE, cx)
            });
        } else if let Some(page) = self.issue.as_ref() {
            page.update(cx, |p, cx| {
                p.refresh_if_stale(crate::service::FOCUS_MIN_AGE, cx)
            });
        } else if let Some(page) = self.notification.as_ref() {
            page.update(cx, |p, cx| {
                p.refresh_if_stale(crate::service::FOCUS_MIN_AGE, cx)
            });
        } else if let Some(full) = self.state.read(cx).selected.clone() {
            // No slot open, a repo selected: the module main page is
            // the list surface — refresh its rows when stale (docs/0035
            // D2, at the refocus floor). The observer runs windowless,
            // so the dispatch hops through the stored window handle (the
            // connect-flow hop); the ident-keyed dedup absorbs a
            // collision with a just-fired open (docs/0031).
            let lane = self.tab.lane();
            let key = self.commits_key(cx);
            if let Some(spec) = self.state.update(cx, |s, _| {
                s.repo_tab_refresh_spec(
                    &full,
                    lane,
                    &key,
                    crate::service::FOCUS_MIN_AGE,
                    Origin::new(Surface::RepoView, Trigger::Refocus),
                )
            }) && !self.state.read(cx).gated_for(spec.kind.lane())
            {
                let window = self.window;
                let page = cx.entity();
                cx.spawn(async move |_this, cx| {
                    let _ = window.update(cx, |_, window, cx| {
                        page.update(cx, |this, cx| this.run_specs(vec![spec], window, cx));
                    });
                })
                .detach();
            }
        } else if let Some((login, scope)) = self.home_account(cx) {
            // No detail page, no repo selected: the home view is the list
            // surface (docs/0047 D3) — same refocus floor as the repo
            // rows.
            if let Some(specs) = self.state.update(cx, |s, _| {
                s.home_refresh_specs(
                    &login,
                    scope,
                    crate::service::FOCUS_MIN_AGE,
                    Origin::new(Surface::ModuleHome, Trigger::Refocus),
                )
            }) && !self
                .state
                .read(cx)
                .gated_for(Kind::AccountPullRequests.lane())
            {
                let window = self.window;
                let page = cx.entity();
                cx.spawn(async move |_this, cx| {
                    let _ = window.update(cx, |_, window, cx| {
                        page.update(cx, |this, cx| this.run_specs(specs, window, cx));
                    });
                })
                .detach();
            }
        }
    }

    /// Effective org login: the Select choice or the default (first =
    /// personal, Q10). Only relevant before orgs arrive (D6 resolves the
    /// choice on arrival).
    fn effective_org(&self, cx: &Context<Self>) -> Option<SharedString> {
        self.org.clone().or_else(|| {
            self.state
                .read(cx)
                .orgs
                .first()
                .map(|o| o.login.clone().into())
        })
    }

    /// Repos under the effective org (all repos before orgs are loaded).
    fn visible_repos(&self, cx: &Context<Self>) -> Vec<gql::Repo> {
        let state = self.state.read(cx);
        match self.effective_org(cx) {
            Some(org) => state
                .repos
                .iter()
                .filter(|r| r.owner.login == org)
                .cloned()
                .collect(),
            None => state.repos.clone(),
        }
    }

    // ------------------------------------------------------------------
    // Home view (docs/0047): the module's landing surface — every open
    // PR across the selected account's repositories.
    // ------------------------------------------------------------------

    /// The account the home lane serves (docs/0047 D2): the effective
    /// login plus its scope — `user:` when the login is the viewer
    /// themselves, `org:` otherwise. `None` before orgs arrive (the view
    /// renders its loading state then, D8).
    fn home_account(&self, cx: &Context<Self>) -> Option<(SharedString, HomeScope)> {
        let login = self.effective_org(cx)?;
        let scope = if self
            .state
            .read(cx)
            .user
            .as_ref()
            .is_some_and(|u| u.login == login.as_ref())
        {
            HomeScope::User
        } else {
            HomeScope::Org
        };
        Some((login, scope))
    }

    /// The home lane's first page for the current account (docs/0047
    /// D3) — module open / account switch / manual refresh all funnel
    /// here; the docs/0031 dedup absorbs repeats.
    fn fetch_home_prs(&mut self, origin: Origin, window: &mut Window, cx: &mut Context<Self>) {
        let Some((login, scope)) = self.home_account(cx) else {
            return;
        };
        let specs = self
            .state
            .update(cx, |s, _| vec![s.home_open_spec(&login, scope, origin)]);
        self.run_specs(specs, window, cx);
    }

    /// docs/0052 D5: the global notifications lane's view-shown fire —
    /// module open re-shows the surface the bell summarizes; the
    /// state-side gate (pause, backoff, `FOCUS_MIN_AGE`) decides whether
    /// a spec comes back at all.
    fn fetch_notifications_if_stale(
        &mut self,
        origin: Origin,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let specs = self.state.update(cx, |s, _| {
            s.notifications_spec_if_stale(crate::service::FOCUS_MIN_AGE, origin)
                .map(|spec| vec![spec])
                .unwrap_or_default()
        });
        self.run_specs(specs, window, cx);
    }

    /// The windowless view-shown hop (docs/0052 D5): the nav observer
    /// carries no window, so the dispatch rides the stored handle — the
    /// same hop the connect flow uses.
    fn fetch_notifications_in(&mut self, origin: Origin, cx: &mut Context<Self>) {
        let specs = self.state.update(cx, |s, _| {
            s.notifications_spec_if_stale(crate::service::FOCUS_MIN_AGE, origin)
                .map(|spec| vec![spec])
                .unwrap_or_default()
        });
        if specs.is_empty() {
            return;
        }
        let window = self.window;
        let page = cx.entity();
        cx.spawn(async move |_this, cx| {
            let _ = window.update(cx, |_, window, cx| {
                page.update(cx, |this, cx| this.run_specs(specs, window, cx));
            });
        })
        .detach();
    }

    /// "Load more" (docs/0047 D4): the next page for the current
    /// account, from its snapshot's cursor. No cursor → no-op (the
    /// button isn't rendered then either).
    fn load_more_home_prs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((login, scope)) = self.home_account(cx) else {
            return;
        };
        let specs = self.state.update(cx, |s, _| {
            let cursor = s
                .home_lanes
                .get(&(login.to_string(), scope))
                .and_then(|e| e.cursor.clone());
            cursor.map(|c| vec![s.home_more_spec(&login, scope, &c)])
        });
        if let Some(specs) = specs {
            self.run_specs(specs, window, cx);
        }
    }

    /// Everything the home view renders from (docs/0047 D2/D6), owned so
    /// the state borrow can drop before the element tree builds:
    /// - `view`: the derived open list + lane books (docs/0073 D3) —
    ///   the lane's server key order ⊗ the store's documents, filtered
    ///   to OPEN at read time; `None` = never landed;
    /// - `clock`: the freshness cluster's clock — the lane's own when
    ///   landed (provenance stays honest, D6);
    /// - `error`: the lane's last failure, shown with Retry only when
    ///   nothing else has rows (D3).
    fn home_source(&self, cx: &Context<Self>) -> HomeSource {
        let state = self.state.read(cx);
        let (login, scope) = match self.home_account(cx) {
            Some(it) => it,
            None => {
                return HomeSource {
                    view: None,
                    clock: LandedClock(None),
                    error: state.home_error.clone(),
                };
            }
        };
        let view = state.lane_view(&login, scope);
        let clock = if view.is_some() {
            LandedClock(state.home_landed)
        } else {
            // Never landed yet — no provenance to show (docs/0051 D5
            // replaced the D6 union with skeletons).
            LandedClock(None)
        };
        HomeSource {
            view,
            clock,
            error: state.home_error.clone(),
        }
    }

    /// The home view itself (docs/0047 D1): header + freshness cluster,
    /// the shared quick filter and PR filter toolbar (D5), the open-PR
    /// rows, and the paging affordances (D4). Replaces the old
    /// "Select a repository" placeholder.
    fn render_home_view(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let HomeSource { view, clock, error } = self.home_source(cx);
        let landed = view.is_some();
        let account = self.home_account(cx);
        let viewer = self.state.read(cx).user.as_ref().map(|u| u.login.clone());
        let search = self.filters.search.clone();
        let filtered = self.filters.prs.is_filtered() || !search.is_empty();

        // Rows via the 0042 engine over the derived lane (docs/0073
        // D3); no lane yet → skeleton rows (docs/0051 D5 — the D6
        // union is gone).
        let (source, total_count, complete, has_cursor) = match view {
            Some(v) => (v.rows, v.total_count, v.complete, v.has_cursor),
            None => (Vec::new(), 0, true, false),
        };
        let pr_rows = filters::apply_prs(&self.filters.prs, &source, viewer.as_deref(), &search);
        let source_len = source.len();

        // Label options derive from the present nodes (docs/0042 D3).
        let pr_labels: Vec<String> = {
            let mut names = std::collections::BTreeSet::new();
            for p in &source {
                for n in filters::label_names(p.labels.as_ref()) {
                    names.insert(n.to_string());
                }
            }
            names.into_iter().collect()
        };

        // The freshness cluster refreshes the account it renders for.
        let refresh = account.clone();
        page_canvas()
            .child(
                h_flex()
                    .id("home-header")
                    .justify_between()
                    .gap_3()
                    .items_start()
                    .child(
                        v_flex()
                            .gap_1()
                            .min_w_0()
                            .child(page_title("Open pull requests"))
                            .when_some(account.as_ref().map(|(l, _)| l.clone()), |col, login| {
                                col.child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(login),
                                )
                            }),
                    )
                    .child(freshness_row(
                        "home-prs",
                        &clock,
                        cx,
                        move |this, window, cx| {
                            if let Some((login, scope)) = refresh.clone() {
                                let specs = this.state.update(cx, |s, _| {
                                    vec![s.home_open_spec(
                                        &login,
                                        scope,
                                        Origin::new(Surface::ModuleHome, Trigger::Manual),
                                    )]
                                });
                                this.run_specs(specs, window, cx);
                            }
                        },
                    )),
            )
            .child(
                div()
                    .id("home-row-search")
                    .w_full()
                    .child(Input::new(&self.row_search).small()),
            )
            .child(
                GroupBox::new()
                    .id("home-prs-box")
                    .title(filters::box_title(
                        "Open PRs",
                        total_count.max(source_len as i64) as usize,
                        pr_rows.len(),
                        filtered,
                    ))
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(self.pr_filter_toolbar(&pr_labels, cx))
                            .child(if pr_rows.is_empty() {
                                if source_len == 0 {
                                    if let Some(err) = &error {
                                        // D3: first-ever fetch failed with
                                        // nothing to fall back on.
                                        v_flex()
                                            .gap_2()
                                            .child(muted_cell_keyed(
                                                format!("Couldn't load open pull requests: {err}"),
                                                Some("home-prs-error"),
                                                cx,
                                            ))
                                            .child(
                                                Button::new("home-prs-retry")
                                                    .debug_selector(|| "home-prs-retry".to_string())
                                                    .label("Retry")
                                                    .ghost()
                                                    .small()
                                                    .on_click(cx.listener(
                                                        |this, _, window, cx| {
                                                            this.fetch_home_prs(
                                                                Origin::new(
                                                                    Surface::ModuleHome,
                                                                    Trigger::Manual,
                                                                ),
                                                                window,
                                                                cx,
                                                            );
                                                        },
                                                    )),
                                            )
                                            .into_any_element()
                                    } else if !landed {
                                        // docs/0051 D5 — skeleton-first:
                                        // the lane (or the orgs it scopes
                                        // to) has not landed yet.
                                        home_skeleton_rows()
                                    } else {
                                        muted_cell_keyed(
                                            "No open pull requests".into(),
                                            Some("home-prs-empty"),
                                            cx,
                                        )
                                    }
                                } else {
                                    muted_cell_keyed(
                                        "No PRs match the current filters".into(),
                                        Some("home-prs-no-match"),
                                        cx,
                                    )
                                }
                            } else {
                                v_flex()
                                    .gap_0p5()
                                    .children(
                                        pr_rows
                                            .iter()
                                            .map(|p| self.render_pr_row_inner(p, true, cx)),
                                    )
                                    // docs/0047 D4: paging affordances —
                                    // unfiltered only (a filter hides rows
                                    // by choice, not by cap; the repo
                                    // view's #87 precedent).
                                    .when(!filtered, |col| {
                                        let load_more = !complete && has_cursor;
                                        let hint = if landed {
                                            // The three truthful states (D4).
                                            home_prs_hint(source_len, total_count)
                                        } else {
                                            // No lane yet: skeletons own
                                            // the row area (docs/0051 D5).
                                            None
                                        };
                                        col.when(load_more, |col| {
                                            col.child(
                                                Button::new("home-prs-more")
                                                    .debug_selector(|| "home-prs-more".to_string())
                                                    .label("Load more")
                                                    .ghost()
                                                    .small()
                                                    .on_click(cx.listener(
                                                        |this, _, window, cx| {
                                                            this.load_more_home_prs(window, cx);
                                                        },
                                                    )),
                                            )
                                        })
                                        .children(
                                            hint.map(|h| {
                                                div()
                                                    .debug_selector(|| "home-prs-hint".to_string())
                                                    .child(h)
                                                    .into_any_element()
                                            }),
                                        )
                                    })
                                    .into_any_element()
                            }),
                    ),
            )
            .into_any_element()
    }

    /// Observer entry (0009 D6): `cx.observe` has no window, so go
    /// through the stored handle. The work is deferred via `cx.spawn`
    /// (the `run_specs` pattern): a notify can land while this page is
    /// mid-update (e.g. the repo-List subscriber notifying the state),
    /// and updating the page synchronously there would double-lease it
    /// and panic. A failed update (window closing) leaves the guards
    /// below stale — the next observer pass retries.
    fn sync_from_state(&mut self, cx: &mut Context<Self>) {
        let window = self.window;
        let page = cx.entity();
        cx.spawn(async move |_this, cx| {
            let _ = window.update(cx, |_, window, cx| {
                page.update(cx, |this, cx| this.sync_from_state_in(window, cx));
            });
        })
        .detach();
    }

    /// Derive the module's full crumb chain from state + the open PR
    /// sub-page, and reconcile it against the navigator (docs/0014 D1,
    /// amended by 0016 D1: the derivation now owns the sub-page suffix
    /// too, so the chain is `[repo]` or `[repo, #number]`). Called on
    /// data changes (end of `sync_from_state_in`), on page
    /// (re)activation (the nav observer), and on PR open/close;
    /// compare-first, so idle ticks write nothing.
    fn sync_repo_crumb(&mut self, cx: &mut Context<Self>) {
        // Gate: never touch crumbs while another page owns the chrome
        // ("github" literal — card.rs:36 precedent).
        let active = matches!(
            self.nav.read(cx).page,
            Page::Module(ref id) if id.as_ref() == "github"
        );
        if !active {
            return;
        }
        let repo_label = {
            let s = self.state.read(cx);
            repo_crumb_label(s.selected.as_deref(), &s.repos, s.auth.connected())
        };
        // Only one sub-page is ever open (docs/0020 D1) — either way the
        // crumb gains the same `#number` suffix.
        let pr_number = self
            .pr
            .as_ref()
            .map(|p| p.read(cx).number)
            .or(self.issue.as_ref().map(|p| p.read(cx).number));
        let mut derived: Vec<SharedString> = Vec::new();
        if let Some(label) = repo_label {
            derived.push(label);
        }
        if self.notification.is_some() {
            // docs/0026 D1: no number — thread ids are huge REST ids,
            // not human labels.
            derived.push("· notification".into());
        } else if let Some(number) = pr_number {
            derived.push(format!("#{number}").into());
        } else if self.creator_open && self.creator.is_some() {
            // The creator's suffix (docs/0024 D3) — no number yet.
            derived.push("#new".into());
        } else if self.creator_pr_open && self.creator_pr.is_some() {
            // The PR creator's suffix (docs/0044 D3) — no number yet.
            derived.push("#new-pr".into());
        }
        let current: Vec<SharedString> = self
            .nav
            .read(cx)
            .crumbs()
            .iter()
            .map(|c| c.label.clone())
            .collect();
        if let Some(labels) = reconcile_repo_crumb(&derived, &current) {
            log::debug!("repo crumb synced: {current:?} -> {labels:?}");
            // With a sub-page open (PR/issue/creator/notification), the repo
            // crumb becomes the back-nav anchor: whichever page is open
            // yields to the repo view on click. The `#number` /
            // `· notification` crumb is current-location, non-clickable
            // (docs/0014 deferred the `on_click` to the sub-pages docs).
            let sub_page_open = pr_number.is_some()
                || self.creator_open
                || self.creator_pr_open
                || self.notification.is_some();
            let page = cx.entity();
            let crumbs: Vec<Crumb> = labels
                .into_iter()
                .enumerate()
                .map(|(ix, label)| {
                    if ix == 0 && sub_page_open {
                        let page = page.clone();
                        Crumb::action(
                            label,
                            Arc::new(move |window, cx| {
                                page.update(cx, |this, cx| {
                                    // #744: unsent review drafts ask first.
                                    if this.pr_may_leave(LeaveIntent::RepoCrumb, window, cx) {
                                        this.show_repo_view(cx);
                                    }
                                });
                            }),
                        )
                    } else {
                        Crumb::label(label)
                    }
                })
                .collect();
            self.nav.update(cx, |nav, cx| {
                nav.set_crumbs(crumbs, cx);
            });
        }
    }

    /// Compare-first sync of the Select/List entities from the state
    /// (0009 D6). The `synced_*` guards are committed only AFTER the
    /// entity updates ran — a sync that never reached a window is not
    /// recorded as done.
    fn sync_from_state_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Sign-out / auth loss auto-closes an open PR page (docs/0016 D1):
        // the connect panel owns the module again. The crumb reconcile at
        // the end of this sync clears the suffix (disconnected = no crumbs).
        if self.pr.is_some() && !self.state.read(cx).auth.connected() {
            self.pr = None;
        }
        // The issue page tears down under the same rule (docs/0020 D1).
        if self.issue.is_some() && !self.state.read(cx).auth.connected() {
            self.issue = None;
        }
        // The notification page tears down under the same rule (docs/0026 D1).
        if self.notification.is_some() && !self.state.read(cx).auth.connected() {
            self.notification = None;
        }
        // The creator too — and unlike Cancel/Esc this *drops* it:
        // drafts are session-scoped to a connected session (docs/0024 D7).
        if self.creator.is_some() && !self.state.read(cx).auth.connected() {
            self.creator = None;
            self.creator_open = false;
        }
        // The PR creator too (docs/0044 D7 — same rule, same drop).
        if self.creator_pr.is_some() && !self.state.read(cx).auth.connected() {
            self.creator_pr = None;
            self.creator_pr_open = false;
        }
        let logins: Vec<SharedString> = self
            .state
            .read(cx)
            .orgs
            .iter()
            .map(|o| o.login.clone().into())
            .collect();
        if logins != self.synced_orgs {
            let choice = next_org_selection(self.org.as_deref(), &logins);
            let org_changed = choice != self.org;
            self.org_select.update(cx, |s, cx| {
                s.set_items(SearchableVec::from(logins.clone()), window, cx);
                if let Some(value) = &choice {
                    s.set_selected_value(value, window, cx);
                }
            });
            self.org = choice.clone();
            self.synced_orgs = logins.clone();
            // docs/0047 D3: orgs arriving (or the resolved choice
            // changing) is the module-open trigger for the home lane.
            if org_changed {
                self.fetch_home_prs(
                    Origin::new(Surface::ModuleHome, Trigger::ViewShown),
                    window,
                    cx,
                );
            }
            log::debug!(
                "org select synced: {} orgs, selected: {:?}",
                self.synced_orgs.len(),
                self.org
            );
        }

        let repos = self.visible_repos(cx);
        let keys: Vec<String> = repos.iter().map(|r| r.full_name.clone()).collect();
        if keys != self.synced_repos {
            let selected = self.state.read(cx).selected.clone();
            let position = selected
                .as_deref()
                .and_then(|f| repos.iter().position(|r| r.full_name == f))
                .map(IndexPath::new);
            self.repo_list.update(cx, |l, cx| {
                l.delegate_mut().set_repos(repos);
                // Non-scrolling selection sync: keep the highlight on
                // the selected repo without yanking the viewport.
                l.set_selected_index(position, window, cx);
            });
            self.synced_repos = keys.clone();
            log::debug!("repo list synced: {} repos", self.synced_repos.len());
        }

        // Keep the module's breadcrumb prefix in step with the selection
        // (docs/0014 D1): repo pick, org switch, sign-out, auth loss and
        // snapshot swaps all land here.
        self.sync_repo_crumb(cx);

        cx.notify();
    }

    /// Record a user-driven move to `place` (docs/0048 D3/D4): the
    /// trail entry carries the payload a restore will hand back to
    /// `restore_place`. Same-key visits dedup in the navigator, which
    /// also makes restore-via-`select_repo`/`open_*` self-neutral.
    fn record_place(&self, place: GithubPlace, cx: &mut Context<Self>) {
        self.nav.update(cx, |nav, cx| {
            nav.visit(
                Route::module_place("github", place.key(), Arc::new(place)),
                cx,
            );
        });
    }

    /// Re-activate a recorded place (docs/0048 D5) — literally the
    /// click handlers, so fetches ride 0031 dedup and the detail
    /// pages' own refresh cadence.
    fn restore_place(&mut self, place: &GithubPlace, window: &mut Window, cx: &mut Context<Self>) {
        // Deep places sit on the repo view: re-select the repo first
        // when the current position differs (the ancestor activation).
        let repo: Option<String> = match place {
            GithubPlace::Repo(..) => None,
            GithubPlace::Pr(r, _) | GithubPlace::Issue(r, _) => Some(r.clone()),
            GithubPlace::NewIssue(r) | GithubPlace::NewPr(r) => Some(r.full_name.clone()),
        };
        if let Some(repo) = repo {
            let selected = self.state.read(cx).selected.as_deref();
            if selected != Some(repo.as_str()) {
                self.select_repo_inner(SharedString::from(repo), window, cx);
            }
        }
        match place {
            GithubPlace::Repo(name, tab) => {
                // Back to the repo place (docs/0048 D5) lands ON the repo
                // view, so no sub-page can outlive the landing beneath it.
                // A live repo click never arrives through an open sub-page
                // (full takeover unmounts the repo list), so the restore
                // closes the takeover slots itself — Esc's close semantics,
                // selection surviving, drafts retained (docs/0024 D7,
                // docs/0044 D7).
                self.close_pr(cx);
                self.close_issue(cx);
                self.close_notification(cx);
                self.creator_open = false;
                self.creator_pr_open = false;
                // The re-select is silent (the trail head IS this route)
                // and the tab re-enters through the live switch path
                // (docs/0077 D1 amended): the freshness gate and the
                // docs/0031 dedup decide what refetches, so Back to a
                // fresh lane is not a refetch storm. A same-tab restore
                // (Pulls after the reset) is the switch's no-op arm.
                let name = SharedString::from(name.clone());
                self.select_repo_inner(name, window, cx);
                self.switch_repo_tab(*tab, window, cx);
            }
            GithubPlace::Pr(repo, number) => {
                // docs/0073 D4 (stage 2): the place carries identity —
                // the store still holds the document (summaries persist
                // for the session); restore re-opens from the entry.
                let row = self
                    .state
                    .read(cx)
                    .pr_entry(repo, *number)
                    .map(|e| e.summary.clone());
                if let Some(row) = &row {
                    self.open_pr(repo, row, None, window, cx);
                }
            }
            GithubPlace::Issue(repo, issue) => self.open_issue(repo, issue, None, window, cx),
            GithubPlace::NewIssue(repo) => self.open_creator(repo, window, cx),
            GithubPlace::NewPr(repo) => self.open_creator_pr(repo, window, cx),
        }
    }

    fn select_repo(
        &mut self,
        full_name: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_repo_inner(full_name.clone(), window, cx);
        // The inner reset landed the tab on Pulls first (docs/0077 D1),
        // so the selection visit is the pulls place.
        self.record_place(GithubPlace::Repo(full_name.to_string(), self.tab), cx);
    }

    /// The repo selection without the trail write — the ancestor
    /// activation a deep restore performs silently (docs/0048 D5: the
    /// deep route is the trail head; an ancestor visit would push and
    /// truncate the forward tail).
    fn select_repo_inner(
        &mut self,
        full_name: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Docs/0007 D6: the view paints immediately from the in-memory
        // snapshot; these specs are the edge-freshness repo-scoped
        // snapshot plus the one-shot repo notifications fetch.
        log::debug!("repo selected: {full_name}");
        // Draft retention is repo-keyed (docs/0024 D7): a switch to
        // another repo drops the old creator entirely.
        if self
            .creator
            .as_ref()
            .is_some_and(|c| c.read(cx).repo != full_name)
        {
            self.creator = None;
            self.creator_open = false;
        }
        if self
            .creator_pr
            .as_ref()
            .is_some_and(|c| c.read(cx).repo != full_name)
        {
            self.creator_pr = None;
            self.creator_pr_open = false;
        }
        // Label choices are repo-local (docs/0042 D2): a switch to
        // another repo resets them; author/sort/search survive.
        self.filters.prs.label = None;
        self.filters.issues.label = None;
        // docs/0077 D1: opening a repo view lands on the Open PRs tab.
        self.tab = RepoTab::Pulls;
        // docs/0081 D7/§14: and the Commits tab on the default branch,
        // unfiltered.
        self.commits_ref = RefKey::Default;
        self.commits_mine = false;
        let spec = self.state.update(cx, |s, cx| {
            s.selected = Some(full_name.to_string());
            let spec = s.repo_lane_spec(
                &full_name,
                RepoLane::Pulls,
                Origin::new(Surface::RepoView, Trigger::ViewShown),
            );
            cx.notify();
            spec
        });
        self.run_specs(vec![spec], window, cx);
    }

    /// Tab switch (docs/0077 D1/D3): state write + notify, then offer
    /// the new lane's spec — the freshness gate (docs/0077 D4) and the
    /// docs/0031 dedup decide whether it flies, so a first visit always
    /// fetches and a rapid toggle back is absorbed.
    fn switch_repo_tab(&mut self, tab: RepoTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        if tab == RepoTab::Commits {
            self.ensure_commits_list(window, cx);
        }
        cx.notify();
        // The switch is a visit (docs/0048 D4 amended): the tab rides the
        // place key, so Back/Forward re-enter the exact lane. Recorded
        // before the connected/gated early-returns — the trail records
        // navigation, not fetches.
        let selected = self.state.read(cx).selected.clone();
        if let Some(full) = selected {
            self.record_place(GithubPlace::Repo(full, tab), cx);
        }
        let (connected, gated, selected) = {
            let s = self.state.read(cx);
            (
                s.auth.connected(),
                s.gated_for(tab.lane().budget()),
                s.selected.clone(),
            )
        };
        if !connected || gated {
            return;
        }
        let Some(full) = selected else {
            return;
        };
        let lane = tab.lane();
        let key = self.commits_key(cx);
        if tab == RepoTab::Commits {
            // docs/0081 D3: a visited book outlives the LRU.
            self.state
                .update(cx, |s, _| s.touch_commit_lane(&full, &key));
        }
        let Some(spec) = self.state.update(cx, |s, _| {
            s.repo_tab_refresh_spec(
                &full,
                lane,
                &key,
                crate::service::FOCUS_MIN_AGE,
                Origin::new(Surface::RepoView, Trigger::ViewShown),
            )
        }) else {
            return;
        };
        self.run_specs(vec![spec], window, cx);
    }

    /// Open the in-app PR page for a snapshot row (docs/0016 D1): the
    /// page takes over the module content, fetches its detail (D2), and
    /// the breadcrumb gains the `#number` suffix. The repo row's old
    /// browser hand-off moves to the page header's ↗ button.
    /// `anchor` (docs/0026 D9, #154): the REST comment id to jump to from
    /// the notification hand-off — `None` from repo rows (no anchor).
    fn open_pr(
        &mut self,
        repo: &str,
        pr: &gql::PullRequest,
        anchor: Option<i64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        log::debug!("pr page opened: {}#{}", repo, pr.number);
        // docs/0073 D4: the clicked row is the document's summary
        // layer — land it so the store holds the document before the
        // page exists (the page paints from the store; it keeps no
        // snapshot of its own).
        self.state.update(cx, |s, _| s.land_summary(repo, pr));
        let page = cx.new(|cx| {
            pr_page::PrDetailPage::new(
                self.state.clone(),
                repo.to_string(),
                pr.number,
                anchor,
                window,
                cx,
            )
        });
        // Esc on the PR page asks its host to close it (docs/0016 D1).
        cx.subscribe_in(
            &page,
            window,
            |this, _, event: &pr_page::PrPageEvent, window, cx| {
                let pr_page::PrPageEvent::Leave(intent) = event;
                match intent {
                    LeaveIntent::Close => this.close_pr(cx),
                    LeaveIntent::RepoCrumb => this.show_repo_view(cx),
                    LeaveIntent::ModuleRoot => this.reenter_root(window, cx),
                }
            },
        )
        .detach();
        // Mutual exclusion (docs/0020 D1): only one sub-page owns the
        // module content — opening a PR closes any open issue page (and
        // hides the creators, docs/0024 D3/docs/0044 D3; drafts
        // survive, D7).
        self.creator_open = false;
        self.creator_pr_open = false;
        self.issue = None;
        self.notification = None;
        self.pr = Some(page.clone());
        self.sync_repo_crumb(cx);
        self.record_place(GithubPlace::Pr(repo.to_string(), pr.number), cx);
        let focus = page.read(cx).focus_handle(cx).clone();
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Open the in-app issue page for a snapshot row (docs/0020 D1): the
    /// PR page's takeover pattern — full-content sub-page, detail fetch,
    /// crumb `#number` suffix. The row's old browser hand-off moves to
    /// the page header's ↗ button. Opening it closes any open PR page
    /// (mutual exclusion, 0020 D1). `anchor` rides from the notification
    /// hand-off (docs/0026 D9); repo rows pass `None`.
    fn open_issue(
        &mut self,
        repo: &str,
        issue: &gql::Issue,
        anchor: Option<i64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        log::debug!("issue page opened: {}#{}", repo, issue.number);
        let page = cx.new(|cx| {
            issue_page::IssueDetailPage::new(
                self.state.clone(),
                repo.to_string(),
                issue.clone(),
                anchor,
                window,
                cx,
            )
        });
        // Esc on the issue page asks its host to close it (docs/0020 D1).
        cx.subscribe_in(
            &page,
            window,
            |this, _, _: &issue_page::IssuePageEvent, _, cx| {
                this.close_issue(cx);
            },
        )
        .detach();
        // Mutual exclusion: opening the issue page hides the creator
        // (docs/0024 D3; its draft survives, D7).
        self.creator_open = false;
        self.creator_pr_open = false;
        self.pr = None;
        self.notification = None;
        self.issue = Some(page.clone());
        self.sync_repo_crumb(cx);
        self.record_place(GithubPlace::Issue(repo.to_string(), issue.clone()), cx);
        let focus = page.read(cx).focus_handle(cx).clone();
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Module-crumb re-entry (docs/0014 §5 D7, issue #100): back to the
    /// module root — the plain repo list. Drops the repo selection
    /// (org-switch idiom: clear + guard-reset so the List highlight
    /// follows through `sync_from_state_in`) and closes any open PR
    /// page. Re-entry with nothing open is a no-op. Unlike `close_pr`,
    /// selection does NOT survive: the crumb click means "repo list",
    /// not "this repo without its PR".
    fn return_to_root(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        log::debug!("module crumb re-entered: returning to repo list");
        self.filters.prs.label = None;
        self.filters.issues.label = None;
        self.state.update(cx, |s, cx| {
            s.selected = None;
            cx.notify();
        });
        self.synced_repos.clear();
        self.sync_from_state_in(window, cx);
        self.close_pr(cx);
        self.close_issue(cx);
        self.close_notification(cx);
        // No selection = no repo the draft could belong to (docs/0024
        // D7, docs/0044 D7).
        self.creator = None;
        self.creator_open = false;
        self.creator_pr = None;
        self.creator_pr_open = false;
        cx.notify();
    }

    /// Whether the open PR page may go now; with unsent review drafts it
    /// opens its discard confirm instead and answers later through
    /// `PrPageEvent::Leave` (#744).
    fn pr_may_leave(
        &self,
        intent: LeaveIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match self.pr.clone() {
            Some(page) => page.update(cx, |p, cx| p.guard_leave(intent, window, cx)),
            None => true,
        }
    }

    /// The repo crumb's body: whichever sub-page is open yields to the
    /// repo view (close_* no-op when absent).
    fn show_repo_view(&mut self, cx: &mut Context<Self>) {
        self.close_pr(cx);
        self.close_issue(cx);
        self.close_notification(cx);
        self.close_creator(cx);
        self.close_creator_pr(cx);
        // A user place-change (docs/0048 D4): the crumb click is "show
        // the repo view", so the trail records it — at the active tab
        // (docs/0077 D1 amended).
        if let Some(name) = self.state.read(cx).selected.clone() {
            self.record_place(GithubPlace::Repo(name.to_string(), self.tab), cx);
        }
    }

    /// Module-crumb re-entry body (docs/0014 §5 D7): root, then the
    /// home lane refresh (docs/0051 D4 — docs/0031 dedup absorbs a
    /// collision with the edge fetch).
    fn reenter_root(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.return_to_root(window, cx);
        self.fetch_home_prs(
            Origin::new(Surface::ModuleHome, Trigger::ViewShown),
            window,
            cx,
        );
        // docs/0052 D5: crumb re-entry is a view-shown edge too.
        self.fetch_notifications_if_stale(
            Origin::new(Surface::ModuleHome, Trigger::ViewShown),
            window,
            cx,
        );
    }

    /// Close the PR sub-page: back to the repo view (selection survives —
    /// the Select/List entities kept their state while unmounted) and the
    /// crumb chain loses its `#number` suffix.
    fn close_pr(&mut self, cx: &mut Context<Self>) {
        if self.pr.take().is_some() {
            log::debug!("pr page closed");
            self.sync_repo_crumb(cx);
            cx.notify();
        }
    }

    /// Close the issue sub-page (docs/0020 D1): the `close_pr` mirror —
    /// back to the repo view, the crumb chain loses its `#number` suffix.
    fn close_issue(&mut self, cx: &mut Context<Self>) {
        if self.issue.take().is_some() {
            log::debug!("issue page closed");
            self.sync_repo_crumb(cx);
            cx.notify();
        }
    }

    /// Open the in-app notification page for a repo-view row (docs/0026
    /// D1): the PR/issue takeover pattern — full-content sub-page, Esc
    /// closes, crumb gains the `· notification` suffix — and the row's
    /// browser hand-off moves into the page header's ↗ button. Opening
    /// it closes any other sub-page (mutual exclusion). The page fires
    /// its mark-read + thread fetch the instant it is built (docs/0026
    /// §4.3), so nothing extra runs here.
    fn open_notification(
        &mut self,
        n: api::Notification,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        log::debug!("notification page opened: thread {}", n.id);
        let page = cx.new(|cx| {
            notification_page::NotificationDetailPage::new(self.state.clone(), n, window, cx)
        });
        // Esc asks the host to close; MarkSettled refetches the unread
        // lists (docs/0026 D3); OpenSubject swaps in the native issue/PR
        // page with a synthesized seed (docs/0026 D6).
        cx.subscribe_in(
            &page,
            window,
            |this, _, event: &NotificationPageEvent, window, cx| match event {
                NotificationPageEvent::CloseRequested => this.close_notification(cx),
                NotificationPageEvent::MarkSettled => {
                    let specs = this
                        .state
                        .update(cx, |s, _| s.mark_refetch_specs(Surface::NotificationPage));
                    this.run_specs(specs, window, cx);
                }
                // docs/0026 D8, owner decision §4.4: a settled done
                // closes the page back to the repo view (the github.com
                // inbox behavior — the thread is gone from the inbox
                // entirely) and refetches so the row + card drop.
                NotificationPageEvent::DoneSettled => {
                    let specs = this
                        .state
                        .update(cx, |s, _| s.mark_refetch_specs(Surface::NotificationPage));
                    this.run_specs(specs, window, cx);
                    this.close_notification(cx);
                }
                NotificationPageEvent::OpenSubject {
                    is_pr,
                    full_name,
                    number,
                    title,
                    anchor,
                } => {
                    this.close_notification(cx);
                    // A synthesized minimal row — initial paint only;
                    // the native page re-fetches full detail on open.
                    let key = format!("{full_name}#{number}");
                    if *is_pr {
                        let pr = gql::PullRequest {
                            key,
                            database_id: None,
                            number: *number,
                            title: title.clone(),
                            draft: false,
                            state: None,
                            author: None,
                            updated_at: None,
                            url: String::new(),
                            status: None,
                            labels: None,
                            review_requests: None,
                        };
                        this.open_pr(full_name, &pr, *anchor, window, cx);
                    } else {
                        let issue = gql::Issue {
                            key,
                            database_id: None,
                            number: *number,
                            state: None,
                            title: title.clone(),
                            author: None,
                            updated_at: None,
                            url: String::new(),
                            labels: None,
                        };
                        this.open_issue(full_name, &issue, *anchor, window, cx);
                    }
                }
            },
        )
        .detach();
        self.creator_open = false;
        self.creator_pr_open = false;
        self.pr = None;
        self.issue = None;
        self.notification = Some(page.clone());
        self.sync_repo_crumb(cx);
        let focus = page.read(cx).focus_handle(cx).clone();
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Close the notification sub-page (docs/0026 D1): the `close_pr`
    /// mirror — back to the repo view, the crumb loses `· notification`.
    fn close_notification(&mut self, cx: &mut Context<Self>) {
        if self.notification.take().is_some() {
            log::debug!("notification page closed");
            self.sync_repo_crumb(cx);
            cx.notify();
        }
    }
    /// Open the issue creator (docs/0024 D3): the 0016 D1 takeover — full
    /// content, crumb `#new`, mutual exclusion with the PR/issue pages.
    /// The entity is reused when one already exists for this repo, so a
    /// cancel/reopen round-trip keeps the draft (D7).
    fn open_creator(&mut self, repo: &gql::Repo, window: &mut Window, cx: &mut Context<Self>) {
        log::debug!("issue creator opened: {}", repo.full_name);
        if self
            .creator
            .as_ref()
            .is_none_or(|c| c.read(cx).repo != repo.full_name)
        {
            let node_id = repo.node_id.clone().unwrap_or_default();
            self.creator = Some(cx.new(|cx| {
                NewIssuePage::new(
                    self.state.clone(),
                    repo.full_name.clone(),
                    node_id,
                    window,
                    cx,
                )
            }));
        } else if let Some(creator) = &self.creator {
            // docs/0046: templates + blank config re-fetch per open; the
            // draft (and the fields) survive.
            creator.update(cx, |page, cx| {
                page.refetch_templates(window, cx);
            });
        }
        // Esc on the creator asks its host to hide it (D7); `Created`
        // hands the host the new issue row (D6).
        cx.subscribe_in(
            self.creator.as_ref().unwrap(),
            window,
            |this, _, event: &NewIssuePageEvent, window, cx| match event {
                NewIssuePageEvent::CloseRequested => this.close_creator(cx),
                NewIssuePageEvent::Created(issue) => {
                    let issue = issue.as_ref();
                    let repo = this.creator.take().map(|c| c.read(cx).repo.clone());
                    this.creator_open = false;
                    if let Some(repo) = repo {
                        // Refetch-on-create (D6, docs/0077 D3): the
                        // created artifact's lane re-lands its tab — the
                        // house one-refetch path, no optimistic insertion.
                        let spec = this.state.update(cx, |s, _| {
                            s.repo_lane_spec(
                                &repo,
                                RepoLane::Issues,
                                Origin::new(Surface::RepoView, Trigger::ViewShown),
                            )
                        });
                        this.run_specs(vec![spec], window, cx);
                        // A synthesized minimal row (gql::created_issue_from_body)
                        // — initial paint only; the detail page re-fetches.
                        this.open_issue(&repo, issue, None, window, cx);
                    }
                }
            },
        )
        .detach();
        self.pr = None;
        self.issue = None;
        self.notification = None;
        self.creator_pr_open = false;
        self.creator_open = true;
        self.sync_repo_crumb(cx);
        self.record_place(GithubPlace::NewIssue(repo.clone()), cx);
        let focus = self
            .creator
            .as_ref()
            .unwrap()
            .read(cx)
            .focus_handle(cx)
            .clone();
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Hide the creator (D7): back to the repo view, the crumb chain
    /// loses its `#new` suffix. The entity intentionally stays alive —
    /// reopening restores the draft.
    fn close_creator(&mut self, cx: &mut Context<Self>) {
        if self.creator_open {
            log::debug!("issue creator closed (draft retained)");
            self.creator_open = false;
            self.sync_repo_crumb(cx);
            cx.notify();
        }
    }

    /// Open the PR creator (docs/0044 D3): the 0024 creator's takeover
    /// pattern — full content, crumb `#new-pr`, mutual exclusion with
    /// the PR/issue pages and the issue creator. The entity is reused
    /// when one already exists for this repo, so a cancel/reopen
    /// round-trip keeps the draft (D7); branches re-fetch per open.
    fn open_creator_pr(&mut self, repo: &gql::Repo, window: &mut Window, cx: &mut Context<Self>) {
        log::debug!("pr creator opened: {}", repo.full_name);
        if self
            .creator_pr
            .as_ref()
            .is_none_or(|c| c.read(cx).repo != repo.full_name)
        {
            let node_id = repo.node_id.clone().unwrap_or_default();
            self.creator_pr = Some(cx.new(|cx| {
                NewPrPage::new(
                    self.state.clone(),
                    repo.full_name.clone(),
                    node_id,
                    window,
                    cx,
                )
            }));
        } else if let Some(creator_pr) = &self.creator_pr {
            // D7: branches re-fetch per open; the draft survives. The
            // PR template body re-fetches with them (docs/0046 §2) —
            // a template edit lands as long as no body was typed.
            creator_pr.update(cx, |page, cx| {
                page.refetch_branches(window, cx);
                page.refetch_pr_templates(window, cx);
            });
        }
        // Esc on the creator asks its host to hide it (D7); `Created`
        // hands the host the new PR row (D6).
        cx.subscribe_in(
            self.creator_pr.as_ref().unwrap(),
            window,
            |this, _, event: &NewPrPageEvent, window, cx| match event {
                NewPrPageEvent::CloseRequested => this.close_creator_pr(cx),
                NewPrPageEvent::Created(pr) => {
                    let pr: &gql::PullRequest = pr; // Box deref — one event, one row
                    let repo = this.creator_pr.take().map(|c| c.read(cx).repo.clone());
                    this.creator_pr_open = false;
                    if let Some(repo) = repo {
                        // Refetch-on-create (D6, docs/0077 D3): the
                        // created artifact's lane re-lands its tab — the
                        // house one-refetch path, no optimistic insertion.
                        let spec = this.state.update(cx, |s, _| {
                            s.repo_lane_spec(
                                &repo,
                                RepoLane::Pulls,
                                Origin::new(Surface::RepoView, Trigger::ViewShown),
                            )
                        });
                        this.run_specs(vec![spec], window, cx);
                        // A synthesized minimal row (gql::created_pr_from_body)
                        // — initial paint only; the detail page re-fetches.
                        this.open_pr(&repo, pr, None, window, cx);
                    }
                }
            },
        )
        .detach();
        self.pr = None;
        self.issue = None;
        self.notification = None;
        self.creator_open = false;
        self.creator_pr_open = true;
        self.sync_repo_crumb(cx);
        self.record_place(GithubPlace::NewPr(repo.clone()), cx);
        let focus = self
            .creator_pr
            .as_ref()
            .unwrap()
            .read(cx)
            .focus_handle(cx)
            .clone();
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Hide the PR creator (D7): back to the repo view, the crumb chain
    /// loses its `#new-pr` suffix. The entity intentionally stays alive.
    fn close_creator_pr(&mut self, cx: &mut Context<Self>) {
        if self.creator_pr_open {
            log::debug!("pr creator closed (draft retained)");
            self.creator_pr_open = false;
            self.sync_repo_crumb(cx);
            cx.notify();
        }
    }

    /// Row-level mark-as-done (docs/0026 D8, #153, owner decision
    /// §4.5): the repo-view rows fire the DELETE directly — no page
    /// takeover. A settled done runs the post-write refetch
    /// (`mark_refetch_specs`) through the stored window handle (the
    /// `sync_from_state` pattern — the click's window is out of reach
    /// by then) so the row leaves the unread list and the card count
    /// drops; a failed one re-arms the row button (shared handling for
    /// HTTP failures, stderr per the degrade rule).
    fn mark_row_done(&mut self, id: String, cx: &mut Context<Self>) {
        if !self.state.read(cx).auth.connected() {
            return; // the repo view is behind the auth gate anyway
        }
        if !self.row_done_inflight.insert(id.clone()) {
            return; // already in flight (double-click guard)
        }
        cx.notify();
        let spec = self.state.update(cx, |s, _| {
            s.spec(
                Kind::ThreadMutation(ThreadOp::MarkDone),
                api::thread_url(&id),
                None,
                Origin::new(Surface::RepoView, Trigger::MutationRefetch),
            )
        });
        log::debug!("thread-mutation: mark-done {id} (repo-view row)");
        let state = self.state.clone();
        let window_handle = self.window;
        let page = cx.entity();
        cx.spawn(async move |_this, cx| {
            let result = run_spec_tracked(state.clone(), spec, cx).await;
            let settled = result.network.is_none() && (200..300).contains(&result.status);
            if settled {
                log::info!(
                    "thread-mutation: mark-done settled ({}, row)",
                    result.status
                );
                // Post-write refetch on the main thread, window in hand
                // (`run_specs` refreshes the frame at the end).
                let _ = window_handle.update(cx, |_, window, cx| {
                    page.update(cx, |this, cx| {
                        this.row_done_inflight.remove(&id);
                        let specs = this
                            .state
                            .update(cx, |s, _| s.mark_refetch_specs(Surface::RepoView));
                        // docs/0077 D3: the write invalidates the repo's
                        // row-lane clocks too; the next view-shown or beat
                        // refetches the active lane.
                        let full = this.state.read(cx).selected.clone();
                        if let Some(full) = full {
                            this.state.update(cx, |s, _| s.invalidate_repo_lanes(&full));
                        }
                        this.run_specs(specs, window, cx);
                    });
                });
            } else {
                if result.network.is_none() {
                    let status = result.status;
                    state.update(cx, |s, cx| {
                        s.apply(result);
                        cx.notify();
                    });
                    log::warn!("thread-mutation: mark-done failed (HTTP {status}, row)");
                } else {
                    log::warn!("thread-mutation: mark-done failed (network, row)");
                }
                let _ = window_handle.update(cx, |_, _, cx| {
                    page.update(cx, |this, cx| {
                        this.row_done_inflight.remove(&id);
                        cx.notify();
                    });
                });
            }
        })
        .detach();
    }

    /// Page-local counterpart of the tick loop: run specs on the background
    /// executor, apply on the main thread, then the §3.12 refresh.
    fn run_specs(&mut self, specs: Vec<FetchSpec>, window: &mut Window, cx: &mut Context<Self>) {
        self.run_specs_on(specs, window.window_handle(), cx);
    }

    /// The handle-taking core (docs/0051 D4): observer-time triggers have
    /// no `&mut Window` — they pass the page's stored handle instead.
    fn run_specs_on(
        &mut self,
        specs: Vec<FetchSpec>,
        window_handle: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        if specs.is_empty() {
            return;
        }
        let state = self.state.clone();
        cx.spawn(async move |_this, cx| {
            for spec in specs {
                // All `run_specs` traffic is reads (repo-open slices, mark
                // refetches) — the skip guard applies (docs/0031 D1/D2);
                // a skipped spec simply waits for its next trigger.
                let Some(result) = run_read_deduped(state.clone(), spec, cx).await else {
                    continue;
                };
                // Lane backoff bookkeeping (docs/0036 D3, docs/0047): the
                // page owns the dispatch, the state owns the clock. Each
                // lane kind stamps only its own clock — unrelated specs
                // stamp nothing.
                let rows_ok = result.network.is_none() && (200..300).contains(&result.status);
                match result.kind {
                    // docs/0081 D1: page 1 of the commits lane shares the
                    // per-repo clock; load-more pages don't (their retry
                    // is the user's, D8).
                    Kind::RepoPulls
                    | Kind::RepoIssues
                    | Kind::RepoNotifications
                    | Kind::RepoCommits => {
                        state.update(cx, |s, _| s.bump_rows_backoff(rows_ok));
                    }
                    Kind::AccountPullRequests => {
                        state.update(cx, |s, _| s.bump_home_backoff(rows_ok));
                    }
                    _ => {}
                }
                cx.update(|cx| {
                    state.update(cx, |s, cx| {
                        s.apply(result);
                        cx.notify();
                    })
                });
            }
            let _ = window_handle.update(cx, |_, window, _| window.refresh());
        })
        .detach();
    }

    /// docs/0051 D5 — has the selected repo's rows lane ever landed this
    /// session? `false` renders skeletons instead of a false "No open
    /// PRs" while the lazy repo-scope fetch flies.
    fn repo_rows_landed(&self, cx: &Context<Self>) -> bool {
        // Lane-keyed (docs/0077 D4): the active tab's own clock — each
        // box's skeleton resolves on its lane's landing.
        let state = self.state.read(cx);
        state.selected.as_deref().is_some_and(|full| {
            state
                .repo_tab_landed(full, self.tab.lane(), &self.commits_key(cx))
                .is_some()
        })
    }

    /// View-shown fetch (docs/0051 D4): the nav-observer's edge trigger —
    /// no `&mut Window` here, so the page's stored handle carries the
    /// final refresh. Auth/pause gates mirror `refresh_open_page`.
    fn fetch_home_prs_shown(&mut self, cx: &mut Context<Self>) {
        let (connected, gated) = {
            let s = self.state.read(cx);
            (
                s.auth.connected(),
                s.gated_for(Kind::AccountPullRequests.lane()),
            )
        };
        if !connected || gated {
            return;
        }
        let Some((login, scope)) = self.home_account(cx) else {
            return;
        };
        let specs = self.state.update(cx, |s, _| {
            vec![s.home_open_spec(
                &login,
                scope,
                Origin::new(Surface::ModuleHome, Trigger::ViewShown),
            )]
        });
        self.run_specs_on(specs, self.window, cx);
    }

    /// Repo-view PR row (docs/0012 D4): the shared body without the repo
    /// chip (docs/0047 D5 — the repo is the page's context there).
    fn render_pr_row(&self, pr: &gql::PullRequest, cx: &Context<Self>) -> AnyElement {
        self.render_pr_row_inner(pr, false, cx)
    }

    /// The shared PR row body; `show_repo` adds the `owner/name` chip —
    /// home rows span repositories, so they carry their context with
    /// them (docs/0047 D5). Click behavior is identical on both
    /// surfaces (docs/0047 D7).
    fn render_pr_row_inner(
        &self,
        pr: &gql::PullRequest,
        show_repo: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        // Row click opens the in-app PR page (docs/0016 D1); the github.com
        // hand-off lives on in the page header's ↗ button.
        let page = cx.entity();
        let repo = self.state.read(cx).selected.clone().unwrap_or_else(|| {
            // Belt: rows render only under a selected repo view; if that
            // ever drifts, trim the `#number` off the row's composite key.
            pr.key.split('#').next().unwrap_or_default().to_string()
        });
        // docs/0072 D4: the document's optimistic write, from the store
        // (computed before the row-click closure moves `repo`).
        let pending = self.state.read(cx).pr_pending(&repo, pr.number);
        // docs/0049 D1: accessible names are explicit — text children do
        // not become the row's name. Reading order mirrors the row:
        // number+title, repo, author, state, CI signal.
        let mut label = format!("PR #{}: {}", pr.number, pr.title);
        if let Some(repo_name) = pr.key.split('#').next()
            && !repo_name.is_empty()
        {
            label.push_str(&format!(" — {repo_name}"));
        }
        if let Some(author) = pr.author.as_ref() {
            label.push_str(&format!(" · {}", author.login));
        }
        if pr.draft {
            label.push_str(" · draft");
        } else if let Some(state) = pr.state.as_deref() {
            label.push_str(&format!(" · {state}"));
        }
        if let Some(kind) = pending {
            label.push_str(&format!(" · {}", kind.label()));
        }
        if let Some(rollup) = gql::rollup_state(pr.status.as_deref()) {
            label.push_str(&format!(" · {}", ci_row_glyph(rollup, cx).2));
        }
        let label: SharedString = label.into();
        let row = pr.clone();
        div()
            .id(SharedString::from(format!("pr-{}", pr.key)))
            // docs/0010 D10: id alone is not probe-visible — the dump
            // and `debug_bounds` read the selector.
            .debug_selector({
                let probe = format!("pr-{}", pr.key);
                move || probe.clone()
            })
            .role(accesskit::Role::ListItem)
            .aria_label(label)
            .px_2()
            .py_1p5()
            .rounded_sm()
            .text_sm()
            .hover(|s| s.bg(cx.theme().secondary))
            .on_click(move |_, window, cx| {
                page.update(cx, |this, cx| this.open_pr(&repo, &row, None, window, cx));
            })
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            // CI signal, glyph-only with a text tooltip
                            // (#67; the 0005-deferred rollup). Nothing
                            // renders without checks (null rollup).
                            .when_some(gql::rollup_state(pr.status.as_deref()), |row, state| {
                                let (icon, color, tip) = ci_row_glyph(state, cx);
                                let key = pr.key.clone();
                                let id: SharedString = format!("pr-ci-{key}").into();
                                let glyph = div()
                                    .id(id.clone())
                                    .debug_selector(move || id.to_string())
                                    // #397: pending spins, GitHub-style; the
                                    // Spinner handles reduced-motion (static,
                                    // zero frames) upstream and scopes the
                                    // animation to this glyph's box.
                                    .child(if state == gql::CheckState::Pending {
                                        Spinner::new().icon(icon).color(color).into_any_element()
                                    } else {
                                        Icon::new(icon)
                                            .size_4()
                                            .text_color(color)
                                            .into_any_element()
                                    })
                                    .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx));
                                row.child(glyph)
                            })
                            .when(pr.draft, |row| {
                                row.child(Badge::new().xsmall().child("Draft"))
                            })
                            .when(show_repo, |row| {
                                // docs/0047 D5: home rows span repositories
                                // — the `owner/name` chip carries their
                                // context (parsed from the composite key,
                                // the row's belt fallback source).
                                let repo_name =
                                    pr.key.split('#').next().unwrap_or_default().to_string();
                                row.child(
                                    div()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(repo_name),
                                )
                            })
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(pr.title.clone()),
                            )
                            .child(
                                div()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("#{}", pr.number)),
                            )
                            // docs/0042 D7 (amended): named label pills
                            // right of #N — github.com's treatment.
                            .when_some(pr.labels.as_ref(), |row, labels| {
                                row.child(label_chips(
                                    labels,
                                    &format!("pr-labels-{}", pr.key),
                                    cx.theme().tokens.group_box.into(),
                                    cx,
                                ))
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .when_some(pending, |row, kind: crate::service::PendingKind| {
                                row.child(
                                    div()
                                        .id(SharedString::from(format!("pr-pending-{}", pr.key)))
                                        .debug_selector({
                                            let probe = format!("pr-pending-{}", pr.key);
                                            move || probe.clone()
                                        })
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(kind.label()),
                                )
                            })
                            .when_some(pr.author.as_ref(), |row, author| {
                                row.child(
                                    h_flex()
                                        .gap_1()
                                        .items_center()
                                        .child(
                                            Avatar::new().src(author.avatar_url.clone()).xsmall(),
                                        )
                                        .child(
                                            div()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(author.login.clone()),
                                        ),
                                )
                            })
                            .child(
                                div()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(relative_age(pr.updated_at.as_deref())),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// Issue rows mirror the PR row treatment (docs/0012 D4, opened
    /// in-app by 0020 D1): same hover; the row opens the issue page,
    /// and the github.com hand-off lives on in the page header's ↗
    /// button.
    fn render_issue_row(&self, issue: &gql::Issue, cx: &Context<Self>) -> AnyElement {
        // Belt: rows render only under a selected repo view (PR-row
        // pattern) — fall back to the row key's `owner/repo` prefix.
        let repo = self
            .state
            .read(cx)
            .selected
            .clone()
            .unwrap_or_else(|| issue.key.split('#').next().unwrap_or_default().to_string());
        // docs/0072 stage 4 (#555): the document's optimistic write,
        // from the store — the pill the PR row grew in stage 2.
        let pending = self.state.read(cx).issue_pending(&repo, issue.number);
        // docs/0049 D1: explicit accessible name (PR-row pattern).
        let mut label = format!("Issue #{}: {}", issue.number, issue.title);
        if let Some(repo_name) = issue.key.split('#').next()
            && !repo_name.is_empty()
        {
            label.push_str(&format!(" — {repo_name}"));
        }
        if let Some(author) = issue.author.as_ref() {
            label.push_str(&format!(" · {}", author.login));
        }
        if let Some(kind) = pending {
            label.push_str(&format!(" · {}", kind.label()));
        }
        let label: SharedString = label.into();
        let row = issue.clone();
        let page = cx.entity();
        div()
            .id(SharedString::from(format!("issue-{}", issue.key)))
            // docs/0010 D10: probe-visible row, mirroring the PR rows.
            .debug_selector({
                let probe = format!("issue-{}", issue.key);
                move || probe.clone()
            })
            .role(accesskit::Role::ListItem)
            .aria_label(label)
            .px_2()
            .py_1p5()
            .rounded_sm()
            .text_sm()
            .hover(|s| s.bg(cx.theme().secondary))
            .on_click(move |_, window, cx| {
                page.update(cx, |this, cx| {
                    this.open_issue(&repo, &row, None, window, cx)
                });
            })
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(issue.title.clone()),
                            )
                            .child(
                                div()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("#{}", issue.number)),
                            )
                            // docs/0042 D7 (amended): as on PR rows.
                            .when_some(issue.labels.as_ref(), |row, labels| {
                                row.child(label_chips(
                                    labels,
                                    &format!("issue-labels-{}", issue.key),
                                    cx.theme().tokens.group_box.into(),
                                    cx,
                                ))
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .when_some(pending, |row, kind: crate::service::IssuePendingKind| {
                                row.child(
                                    div()
                                        .id(SharedString::from(format!(
                                            "issue-pending-{}",
                                            issue.key
                                        )))
                                        .debug_selector({
                                            let probe = format!("issue-pending-{}", issue.key);
                                            move || probe.clone()
                                        })
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(kind.label()),
                                )
                            })
                            .when_some(issue.author.as_ref(), |row, author| {
                                row.child(
                                    h_flex()
                                        .gap_1()
                                        .items_center()
                                        .child(
                                            Avatar::new().src(author.avatar_url.clone()).xsmall(),
                                        )
                                        .child(
                                            div()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(author.login.clone()),
                                        ),
                                )
                            })
                            .child(
                                div()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(relative_age(issue.updated_at.as_deref())),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// Notification rows mirror the PR/issue row treatment (doc 0012 D4,
    /// opened in-app by docs/0026 D1 — the browser hand-off lives on in
    /// the page header's ↗ button, kind-aware (D5). The page itself
    /// fires the mark-read + thread fetch at open (§4.3).
    fn render_notification_row(&self, n: &api::Notification, cx: &Context<Self>) -> AnyElement {
        let page = cx.entity();
        let row = n.clone();
        // docs/0049 D1: explicit accessible name — kind + subject title,
        // repo, read state, reason.
        let mut label = format!("{}: {}", n.subject.kind, n.subject.title);
        label.push_str(&format!(" — {}", n.repository.full_name));
        label.push_str(if n.unread { " · unread" } else { " · read" });
        if let Some(reason) = n.reason.as_deref() {
            label.push_str(&format!(" · {reason}"));
        }
        let label: SharedString = label.into();
        div()
            .id(SharedString::from(format!("notif-{}", n.id)))
            .role(accesskit::Role::ListItem)
            .aria_label(label)
            .debug_selector({
                let id = n.id.clone();
                move || format!("notif-{id}")
            })
            .px_2()
            .py_1p5()
            .rounded_sm()
            .text_sm()
            .hover(|s| s.bg(cx.theme().secondary))
            .on_click(move |_, window, cx| {
                page.update(cx, |this, cx| {
                    this.open_notification(row.clone(), window, cx)
                });
            })
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(Badge::new().xsmall().child(n.subject.kind.clone()))
                            .child(n.subject.title.clone()),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(relative_age(n.updated_at.as_deref())),
                            )
                            // Row-level done (docs/0026 D8, #153, owner
                            // decision §4.5): icon-only, disabled while
                            // this row's DELETE is in flight. The guard
                            // div swallows the click — gpui bubbles clicks
                            // up and Button only stops propagation while
                            // loading, so without it the row's open
                            // handler would also fire.
                            .child(
                                div()
                                    .id(SharedString::from(format!("notif-done-guard-{}", n.id)))
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .on_mouse_up(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .child({
                                        let id = n.id.clone();
                                        let selector_id = n.id.clone();
                                        let disabled = self.row_done_inflight.contains(&n.id);
                                        let page = cx.entity();
                                        Button::new(SharedString::from(format!(
                                            "notif-done-{}",
                                            n.id
                                        )))
                                        .debug_selector(move || format!("notif-done-{selector_id}"))
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::CircleCheck)
                                        .tooltip("Mark as done")
                                        // docs/0049 D1: icon-only buttons
                                        // never rely on the tooltip for
                                        // their accessible name.
                                        .accessibility_label("Mark as done")
                                        .disabled(disabled)
                                        .on_click(
                                            move |_, _, cx| {
                                                page.update(cx, |this, cx| {
                                                    this.mark_row_done(id.clone(), cx)
                                                });
                                            },
                                        )
                                    }),
                            ),
                    ),
            )
            .into_any_element()
    }
}

/// The repo's notifications page on GitHub (docs/0015 D2): the ↗
/// fallback target on the notification page (docs/0026 D5) for
/// subjects with no faithful web URL. Pure — table-tested.
pub(crate) fn notification_url(repo_full_name: &str) -> String {
    format!("https://github.com/{repo_full_name}/notifications")
}

/// The "+N more on GitHub" tail for capped row lists (0007 D2, #87):
/// the snapshot carries the server's exact `total_count` next to the
/// first-100 `nodes`, so the overflow is `total − rendered` whenever the
/// cap bit. `None` renders nothing — repos under the cap look exactly
/// as before. `rendered` is the node slice the query capped at 100, so
/// the arithmetic fits either side of the i64/usize border. Pure —
/// table-tested.
/// One GitHub-style label chip (docs/0042 D7 as amended by owner
/// feedback; the 0022 Q2 / #119 pill treatment): the label's own color
/// when the API carried one — the docs/0002-sanctioned `data_color`
/// exception, all construction inside `dashboard_core::data_color` —
/// else the muted token pill. Shared with the issue page's rail, the
/// only place a label name is rendered as its color.
pub(crate) fn label_pill(name: &str, color: Option<&str>, surface: gpui::Hsla, cx: &App) -> Div {
    let chip = color.and_then(|hex| dashboard_core::data_color::data_chip(hex, surface.into()));
    let (bg, fg, border) = match chip {
        Some(c) => (c.bg, c.fg, c.border),
        None => (
            cx.theme().transparent,
            cx.theme().muted_foreground,
            cx.theme().border,
        ),
    };
    div()
        .px_2()
        .py_0p5()
        .rounded_full()
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_xs()
        .text_color(fg)
        .child(name.to_string())
}

/// Up to three named label chips (docs/0042 D7 as amended): GitHub's
/// website treatment — the pills ride the row's title cluster, name
/// visible in the label's own color through the docs/0002-sanctioned
/// `data_color` exception. `total_count` beyond what's shown collapses
/// into a muted `+n` pill whose tooltip carries the hidden names.
fn label_chips(labels: &gql::LabelSummary, key: &str, surface: gpui::Hsla, cx: &App) -> AnyElement {
    const MAX_CHIPS: usize = 3;
    let shown = labels.nodes.iter().take(MAX_CHIPS).collect::<Vec<_>>();
    if shown.is_empty() {
        return div().into_any_element();
    }
    let mut chips = h_flex().gap_1().items_center();
    for label in &shown {
        chips = chips.child(label_pill(&label.name, label.color.as_deref(), surface, cx));
    }
    let overflow = labels.total_count as usize - shown.len();
    if overflow > 0 {
        let tip = labels
            .nodes
            .iter()
            .skip(MAX_CHIPS)
            .map(|l| l.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        // Tooltips need a stateful element — wrap the `+n` pill.
        chips = chips.child(
            div()
                .id(SharedString::from(format!("{key}-more")))
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .child(label_pill(&format!("+{overflow}"), None, surface, cx)),
        );
    }
    chips
        .id(SharedString::from(key.to_string()))
        .debug_selector({
            let probe = key.to_string();
            move || probe.clone()
        })
        .into_any_element()
}

fn more_hint_count(total_count: i64, rendered: usize) -> Option<usize> {
    let rendered = rendered as i64;
    (total_count > rendered).then(|| (total_count - rendered) as usize)
}

/// Resolve the org choice after the login list changes (0009 D6): keep the
/// current org when it still exists, otherwise fall back to the first
/// login (the personal account, Q10). Pure — table-tested.
fn next_org_selection(current: Option<&str>, logins: &[SharedString]) -> Option<SharedString> {
    match current {
        Some(current) if logins.iter().any(|l| &**l == current) => Some(current.to_string().into()),
        _ => logins.first().cloned(),
    }
}

/// The issue-creation visibility gate (docs/0024 D2), hidden-not-disabled:
/// connected, the repo allows issues, the node id is known (the
/// `createIssue` target), and the repo is not archived (#275 — GitHub
/// rejects creation on archived repos upstream). No `viewerPermission`
/// term — any reader can file issues. Pure — table-tested.
pub(crate) fn create_allowed(repo: &gql::Repo, connected: bool) -> bool {
    connected && repo.has_issues_enabled && repo.node_id.is_some() && !repo.is_archived
}

/// The PR-creation visibility gate (docs/0044 D2), hidden-not-disabled:
/// connected, the repo's GraphQL node id is known (the
/// `createPullRequest` target), and the repo is not archived (#275).
/// No upstream creation flag exists (§2) and `hasIssuesEnabled` has no
/// PR analog — anything that cannot succeed dies as a scoped form error
/// or an empty head select, never a silent post. Pure — table-tested.
pub(crate) fn create_pr_allowed(repo: &gql::Repo, connected: bool) -> bool {
    connected && repo.node_id.is_some() && !repo.is_archived
}

/// The module-level crumb label: the short `name` of the selected repo
/// (docs/0014 Q1/D2), resolved with the same lookup the right column
/// uses — a selected repo that no longer exists in the snapshot is *not*
/// a crumb (the pane beside it shows "Select a repository"; the chrome
/// agrees). Disconnected states never carry a repo crumb. Pure —
/// table-tested.
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

/// Decide whether the navigator's crumbs must be rewritten (docs/0014 D3,
/// amended by 0016 D1). `Some(labels)` = write exactly these; `None` =
/// leave the navigator alone — the compare-first guard that keeps idle
/// ticks write-free. 0014's module-owned chain was a prefix (the repo
/// label) that tolerated unknown suffixes; since the PR sub-page landed,
/// the module derives the **whole** chain — `[repo]` or `[repo, #n]` —
/// so the no-op predicate collapses to equality (any mismatch is stale:
/// a lingering suffix after close, a missing one after open, a changed
/// repo). Pure — table-tested.
fn reconcile_repo_crumb(
    derived: &[SharedString],
    current: &[SharedString],
) -> Option<Vec<SharedString>> {
    if derived == current {
        None
    } else {
        Some(derived.to_vec())
    }
}

/// Repo-list delegate (0009 D2): the visible repos plus the filtered view
/// driven by the List's search input. Data-only — selection reacts flow
/// through `ListEvent` on the page, not through the delegate.
#[derive(Default)]
struct RepoListDelegate {
    all: Vec<gql::Repo>,
    matched: Vec<gql::Repo>,
    query: String,
    selected: Option<IndexPath>,
}

impl RepoListDelegate {
    /// Replace the data, re-applying the active query (background
    /// refreshes must not clear the user's filter).
    fn set_repos(&mut self, repos: Vec<gql::Repo>) {
        self.all = repos;
        self.apply_query(&self.query.clone());
    }

    /// Case-insensitive substring match on name or description (D2).
    fn apply_query(&mut self, query: &str) {
        self.query = query.to_string();
        let q = self.query.to_lowercase();
        self.matched = self
            .all
            .iter()
            .filter(|r| {
                q.is_empty()
                    || r.name.to_lowercase().contains(&q)
                    || r.description
                        .as_deref()
                        .is_some_and(|d| d.to_lowercase().contains(&q))
            })
            .cloned()
            .collect();
    }

    fn repo_at(&self, ix: IndexPath) -> Option<&gql::Repo> {
        self.matched.get(ix.row)
    }

    fn repo_full_name(&self, ix: IndexPath) -> Option<SharedString> {
        self.repo_at(ix).map(|r| r.full_name.clone().into())
    }
}

impl ListDelegate for RepoListDelegate {
    type Item = ListItem;

    fn perform_search(
        &mut self,
        query: &str,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        self.apply_query(query);
        Task::ready(())
    }

    fn items_count(&self, _section: usize, _cx: &App) -> usize {
        self.matched.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let repo = self.repo_at(ix)?.clone();
        Some(
            ListItem::new(SharedString::from(format!("repo-{}", repo.key)))
                .text_sm()
                .child(
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .child(div().truncate().child(repo.name.clone()))
                        .when(repo.private, |row| {
                            // GitHub's own list treatment: small muted lock
                            // glyph, no text badge.
                            row.child(private_lock(cx.theme().muted_foreground))
                        }),
                ),
        )
    }

    fn render_empty(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        let text = if self.query.trim().is_empty() {
            "No repositories yet"
        } else {
            "No matches"
        };
        h_flex()
            .size_full()
            .justify_center()
            .pt_6()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(text)
            .into_any_element()
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) {
        self.selected = ix;
    }
}

/// docs/0044 §9 — the ref selector's server-search debounce (the
/// creator's branch-search delay).
const REF_SEARCH_DELAY: Duration = Duration::from_millis(300);

impl Render for GithubPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let auth = self.state.read(cx).auth.clone();
        // A restore can land on the Commits tab without a switch.
        if self.tab == RepoTab::Commits {
            self.ensure_commits_list(window, cx);
            self.sync_ref_select(window, cx);
        }

        // Non-connected: the connect panel owns the page (§6.4 auth row).
        if !auth.connected() {
            return connect_panel(&auth, cx);
        }

        // PR sub-page takeover (docs/0016 D1): while a PR is open its page
        // replaces the whole module content — the two-column layout (and
        // its org Select / repo list) unmounts until the page closes.
        if let Some(pr) = &self.pr {
            return pr.clone().into_any_element();
        }
        // Issue sub-page takeover (docs/0020 D1) — the PR page's
        // sibling: never both open (mutual exclusion), so a plain
        // `pr`-first check suffices.
        if let Some(issue) = &self.issue {
            return issue.clone().into_any_element();
        }
        // Notification sub-page takeover (docs/0026 D1) — the third
        // sibling, same exclusivity.
        if let Some(notification) = &self.notification {
            return notification.clone().into_any_element();
        }
        // Creator takeover (docs/0024 D3): same full-content rule.
        if self.creator_open {
            if let Some(creator) = &self.creator {
                return creator.clone().into_any_element();
            }
            self.creator_open = false; // stale flag without a page —
        }
        // The PR creator (docs/0044 D3): the sibling takeover.
        if self.creator_pr_open {
            if let Some(creator_pr) = &self.creator_pr {
                return creator_pr.clone().into_any_element();
            }
            self.creator_pr_open = false; // stale flag without a page
        }

        // docs/0042 D1: rows render from the held snapshot nodes — the
        // filter/sort engine runs inside render_right_column at render
        // time, so the full node lists are handed over untouched.
        let (selected_full, repo_notifs) = {
            let state = self.state.read(cx);
            let notifs = state
                .selected
                .as_ref()
                .and_then(|f| state.repo_notifications.get(f))
                .cloned()
                .unwrap_or_default();
            (state.selected.clone(), notifs)
        };
        let selected_repo = selected_full.as_ref().and_then(|full| {
            self.state
                .read(cx)
                .repos
                .iter()
                .find(|r| &r.full_name == full)
                .cloned()
        });

        let left = self.render_left_column(cx);
        let right = self.render_right_column(&selected_repo, &repo_notifs, cx);
        h_flex()
            .size_full()
            .child(left)
            .child(right)
            .into_any_element()
    }
}

impl GithubPage {
    fn render_left_column(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let (snapshot_age, notifications_age) = {
            let state = self.state.read(cx);
            (state.snapshot_age, state.notifications_age)
        };
        v_flex()
            .w(px(RECONCILE_COL_W))
            .flex_none()
            .h_full()
            .border_r_1()
            .border_color(cx.theme().border)
            .p_2()
            .gap_2()
            // Cold-start staleness chip (docs/0034 D3) — top of the
            // module main page, independent of any repo selection.
            .when_some(
                stale_chip_label(snapshot_age, notifications_age),
                |col, chip| {
                    col.child(
                        div()
                            .debug_selector(|| "github-stale".to_string())
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(chip),
                    )
                },
            )
            // The Select's outer wrapper is hardcoded `size_full()`
            // (select.rs RenderOnce) and style refinements only reach
            // the inner trigger — in this full-height column it would
            // resolve to the whole column and starve the List below to
            // zero. A flex_none slot bounds it to the trigger's height.
            .child(
                div().flex_none().child(
                    Select::new(&self.org_select)
                        .accessibility_label("Organization")
                        .placeholder("Organization…")
                        .search_placeholder("Filter organizations…")
                        .empty(|_, cx| {
                            h_flex()
                                .w_full()
                                .justify_center()
                                .py_4()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("No organizations yet")
                        })
                        .small()
                        .w_full(),
                ),
            )
            // The List is a direct flex child with flex_1 — the blessed
            // upstream usage (list_story.rs).
            .child(
                List::new(&self.repo_list)
                    .search_placeholder("Filter repositories…")
                    .small()
                    .flex_1()
                    .min_h_0()
                    .w_full(),
            )
            .into_any_element()
    }

    /// Right column — independently scrollable (docs/0005 §6.3).
    fn render_right_column(
        &mut self,
        selected_repo: &Option<gql::Repo>,
        repo_notifs: &[api::Notification],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // docs/0081 D9: the Commits tab bounds its own height — the
        // chrome pins and the virtual list scrolls internally; the
        // other tabs keep the one-scroll column (F1).
        let pinned = selected_repo.is_some() && self.tab == RepoTab::Commits;
        if pinned {
            self.sync_commits(cx);
        }
        let detail: AnyElement = match selected_repo {
            // docs/0047 D1: the home view replaces the placeholder — the
            // module's landing surface is the account's open PRs.
            None => self.render_home_view(cx),
            Some(repo) => page_canvas()
                .when(pinned, |canvas| canvas.size_full().min_h_0())
                // Header (the PR page's idiom): title and description
                // left, the freshness cluster right-aligned — one
                // freshness cluster per page (docs/0037 Q1), not a
                // third text row stacked under the description.
                .child(
                    h_flex()
                        .id("repo-header")
                        .justify_between()
                        .gap_3()
                        .items_start()
                        .child(
                            v_flex()
                                .gap_1()
                                .min_w_0()
                                .flex_1()
                                .child(
                                    h_flex()
                                        .id("repo-link")
                                        .gap_1p5()
                                        .items_center()
                                        .cursor_pointer()
                                        .hover(|s| s.underline())
                                        .role(accesskit::Role::Link)
                                        .aria_label(format!(
                                            "Open repository {} on GitHub",
                                            repo.full_name
                                        ))
                                        // Same affordance as PR rows: the repo title
                                        // links to the repo on github.com. URL is
                                        // derived from full_name — cache-proof (no
                                        // dependency on html_url being present in
                                        // last-known payloads).
                                        .on_click({
                                            let full_name = repo.full_name.clone();
                                            move |_, _, cx| {
                                                cx.open_url(&format!(
                                                    "https://github.com/{full_name}"
                                                ))
                                            }
                                        })
                                        .child(page_title(repo.full_name.clone()))
                                        .when(repo.private, |row| {
                                            row.child(private_lock(cx.theme().muted_foreground))
                                        }),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(
                                            repo.description
                                                .clone()
                                                .unwrap_or_else(|| "No description".into()),
                                        ),
                                ),
                        )
                        // The freshness cluster (docs/0037 D6): the
                        // synced label reads the same clock the beat
                        // gates on (the selected ref's book on the
                        // Commits tab, docs/0081 D7); blank until this
                        // session's first landing (0033 D2). Manual
                        // refresh dispatches the open pair, ignoring
                        // age/backoff (user-invoked) — the surface's
                        // only refresh affordance since the textual
                        // button went away (docs/0024 D2 amended).
                        .child(freshness_row(
                            "repo-rows",
                            &LandedClock(self.state.read(cx).repo_tab_landed(
                                &repo.full_name,
                                self.tab.lane(),
                                &self.commits_key(cx),
                            )),
                            cx,
                            {
                                let full = repo.full_name.clone();
                                let lane = self.tab.lane();
                                move |this, window, cx| {
                                    // User-invoked (docs/0052): the active
                                    // lane's spec, Trigger::Manual — ignoring
                                    // age/backoff (docs/0077 D3) and the
                                    // in-flight dedup (docs/0031 D2 amended).
                                    let spec = this.state.read(cx).repo_tab_spec(
                                        &full,
                                        lane,
                                        &this.commits_key(cx),
                                        Origin::new(Surface::RepoView, Trigger::Manual),
                                    );
                                    this.run_specs(vec![spec], window, cx);
                                }
                            },
                        )),
                )
                .child(
                    // Shared quick filter (docs/0042 D5): one input
                    // narrows all three boxes; text or exact #number.
                    div()
                        .id("repo-row-search")
                        .w_full()
                        .child(Input::new(&self.row_search).small()),
                )
                .child({
                    // The actions row (docs/0024 D2, docs/0044 D2): New
                    // issue + New pull request (both gated,
                    // hidden-not-disabled).
                    let connected = self.state.read(cx).auth.connected();
                    let issue_gate = selected_repo
                        .as_ref()
                        .map(|r| create_allowed(r, connected))
                        .unwrap_or(false);
                    let pr_gate = selected_repo
                        .as_ref()
                        .map(|r| create_pr_allowed(r, connected))
                        .unwrap_or(false);
                    h_flex()
                        .justify_end()
                        .gap_2()
                        .when(pr_gate, |row| {
                            let repo = selected_repo.clone().unwrap();
                            row.child(
                                Button::new("new-pr-open")
                                    .debug_selector(|| "new-pr-open".to_string())
                                    .label("New pull request")
                                    .ghost()
                                    .small()
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_creator_pr(&repo, window, cx);
                                    })),
                            )
                        })
                        .when(issue_gate, |row| {
                            let repo = selected_repo.clone().unwrap();
                            row.child(
                                Button::new("new-issue-open")
                                    .debug_selector(|| "new-issue-open".to_string())
                                    .label("New issue")
                                    .ghost()
                                    .small()
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_creator(&repo, window, cx);
                                    })),
                            )
                        })
                })
                // The tab strip (docs/0077 D6): one tab per lane; the
                // active tab's GroupBox renders below, nothing else.
                .child(self.repo_tab_strip(cx))
                .child(match self.tab {
                    RepoTab::Pulls => self.repo_pulls_box(repo, cx),
                    RepoTab::Issues => self.repo_issues_box(repo, cx),
                    RepoTab::Notifications => self.repo_notifs_box(repo_notifs, cx),
                    RepoTab::Commits => self.repo_commits_box(repo, cx),
                })
                .into_any_element(),
        };
        if pinned {
            return v_flex()
                .id("repo-detail")
                .flex_1()
                .h_full()
                .min_h_0()
                .child(detail)
                .into_any_element();
        }
        div()
            .id("repo-detail")
            .flex_1()
            .h_full()
            .overflow_y_scroll()
            .child(detail)
            .into_any_element()
    }

    /// The repo view's tab strip (docs/0077 D6): the usage-analytics
    /// strip is the in-app precedent (underline variant). Labels carry
    /// the accessible names; the pinned `Tab` primitive derives its
    /// element ids from the bar + index (no per-tab debug id exists —
    /// the docs/0077 D6 note). Switching routes through
    /// `switch_repo_tab`, which offers the lane.
    fn repo_tab_strip(&self, cx: &Context<Self>) -> AnyElement {
        TabBar::new("repo-tabs")
            .underline()
            .selected_index(self.tab.index())
            .on_click(cx.listener(|this, ix: &usize, window, cx| {
                if let Some(tab) = RepoTab::from_index(*ix) {
                    this.switch_repo_tab(tab, window, cx);
                }
            }))
            .child(Tab::new().label(RepoTab::Pulls.label()))
            .child(Tab::new().label(RepoTab::Issues.label()))
            .child(Tab::new().label(RepoTab::Notifications.label()))
            .child(Tab::new().label(RepoTab::Commits.label()))
            .into_any_element()
    }

    /// The Open PRs box (docs/0077 D6): rows and filter options derive
    /// only while the tab is active — docs/0042 filters over docs/0073
    /// derived rows, unchanged from the pre-tabs render.
    fn repo_pulls_box(&self, repo: &gql::Repo, cx: &Context<Self>) -> AnyElement {
        let (pr_rows, pr_held, pr_filtered, label_options) = {
            let state = self.state.read(cx);
            let viewer = state.user.as_ref().map(|u| u.login.clone());
            let search = self.filters.search.clone();
            let derived = state.repo_pr_rows(repo);
            let pr_held = derived.len();
            // docs/0073 D3 (stage 2): options derive from the derived
            // rows — the rows the list actually shows.
            let mut pr_labels: std::collections::BTreeSet<String> = Default::default();
            for p in &derived {
                for n in filters::label_names(p.labels.as_ref()) {
                    pr_labels.insert(n.to_string());
                }
            }
            // Owned: the filter returns references into `derived`, so
            // the rows must outlive it.
            let pr_rows =
                filters::apply_prs(&self.filters.prs, &derived, viewer.as_deref(), &search)
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>();
            let pr_filtered = self.filters.prs.is_filtered() || !search.is_empty();
            (
                pr_rows,
                pr_held,
                pr_filtered,
                pr_labels.into_iter().collect::<Vec<_>>(),
            )
        };
        GroupBox::new()
            .id("prs-box")
            .title(filters::box_title(
                "Open PRs",
                repo.prs.total_count.max(pr_rows.len() as i64) as usize,
                pr_rows.len(),
                pr_filtered,
            ))
            .child(
                v_flex()
                    .gap_0p5()
                    .child(self.pr_filter_toolbar(&label_options, cx))
                    .child(if pr_rows.is_empty() {
                        if pr_held == 0 {
                            if self.repo_rows_landed(cx) {
                                muted_cell("No open PRs", cx)
                            } else {
                                // docs/0051 D5 — the lazy lane fetch is
                                // still in flight.
                                home_skeleton_rows()
                            }
                        } else {
                            muted_cell("No PRs match the current filters", cx)
                        }
                    } else {
                        v_flex()
                            .gap_0p5()
                            .children(pr_rows.iter().map(|p| self.render_pr_row(p, cx)))
                            // #87: the tail cell says what the cap
                            // swallowed — while filtered the hidden rows
                            // are filtered, not capped, so the hint is
                            // unfiltered-only.
                            .when(!pr_filtered, |col| {
                                col.children(
                                    more_hint_count(repo.prs.total_count, pr_held)
                                        .map(|n| more_cell(n, "repo-prs-more", cx)),
                                )
                            })
                            .into_any_element()
                    })
                    .into_any_element(),
            )
            .into_any_element()
    }

    /// The Open issues box — symmetric to [`Self::repo_pulls_box`]
    /// (docs/0077 D6).
    fn repo_issues_box(&self, repo: &gql::Repo, cx: &Context<Self>) -> AnyElement {
        let (issue_rows, issue_filtered, label_options) = {
            let state = self.state.read(cx);
            let viewer = state.user.as_ref().map(|u| u.login.clone());
            let search = self.filters.search.clone();
            // docs/0072 stage 4 (#555): issue rows derive — the
            // snapshot's held rows ⊗ the documents, filtered to
            // not-closed at read time.
            let derived = state.repo_issue_rows(repo);
            let mut issue_labels: std::collections::BTreeSet<String> = Default::default();
            for i in &derived {
                for n in filters::label_names(i.labels.as_ref()) {
                    issue_labels.insert(n.to_string());
                }
            }
            let issue_rows =
                filters::apply_issues(&self.filters.issues, &derived, viewer.as_deref(), &search)
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>();
            let issue_filtered = self.filters.issues.is_filtered() || !search.is_empty();
            (
                issue_rows,
                issue_filtered,
                issue_labels.into_iter().collect::<Vec<_>>(),
            )
        };
        GroupBox::new()
            .id("issues-box")
            .title(filters::box_title(
                "Open Issues",
                repo.issues.total_count.max(issue_rows.len() as i64) as usize,
                issue_rows.len(),
                issue_filtered,
            ))
            .child(
                v_flex()
                    .gap_0p5()
                    .child(self.issue_filter_toolbar(&label_options, cx))
                    .child(if issue_rows.is_empty() {
                        if self.state.read(cx).repo_issue_rows(repo).is_empty() {
                            if self.repo_rows_landed(cx) {
                                muted_cell("No open issues", cx)
                            } else {
                                // docs/0051 D5 — same lazy gate as the
                                // PR box.
                                home_skeleton_rows()
                            }
                        } else {
                            muted_cell("No issues match the current filters", cx)
                        }
                    } else {
                        v_flex()
                            .gap_0p5()
                            .children(issue_rows.iter().map(|i| self.render_issue_row(i, cx)))
                            // #87, mirroring prs-box (filtered hides
                            // rows, the cap hint doesn't).
                            .when(!issue_filtered, |col| {
                                col.children(
                                    more_hint_count(
                                        repo.issues.total_count,
                                        self.state.read(cx).repo_issue_rows(repo).len(),
                                    )
                                    .map(|n| more_cell(n, "repo-issues-more", cx)),
                                )
                            })
                            .into_any_element()
                    })
                    .into_any_element(),
            )
            .into_any_element()
    }

    /// The Notifications box (docs/0077 D5/D6): today's per-repo
    /// behavior — ident-keyed fetch, the bell's re-derived slice,
    /// docs/0042 notif filters — now gated by its own lane clock, so a
    /// never-fetched tab skeletons instead of a false "No unread".
    fn repo_notifs_box(&self, repo_notifs: &[api::Notification], cx: &Context<Self>) -> AnyElement {
        let search = self.filters.search.clone();
        let notif_rows = filters::apply_notifs(&self.filters.notifs, repo_notifs, &search);
        let notif_filtered = self.filters.notifs.is_filtered() || !search.is_empty();
        let mut reasons: std::collections::BTreeSet<String> = Default::default();
        let mut kinds: std::collections::BTreeSet<String> = Default::default();
        for n in repo_notifs {
            if let Some(r) = n.reason.as_ref() {
                reasons.insert(r.clone());
            }
            kinds.insert(n.subject.kind.clone());
        }
        GroupBox::new()
            .id("notifs-box")
            .title(filters::box_title(
                "Notifications",
                repo_notifs.len(),
                notif_rows.len(),
                notif_filtered,
            ))
            .child(
                v_flex()
                    .gap_0p5()
                    .child(self.notif_filter_toolbar(
                        &reasons.into_iter().collect::<Vec<_>>(),
                        &kinds.into_iter().collect::<Vec<_>>(),
                        cx,
                    ))
                    .child(if notif_rows.is_empty() {
                        if repo_notifs.is_empty() {
                            if self.repo_rows_landed(cx) {
                                muted_cell("No unread notifications", cx)
                            } else {
                                home_skeleton_rows()
                            }
                        } else {
                            muted_cell("No notifications match the current filters", cx)
                        }
                    } else {
                        v_flex()
                            .gap_0p5()
                            .children(
                                notif_rows
                                    .iter()
                                    .map(|n| self.render_notification_row(n, cx)),
                            )
                            .into_any_element()
                    })
                    .into_any_element(),
            )
            .into_any_element()
    }

    /// The Commits tab body (docs/0081 D5/D8/D9): the ref and count
    /// row, then the state the book is in — skeletons, the empty and
    /// error cells, or the virtual list with its tail strip. Fills the
    /// pinned column (`flex_1().min_h_0()`).
    fn repo_commits_box(&self, repo: &gql::Repo, cx: &Context<Self>) -> AnyElement {
        let key = self.commits_key(cx);
        let mine = key.filter == CommitFilter::Mine;
        let lane = self
            .state
            .read(cx)
            .commit_lanes
            .get(&(repo.full_name.clone(), key))
            .cloned()
            .unwrap_or_default();
        // A picked branch is named by the pick; the default ref is
        // unknown until page 1 lands, and absent for an empty
        // repository (`defaultBranchRef: null`).
        let ref_name = match &self.commits_ref {
            RefKey::Branch(name) => name.clone(),
            RefKey::Default => lane
                .ref_name
                .clone()
                .or_else(|| self.default_ref_name(&repo.full_name, cx))
                .unwrap_or_else(|| "the default branch".to_string()),
        };
        let muted = cx.theme().muted_foreground;
        let header = h_flex()
            .id("commits-header")
            .flex_none()
            .justify_between()
            .items_center()
            .gap_2()
            .text_sm()
            // D7: the ref selector, left-aligned (GitHub's branch button).
            .when_some(self.ref_select.as_ref(), |row, select| {
                row.child(
                    div()
                        .id("commits-ref")
                        .debug_selector(|| "commits-ref".to_string())
                        // Hugs the branch name like GitHub's branch
                        // button (critique iteration 1), bounded so a
                        // long name truncates instead of pushing.
                        .min_w(px(96.))
                        .max_w(px(280.))
                        .child(
                            Select::new(select)
                                .id("commits-ref-select")
                                .small()
                                .accessibility_label("Branch")
                                .placeholder("Default branch")
                                .search_placeholder("Find a branch…")
                                .menu_width(px(280.))
                                .empty(|_, cx| {
                                    h_flex()
                                        .w_full()
                                        .justify_center()
                                        .py_4()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("No matching branches")
                                        .into_any_element()
                                }),
                        ),
                )
            })
            // §14: the server-side author filter, beside the ref it
            // narrows.
            .child(self.commits_mine_toggle(cx))
            // The count sits right even before the ref label lands.
            .child(div().flex_1())
            .when(lane.landed.is_some() && lane.total_count > 0, |row| {
                row.child(
                    div()
                        .id("commits-count")
                        .debug_selector(|| "commits-count".to_string())
                        .text_color(muted)
                        .child(commit_count_label(lane.total_count, mine)),
                )
            });
        let body: AnyElement = if lane.rows.is_empty() {
            if let Some(err) = lane.error.as_ref() {
                // D8: page-1 error, nothing held — name the cause, offer Retry.
                v_flex()
                    .id("commits-error")
                    .debug_selector(|| "commits-error".to_string())
                    .gap_2()
                    .items_start()
                    .px_2()
                    .py_1p5()
                    .text_sm()
                    .child(
                        div()
                            .text_color(cx.theme().danger)
                            .child(format!("Couldn't load commits: {err}")),
                    )
                    .child(self.commits_retry_button("commits-retry", false, cx))
                    .into_any_element()
            } else if lane.landed.is_some() {
                let text = if mine {
                    format!("No commits by you on {ref_name}")
                } else {
                    format!("No commits on {ref_name}")
                };
                muted_cell(&text, cx)
            } else {
                commits_skeleton_rows()
            }
        } else {
            v_flex()
                .flex_1()
                .min_h_0()
                .gap_1()
                .when(lane.rewritten, |col| {
                    col.child(
                        div()
                            .id("commits-rewritten")
                            .debug_selector(|| "commits-rewritten".to_string())
                            .flex_none()
                            .px_2()
                            .text_xs()
                            .text_color(muted)
                            .child("History was rewritten — showing the latest commits"),
                    )
                })
                .child(
                    div()
                        .id("commits-list")
                        .debug_selector(|| "commits-list".to_string())
                        .flex_1()
                        .min_h_0()
                        // The pinned `ListState` owns the `List` node and
                        // takes no name: a named group carries it.
                        .role(accesskit::Role::Group)
                        .aria_label(if mine {
                            format!("Your commits on {ref_name}")
                        } else {
                            format!("Commits on {ref_name}")
                        })
                        .map(|el| match self.commits_list.as_ref() {
                            // D8: filtered, no match. Rendered here, not as
                            // the list's empty view — the pinned `List`
                            // keeps drawing a stale scrollbar thumb once its
                            // rows go to zero.
                            Some(list) if list.read(cx).delegate().rows().is_empty() => {
                                el.child(commits_no_match(cx))
                            }
                            Some(list) => el.child(List::new(list).size_full()),
                            None => el,
                        }),
                )
                .child(self.commits_tail(&lane, cx))
                .into_any_element()
        };
        v_flex()
            .id("commits-box")
            .flex_1()
            .min_h_0()
            .gap_2()
            .child(header)
            .child(body)
            .into_any_element()
    }

    /// The tail strip under the list (docs/0081 D8): paging status,
    /// the load-more failure with Retry, the end of history, the cap
    /// hand-off, or the filtered count. Pinned below the list rather
    /// than a virtual tail row — the pinned `List` sizes every section
    /// footer from section 0's, so a last-section footer can't be the
    /// only one.
    fn commits_tail(&self, lane: &crate::service::CommitLane, cx: &Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        // A status line (rubric 1): its text is its accessible name, so
        // the paging state is announced and assertable.
        let strip = |label: &str| {
            h_flex()
                .id("commits-more")
                .debug_selector(|| "commits-more".to_string())
                .role(accesskit::Role::Status)
                .aria_label(label.to_string())
                .flex_none()
                .gap_2()
                .items_center()
                .px_2()
                .py_1()
                .text_xs()
                .text_color(muted)
        };
        if !self.filters.search.is_empty() {
            // D6/F2: the filter covers the loaded rows only.
            let text = format!(
                "Filtering {} loaded commits; clear to load more",
                lane.rows.len()
            );
            return strip(&text).child(text).into_any_element();
        }
        if let Some(err) = lane.more_error.as_ref() {
            let text = format!("Couldn't load older commits: {err}");
            return strip(&text)
                .child(div().text_color(cx.theme().danger).child(text))
                .child(self.commits_retry_button("commits-more-retry", true, cx))
                .into_any_element();
        }
        if lane.loading_more {
            let text = "Loading older commits…";
            return strip(text)
                .child(Spinner::new().xsmall().color(muted))
                .child(text)
                .into_any_element();
        }
        if lane.complete {
            let text = format!(
                "Beginning of history · {}",
                commit_count_label(lane.rows.len() as i64, false)
            );
            return strip(&text).child(text).into_any_element();
        }
        if lane.capped() {
            let url = self.commits_history_url(lane, cx);
            return strip("Older history on GitHub")
                .child(
                    h_flex()
                        .id("commits-cap-link")
                        .debug_selector(|| "commits-cap-link".to_string())
                        .role(accesskit::Role::Link)
                        .aria_label("Older history on GitHub")
                        .gap_1()
                        .items_center()
                        .cursor_pointer()
                        .hover(|s| s.underline())
                        .on_click(move |_, _, cx| cx.open_url(&url))
                        .child("Older history on GitHub")
                        .child(Icon::new(IconName::ExternalLink).size_3()),
                )
                .into_any_element();
        }
        // More history exists and nothing is in flight: the list asks
        // for it near the bottom (D2, `Trigger::Paginate`).
        let text = format!(
            "{} loaded",
            commit_count_label(lane.rows.len() as i64, false)
        );
        strip(&text).child(text).into_any_element()
    }

    /// github.com's commits page for the book's ref — the cap hand-off;
    /// a "mine" book hands off to GitHub's own author filter (§14).
    fn commits_history_url(&self, lane: &crate::service::CommitLane, cx: &Context<Self>) -> String {
        let state = self.state.read(cx);
        let full = state.selected.clone().unwrap_or_default();
        let mut url = match lane.ref_name.as_deref() {
            Some(r) => format!("https://github.com/{full}/commits/{r}"),
            None => format!("https://github.com/{full}/commits"),
        };
        if self.commits_key(cx).filter == CommitFilter::Mine
            && let Some(user) = state.user.as_ref()
        {
            url.push_str(&format!("?author={}", user.login));
        }
        url
    }

    /// Retry for the commits book (docs/0081 D8): page 1 (`more` =
    /// false) re-sends the lane's spec, the tail's Retry re-asks for
    /// the failed page — both `Trigger::Manual`.
    fn commits_retry_button(&self, id: &'static str, more: bool, cx: &Context<Self>) -> Button {
        Button::new(id)
            .debug_selector(move || id.to_string())
            .label("Retry")
            .ghost()
            .xsmall()
            .on_click(cx.listener(move |this, _, window, cx| {
                let origin = Origin::new(Surface::RepoView, Trigger::Manual);
                if more {
                    this.load_more_commits(origin, window, cx);
                    return;
                }
                let Some(full) = this.state.read(cx).selected.clone() else {
                    return;
                };
                let spec = this
                    .state
                    .read(cx)
                    .commits_spec(&full, &this.commits_key(cx), origin);
                this.run_specs(vec![spec], window, cx);
            }))
    }

    /// docs/0081 D2/D10 — ask for the next commits page:
    /// `Trigger::Paginate` from the list's scroll threshold,
    /// `Trigger::Manual` from the tail's Retry. The spec builder holds
    /// the single-flight lock, the cap and the rate-limit gate.
    pub(crate) fn load_more_commits(
        &mut self,
        origin: Origin,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(full) = self.state.read(cx).selected.clone() else {
            return;
        };
        let key = self.commits_key(cx);
        let spec = self.state.update(cx, |s, cx| {
            let spec = s.commits_more_spec(&full, &key, origin);
            if spec.is_some() {
                // The lock flips the tail to "Loading older commits…".
                cx.notify();
            }
            spec
        });
        if let Some(spec) = spec {
            log::debug!("commits: next page of {full} ({})", origin.trigger.as_str());
            self.run_specs(vec![spec], window, cx);
        }
    }

    /// Copy the active commits book into the list delegate (docs/0081
    /// D5), compare-first; a repo switch resets scroll and selection,
    /// a prepend keeps the reader's place (D4/D9).
    fn sync_commits(&mut self, cx: &mut Context<Self>) {
        let Some(list) = self.commits_list.clone() else {
            return;
        };
        let Some(full) = self.state.read(cx).selected.clone() else {
            return;
        };
        let key = self.commits_key(cx);
        let (rows, can_more) = self
            .state
            .read(cx)
            .commit_lanes
            .get(&(full.clone(), key.clone()))
            .map(|l| (l.rows.clone(), l.can_load_more()))
            .unwrap_or_default();
        let query = self.filters.search.clone();
        // A repo, ref or author-filter switch, or a new filter text,
        // starts the list from the top.
        let book = (full, key);
        let switched = self.commits_synced.as_ref() != Some(&book)
            || list.read(cx).delegate().query() != query;
        let old_first = list
            .read(cx)
            .delegate()
            .rows()
            .first()
            .map(|r| r.oid.clone());
        let changed = list.update(cx, |list, cx| {
            let changed = list.delegate_mut().set_book(&rows, &query, can_more);
            if changed {
                cx.notify();
            }
            changed
        });
        if switched {
            self.commits_synced = Some(book);
            list.update(cx, |list, cx| {
                list.scroll_handle()
                    .base_handle()
                    .set_offset(point(px(0.), px(0.)));
                cx.notify();
            });
        } else if changed && let Some(old_first) = old_first {
            self.anchor_commits_scroll(&list, &old_first, cx);
        }
    }

    /// The Commits list, created on first need (see the field).
    fn ensure_commits_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.commits_list.is_some() {
            return;
        }
        let page = cx.entity().downgrade();
        let list = cx.new(|cx| ListState::new(CommitsDelegate::new(page), window, cx));
        // docs/0081 D6: Tab from the tab strip lands on the list. The
        // pinned `ListState` creates its handle without a tab stop; the
        // flag lives on the shared handle state.
        let _ = list.read(cx).focus_handle(cx).tab_stop(true);
        self.commits_list = Some(list);
        self.ensure_ref_select(window, cx);
    }

    /// docs/0081 D7 — build the branch selector (next to the commits
    /// list, see `ensure_commits_list`), wire its pick and its open
    /// edge.
    fn ensure_ref_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ref_select.is_some() {
            return;
        }
        let page = cx.entity().downgrade();
        let select = cx.new(|cx| {
            SelectState::new(RefSearchDelegate::new(Vec::new(), page), None, window, cx)
                .searchable(true)
        });
        cx.subscribe_in(
            &select,
            window,
            |this, _, event: &SelectEvent<RefSearchDelegate>, window, cx| {
                if let SelectEvent::Confirm(Some(name)) = event {
                    this.pick_ref(name.to_string(), window, cx);
                }
            },
        )
        .detach();
        cx.observe_in(&select, window, |this, select, window, cx| {
            let open = this
                .ref_select_trigger
                .as_ref()
                .is_some_and(|closed| select.read(cx).focus_handle(cx) != *closed);
            let opened = open && !this.ref_select_open;
            this.ref_select_open = open;
            if opened {
                this.fetch_ref_branches(window, cx);
            }
        })
        .detach();
        self.ref_select_trigger = Some(select.read(cx).focus_handle(cx));
        self.ref_select = Some(select);
    }

    /// docs/0081 D7 — the default branch's name, from the default book
    /// or the branch list, whichever knows it.
    fn default_ref_name(&self, full: &str, cx: &App) -> Option<String> {
        self.state
            .read(cx)
            .commit_lanes
            .get(&(full.to_string(), CommitsKey::default()))
            .and_then(|l| l.ref_name.clone())
            .or_else(|| {
                self.ref_source
                    .page_for(full)
                    .and_then(|p| p.default_branch.clone())
            })
    }

    /// docs/0081 D7 — push the branch list and the selected ref into
    /// the selector, compare-first. Until the list is fetched the menu
    /// holds just the current ref, so the trigger names it.
    fn sync_ref_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(select) = self.ref_select.clone() else {
            return;
        };
        let Some(full) = self.state.read(cx).selected.clone() else {
            return;
        };
        let default_name = self.default_ref_name(&full, cx);
        let current = match &self.commits_ref {
            RefKey::Branch(name) => Some(name.clone()),
            RefKey::Default => default_name.clone(),
        };
        let items = commits::ref_items(
            self.ref_source.names_for(&full),
            default_name.as_deref(),
            current.as_deref(),
        );
        let label: Option<SharedString> = current.map(Into::into);
        let (old_items, old_label) = self.ref_synced.take().unwrap_or_default();
        if old_items != items {
            let delegate = RefSearchDelegate::new(items.clone(), cx.entity().downgrade());
            select.update(cx, |s, cx| s.set_items(delegate, window, cx));
        }
        if old_label != label {
            select.update(cx, |s, cx| match &label {
                Some(name) => s.set_selected_value(name, window, cx),
                None => s.set_selected_index(None, window, cx),
            });
        }
        self.ref_synced = Some((items, label));
    }

    /// docs/0081 D7 — the selector opened: fetch the repo's branch list
    /// on the first open (and again after a failure). Page-owned and
    /// stateless (the docs/0044 picker source), billed as an expand.
    fn fetch_ref_branches(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (connected, gated, selected) = {
            let s = self.state.read(cx);
            (
                s.auth.connected(),
                s.gated_for(Kind::RepoBranches.lane()),
                s.selected.clone(),
            )
        };
        let Some(full) = selected else {
            return;
        };
        if !connected || gated || !self.ref_source.needs_fetch(&full) {
            return;
        }
        let (owner, name) = crate::pr_page::split_repo(&full);
        let spec = FetchSpec {
            origin: Origin::new(Surface::RepoView, Trigger::Expand),
            kind: Kind::RepoBranches,
            url: gql::GRAPHQL_ENDPOINT.to_string(),
            etag: None,
            ident: Some(full.clone()),
            max_pages: 1,
            variables: Some(serde_json::json!({ "owner": owner, "name": name, "query": null })),
        };
        log::debug!("commits: fetching branches of {full}");
        self.ref_source = RefSource {
            repo: Some(full.clone()),
            loading: true,
            search_gen: self.ref_source.search_gen + 1,
            ..RefSource::default()
        };
        let state = self.state.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = run_spec_tracked(state.clone(), spec, cx).await;
            let _ = cx.update(|_, cx| {
                let page = commits::branches_from_result(result, &state, cx);
                this.update(cx, |this, cx| {
                    if this.ref_source.repo.as_deref() != Some(full.as_str()) {
                        return; // the repo changed under the fetch
                    }
                    match &page {
                        Ok(p) => log::info!(
                            "commits: branches of {full}: {} of {}",
                            p.names.len(),
                            p.total_count
                        ),
                        Err(e) => log::warn!("commits: branches of {full} failed: {e}"),
                    }
                    this.ref_source.loading = false;
                    this.ref_source.page = Some(page);
                    cx.notify();
                })
            });
        })
        .detach();
        cx.notify();
    }

    /// docs/0044 §9 / docs/0081 D7 — a keystroke in the selector's
    /// search box. The local filter already applied; only a repo whose
    /// branches outrun the loaded page asks the server (`refs(query:)`),
    /// once typing pauses.
    pub(crate) fn queue_ref_search(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(full) = self.state.read(cx).selected.clone() else {
            return;
        };
        let truncated = self
            .ref_source
            .page_for(&full)
            .is_some_and(|p| crate::new_pr_page::branches_truncated(p.total_count, &p.names));
        self.ref_source.search_gen += 1;
        let generation = self.ref_source.search_gen;
        if query.is_empty() || !truncated {
            if self.ref_source.search.take().is_some() {
                cx.notify();
            }
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(REF_SEARCH_DELAY).await;
            let _ = cx.update(|window, cx| {
                this.update(cx, |page, cx| {
                    page.fire_ref_search(full, generation, query, window, cx)
                })
            });
        })
        .detach();
    }

    /// The debounce fired: superseded generations drop; a failure keeps
    /// the local filter's view (docs/0044 D6 conservatism).
    fn fire_ref_search(
        &mut self,
        full: String,
        generation: u64,
        query: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if generation != self.ref_source.search_gen
            || self.ref_source.repo.as_deref() != Some(full.as_str())
        {
            return;
        }
        let (owner, name) = crate::pr_page::split_repo(&full);
        let spec = FetchSpec {
            origin: Origin::new(Surface::RepoView, Trigger::Expand),
            kind: Kind::RepoBranches,
            url: gql::GRAPHQL_ENDPOINT.to_string(),
            etag: None,
            ident: Some(full.clone()),
            max_pages: 1,
            variables: Some(serde_json::json!({ "owner": owner, "name": name, "query": query })),
        };
        let state = self.state.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = run_spec_tracked(state.clone(), spec, cx).await;
            let _ = cx.update(|_, cx| {
                let page = commits::branches_from_result(result, &state, cx);
                this.update(cx, |this, cx| {
                    if generation != this.ref_source.search_gen {
                        return;
                    }
                    match page {
                        Ok(p) => {
                            this.ref_source.search = Some(p);
                            cx.notify();
                        }
                        Err(e) => {
                            log::warn!("commits: branch search failed — local filter stays: {e}")
                        }
                    }
                })
            });
        })
        .detach();
    }

    /// docs/0081 D7 — a branch was picked: switch the tab's book and
    /// offer its page 1. A held book paints at once and is age-gated
    /// (`FOCUS_MIN_AGE`); a fresh one fetches `RepoCommitsRef`.
    pub(crate) fn pick_ref(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(full) = self.state.read(cx).selected.clone() else {
            return;
        };
        let rk = commits::ref_key_for(&name, self.default_ref_name(&full, cx).as_deref());
        if rk == self.commits_ref {
            return;
        }
        log::info!("commits: {full} now on {name}");
        self.commits_ref = rk;
        cx.notify();
        self.offer_commits_book(&full, window, cx);
    }

    /// Offer the Commits tab's current `(ref, filter)` book (docs/0081
    /// D7/§14): stamp it most recently used, then its page 1 through
    /// the `FOCUS_MIN_AGE` gate as `ViewShown` — a held book paints at
    /// once and stays quiet, a fresh one fetches.
    fn offer_commits_book(&mut self, full: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (connected, gated) = {
            let s = self.state.read(cx);
            (s.auth.connected(), s.gated_for(Kind::RepoCommits.lane()))
        };
        if !connected || gated {
            return;
        }
        let key = self.commits_key(cx);
        let spec = self.state.update(cx, |s, _| {
            s.touch_commit_lane(full, &key);
            s.commits_refresh_spec(
                full,
                &key,
                crate::service::FOCUS_MIN_AGE,
                Origin::new(Surface::RepoView, Trigger::ViewShown),
            )
        });
        if let Some(spec) = spec {
            self.run_specs(vec![spec], window, cx);
        }
    }

    /// docs/0081 §14 — the book the Commits tab shows: the selected ref,
    /// filtered to the viewer's commits when the toggle is on and the
    /// viewer's node id is known (until then the toggle is disabled and
    /// the tab shows the unfiltered book).
    pub(crate) fn commits_key(&self, cx: &App) -> CommitsKey {
        let mine = self.commits_mine && self.state.read(cx).viewer_node_id().is_some();
        CommitsKey {
            rk: self.commits_ref.clone(),
            filter: if mine {
                CommitFilter::Mine
            } else {
                CommitFilter::All
            },
        }
    }

    /// docs/0081 §14 — flip the "mine" toggle: switch to that filter's
    /// book for the same ref and offer its page 1 (`history(author:)`).
    pub(crate) fn toggle_commits_mine(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(full) = self.state.read(cx).selected.clone() else {
            return;
        };
        if self.state.read(cx).viewer_node_id().is_none() {
            return;
        }
        self.commits_mine = !self.commits_mine;
        log::info!(
            "commits: {full} {}",
            if self.commits_mine {
                "filtered to my commits"
            } else {
                "showing all authors"
            }
        );
        cx.notify();
        self.offer_commits_book(&full, window, cx);
    }

    /// The "Mine" toggle in the Commits body header (docs/0081 §14): a
    /// toggle button (pressed state for assistive tech, a check glyph
    /// and the selected fill when on), disabled until
    /// the session inventory has fetched the viewer's node id.
    fn commits_mine_toggle(&self, cx: &Context<Self>) -> Button {
        let known = self.state.read(cx).viewer_node_id().is_some();
        let on = self.commits_mine && known;
        Button::new("commits-mine")
            .debug_selector(|| "commits-mine".to_string())
            .label("Mine")
            .small()
            .outline()
            .selected(on)
            .toggled(on)
            // The on state shows as a glyph, not only the selected fill
            // (rubric 17; critique iteration 1).
            .when(on, |b| b.icon(IconName::Check))
            .disabled(!known)
            .tooltip(if known {
                "Only commits you authored"
            } else {
                "Available once your account has loaded"
            })
            .on_click(cx.listener(|this, _, window, cx| {
                this.toggle_commits_mine(window, cx);
            }))
    }

    /// docs/0081 D4/D9 — after a prepend of k commits while the reader
    /// is scrolled down, shift the offset by the height the new rows
    /// (and any new day header) added above the viewport, so the rows
    /// they were reading stay put. The virtual list only knows that
    /// height after its next layout, so this waits a frame or two for
    /// the max offset to move. At the top the new commits simply show.
    fn anchor_commits_scroll(
        &mut self,
        list: &Entity<ListState<CommitsDelegate>>,
        old_first: &str,
        cx: &mut Context<Self>,
    ) {
        let weak = list.downgrade();
        let list = list.read(cx);
        let prepended = list
            .delegate()
            .rows()
            .iter()
            .position(|r| r.oid == old_first);
        let handle = list.scroll_handle().base_handle().clone();
        let offset = handle.offset();
        match prepended {
            Some(0) => return,
            Some(_) if offset.y < px(0.) => {}
            Some(_) => return,
            // D4: a replaced book restarts from the top.
            None => {
                handle.set_offset(point(px(0.), px(0.)));
                return;
            }
        }
        let old_max = handle.max_offset();
        let list = weak;
        cx.spawn(async move |_, cx| {
            for _ in 0..30 {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                let max = handle.max_offset();
                if max.y != old_max.y {
                    handle.set_offset(point(offset.x, offset.y - (max.y - old_max.y)));
                    _ = list.update(cx, |_, cx| cx.notify());
                    break;
                }
            }
        })
        .detach();
    }

    // ------------------------------------------------------------------
    // Repo-view filter toolbars (docs/0042 D3/D6)
    // ------------------------------------------------------------------

    /// One dropdown filter button (docs/0022-style PopupMenu): the label
    /// shows the active choice; a `•` prefix marks non-default state so
    /// a forgotten filter is visible even collapsed. `apply` runs
    /// deferred off the menu click (the docs/0023 D4 pattern).
    #[allow(clippy::too_many_arguments)]
    fn filter_menu_button<T: Clone + PartialEq + 'static>(
        &self,
        id: &'static str,
        label: String,
        non_default: bool,
        choices: Vec<(String, T)>,
        active: T,
        apply: fn(&mut Self, T, &mut Context<Self>),
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let weak = cx.entity().downgrade();
        Button::new(id)
            .debug_selector({
                let probe = id.to_string();
                move || probe.clone()
            })
            .ghost()
            .small()
            .label(if non_default {
                format!("• {label}")
            } else {
                label
            })
            .dropdown_menu(move |menu, _, _| {
                let mut next = menu;
                for (text, value) in choices.clone() {
                    let is_checked = value == active;
                    let weak = weak.clone();
                    let probe = format!("{id}-{}", text);
                    next = next.item(
                        PopupMenuItem::element(move |_, _| {
                            h_flex()
                                .debug_selector({
                                    let probe = probe.clone();
                                    move || probe.clone()
                                })
                                .child(text.clone())
                        })
                        .checked(is_checked)
                        .on_click(move |_, window, cx| {
                            let weak = weak.clone();
                            let value = value.clone();
                            window.defer(cx, move |_, cx| {
                                let _ = weak.update(cx, |this, cx| apply(this, value, cx));
                            });
                        }),
                    );
                }
                next
            })
    }

    /// The quick-filter event handler (docs/0042 D5): reads the input
    /// and re-renders when it moved. Extracted from the subscription so
    /// tests drive the same path (`set_value` suppresses events).
    fn apply_row_search(&mut self, cx: &mut Context<Self>) {
        let q = self.row_search.read(cx).value().trim().to_string();
        if self.filters.search != q {
            self.filters.search = q;
            cx.notify();
        }
    }

    fn set_pr_author(&mut self, v: filters::AuthorFilter, cx: &mut Context<Self>) {
        self.filters.prs.author = v;
        cx.notify();
    }

    fn set_pr_reviews(&mut self, v: filters::ReviewsFilter, cx: &mut Context<Self>) {
        self.filters.prs.reviews = v;
        cx.notify();
    }

    fn set_pr_label(&mut self, v: Option<String>, cx: &mut Context<Self>) {
        self.filters.prs.label = v;
        cx.notify();
    }

    fn set_pr_sort(&mut self, v: filters::Sort, cx: &mut Context<Self>) {
        self.filters.prs.sort = v;
        cx.notify();
    }

    fn set_issue_author(&mut self, v: filters::AuthorFilter, cx: &mut Context<Self>) {
        self.filters.issues.author = v;
        cx.notify();
    }

    fn set_issue_label(&mut self, v: Option<String>, cx: &mut Context<Self>) {
        self.filters.issues.label = v;
        cx.notify();
    }

    fn set_issue_sort(&mut self, v: filters::Sort, cx: &mut Context<Self>) {
        self.filters.issues.sort = v;
        cx.notify();
    }

    fn set_notif_reason(&mut self, v: Option<String>, cx: &mut Context<Self>) {
        self.filters.notifs.reason = v;
        cx.notify();
    }

    fn set_notif_kind(&mut self, v: Option<String>, cx: &mut Context<Self>) {
        self.filters.notifs.kind = v;
        cx.notify();
    }

    fn set_notif_sort(&mut self, v: filters::NotifSort, cx: &mut Context<Self>) {
        self.filters.notifs.sort = v;
        cx.notify();
    }

    fn clear_pr_filters(&mut self, cx: &mut Context<Self>) {
        self.filters.prs = filters::PrFilter::default();
        cx.notify();
    }

    fn clear_issue_filters(&mut self, cx: &mut Context<Self>) {
        self.filters.issues = filters::IssueFilter::default();
        cx.notify();
    }

    fn clear_notif_filters(&mut self, cx: &mut Context<Self>) {
        self.filters.notifs = filters::NotifFilter::default();
        cx.notify();
    }

    /// The PR box toolbar (docs/0042 D3): author, review requests,
    /// label, sort — plus a Clear that appears only while filtered.
    /// The label menu is hidden while the repo's rows carry no labels.
    fn pr_filter_toolbar(&self, label_options: &[String], cx: &Context<Self>) -> AnyElement {
        use filters::{AuthorFilter, ReviewsFilter, Sort};
        let f = &self.filters.prs;
        let author_label = match f.author {
            AuthorFilter::All => "Author",
            AuthorFilter::Mine => "Author: mine",
            AuthorFilter::Others => "Author: others",
        };
        let reviews_label = match f.reviews {
            ReviewsFilter::All => "Reviews",
            ReviewsFilter::RequestedOfMe => "Reviews: requested of me",
        };
        let label_button_label = match &f.label {
            None => "Label".to_string(),
            Some(name) => format!("Label: {name}"),
        };
        let sort_label = match f.sort {
            Sort::UpdatedDesc => "Updated ↓",
            Sort::UpdatedAsc => "Updated ↑",
            Sort::NumberDesc => "Newest #",
            Sort::NumberAsc => "Oldest #",
        };
        let mut row = h_flex().gap_1().flex_wrap().child(self.filter_menu_button(
            "pr-filter-author",
            author_label.to_string(),
            f.author != AuthorFilter::All,
            vec![
                ("All authors".to_string(), AuthorFilter::All),
                ("Mine".to_string(), AuthorFilter::Mine),
                ("Others".to_string(), AuthorFilter::Others),
            ],
            f.author,
            Self::set_pr_author,
            cx,
        ));
        row = row.child(self.filter_menu_button(
            "pr-filter-reviews",
            reviews_label.to_string(),
            f.reviews != ReviewsFilter::All,
            vec![
                ("All reviews".to_string(), ReviewsFilter::All),
                ("Requested of me".to_string(), ReviewsFilter::RequestedOfMe),
            ],
            f.reviews,
            Self::set_pr_reviews,
            cx,
        ));
        if !label_options.is_empty() {
            let mut choices = vec![("All labels".to_string(), None::<String>)];
            choices.extend(label_options.iter().map(|n| (n.clone(), Some(n.clone()))));
            row = row.child(self.filter_menu_button(
                "pr-filter-label",
                label_button_label,
                f.label.is_some(),
                choices,
                f.label.clone(),
                Self::set_pr_label,
                cx,
            ));
        }
        row = row.child(self.filter_menu_button(
            "pr-filter-sort",
            sort_label.to_string(),
            f.sort != Sort::default(),
            vec![
                ("Updated ↓".to_string(), Sort::UpdatedDesc),
                ("Updated ↑".to_string(), Sort::UpdatedAsc),
                ("Newest #".to_string(), Sort::NumberDesc),
                ("Oldest #".to_string(), Sort::NumberAsc),
            ],
            f.sort,
            Self::set_pr_sort,
            cx,
        ));
        if f.is_filtered() {
            row = row.child(
                Button::new("pr-filter-clear")
                    .debug_selector(|| "pr-filter-clear".to_string())
                    .ghost()
                    .small()
                    .label("Clear")
                    .on_click(cx.listener(|this, _, _, cx| this.clear_pr_filters(cx))),
            );
        }
        row.into_any_element()
    }

    /// The issue box toolbar: author, label, sort (docs/0042 D3).
    fn issue_filter_toolbar(&self, label_options: &[String], cx: &Context<Self>) -> AnyElement {
        use filters::{AuthorFilter, Sort};
        let f = &self.filters.issues;
        let author_label = match f.author {
            AuthorFilter::All => "Author",
            AuthorFilter::Mine => "Author: mine",
            AuthorFilter::Others => "Author: others",
        };
        let label_button_label = match &f.label {
            None => "Label".to_string(),
            Some(name) => format!("Label: {name}"),
        };
        let sort_label = match f.sort {
            Sort::UpdatedDesc => "Updated ↓",
            Sort::UpdatedAsc => "Updated ↑",
            Sort::NumberDesc => "Newest #",
            Sort::NumberAsc => "Oldest #",
        };
        let mut row = h_flex().gap_1().flex_wrap().child(self.filter_menu_button(
            "issue-filter-author",
            author_label.to_string(),
            f.author != AuthorFilter::All,
            vec![
                ("All authors".to_string(), AuthorFilter::All),
                ("Mine".to_string(), AuthorFilter::Mine),
                ("Others".to_string(), AuthorFilter::Others),
            ],
            f.author,
            Self::set_issue_author,
            cx,
        ));
        if !label_options.is_empty() {
            let mut choices = vec![("All labels".to_string(), None::<String>)];
            choices.extend(label_options.iter().map(|n| (n.clone(), Some(n.clone()))));
            row = row.child(self.filter_menu_button(
                "issue-filter-label",
                label_button_label,
                f.label.is_some(),
                choices,
                f.label.clone(),
                Self::set_issue_label,
                cx,
            ));
        }
        row = row.child(self.filter_menu_button(
            "issue-filter-sort",
            sort_label.to_string(),
            f.sort != Sort::default(),
            vec![
                ("Updated ↓".to_string(), Sort::UpdatedDesc),
                ("Updated ↑".to_string(), Sort::UpdatedAsc),
                ("Newest #".to_string(), Sort::NumberDesc),
                ("Oldest #".to_string(), Sort::NumberAsc),
            ],
            f.sort,
            Self::set_issue_sort,
            cx,
        ));
        if f.is_filtered() {
            row = row.child(
                Button::new("issue-filter-clear")
                    .debug_selector(|| "issue-filter-clear".to_string())
                    .ghost()
                    .small()
                    .label("Clear")
                    .on_click(cx.listener(|this, _, _, cx| this.clear_issue_filters(cx))),
            );
        }
        row.into_any_element()
    }

    /// The notifications toolbar: reason, kind, updated order
    /// (docs/0042 D3) — options derive from the unread inbox rows.
    fn notif_filter_toolbar(
        &self,
        reasons: &[String],
        kinds: &[String],
        cx: &Context<Self>,
    ) -> AnyElement {
        use filters::NotifSort;
        let f = &self.filters.notifs;
        let reason_label = match &f.reason {
            None => "Reason".to_string(),
            Some(r) => format!("Reason: {r}"),
        };
        let kind_label = match &f.kind {
            None => "Kind".to_string(),
            Some(k) => format!("Kind: {k}"),
        };
        let sort_label = match f.sort {
            NotifSort::UpdatedDesc => "Updated ↓",
            NotifSort::UpdatedAsc => "Updated ↑",
        };
        let mut row = h_flex().gap_1().flex_wrap();
        if !reasons.is_empty() {
            let mut choices = vec![("All reasons".to_string(), None::<String>)];
            choices.extend(reasons.iter().map(|r| (r.clone(), Some(r.clone()))));
            row = row.child(self.filter_menu_button(
                "notif-filter-reason",
                reason_label,
                f.reason.is_some(),
                choices,
                f.reason.clone(),
                Self::set_notif_reason,
                cx,
            ));
        }
        if !kinds.is_empty() {
            let mut choices = vec![("All kinds".to_string(), None::<String>)];
            choices.extend(kinds.iter().map(|k| (k.clone(), Some(k.clone()))));
            row = row.child(self.filter_menu_button(
                "notif-filter-kind",
                kind_label,
                f.kind.is_some(),
                choices,
                f.kind.clone(),
                Self::set_notif_kind,
                cx,
            ));
        }
        row = row.child(self.filter_menu_button(
            "notif-filter-sort",
            sort_label.to_string(),
            f.sort != NotifSort::default(),
            vec![
                ("Updated ↓".to_string(), NotifSort::UpdatedDesc),
                ("Updated ↑".to_string(), NotifSort::UpdatedAsc),
            ],
            f.sort,
            Self::set_notif_sort,
            cx,
        ));
        if f.is_filtered() {
            row = row.child(
                Button::new("notif-filter-clear")
                    .debug_selector(|| "notif-filter-clear".to_string())
                    .ghost()
                    .small()
                    .label("Clear")
                    .on_click(cx.listener(|this, _, _, cx| this.clear_notif_filters(cx))),
            );
        }
        row.into_any_element()
    }
}

/// Private-repo indicator: octicon lock-16 at 12 px in the muted color —
/// the same treatment as github.com's own repo lists (glyph only, no text).
fn private_lock(color: gpui::Hsla) -> Icon {
    Icon::empty().path(REPO_LOCK).size_3().text_color(color)
}

/// Rollup state → (glyph, theme color, tooltip) for the PR rows
/// (#67) and the commit rows (docs/0081 D5) — same tone contract as pr_page's `check_visual` (D7):
/// success green, failure red, pending amber. The Neutral arm is
/// unreachable via `rollup_state` (null rollup → no glyph) but
/// keeps the match exhaustive. Pending renders spinning upstream
/// (`Spinner`) at the call site — #397, decoration only; the
/// tooltip carries the semantics.
pub(crate) fn ci_row_glyph(
    state: gql::CheckState,
    cx: &App,
) -> (IconName, gpui::Hsla, &'static str) {
    match state {
        gql::CheckState::Success => (IconName::CircleCheck, cx.theme().success, "CI: passing"),
        gql::CheckState::Failure => (IconName::CircleX, cx.theme().danger, "CI: failing"),
        gql::CheckState::Pending => (IconName::LoaderCircle, cx.theme().warning, "CI: pending"),
        // docs/0066: unreachable via `rollup_state` (null rollup → no
        // glyph) but the match stays exhaustive — the Neutral arm's
        // treatment.
        gql::CheckState::Waiting => (
            IconName::Pause,
            cx.theme().warning,
            "CI: waiting for approval",
        ),
        gql::CheckState::Neutral => (IconName::Dash, cx.theme().muted_foreground, "CI: no status"),
        gql::CheckState::Cancelled => (IconName::Ban, cx.theme().muted_foreground, "CI: cancelled"),
    }
}

fn muted_cell(text: &str, cx: &Context<GithubPage>) -> AnyElement {
    muted_cell_keyed(text.to_string(), None, cx)
}

/// The #87 tail cell — same muted styling, but probe-visible (0010 D10:
/// `debug_bounds` sees only `debug_selector` keys; a bare div is invisible
/// to probes).
fn more_cell(n: usize, key: &'static str, cx: &Context<GithubPage>) -> AnyElement {
    muted_cell_keyed(format!("+{n} more on GitHub"), Some(key), cx)
}

/// Skeleton placeholder rows (docs/0051 D5): shown while a lane's first
/// fetch is in flight. Three bar-shaped placeholders approximate a row
/// list; the animation comes from the component itself.
fn home_skeleton_rows() -> AnyElement {
    v_flex()
        .gap_1()
        .child(Skeleton::new().h_6())
        .child(Skeleton::new().h_6().secondary())
        .child(Skeleton::new().h_6())
        .into_any_element()
}
/// docs/0081 D8 — the Commits tab's first-load skeleton: a day header
/// bar over six two-line rows (the `home_skeleton_rows` rhythm at the
/// commit row's shape).
fn commits_skeleton_rows() -> AnyElement {
    v_flex()
        .id("commits-skeleton")
        .debug_selector(|| "commits-skeleton".to_string())
        .gap_3()
        .px_2()
        .pt_3()
        .child(Skeleton::new().h_3().w_40())
        .children((0..6).map(|i| {
            v_flex()
                .gap_1p5()
                .child(if i % 2 == 0 {
                    Skeleton::new().h_4().w_3_4()
                } else {
                    Skeleton::new().h_4().w_2_3().secondary()
                })
                .child(Skeleton::new().h_3().w_1_3().secondary())
        }))
        .into_any_element()
}

/// docs/0081 D8 — the filtered-no-match cell: the other repo tabs'
/// muted cell (top-left), as a status so it is announced.
fn commits_no_match(cx: &Context<GithubPage>) -> AnyElement {
    div()
        .id("commits-no-match")
        .debug_selector(|| "commits-no-match".to_string())
        .role(accesskit::Role::Status)
        .aria_label("No loaded commits match")
        .px_2()
        .py_1p5()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child("No loaded commits match")
        .into_any_element()
}

/// "1 commit" / "425 commits" — the header count and the tail strip.
/// The body header's count: `history.totalCount`, which counts the
/// filtered history on a "mine" book (docs/0081 §14).
fn commit_count_label(n: i64, mine: bool) -> String {
    let count = if n == 1 {
        "1 commit".to_string()
    } else {
        format!("{n} commits")
    };
    if mine {
        format!("{count} by you")
    } else {
        count
    }
}

fn muted_cell_keyed(
    text: String,
    key: Option<&'static str>,
    cx: &Context<GithubPage>,
) -> AnyElement {
    let cell = div()
        .px_2()
        .py_1p5()
        .text_sm()
        .text_color(cx.theme().muted_foreground);
    let cell = match key {
        Some(k) => cell.debug_selector(move || k.to_string()),
        None => cell,
    };
    cell.child(text).into_any_element()
}

/// The §6.2 connect panel — owns the page until the app is connected.
fn connect_panel(auth: &AuthStatus, cx: &mut Context<GithubPage>) -> AnyElement {
    let (body, action_label) = match auth {
        AuthStatus::ConnectFlow(ConnectPhase::RequestingCode) => {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .child(Spinner::new())
                .child("Contacting GitHub…")
                .into_any_element();
        }
        AuthStatus::ConnectFlow(ConnectPhase::AwaitApproval {
            user_code,
            verification_uri,
        }) => {
            let code = user_code.clone();
            let uri = verification_uri.clone();
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .child(github_mark(cx).into_any_element())
                .child("Enter this code on github.com/login/device:")
                .child(
                    div()
                        .text_2xl()
                        .font_family("monospace")
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(code.clone()),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("copy-code")
                                .label("Copy code")
                                .ghost()
                                .small()
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                                }),
                        )
                        .child(
                            Button::new("open-device")
                                .label("Open github.com/login/device")
                                .small()
                                .on_click(move |_, _, cx| {
                                    cx.open_url(&uri);
                                }),
                        ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Spinner::new().small())
                        .child("Waiting for approval…"),
                )
                .into_any_element();
        }
        AuthStatus::ConnectFlow(ConnectPhase::Denied) => (
            "Connection was cancelled on GitHub. You can start again.".to_string(),
            Some("Try again"),
        ),
        AuthStatus::ConnectFlow(ConnectPhase::Expired) => (
            "The code expired after 15 minutes. Start again?".to_string(),
            Some("Start again"),
        ),
        AuthStatus::ConnectFlow(ConnectPhase::Failed(m)) => {
            (format!("Something went wrong: {m}"), Some("Retry"))
        }
        AuthStatus::KeychainError(e) => (
            format!(
                "No OS keychain is available ({e}). Start gnome-keyring or a \
                 compatible Secret Service provider, then retry."
            ),
            None,
        ),
        AuthStatus::TokenRejected => (
            "GitHub revoked this connection. Re-authorize to continue.".to_string(),
            Some("Reconnect"),
        ),
        AuthStatus::Disconnected => (
            "Connect your GitHub account to see notifications and pull \
             requests across your repositories."
                .to_string(),
            Some("Connect GitHub"),
        ),
        AuthStatus::Connected => unreachable!("panel only renders when not connected"),
    };

    let mut panel = v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_3()
        .child(github_mark(cx).into_any_element())
        .child(
            div()
                .max_w(px(CONNECT_BLURB_MAX_W))
                .text_center()
                .text_color(cx.theme().muted_foreground)
                .child(body),
        );
    if let Some(label) = action_label {
        panel = panel.child(
            Button::new("page-connect")
                .label(label)
                .on_click(cx.listener(|this, _, window, cx| {
                    start_connect(this.state.clone(), window.window_handle(), cx);
                })),
        );
    }
    panel.into_any_element()
}

/// Cold-start staleness threshold (docs/0034 D3/D6) — the disk-seeded
/// state is marked once the older cache blob exceeds this.
pub(crate) const STALE_AGE: Duration = Duration::from_secs(30 * 60);

/// The 0034 chip: the older of the two disk-seeded ages past
/// [`STALE_AGE`], as copy — `None` when fresh enough or nothing was
/// seeded (first run renders no staleness it cannot have).
pub(crate) fn stale_chip_label(
    snapshot_age: Option<Duration>,
    notifications_age: Option<Duration>,
) -> Option<SharedString> {
    let age = snapshot_age.max(notifications_age)?;
    let label = synced_label(Some(age)).unwrap_or_default();
    (age >= STALE_AGE).then(|| format!("last {label} — reconnect to update").into())
}

#[cfg(test)]
mod tests {
    use crate::drafts::DraftStore;
    use crate::usage::{Origin, Surface, Trigger};
    // NOTE: no `use super::*` here — page.rs glob-imports `gpui::*`, whose
    // `test` re-export would shadow the builtin `#[test]` attribute and
    // blow the proc-macro stack (rustc SIGSEGV). Import selectively.
    use super::{
        GithubPage, IndexPath, RepoListDelegate, RepoTab, create_allowed, create_pr_allowed,
        home_prs_hint, more_hint_count, next_org_selection, notification_url, reconcile_repo_crumb,
        repo_crumb_label,
    };
    use crate::api;
    use crate::gql;
    use gpui::AppContext as _;
    use gpui::{SharedString, TestAppContext};
    use gpui_component::list::ListState;
    use gpui_component::searchable_list::SearchableVec;
    use gpui_component::select::SelectState;
    use std::sync::Arc;

    /// docs/0042 §6 fixture: labels + review requests + authors on one
    /// repo; the viewer login is "me".
    fn pr(number: i64, title: &str, author: Option<&str>) -> gql::PullRequest {
        gql::PullRequest {
            key: format!("acme/one#{number}"),
            database_id: None,
            number,
            title: title.to_string(),
            draft: false,
            state: Some("OPEN".into()),
            author: author.map(|login| gql::Author {
                login: login.to_string(),
                avatar_url: String::new(),
            }),
            updated_at: None,
            url: "https://probe".into(),
            status: None,
            labels: None,
            review_requests: None,
        }
    }

    fn notif(id: &str, kind: &str, reason: &str, title: &str) -> api::Notification {
        api::Notification {
            id: id.to_string(),
            subject: api::Subject {
                title: title.to_string(),
                kind: kind.to_string(),
                url: None,
                latest_comment_url: None,
            },
            repository: api::RepoRef {
                full_name: "acme/one".into(),
            },
            updated_at: None,
            unread: true,
            reason: Some(reason.to_string()),
        }
    }

    #[allow(clippy::type_complexity)]
    fn filters_probe_page(
        cx: &mut gpui::TestAppContext,
    ) -> (gpui::Entity<GithubPage>, &mut gpui::VisualTestContext) {
        use crate::auth::MockTokenStore;
        use crate::http::mock::MockHttp;
        use crate::http::{Http, SharedToken};
        use crate::service::{GithubCache, GithubState};
        use dashboard_core::nav::Navigator;
        use dashboard_core::paths::AppPaths;

        cx.update(gpui_component::init);
        cx.update(dashboard_core::status::init);
        let state = cx.update(|cx| {
            let client: Arc<dyn Http> = Arc::new(MockHttp::new(vec![
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
            ]));
            let shared: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
            cx.new(|_| {
                GithubState::new(
                    client,
                    Arc::new(MockTokenStore::with("t0k".into())),
                    shared,
                    refresh,
                    GithubCache::new(AppPaths::init()),
                    DraftStore::at_tmp("page-1"),
                )
            })
        });
        let mut r = repo("acme/one", None);
        let mut mine = pr(1, "my pr", Some("me"));
        mine.labels = Some(gql::LabelSummary {
            total_count: 1,
            nodes: vec![gql::LabelRef {
                name: "bug".into(),
                color: Some("#d73a4a".into()),
            }],
        });
        mine.review_requests = Some(gql::ReviewRequestsSummary {
            total_count: 1,
            nodes: vec![gql::ReviewRequestRef {
                requested_reviewer: Some(gql::RequestedReviewer::User { login: "me".into() }),
            }],
        });
        let mut theirs = pr(2, "their pr", Some("you"));
        theirs.labels = Some(gql::LabelSummary {
            total_count: 1,
            nodes: vec![gql::LabelRef {
                name: "ui".into(),
                color: Some("#0e8a16".into()),
            }],
        });
        r.prs.total_count = 2;
        r.prs.nodes = vec![mine, theirs];
        let mut issue = gql::Issue {
            key: "acme/one#9".into(),
            database_id: None,
            number: 9,
            state: None,
            title: "some issue".into(),
            author: Some(gql::Author {
                login: "you".into(),
                avatar_url: String::new(),
            }),
            updated_at: None,
            url: "https://probe".into(),
            labels: None,
        };
        issue.labels = Some(gql::LabelSummary {
            total_count: 1,
            nodes: vec![gql::LabelRef {
                name: "ui".into(),
                color: None,
            }],
        });
        r.issues.total_count = 1;
        r.issues.nodes = vec![issue];
        cx.update(|cx| {
            state.update(cx, |s, cx| {
                s.auth = crate::service::AuthStatus::Connected;
                s.user = Some(gql::User {
                    login: "me".into(),
                    avatar_url: String::new(),
                    id: String::new(),
                });
                s.repos = vec![r];
                s.selected = Some("acme/one".into());
                s.repo_notifications.insert(
                    "acme/one".into(),
                    vec![
                        notif("n1", "PullRequest", "review_requested", "PR one"),
                        notif("n2", "Issue", "mention", "Issue two"),
                    ],
                );
                cx.notify();
            });
        });
        let nav = cx.update(|cx| cx.new(|_| Navigator::default()));
        cx.add_window_view(|window, cx| GithubPage::new(state.clone(), nav.clone(), window, cx))
    }

    #[gpui::test]
    fn repo_view_filter_toolbars_narrow_the_rows(cx: &mut gpui::TestAppContext) {
        // docs/0042 §6: default shows everything; author/reviews/label
        // filters narrow; Clear restores.
        let (page, vcx) = filters_probe_page(cx);
        let _ = vcx.debug_bounds("flush"); // first-paint flush
        vcx.run_until_parked();
        assert!(
            vcx.debug_bounds("pr-acme/one#1").is_some(),
            "the active tab's rows render"
        );
        assert!(
            vcx.debug_bounds("pr-filter-author").is_some(),
            "toolbar visible"
        );

        // Author=Others hides my row.
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| {
                p.set_pr_author(crate::filters::AuthorFilter::Others, cx)
            });
        });
        assert!(vcx.debug_bounds("pr-acme/one#1").is_none(), "my row hidden");
        assert!(vcx.debug_bounds("pr-acme/one#2").is_some());
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| {
                p.set_pr_author(crate::filters::AuthorFilter::All, cx)
            });
        });

        // Requested of me: only the PR that requests "me".
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| {
                p.set_pr_reviews(crate::filters::ReviewsFilter::RequestedOfMe, cx)
            });
        });
        assert!(vcx.debug_bounds("pr-acme/one#1").is_some());
        assert!(vcx.debug_bounds("pr-acme/one#2").is_none());
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| {
                p.set_pr_reviews(crate::filters::ReviewsFilter::All, cx)
            });
        });

        // Label=bug: only the labeled PR.
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| p.set_pr_label(Some("bug".into()), cx));
        });
        assert!(vcx.debug_bounds("pr-acme/one#2").is_none());

        // Clear restores everything in the box.
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| p.clear_pr_filters(cx));
            let _ = window;
        });
        assert!(vcx.debug_bounds("pr-acme/one#2").is_some());

        // The Issues tab renders its rows (docs/0077 D6: one box at a
        // time); the PR filters don't ride along.
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| {
                p.tab = RepoTab::Issues;
                cx.notify();
            });
        });
        assert!(vcx.debug_bounds("issue-acme/one#9").is_some());

        // The Notifications tab likewise.
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| {
                p.tab = RepoTab::Notifications;
                cx.notify();
            });
        });
        assert!(vcx.debug_bounds("notif-n1").is_some());
    }

    #[gpui::test]
    fn shared_row_search_filters_all_three_boxes(cx: &mut gpui::TestAppContext) {
        // docs/0042 D5/§6: the shared input narrows PRs, issues and
        // notifications at once; #number matches exactly.
        let (page, vcx) = filters_probe_page(cx);
        let _ = vcx.debug_bounds("flush"); // first-paint flush
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| {
                p.row_search
                    .update(cx, |input, cx| input.set_value("issue", window, cx));
                // `set_value` suppresses Change events (upstream
                // InputState) — run the same handler the event runs.
                p.apply_row_search(cx);
            });
        });
        // "issue" matches the issue and the "Issue two" notification,
        // not the PRs — asserted per tab (docs/0077 D6: one box at a
        // time, one shared search).
        assert!(vcx.debug_bounds("pr-acme/one#1").is_none());
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| {
                p.tab = RepoTab::Issues;
                cx.notify();
            });
        });
        assert!(vcx.debug_bounds("issue-acme/one#9").is_some());
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| {
                p.tab = RepoTab::Notifications;
                cx.notify();
            });
        });
        assert!(vcx.debug_bounds("notif-n2").is_some());
        assert!(vcx.debug_bounds("notif-n1").is_none());
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| {
                p.tab = RepoTab::Pulls;
                cx.notify();
            });
        });
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| {
                p.row_search
                    .update(cx, |input, cx| input.set_value("", window, cx));
                p.apply_row_search(cx);
            });
        });
        assert!(vcx.debug_bounds("pr-acme/one#1").is_some());
    }

    #[gpui::test]
    fn notif_reason_filter_narrows_the_inbox_box(cx: &mut gpui::TestAppContext) {
        // docs/0042 §6: reason filter over the unread inbox rows.
        let (page, vcx) = filters_probe_page(cx);
        let _ = vcx.debug_bounds("flush"); // first-paint flush
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| {
                p.tab = RepoTab::Notifications;
                cx.notify();
            });
        });
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| {
                p.set_notif_reason(Some("review_requested".into()), cx)
            });
        });
        assert!(vcx.debug_bounds("notif-n1").is_some());
        assert!(vcx.debug_bounds("notif-n2").is_none());
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| p.clear_notif_filters(cx));
        });
        assert!(vcx.debug_bounds("notif-n2").is_some());
    }

    #[gpui::test]
    fn label_filter_resets_on_repo_switch_but_author_survives(cx: &mut gpui::TestAppContext) {
        // docs/0042 D2/§6: label choices are repo-local; author/filter
        // state survives the switch.
        use crate::filters::AuthorFilter;
        let (page, vcx) = filters_probe_page(cx);
        let _ = vcx.debug_bounds("flush"); // first-paint flush
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| {
                p.set_pr_label(Some("bug".into()), cx);
                p.set_issue_label(Some("ui".into()), cx);
                p.set_pr_author(AuthorFilter::Mine, cx);
            });
        });
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| {
                p.select_repo("acme/two".into(), window, cx);
            });
        });
        vcx.update(|_, cx| {
            page.update(cx, |p, _| {
                assert_eq!(p.filters.prs.label, None, "label filter reset");
                assert_eq!(p.filters.issues.label, None, "issue label filter reset");
                assert_eq!(
                    p.filters.prs.author,
                    AuthorFilter::Mine,
                    "author filter survives"
                );
            });
        });
    }

    #[gpui::test]
    fn label_chips_render_on_labeled_rows(cx: &mut gpui::TestAppContext) {
        // docs/0042 D7 (amended): named pills ride the rows' title
        // cluster (via the probe-visible chips container) — on labeled
        // PR and issue rows alike; a color-less label still names
        // itself through the muted pill.
        let (page, vcx) = filters_probe_page(cx);
        let _ = vcx.debug_bounds("flush"); // first-paint flush
        vcx.run_until_parked();
        assert!(
            vcx.debug_bounds("pr-labels-acme/one#1").is_some(),
            "label chips render on the labeled PR row"
        );
        assert!(
            vcx.debug_bounds("pr-labels-acme/one#2").is_some(),
            "labeled PR #2 renders chips too"
        );
        // The issue chip lives behind its tab.
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| {
                p.tab = RepoTab::Issues;
                cx.notify();
            });
        });
        assert!(
            vcx.debug_bounds("issue-labels-acme/one#9").is_some(),
            "the color-less issue label still renders as a named pill"
        );
    }

    fn repo(key: &str, description: Option<&str>) -> gql::Repo {
        let (owner, name) = key.split_once('/').unwrap_or(("acme", key));
        gql::Repo {
            key: key.to_string(),
            database_id: None,
            name: name.to_string(),
            full_name: key.to_string(),
            node_id: None,
            has_issues_enabled: true,
            owner: gql::Owner {
                login: owner.to_string(),
            },
            description: description.map(str::to_string),
            private: false,
            is_archived: false,
            pushed_at: None,
            prs: gql::PrSummary {
                total_count: 0,
                nodes: Vec::new(),
            },
            issues: gql::IssueSummary {
                total_count: 0,
                nodes: Vec::new(),
            },
        }
    }

    fn keys(delegate: &RepoListDelegate) -> Vec<&str> {
        delegate
            .matched
            .iter()
            .map(|r| r.full_name.as_str())
            .collect()
    }

    #[test]
    fn repo_filter_matches_name_or_description_case_insensitively() {
        let mut d = RepoListDelegate::default();
        d.set_repos(vec![
            repo("acme/widget", Some("A delightful toolkit")),
            repo("acme/other", None),
            repo("acme/zephyr", Some("wind speed monitor")),
        ]);
        assert_eq!(keys(&d), ["acme/widget", "acme/other", "acme/zephyr"]);

        d.apply_query("WIDGET");
        assert_eq!(keys(&d), ["acme/widget"]);

        d.apply_query("wind");
        assert_eq!(keys(&d), ["acme/zephyr"]);

        // `full_name` is deliberately not a match key (D2): the rows show
        // only the name, and the org filter already scopes the owner.
        d.apply_query("acme/");
        assert_eq!(keys(&d), [] as [&str; 0]);
    }

    #[test]
    fn set_repos_keeps_the_active_query() {
        let mut d = RepoListDelegate::default();
        d.set_repos(vec![repo("acme/one", None), repo("acme/two", None)]);
        d.apply_query("two");
        assert_eq!(keys(&d), ["acme/two"]);

        // Background refresh arrives: the filter survives, the data swaps.
        d.set_repos(vec![repo("acme/two", None), repo("acme/three", None)]);
        assert_eq!(keys(&d), ["acme/two"]);

        // Clearing the query restores the full new list.
        d.apply_query("");
        assert_eq!(keys(&d), ["acme/two", "acme/three"]);
    }

    #[test]
    fn no_matches_after_filter() {
        let mut d = RepoListDelegate::default();
        d.set_repos(vec![repo("acme/one", None)]);
        d.apply_query("nothing-matches");
        assert!(d.matched.is_empty());
        assert_eq!(d.repo_full_name(IndexPath::new(0)), None);
    }

    #[test]
    fn repo_full_name_maps_indices_into_matched() {
        let mut d = RepoListDelegate::default();
        d.set_repos(vec![repo("acme/one", None), repo("acme/two", None)]);
        d.apply_query("two");
        assert_eq!(d.repo_full_name(IndexPath::new(0)), Some("acme/two".into()));
        // Out-of-range index (row 1 exists only unfiltered).
        assert_eq!(d.repo_full_name(IndexPath::new(1)), None);
    }

    #[test]
    fn next_org_selection_table() {
        let empty: Vec<SharedString> = Vec::new();
        let logins: Vec<SharedString> = vec!["acme".into(), "beta".into()];

        // Nothing to pick.
        assert_eq!(next_org_selection(None, &empty), None);
        // Q10 default: the first login (the personal account).
        assert_eq!(next_org_selection(None, &logins), Some("acme".into()));
        // Keep the current org when it still exists.
        assert_eq!(
            next_org_selection(Some("beta"), &logins),
            Some("beta".into())
        );
        // Vanished: reset to the first login.
        assert_eq!(
            next_org_selection(Some("gone"), &logins),
            Some("acme".into())
        );
        // Vanished and nothing left.
        assert_eq!(next_org_selection(Some("gone"), &empty), None);
    }

    // --- Notification row hand-off (doc 0015) — pure fn, table-tested --

    #[test]
    fn notification_url_targets_repo_notifications_page() {
        assert_eq!(
            notification_url("acme/widget"),
            "https://github.com/acme/widget/notifications"
        );
        // The full name arrives from `RepoRef` (`owner/repo`, no slashes
        // beyond the first) — a stray form must still produce a sane URL
        // rather than something that escapes the path.
        assert_eq!(
            notification_url("acme"),
            "https://github.com/acme/notifications"
        );
    }
    /// Entity wiring smoke test (0009 §4): both left-column components
    /// construct against a real window, and their selection APIs round-trip.
    #[gpui::test]
    fn select_and_list_entities_construct_and_select(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let cx = cx.add_empty_window();
        cx.update(|window, cx| {
            let logins: Vec<SharedString> = vec!["acme".into(), "beta".into()];
            let select = cx.new(|cx| {
                SelectState::new(SearchableVec::from(logins), None, window, cx).searchable(true)
            });
            select.update(cx, |s, cx| {
                let beta: SharedString = "beta".into();
                s.set_selected_value(&beta, window, cx);
                assert_eq!(s.selected_value(), Some(&beta));
            });

            let mut delegate = RepoListDelegate::default();
            delegate.set_repos(vec![repo("acme/one", None), repo("acme/two", None)]);
            let list = cx.new(|cx| ListState::new(delegate, window, cx).searchable(true));
            list.update(cx, |l, cx| {
                l.set_selected_index(Some(IndexPath::new(1)), window, cx);
                assert_eq!(l.selected_index(), Some(IndexPath::new(1)));
                assert_eq!(
                    l.delegate().repo_full_name(IndexPath::new(1)),
                    Some("acme/two".into())
                );
            });
        });
    }

    /// #67, rendered: a PR whose rollup carries a state shows the CI
    /// glyph (probe key `pr-ci-{key}`); a PR with no checks renders
    /// nothing. Mounted page, real rows.
    #[gpui::test]
    fn ci_glyph_renders_only_for_rolled_up_prs(cx: &mut TestAppContext) {
        use std::sync::Arc;

        use crate::auth::MockTokenStore;
        use crate::http::mock::MockHttp;
        use crate::http::{Http, SharedToken};
        use crate::service::{GithubCache, GithubState};
        use dashboard_core::nav::Navigator;
        use dashboard_core::paths::AppPaths;

        cx.update(gpui_component::init);
        cx.update(dashboard_core::status::init);
        let state = cx.update(|cx| {
            let client: Arc<dyn Http> = Arc::new(MockHttp::new(vec![
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
            ]));
            let shared: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
            cx.new(|_| {
                GithubState::new(
                    client,
                    Arc::new(MockTokenStore::with("t0k".into())),
                    shared,
                    refresh,
                    GithubCache::new(AppPaths::init()),
                    DraftStore::at_tmp("page-2"),
                )
            })
        });
        let mut r = repo("acme/one", None);
        r.prs.total_count = 2;
        r.prs.nodes = vec![
            gql::PullRequest {
                key: "acme/one#0".into(),
                database_id: None,
                number: 0,
                title: "checked".into(),
                draft: false,
                state: Some("OPEN".into()),
                author: None,
                updated_at: None,
                url: "https://probe".into(),
                status: Some("SUCCESS".into()),
                labels: None,
                review_requests: None,
            },
            gql::PullRequest {
                key: "acme/one#1".into(),
                database_id: None,
                number: 1,
                title: "unchecked".into(),
                draft: false,
                state: Some("OPEN".into()),
                author: None,
                updated_at: None,
                url: "https://probe".into(),
                status: None,
                labels: None,
                review_requests: None,
            },
        ];
        cx.update(|cx| {
            state.update(cx, |s, cx| {
                s.auth = crate::service::AuthStatus::Connected;
                s.repos = vec![r];
                s.selected = Some("acme/one".into());
                cx.notify();
            });
        });
        let nav = cx.update(|cx| cx.new(|_| Navigator::default()));
        let (_, vcx) = cx
            .add_window_view(|window, cx| GithubPage::new(state.clone(), nav.clone(), window, cx));
        let _ = vcx.debug_bounds("pr-ci-acme/one#0"); // flush frames
        assert!(
            vcx.debug_bounds("pr-ci-acme/one#0").is_some(),
            "rollup SUCCESS renders the glyph"
        );
        assert!(
            vcx.debug_bounds("pr-ci-acme/one#1").is_none(),
            "null rollup renders no glyph"
        );
    }

    /// #87, rendered: a repo past the 100-row cap shows the muted
    /// "+N more on GitHub" tail in the capped box only; the box at the
    /// cap and everything under it stay exactly as before.
    #[gpui::test]
    fn more_hint_renders_only_past_the_cap(cx: &mut TestAppContext) {
        use std::sync::Arc;

        use crate::auth::MockTokenStore;
        use crate::http::mock::MockHttp;
        use crate::http::{Http, SharedToken};
        use crate::service::{GithubCache, GithubState};
        use dashboard_core::nav::Navigator;
        use dashboard_core::paths::AppPaths;

        cx.update(gpui_component::init);
        cx.update(dashboard_core::status::init);
        // Two scripted failures: attaching the page + the select sync
        // may fire specs; results are never read.
        let state = cx.update(|cx| {
            let client: Arc<dyn Http> = Arc::new(MockHttp::new(vec![
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
            ]));
            let shared: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
            cx.new(|_| {
                GithubState::new(
                    client,
                    Arc::new(MockTokenStore::with("t0k".into())),
                    shared,
                    refresh,
                    GithubCache::new(AppPaths::init()),
                    DraftStore::at_tmp("page-3"),
                )
            })
        });
        // Connected + one repo over the cap on issues, exactly at it on
        // PRs (docs/0007 D2: 100-node page + exact total). Nodes exist —
        // the hint lives in the non-empty branch, beside real rows.
        let mut r = repo("acme/one", None);
        r.prs.total_count = 2; // == nodes.len(): under the cap, no hint
        r.prs.nodes = (0..2)
            .map(|i| gql::PullRequest {
                key: format!("acme/one#{i}"),
                database_id: None,
                number: i,
                title: "p".into(),
                draft: false,
                state: Some("OPEN".into()),
                author: None,
                updated_at: None,
                url: "https://probe".into(),
                status: None,
                labels: None,
                review_requests: None,
            })
            .collect();
        r.issues.total_count = 137;
        r.issues.nodes = (0..2)
            .map(|i| gql::Issue {
                key: format!("acme/one#{i}"),
                database_id: None,
                number: i,
                state: None,
                title: "i".into(),
                author: None,
                updated_at: None,
                url: "https://probe".into(),
                labels: None,
            })
            .collect();
        cx.update(|cx| {
            state.update(cx, |s, cx| {
                s.auth = crate::service::AuthStatus::Connected;
                s.repos = vec![r];
                s.selected = Some("acme/one".into());
                cx.notify();
            });
        });
        let nav = cx.update(|cx| cx.new(|_| Navigator::default()));
        let (page, vcx) = cx
            .add_window_view(|window, cx| GithubPage::new(state.clone(), nav.clone(), window, cx));
        let _ = vcx.debug_bounds("repo-prs-more"); // flush frames
        assert!(
            vcx.debug_bounds("repo-prs-more").is_none(),
            "PRs under the cap (2/2) carry no hint"
        );
        // The issues hint lives behind its tab.
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| {
                p.tab = RepoTab::Issues;
                cx.notify();
            });
        });
        assert!(
            vcx.debug_bounds("repo-issues-more").is_some(),
            "issues 137/100 shows the +37 more cell under the rows"
        );
    }
    // --- auto-refresh host gating (docs/0032 D1) --------------------------

    /// docs/0052 D4: the refocus entry point is nav-gated and
    /// slot-gated: no dispatch while another module owns the screen,
    /// none with no page open, one per surface on refocus while the
    /// github module shows an open page.
    #[gpui::test]
    fn refresh_open_page_gates_on_nav_and_slots(cx: &mut TestAppContext) {
        use std::sync::Arc;

        use crate::auth::MockTokenStore;
        use crate::http::mock::MockHttp;
        use crate::http::{Http, SharedToken};
        use crate::pr_page;
        use crate::service::{AuthStatus, GithubCache, GithubState, RepoLane};
        use dashboard_core::nav::{Navigator, Page};
        use dashboard_core::paths::AppPaths;

        cx.update(gpui_component::init);
        cx.update(dashboard_core::status::init);
        let cx = cx.add_empty_window();

        // 1: the eager detail fetch (fails -> the lane backoffs, docs/
        // 0036 D2); 2+3: the selected repo's row pair (docs/0035 D2).
        let mock = Arc::new(MockHttp::new(vec![
            Err(crate::http::HttpError("probe".into())),
            Err(crate::http::HttpError("probe".into())),
            Err(crate::http::HttpError("probe".into())),
        ]));
        let (state, nav, page) = cx.update(|window, cx| {
            let shared: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let state = cx.new(|_| {
                GithubState::new(
                    mock.clone() as Arc<dyn Http>,
                    Arc::new(MockTokenStore::with("t0k".into())),
                    shared,
                    refresh,
                    GithubCache::new(AppPaths::init()),
                    DraftStore::at_tmp("page-4"),
                )
            });
            state.update(cx, |s, _| {
                s.selected = Some("acme/one".into());
                s.auth = AuthStatus::Connected;
                // Hermetic: the disk cache can seed real accounts, and any
                // resolved account fires the docs/0047 D3 module-open home
                // fetch — an extra request this ledger must not count.
                s.orgs.clear();
                s.user = None;
                // The docs/0052 D5 view-shown bell lane is likewise not
                // under test here — seed its clock so the nav edge at
                // the step below stays a no-op.
                s.notifications_landed = Some(std::time::Instant::now());
            });
            let nav = cx.new(|_| Navigator::default());
            let page = cx.new(|cx| GithubPage::new(state.clone(), nav.clone(), window, cx));
            page.update(cx, |p, cx| {
                let summary = crate::gql::PullRequest {
                    key: "acme/one#7".into(),
                    database_id: None,
                    number: 7,
                    title: "gating probe".into(),
                    draft: false,
                    state: None,
                    author: None,
                    updated_at: None,
                    url: "https://probe".into(),
                    status: None,
                    labels: None,
                    review_requests: None,
                };
                // docs/0073 D4: the ctor no longer lands — land the
                // row so the store holds the document first.
                state.update(cx, |s, _| s.land_summary("acme/one", &summary));
                p.pr = Some(cx.new(|cx| {
                    pr_page::PrDetailPage::new(
                        state.clone(),
                        "acme/one".into(),
                        summary.number,
                        None,
                        window,
                        cx,
                    )
                }));
            });
            (state, nav, page)
        });

        cx.run_until_parked();
        assert_eq!(mock.recorded().len(), 1, "the eager detail fetch only");

        // Another module owns the screen (Navigator defaults to Home):
        // the refocus must not touch the open page.
        cx.update(|_, cx| page.update(cx, |p, cx| p.refresh_open_page(cx)));
        assert_eq!(mock.recorded().len(), 1, "nav-inactive beats are no-ops");

        // The github module active + a failed detail lane whose last
        // attempt is fresh (docs/0036 D2): the refocus no longer fires
        // it — the first retry waits out the backoff.
        cx.update(|_, cx| {
            nav.update(cx, |n, _| n.page = Page::Module("github".into()));
            page.update(cx, |p, cx| p.refresh_open_page(cx));
        });
        cx.run_until_parked();
        assert_eq!(
            mock.recorded().len(),
            1,
            "a fresh attempt backoffs the retry"
        );

        // The pause of the lane's own budget silences the beat (docs/0036
        // D4, per budget since docs/0082 Q10): no slot, a repo selected,
        // rows never landed — the pulls lane is GraphQL, so a GraphQL
        // pause leaves nothing to fire.
        cx.update(|_, cx| {
            page.update(cx, |p, _| p.pr = None);
            state.update(cx, |s, _| {
                s.rate_limited_until
                    .arm(crate::rate_pause::Lane::Graphql, u64::MAX / 2)
            });
            page.update(cx, |p, cx| p.refresh_open_page(cx));
        });
        cx.run_until_parked();
        assert_eq!(
            mock.recorded().len(),
            1,
            "the rate-limit pause gates the beat"
        );

        // Pause lifted: the repo-rows arm fires — the module main page
        // is the list surface and the lane (the default Open PRs tab)
        // was never landed this session → its single spec dispatches
        // (docs/0035 D2, docs/0077 D3).
        cx.update(|_, cx| {
            state.update(cx, |s, _| s.rate_limited_until.clear());
            page.update(cx, |p, cx| p.refresh_open_page(cx));
        });
        cx.run_until_parked();
        assert_eq!(
            mock.recorded().len(),
            2,
            "selected repo: the pulls lane refreshes on the beat"
        );

        // Landed rows make the refocus a no-op (docs/0035 D1): a
        // just-landed entry is younger than the 15 s floor.
        cx.update(|_, cx| {
            state.update(cx, |s, _| {
                s.rows_landed.insert(
                    ("acme/one".to_string(), RepoLane::Pulls),
                    std::time::Instant::now(),
                );
            });
            page.update(cx, |p, cx| p.refresh_open_page(cx));
        });
        cx.run_until_parked();
        assert_eq!(mock.recorded().len(), 2, "fresh rows: beat is a no-op");
    }

    /// docs/0077 D3 — the lane map end to end: repo open fires the
    /// pulls lane (the D1 default), a first tab switch fetches its
    /// lane, a re-entry inside the freshness floor doesn't, and a repo
    /// switch resets the tab to Pulls and fires the new repo's lane.
    #[gpui::test]
    fn repo_tabs_drive_the_lane_map(cx: &mut TestAppContext) {
        use std::sync::Arc;

        use crate::auth::MockTokenStore;
        use crate::http::mock::MockHttp;
        use crate::http::{Http, SharedToken};
        use crate::service::{AuthStatus, GithubCache, GithubState};
        use dashboard_core::nav::Navigator;
        use dashboard_core::paths::AppPaths;

        cx.update(gpui_component::init);
        cx.update(dashboard_core::status::init);
        let vcx = cx.add_empty_window();

        // 1: the open's pulls lane; 2: the issues switch; 3: the
        // notifications switch; 4: the second repo's pulls lane.
        let pulls_body = r#"{"data":{"repository":{"name":"one","nameWithOwner":"acme/one",
            "owner":{"login":"acme"},"pullRequests":{"totalCount":0,"nodes":[]}}}}"#;
        let issues_body = r#"{"data":{"repository":{"name":"one","nameWithOwner":"acme/one",
            "owner":{"login":"acme"},"issues":{"totalCount":0,"nodes":[]}}}}"#;
        let pulls_two_body = r#"{"data":{"repository":{"name":"two","nameWithOwner":"acme/two",
            "owner":{"login":"acme"},"pullRequests":{"totalCount":0,"nodes":[]}}}}"#;
        let mock = Arc::new(MockHttp::new(vec![
            Ok(crate::http::HttpResult {
                status: 200,
                etag: None,
                rate: None,
                body: Some(pulls_body.into()),
            }),
            Ok(crate::http::HttpResult {
                status: 200,
                etag: None,
                rate: None,
                body: Some(issues_body.into()),
            }),
            Ok(crate::http::HttpResult {
                status: 200,
                etag: None,
                rate: None,
                body: Some("[]".into()),
            }),
            Ok(crate::http::HttpResult {
                status: 200,
                etag: None,
                rate: None,
                body: Some(pulls_two_body.into()),
            }),
        ]));
        let (state, page) = vcx.update(|window, cx| {
            let shared: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let state = cx.new(|_| {
                GithubState::new(
                    mock.clone() as Arc<dyn Http>,
                    Arc::new(MockTokenStore::with("t0k".into())),
                    shared,
                    refresh,
                    GithubCache::new(AppPaths::init()),
                    DraftStore::at_tmp("repo-tabs-lane-map"),
                )
            });
            state.update(cx, |s, _| {
                s.auth = AuthStatus::Connected;
                s.orgs.clear();
                s.user = None;
            });
            let nav = cx.new(|_| Navigator::default());
            let page = cx.new(|cx| GithubPage::new(state.clone(), nav.clone(), window, cx));
            (state, page)
        });
        let _ = state;

        // Repo open: the pulls lane fires; the tab is the D1 default.
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| p.select_repo("acme/one".into(), window, cx));
        });
        vcx.run_until_parked();
        let recorded = mock.recorded();
        assert_eq!(recorded.len(), 1, "open fires exactly the pulls lane");
        assert!(
            recorded[0]
                .body
                .as_deref()
                .unwrap_or("")
                .contains("query RepoPulls"),
            "open fires RepoPulls, got {:?}",
            recorded[0].body
        );
        vcx.update(|_, cx| {
            page.update(cx, |p, _| assert_eq!(p.tab, RepoTab::Pulls));
        });

        // First switch to Issues: fetches. The pulls lane's clock was
        // just stamped, so toggling back to Pulls must NOT re-fetch.
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| p.switch_repo_tab(RepoTab::Issues, window, cx));
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| p.switch_repo_tab(RepoTab::Pulls, window, cx));
        });
        vcx.run_until_parked();
        let recorded = mock.recorded();
        assert_eq!(recorded.len(), 2, "fresh-lane re-entry is a no-op");
        assert!(
            recorded[1]
                .body
                .as_deref()
                .unwrap_or("")
                .contains("query RepoIssues"),
            "the Issues switch fires RepoIssues, got {:?}",
            recorded[1].body
        );

        // Notifications switch: the REST lane (first visit fetches).
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| {
                p.switch_repo_tab(RepoTab::Notifications, window, cx)
            });
        });
        vcx.run_until_parked();
        let recorded = mock.recorded();
        assert_eq!(recorded.len(), 3);
        assert!(
            recorded[2].url.contains("/notifications"),
            "the notifications switch fires the REST lane, got {}",
            recorded[2].url
        );

        // Repo switch: the tab resets to Pulls and the new repo's lane
        // fires (docs/0077 D1/D3).
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| p.select_repo("acme/two".into(), window, cx));
        });
        vcx.run_until_parked();
        let recorded = mock.recorded();
        assert_eq!(recorded.len(), 4);
        assert!(
            recorded[3]
                .body
                .as_deref()
                .unwrap_or("")
                .contains("\"name\":\"two\""),
            "the new repo's pulls lane fires, got {:?}",
            recorded[3].body
        );
        vcx.update(|_, cx| {
            page.update(cx, |p, _| assert_eq!(p.tab, RepoTab::Pulls));
        });
    }

    /// docs/0081 D1/D2/D6 — the Commits tab end to end: repo open
    /// fires no commits flight; entering the tab fires page 1 exactly
    /// once (a fresh re-entry is gated); the landed book syncs into the
    /// list; the quick filter turns paging off; a load-more is single
    /// flight and pinned to the tip.
    #[gpui::test]
    fn commits_tab_offers_its_lane_and_pages_on_demand(cx: &mut TestAppContext) {
        use std::sync::Arc;

        use crate::auth::MockTokenStore;
        use crate::http::mock::MockHttp;
        use crate::http::{Http, SharedToken};
        use crate::service::{AuthStatus, GithubCache, GithubState};
        use crate::usage::{Origin, Surface, Trigger};
        use dashboard_core::nav::Navigator;
        use dashboard_core::paths::AppPaths;
        use gpui_component::list::ListDelegate as _;

        cx.update(gpui_component::init);
        cx.update(dashboard_core::status::init);
        let vcx = cx.add_empty_window();

        let node = |oid: &str| {
            format!(
                r#"{{"oid":"{oid}","abbreviatedOid":"{short}","messageHeadline":"commit {oid}","committedDate":"2026-10-22T20:14:08Z","authoredDate":"2026-10-22T20:14:08Z","url":"https://github.com/acme/one/commit/{oid}","author":{{"name":"N","email":"n@x","avatarUrl":null,"user":{{"login":"noosxe"}}}},"signature":null,"statusCheckRollup":{{"state":"SUCCESS"}}}}"#,
                short = &oid[..7]
            )
        };
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let pulls_body = r#"{"data":{"repository":{"name":"one","nameWithOwner":"acme/one",
            "owner":{"login":"acme"},"pullRequests":{"totalCount":0,"nodes":[]}}}}"#;
        let commits_body = format!(
            r#"{{"data":{{"rateLimit":{{"cost":1,"remaining":4999,"resetAt":"2099-01-01T00:00:00Z","limit":5000}},"repository":{{"defaultBranchRef":{{"name":"main","target":{{"oid":"{a}","history":{{"totalCount":120,"pageInfo":{{"hasNextPage":true,"endCursor":"{a} 1"}},"nodes":[{},{}]}}}}}}}}}}}}"#,
            node(&a),
            node(&b)
        );
        let ok = |body: String| {
            Ok(crate::http::HttpResult {
                status: 200,
                etag: None,
                rate: None,
                body: Some(body),
            })
        };
        let mock = Arc::new(MockHttp::new(vec![
            ok(pulls_body.to_string()),
            ok(commits_body),
            ok(r#"{"data":null}"#.to_string()),
        ]));
        let page = vcx.update(|window, cx| {
            let shared: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let state = cx.new(|_| {
                GithubState::new(
                    mock.clone() as Arc<dyn Http>,
                    Arc::new(MockTokenStore::with("t0k".into())),
                    shared,
                    refresh,
                    GithubCache::new(AppPaths::init()),
                    DraftStore::at_tmp("repo-commits-tab"),
                )
            });
            state.update(cx, |s, _| {
                s.auth = AuthStatus::Connected;
                s.orgs.clear();
                s.user = None;
            });
            let nav = cx.new(|_| Navigator::default());
            cx.new(|cx| GithubPage::new(state, nav, window, cx))
        });
        let commits_flights = || {
            mock.recorded()
                .iter()
                .filter(|r| {
                    r.body
                        .as_deref()
                        .unwrap_or("")
                        .contains("query RepoCommits")
                })
                .count()
        };

        vcx.update(|window, cx| {
            page.update(cx, |p, cx| p.select_repo("acme/one".into(), window, cx));
        });
        vcx.run_until_parked();
        assert_eq!(commits_flights(), 0, "repo open never fetches commits");

        vcx.update(|window, cx| {
            page.update(cx, |p, cx| p.switch_repo_tab(RepoTab::Commits, window, cx));
        });
        vcx.run_until_parked();
        assert_eq!(commits_flights(), 1, "entering the tab offers page 1");
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| {
                p.switch_repo_tab(RepoTab::Pulls, window, cx);
                p.switch_repo_tab(RepoTab::Commits, window, cx);
            });
        });
        vcx.run_until_parked();
        assert_eq!(commits_flights(), 1, "a fresh book's re-entry is gated");

        // The landed book syncs into the list (render's sync step).
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| {
                p.sync_commits(cx);
                let list = p.commits_list.as_ref().unwrap().read(cx);
                assert_eq!(list.delegate().rows().len(), 2);
                assert!(list.delegate().has_more(cx), "history continues");
                assert_eq!(
                    super::GithubPlace::Repo("acme/one".into(), p.tab).key(),
                    "repo:acme/one:commits"
                );
            });
        });

        // D6/F2: filter text turns paging off; clearing it restores it.
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| {
                p.filters.search = "zzz".into();
                p.sync_commits(cx);
                let list = p.commits_list.as_ref().unwrap().read(cx);
                assert!(list.delegate().rows().is_empty());
                assert!(
                    !list.delegate().has_more(cx),
                    "no chain-fetch while filtered"
                );
                p.filters.search.clear();
                p.sync_commits(cx);
            });
        });

        // Load-more: single flight, pinned to the tip, `Paginate`.
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| {
                let origin = Origin::new(Surface::RepoView, Trigger::Paginate);
                p.load_more_commits(origin, window, cx);
                p.load_more_commits(origin, window, cx);
            });
        });
        vcx.run_until_parked();
        let recorded = mock.recorded();
        let more: Vec<_> = recorded
            .iter()
            .filter(|r| {
                r.body
                    .as_deref()
                    .unwrap_or("")
                    .contains("query RepoCommitsMore")
            })
            .collect();
        assert_eq!(more.len(), 1, "the loading_more lock refuses re-entry");
        let body = more[0].body.as_deref().unwrap_or("");
        assert!(
            body.contains(&format!(r#""tip":"{a}""#)),
            "pinned to page_tip: {body}"
        );
        assert!(body.contains(&format!(r#""after":"{a} 1""#)), "{body}");
    }

    /// docs/0081 D7 — the ref selector: the trigger names the default
    /// branch from `defaultBranchRef.name` before any branch fetch; the
    /// branch list lands default-first; picking a branch fetches its
    /// book once (`RepoCommitsRef`) and the list follows it; a held
    /// book re-picked inside `FOCUS_MIN_AGE` paints with no flight; a
    /// repo selection resets the tab to the default ref.
    #[gpui::test]
    fn commits_ref_select_switches_books_and_resets_per_repo(cx: &mut TestAppContext) {
        use std::sync::Arc;

        use crate::auth::MockTokenStore;
        use crate::http::mock::MockHttp;
        use crate::http::{Http, SharedToken};
        use crate::service::{AuthStatus, GithubCache, GithubState, RefKey, RepoLane};
        use dashboard_core::nav::Navigator;
        use dashboard_core::paths::AppPaths;

        cx.update(gpui_component::init);
        cx.update(dashboard_core::status::init);
        let vcx = cx.add_empty_window();

        let node = |oid: &str| {
            format!(
                r#"{{"oid":"{oid}","abbreviatedOid":"{short}","messageHeadline":"commit {oid}","committedDate":"2026-10-22T20:14:08Z","authoredDate":"2026-10-22T20:14:08Z","url":"https://github.com/acme/one/commit/{oid}","author":{{"name":"N","email":"n@x","avatarUrl":null,"user":{{"login":"noosxe"}}}},"signature":null,"statusCheckRollup":null}}"#,
                short = &oid[..7]
            )
        };
        let history = |tip: &str, nodes: &[&str]| {
            let nodes: Vec<String> = nodes.iter().map(|o| node(o)).collect();
            format!(
                r#"{{"oid":"{tip}","history":{{"totalCount":{n},"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[{}]}}}}"#,
                nodes.join(","),
                n = nodes.len()
            )
        };
        let (a, b, f) = ("a".repeat(40), "b".repeat(40), "f".repeat(40));
        let pulls_body = r#"{"data":{"repository":{"name":"one","nameWithOwner":"acme/one",
            "owner":{"login":"acme"},"pullRequests":{"totalCount":0,"nodes":[]}}}}"#;
        let default_body = format!(
            r#"{{"data":{{"repository":{{"defaultBranchRef":{{"name":"main","target":{}}}}}}}}}"#,
            history(&a, &[&a, &b])
        );
        let branches_body = r#"{"data":{"repository":{"defaultBranchRef":{"name":"main"},
            "refs":{"totalCount":2,"nodes":[{"name":"feat"},{"name":"main"}]}}}}"#;
        let feat_body = format!(
            r#"{{"data":{{"repository":{{"ref":{{"name":"feat","target":{}}}}}}}}}"#,
            history(&f, &[&f])
        );
        let ok = |body: String| {
            Ok(crate::http::HttpResult {
                status: 200,
                etag: None,
                rate: None,
                body: Some(body),
            })
        };
        let mock = Arc::new(MockHttp::new(vec![
            ok(pulls_body.to_string()),
            ok(default_body),
            ok(branches_body.to_string()),
            ok(feat_body),
            ok(pulls_body.to_string()),
        ]));
        let page = vcx.update(|window, cx| {
            let shared: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let state = cx.new(|_| {
                GithubState::new(
                    mock.clone() as Arc<dyn Http>,
                    Arc::new(MockTokenStore::with("t0k".into())),
                    shared,
                    refresh,
                    GithubCache::new(AppPaths::init()),
                    DraftStore::at_tmp("repo-commits-ref"),
                )
            });
            state.update(cx, |s, _| {
                s.auth = AuthStatus::Connected;
                s.orgs.clear();
                s.user = None;
            });
            let nav = cx.new(|_| Navigator::default());
            cx.new(|cx| GithubPage::new(state, nav, window, cx))
        });
        let flights = |op: &str| {
            let needle = format!("query {op}(");
            mock.recorded()
                .iter()
                .filter(|r| r.body.as_deref().unwrap_or("").contains(&needle))
                .count()
        };
        let selected = |cx: &mut gpui::VisualTestContext| {
            cx.update(|_, cx| {
                page.read(cx)
                    .ref_select
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .selected_value()
                    .map(|v| v.to_string())
            })
        };
        let resync = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| {
                page.update(cx, |p, cx| {
                    p.sync_ref_select(window, cx);
                    p.sync_commits(cx);
                })
            });
        };

        // Parked between flights: the mock answers in arrival order.
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| p.select_repo("acme/one".into(), window, cx));
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| p.switch_repo_tab(RepoTab::Commits, window, cx));
        });
        vcx.run_until_parked();
        resync(vcx);
        assert_eq!(
            flights("RepoBranches"),
            0,
            "branches load on first open only"
        );
        assert_eq!(
            selected(vcx).as_deref(),
            Some("main"),
            "the default ref is labelled from defaultBranchRef.name"
        );

        // The open edge fetches the branch list, default first.
        vcx.update(|window, cx| page.update(cx, |p, cx| p.fetch_ref_branches(window, cx)));
        vcx.run_until_parked();
        resync(vcx);
        assert_eq!(flights("RepoBranches"), 1);
        vcx.update(|_, cx| {
            let p = page.read(cx);
            let (items, _) = p.ref_synced.as_ref().unwrap();
            let names: Vec<&str> = items.iter().map(|i| i.name.as_ref()).collect();
            assert_eq!(names, ["main", "feat"]);
            assert!(items[0].default && !items[1].default);
        });

        // Picking a branch: its own book, fetched once, list follows.
        vcx.update(|window, cx| page.update(cx, |p, cx| p.pick_ref("feat".into(), window, cx)));
        vcx.run_until_parked();
        resync(vcx);
        assert_eq!(flights("RepoCommitsRef"), 1, "a fresh book fetches");
        vcx.update(|_, cx| {
            let p = page.read(cx);
            assert_eq!(p.commits_ref, RefKey::Branch("feat".into()));
            let rows = p.commits_list.as_ref().unwrap().read(cx).delegate().rows();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].oid, f);
            assert!(
                p.state
                    .read(cx)
                    .repo_tab_landed("acme/one", RepoLane::Commits, &p.commits_key(cx))
                    .is_some(),
                "the freshness cluster reads the branch book's clock"
            );
        });
        assert_eq!(selected(vcx).as_deref(), Some("feat"));

        // Back to main and over to feat again: held, fresh books paint
        // with no flight.
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| {
                p.pick_ref("main".into(), window, cx);
                assert_eq!(p.commits_ref, RefKey::Default, "main is the default book");
            })
        });
        vcx.run_until_parked();
        resync(vcx);
        vcx.update(|_, cx| {
            let rows_len = page
                .read(cx)
                .commits_list
                .as_ref()
                .unwrap()
                .read(cx)
                .delegate()
                .rows()
                .len();
            assert_eq!(rows_len, 2);
        });
        vcx.update(|window, cx| page.update(cx, |p, cx| p.pick_ref("feat".into(), window, cx)));
        vcx.run_until_parked();
        assert_eq!(flights("RepoCommitsRef"), 1, "a held book is age-gated");
        assert_eq!(flights("RepoCommits"), 1);

        // A repo selection resets the tab to the default ref.
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| p.select_repo("acme/one".into(), window, cx))
        });
        vcx.run_until_parked();
        resync(vcx);
        vcx.update(|_, cx| assert_eq!(page.read(cx).commits_ref, RefKey::Default));
        assert_eq!(selected(vcx).as_deref(), Some("main"));
    }

    /// docs/0081 §14 — the "mine" toggle: inert until the viewer's node
    /// id is known; on, it switches to the filtered book and fetches its
    /// page 1 with `history(author: { id })`; off, the held unfiltered
    /// book paints with no flight; it survives a branch pick (the
    /// branch's own mine book) and a repo selection resets it.
    #[gpui::test]
    fn commits_mine_toggle_switches_books_and_resets_per_repo(cx: &mut TestAppContext) {
        use std::sync::Arc;

        use crate::auth::MockTokenStore;
        use crate::http::mock::MockHttp;
        use crate::http::{Http, SharedToken};
        use crate::service::{AuthStatus, CommitFilter, GithubCache, GithubState, RefKey};
        use dashboard_core::nav::Navigator;
        use dashboard_core::paths::AppPaths;

        cx.update(gpui_component::init);
        cx.update(dashboard_core::status::init);
        let vcx = cx.add_empty_window();

        let node = |oid: &str| {
            format!(
                r#"{{"oid":"{oid}","abbreviatedOid":"{short}","messageHeadline":"commit {oid}","committedDate":"2026-10-22T20:14:08Z","authoredDate":"2026-10-22T20:14:08Z","url":"https://github.com/acme/one/commit/{oid}","author":{{"name":"N","email":"n@x","avatarUrl":null,"user":{{"login":"me"}}}},"signature":null,"statusCheckRollup":null}}"#,
                short = &oid[..7]
            )
        };
        let history = |tip: &str, nodes: &[&str]| {
            let nodes: Vec<String> = nodes.iter().map(|o| node(o)).collect();
            format!(
                r#"{{"oid":"{tip}","history":{{"totalCount":{n},"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[{}]}}}}"#,
                nodes.join(","),
                n = nodes.len()
            )
        };
        let (a, b, m, f) = (
            "a".repeat(40),
            "b".repeat(40),
            "c".repeat(40),
            "f".repeat(40),
        );
        let pulls_body = r#"{"data":{"repository":{"name":"one","nameWithOwner":"acme/one",
            "owner":{"login":"acme"},"pullRequests":{"totalCount":0,"nodes":[]}}}}"#;
        let default_body = format!(
            r#"{{"data":{{"repository":{{"defaultBranchRef":{{"name":"main","target":{}}}}}}}}}"#,
            history(&a, &[&a, &b])
        );
        // The ref tip (`a`) is not the viewer's: only `m` is.
        let mine_body = format!(
            r#"{{"data":{{"repository":{{"defaultBranchRef":{{"name":"main","target":{}}}}}}}}}"#,
            history(&a, &[&m])
        );
        let feat_mine_body = format!(
            r#"{{"data":{{"repository":{{"ref":{{"name":"feat","target":{}}}}}}}}}"#,
            history(&f, &[&f])
        );
        let ok = |body: String| {
            Ok(crate::http::HttpResult {
                status: 200,
                etag: None,
                rate: None,
                body: Some(body),
            })
        };
        let mock = Arc::new(MockHttp::new(vec![
            ok(pulls_body.to_string()),
            ok(default_body),
            ok(mine_body),
            ok(feat_mine_body),
            ok(pulls_body.to_string()),
        ]));
        let (page, state) = vcx.update(|window, cx| {
            let shared: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let state = cx.new(|_| {
                GithubState::new(
                    mock.clone() as Arc<dyn Http>,
                    Arc::new(MockTokenStore::with("t0k".into())),
                    shared,
                    refresh,
                    GithubCache::new(AppPaths::init()),
                    DraftStore::at_tmp("repo-commits-mine"),
                )
            });
            state.update(cx, |s, _| {
                s.auth = AuthStatus::Connected;
                s.orgs.clear();
                s.user = None;
            });
            let nav = cx.new(|_| Navigator::default());
            let page = cx.new(|cx| GithubPage::new(state.clone(), nav, window, cx));
            (page, state)
        });
        // (author id, op) of every commits flight, in order.
        let flights = || {
            mock.recorded()
                .iter()
                .filter_map(|r| {
                    let body: serde_json::Value =
                        serde_json::from_str(r.body.as_deref().unwrap_or("")).ok()?;
                    let query = body["query"].as_str()?;
                    let op = ["RepoCommitsRef", "RepoCommits"]
                        .into_iter()
                        .find(|op| query.contains(&format!("query {op}(")))?;
                    let author = body["variables"]["author"]["id"]
                        .as_str()
                        .map(str::to_string);
                    Some((author, op))
                })
                .collect::<Vec<_>>()
        };
        let rows = |cx: &mut gpui::VisualTestContext| {
            cx.update(|_, cx| {
                page.update(cx, |p, cx| p.sync_commits(cx));
                page.read(cx)
                    .commits_list
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .delegate()
                    .rows()
                    .iter()
                    .map(|r| r.oid.clone())
                    .collect::<Vec<_>>()
            })
        };
        let toggle = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| page.update(cx, |p, cx| p.toggle_commits_mine(window, cx)));
            cx.run_until_parked();
        };

        vcx.update(|window, cx| {
            page.update(cx, |p, cx| p.select_repo("acme/one".into(), window, cx));
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| p.switch_repo_tab(RepoTab::Commits, window, cx));
        });
        vcx.run_until_parked();
        assert_eq!(rows(vcx), [a.clone(), b.clone()]);

        // No viewer id yet: the toggle is inert.
        toggle(vcx);
        vcx.update(|_, cx| assert!(!page.read(cx).commits_mine));
        assert_eq!(flights(), [(None, "RepoCommits")]);

        vcx.update(|_, cx| {
            state.update(cx, |s, _| {
                s.user = Some(gql::User {
                    login: "me".into(),
                    avatar_url: String::new(),
                    id: "U_me".into(),
                })
            })
        });
        toggle(vcx);
        assert_eq!(
            flights(),
            [(None, "RepoCommits"), (Some("U_me".into()), "RepoCommits")],
            "on: the mine book's page 1 carries the author"
        );
        assert_eq!(rows(vcx), std::slice::from_ref(&m));
        vcx.update(|_, cx| {
            let p = page.read(cx);
            assert_eq!(p.commits_key(cx).filter, CommitFilter::Mine);
            let lane = &p.state.read(cx).commit_lanes[&("acme/one".to_string(), p.commits_key(cx))];
            assert_eq!(lane.ref_name.as_deref(), Some("main"));
        });

        // Off: the held unfiltered book paints, no flight.
        toggle(vcx);
        assert_eq!(flights().len(), 2);
        assert_eq!(rows(vcx), [a.clone(), b.clone()]);

        // On again, then a branch pick keeps the filter.
        toggle(vcx);
        assert_eq!(flights().len(), 2, "a held mine book is age-gated");
        vcx.update(|window, cx| page.update(cx, |p, cx| p.pick_ref("feat".into(), window, cx)));
        vcx.run_until_parked();
        assert_eq!(
            flights().last().cloned(),
            Some((Some("U_me".into()), "RepoCommitsRef"))
        );
        assert_eq!(rows(vcx), std::slice::from_ref(&f));

        // A repo selection resets the toggle with the ref.
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| p.select_repo("acme/one".into(), window, cx))
        });
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            let p = page.read(cx);
            assert!(!p.commits_mine);
            assert_eq!(p.commits_ref, RefKey::Default);
            assert_eq!(p.commits_key(cx).filter, CommitFilter::All);
        });
    }

    /// The header refresh button (docs/0024 D2 amended) refreshes the
    /// ACTIVE tab's lane: clicked on the Issues tab it must fire
    /// RepoIssues (docs/0077 D3's manual-refresh contract), and back on
    /// Pulls it must fire RepoPulls. Drives the real button through the
    /// window, not the closure's inner path.
    #[gpui::test]
    fn repo_refresh_button_refreshes_the_active_tab(cx: &mut TestAppContext) {
        use std::sync::Arc;

        use crate::auth::MockTokenStore;
        use crate::http::mock::MockHttp;
        use crate::http::{Http, SharedToken};
        use crate::service::{AuthStatus, GithubCache, GithubState};
        use dashboard_core::nav::Navigator;

        cx.update(gpui_component::init);
        cx.update(dashboard_core::status::init);
        let ok_lane = |lane_field: &str| {
            let body = format!(
                r#"{{"data":{{"repository":{{"name":"one","nameWithOwner":"acme/one",
                "owner":{{"login":"acme"}},"{lane_field}":{{"totalCount":0,"nodes":[]}}}}}}}}"#
            );
            Ok(crate::http::HttpResult {
                status: 200,
                etag: None,
                rate: None,
                body: Some(body),
            })
        };
        // 1: the open's pulls lane; 2: the issues switch; 3: the pull
        // on Issues; 4: the pull back on Pulls. Extra heads absorb any
        // attach-edge specs; results are never read.
        let mock = Arc::new(MockHttp::new(vec![
            ok_lane("pullRequests"),
            ok_lane("issues"),
            ok_lane("issues"),
            ok_lane("pullRequests"),
            ok_lane("pullRequests"),
            ok_lane("pullRequests"),
        ]));
        let (state, nav) = cx.update(|cx| {
            let shared: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let state = cx.new(|_| {
                GithubState::new(
                    mock.clone() as Arc<dyn Http>,
                    Arc::new(MockTokenStore::with("t0k".into())),
                    shared,
                    refresh,
                    GithubCache::at_tmp("repo-refresh-active-tab"),
                    DraftStore::at_tmp("repo-refresh-active-tab"),
                )
            });
            state.update(cx, |s, _| {
                s.auth = AuthStatus::Connected;
                s.orgs.clear();
                s.user = None;
                s.repos = vec![repo("acme/one", None)];
            });
            let nav = cx.new(|_| Navigator::default());
            (state, nav)
        });
        let (page, vcx) = cx
            .add_window_view(|window, cx| GithubPage::new(state.clone(), nav.clone(), window, cx));

        vcx.update(|window, cx| {
            page.update(cx, |p, cx| p.select_repo("acme/one".into(), window, cx));
        });
        vcx.run_until_parked();
        let _ = vcx.debug_bounds("repo-rows-refresh"); // flush frames
        let on_pulls = vcx.debug_bounds("repo-rows-refresh");
        assert!(
            on_pulls.is_some(),
            "the header refresh button renders on the Pulls tab"
        );
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| p.switch_repo_tab(RepoTab::Issues, window, cx));
        });
        // A manual click right after the switch: whether it lands
        // inside the switch's in-flight window or after it, it still
        // dispatches (docs/0031 D2 amended — the guard-level contract
        // is pinned by
        // read_deduped_manual_trigger_rides_out_an_in_flight_read).
        let race = vcx
            .debug_bounds("repo-rows-refresh")
            .expect("the header refresh button renders on the Issues tab");
        vcx.simulate_click(race.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
        let on_issues = vcx.debug_bounds("repo-rows-refresh");
        assert!(
            on_issues.is_some(),
            "the header refresh button renders on the Issues tab"
        );

        // The click drives the real header button on the Issues tab.
        let refresh = vcx
            .debug_bounds("repo-rows-refresh")
            .expect("the header refresh button renders");
        vcx.simulate_click(refresh.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
        let recorded = mock.recorded();
        assert_eq!(
            recorded.len(),
            4,
            "the switch + the race-window click each fire"
        );
        assert!(
            recorded[2]
                .body
                .as_deref()
                .unwrap_or("")
                .contains("query RepoIssues"),
            "the Issues switch fires RepoIssues, got {:?}",
            recorded[2].body
        );
        assert!(
            recorded[3]
                .body
                .as_deref()
                .unwrap_or("")
                .contains("query RepoIssues"),
            "the race-window manual click fires RepoIssues too, got {:?}",
            recorded[3].body
        );

        // Same click on the Pulls tab fires the pulls lane.
        vcx.update(|window, cx| {
            page.update(cx, |p, cx| p.switch_repo_tab(RepoTab::Pulls, window, cx));
        });
        vcx.run_until_parked();
        let refresh = vcx
            .debug_bounds("repo-rows-refresh")
            .expect("the header refresh button renders");
        vcx.simulate_click(refresh.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
        let recorded = mock.recorded();
        // The switch back lands inside the freshness floor (docs/0077
        // D3): no refetch — the click is the only Pulls-tab request.
        assert_eq!(recorded.len(), 5, "switch-back stays quiet; click fires");
        assert!(
            recorded[4]
                .body
                .as_deref()
                .unwrap_or("")
                .contains("query RepoPulls"),
            "refresh on the Pulls tab fires RepoPulls, got {:?}",
            recorded[4].body
        );
    }

    /// docs/0052 D5: the module view-shown notifications fire — the
    /// nav-edge call sites run through the state-side gate: fresh or
    /// backed-off lanes dispatch nothing, aged lanes dispatch the page.
    #[gpui::test]
    fn module_view_shown_fires_notifications_when_stale(cx: &mut TestAppContext) {
        use std::time::{Duration, Instant};

        use crate::auth::MockTokenStore;
        use crate::http::mock::MockHttp;
        use crate::http::{Http, HttpResult, SharedToken};
        use crate::service::{AuthStatus, GithubCache, GithubState};
        use dashboard_core::nav::{Navigator, Page};
        use dashboard_core::paths::AppPaths;

        cx.update(gpui_component::init);
        cx.update(dashboard_core::status::init);
        let cx = cx.add_empty_window();

        // Two view-shown fires, both landing an empty first page.
        let mock = Arc::new(MockHttp::new(vec![
            Ok(HttpResult {
                status: 200,
                etag: Some("\"v1\"".into()),
                rate: None,
                body: Some("[]".into()),
            }),
            Ok(HttpResult {
                status: 200,
                etag: Some("\"v1\"".into()),
                rate: None,
                body: Some("[]".into()),
            }),
        ]));
        let (state, _nav, page, records) = cx.update(|window, cx| {
            let shared: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
            // docs/0053 §5: the dispatch table rides a private records
            // sink so the test can assert origins and flight counts.
            let records = Arc::new(dashboard_core::usage_records::MemorySink::new(16));
            let recorder = dashboard_core::usage_records::Recorder::new("github", records.clone());
            let state = cx.new(|_| {
                GithubState::with_recorder(
                    mock.clone() as Arc<dyn Http>,
                    Arc::new(MockTokenStore::with("t0k".into())),
                    shared,
                    refresh,
                    GithubCache::new(AppPaths::init()),
                    DraftStore::at_tmp("page-ns-view"),
                    recorder,
                    None,
                )
            });
            state.update(cx, |s, _| {
                s.auth = AuthStatus::Connected;
                // Hermetic: keep the tick's bootstrap arm and the
                // docs/0047 D3 module-open home fetch out of the ledger.
                s.needs_bootstrap = false;
                s.orgs.clear();
                s.user = None;
            });
            let nav = cx.new(|_| Navigator::new(false));
            nav.update(cx, |n, _| n.page = Page::Module("github".into()));
            let page = cx.new(|cx| GithubPage::new(state.clone(), nav.clone(), window, cx));
            (state, nav, page, records)
        });

        // Never landed: the view-shown fire dispatches the page.
        cx.update(|window, cx| {
            page.update(cx, |p, cx| {
                p.fetch_notifications_if_stale(
                    Origin::new(Surface::ModuleHome, Trigger::ViewShown),
                    window,
                    cx,
                )
            })
        });
        cx.run_until_parked();
        assert_eq!(mock.recorded().len(), 1, "never landed: the lane fires");
        {
            use dashboard_core::usage_records::UsageSource as _;
            let rows = records.snapshot();
            assert_eq!(rows.len(), mock.recorded().len(), "one record per flight");
            assert_eq!(rows[0].op.as_ref(), "notifications");
            assert_eq!(rows[0].cost, Some(1));
            assert_eq!(rows[0].surface.as_ref(), "module-home");
            assert_eq!(rows[0].trigger.as_ref(), "view-shown");
        }

        // Landed just now: inside the 15 s floor — no dispatch.
        cx.update(|window, cx| {
            page.update(cx, |p, cx| {
                p.fetch_notifications_if_stale(
                    Origin::new(Surface::ModuleHome, Trigger::ViewShown),
                    window,
                    cx,
                )
            })
        });
        cx.run_until_parked();
        assert_eq!(mock.recorded().len(), 1, "fresh lane skips");

        // Aged past the floor: the lane fires again.
        state.update(cx, |s, _| {
            s.notifications_landed = Some(Instant::now() - Duration::from_secs(20))
        });
        cx.update(|window, cx| {
            page.update(cx, |p, cx| {
                p.fetch_notifications_if_stale(
                    Origin::new(Surface::ModuleHome, Trigger::ViewShown),
                    window,
                    cx,
                )
            })
        });
        cx.run_until_parked();
        assert_eq!(mock.recorded().len(), 2, "aged lane fires");
        {
            use dashboard_core::usage_records::UsageSource as _;
            let rows = records.snapshot();
            assert_eq!(rows.len(), mock.recorded().len());
            assert!(
                rows.iter().all(|r| r.surface.as_ref() != "background"),
                "no Background records across the dispatch table"
            );
        }
    }

    // docs/0037 D6: the repo page's freshness cluster — the synced
    // label reads this session's `rows_landed` clock, blank until the
    // first landing (0033 D2's honest blank), and the stale chip
    // stays independent of it (module-level cold start).
    #[gpui::test]
    fn repo_page_freshness_cluster_tracks_rows_landed(cx: &mut TestAppContext) {
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        use crate::auth::MockTokenStore;
        use crate::http::mock::MockHttp;
        use crate::http::{Http, SharedToken};
        use crate::service::{AuthStatus, GithubCache, GithubState, RepoLane};
        use dashboard_core::nav::Navigator;

        cx.update(gpui_component::init);
        cx.update(dashboard_core::status::init);
        let (state, nav) = cx.update(|cx| {
            let shared: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let state = cx.new(|_| {
                GithubState::new(
                    Arc::new(MockHttp::new(vec![])) as Arc<dyn Http>,
                    Arc::new(MockTokenStore::with("t0k".into())),
                    shared,
                    refresh,
                    GithubCache::at_tmp("rows-synced"),
                    DraftStore::at_tmp("rows-synced"),
                )
            });
            state.update(cx, |s, _| {
                s.auth = AuthStatus::Connected;
                s.selected = Some("acme/one".into());
            });
            let nav = cx.new(|_| Navigator::default());
            (state, nav)
        });

        // Nothing landed this session: the label is an honest blank.
        {
            let (_page, vcx) = cx.add_window_view(|window, cx| {
                GithubPage::new(state.clone(), nav.clone(), window, cx)
            });
            let _ = vcx.debug_bounds("repo-rows-synced"); // flush frames
            assert!(
                vcx.debug_bounds("repo-rows-synced").is_none(),
                "never-landed rows render no label"
            );
        }

        // A repo-snapshot landing stamps the clock: the label appears.
        cx.update(|cx| {
            state.update(cx, |s, cx| {
                // The cluster lives in the selected-repo header — the
                // repo must be present for that header to render at all.
                let repo = gql::repo_from_value(&serde_json::json!({
                    "databaseId": 7,
                    "name": "one",
                    "nameWithOwner": "acme/one",
                    "description": null,
                    "isPrivate": false,
                    "pushedAt": "2026-09-01T00:00:00Z",
                    "owner": {"login": "o"},
                    "pullRequests": {"totalCount": 0, "nodes": []},
                    "issues": {"totalCount": 0, "nodes": []}
                }))
                .unwrap();
                s.repos.push(repo);
                s.rows_landed.insert(
                    ("acme/one".to_string(), RepoLane::Pulls),
                    Instant::now() - Duration::from_secs(120),
                );
                cx.notify();
            });
        });
        {
            let (_page, vcx) = cx.add_window_view(|window, cx| {
                GithubPage::new(state.clone(), nav.clone(), window, cx)
            });
            let _ = vcx.debug_bounds("repo-rows-synced"); // flush frames
            assert!(
                vcx.debug_bounds("repo-rows-synced").is_some(),
                "landed rows show the synced label"
            );
        }
    }

    /// docs/0034 D3: a stale disk-seeded state marks both cold-start
    /// surfaces (home card, module main page); clearing the ages clears
    /// the chips. The clear RULE itself — a landed apply — is pinned in
    /// the service tests; here it is the render wiring.
    #[gpui::test]
    fn stale_seed_marks_card_and_main_page(cx: &mut TestAppContext) {
        use std::sync::Arc;
        use std::time::{Duration, SystemTime};

        use crate::auth::MockTokenStore;
        use crate::card::GithubCard;
        use crate::http::mock::MockHttp;
        use crate::http::{Http, SharedToken};
        use crate::service::{GithubCache, GithubState};
        use dashboard_core::nav::{Navigator, Page};
        cx.update(gpui_component::init);
        cx.update(dashboard_core::status::init);

        // A stale-seeded connected state: the legacy mock token connects
        // at construction; both blobs backdated past STALE_AGE (2h).
        let cache = GithubCache::at_tmp("chip");
        cache.put("notifications", &serde_json::json!([]));
        let snapshot = gql::SnapshotCache {
            schema: 2,
            user: None,
            orgs: vec![],
            repos: vec![],
        };
        cache.put(gql::SNAPSHOT_CACHE_KEY, &snapshot);
        for rel in ["notifications", gql::SNAPSHOT_CACHE_KEY] {
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(cache.last_known_path(rel))
                .unwrap();
            file.set_times(
                std::fs::FileTimes::new()
                    .set_modified(SystemTime::now() - Duration::from_secs(2 * 3600)),
            )
            .unwrap();
        }
        let (state, nav) = cx.update(|cx| {
            let shared: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let state = cx.new(|_| {
                GithubState::new(
                    Arc::new(MockHttp::new(vec![])) as Arc<dyn Http>,
                    Arc::new(MockTokenStore::with("t0k".into())),
                    shared,
                    refresh,
                    cache,
                    DraftStore::at_tmp("page-helper"),
                )
            });
            let nav = cx.new(|_| Navigator::default());
            (state, nav)
        });

        // docs/0051 D4: park the nav off Home so the card's
        // construction-time totals fetch does not fire — this test's
        // subject is the seeded-age chip, not the fetch lane (the
        // trigger itself is covered by its own test).
        cx.update(|cx| {
            nav.update(cx, |nav, cx| nav.go(Page::Settings, cx));
        });
        // The card surface: chip present while the ages are stale.
        {
            let (_card, card_vcx) = cx.add_window_view(|window, cx| {
                GithubCard::new(state.clone(), nav.clone(), window, cx)
            });
            let _ = card_vcx.debug_bounds("github-card-stale"); // flush frames
            assert!(
                card_vcx.debug_bounds("github-card-stale").is_some(),
                "the stale-seeded card shows the chip"
            );
        }

        // The main-page surface: same seed, same chip at the column top.
        {
            let (_page, page_vcx) = cx.add_window_view(|window, cx| {
                GithubPage::new(state.clone(), nav.clone(), window, cx)
            });
            assert!(
                page_vcx.debug_bounds("github-stale").is_some(),
                "the stale-seeded main page shows the chip"
            );

            // Ages cleared (a landed apply does this — service-tested):
            // the chip goes away. The page syncs from the state.
            page_vcx.update(|_, cx| {
                state.update(cx, |s, cx| {
                    s.snapshot_age = None;
                    s.notifications_age = None;
                    cx.notify();
                });
            });
            assert!(
                page_vcx.debug_bounds("github-stale").is_none(),
                "cleared ages: no main-page chip"
            );
        }

        // The card re-mounted on the cleared state: no chip. (The card's
        // own clear-on-render — it observes the state.)
        {
            let (_card, card_vcx) = cx.add_window_view(|window, cx| {
                GithubCard::new(state.clone(), nav.clone(), window, cx)
            });
            let _ = card_vcx.debug_bounds("github-card-stale"); // flush frames
            assert!(
                card_vcx.debug_bounds("github-card-stale").is_none(),
                "cleared ages: no card chip"
            );
        }
    }

    /// Module-crumb re-entry (docs/0014 §5 D7, issue #100):
    /// `return_to_root` drops the repo selection (state + List
    /// highlight) and closes the PR sub-page; the navigator is left
    /// with no module crumbs.
    #[gpui::test]
    fn return_to_root_clears_selection_and_pr(cx: &mut TestAppContext) {
        use std::sync::Arc;

        use crate::auth::MockTokenStore;
        use crate::http::mock::MockHttp;
        use crate::http::{Http, SharedToken};
        use crate::pr_page;
        use crate::service::{GithubCache, GithubState};
        use dashboard_core::nav::Navigator;
        use dashboard_core::paths::AppPaths;

        cx.update(gpui_component::init);
        // GithubState's spec runner tracks in-flight fetches via the
        // status-bar global (docs/0013) — `main` inits it before any
        // module; tests must too.
        cx.update(dashboard_core::status::init);
        let cx = cx.add_empty_window();
        cx.update(|window, cx| {
            // Two scripted failures: `PrDetailPage::new` starts its detail
            // fetch eagerly; the probe never depends on its result.
            let client: Arc<dyn Http> = Arc::new(MockHttp::new(vec![
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
            ]));
            let shared: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let state = cx.new(|_| {
                GithubState::new(
                    client,
                    Arc::new(MockTokenStore::with("t0k".into())),
                    shared,
                    refresh,
                    GithubCache::new(AppPaths::init()),
                    DraftStore::at_tmp("page-5"),
                )
            });
            state.update(cx, |s, _| s.selected = Some("acme/one".into()));
            let nav = cx.new(|_| Navigator::default());
            let page = cx.new(|cx| GithubPage::new(state.clone(), nav.clone(), window, cx));
            // An open PR sub-page (what `close_pr` must clear).
            page.update(cx, |p, cx| {
                let summary = gql::PullRequest {
                    key: "acme/one#7".into(),
                    database_id: None,
                    number: 7,
                    title: "probe".into(),
                    draft: false,
                    state: None,
                    author: None,
                    updated_at: None,
                    url: "https://probe".into(),
                    status: None,
                    labels: None,
                    review_requests: None,
                };
                // docs/0073 D4: the ctor no longer lands — land the
                // row so the store holds the document first.
                state.update(cx, |s, _| s.land_summary("acme/one", &summary));
                p.pr = Some(cx.new(|cx| {
                    pr_page::PrDetailPage::new(
                        state.clone(),
                        "acme/one".into(),
                        summary.number,
                        None,
                        window,
                        cx,
                    )
                }));
            });

            page.update(cx, |p, cx| p.return_to_root(window, cx));

            assert_eq!(state.read(cx).selected, None, "selection dropped");
            assert!(page.read(cx).pr.is_none(), "PR page closed");
            assert!(
                page.read(cx).repo_list.read(cx).selected_index().is_none(),
                "List highlight cleared"
            );
            assert!(
                nav.read(cx).crumbs().is_empty(),
                "crumbs back to the module root"
            );
        });
    }
    /// Draft retention (docs/0024 D7): close_creator hides without
    /// dropping — reopen restores the draft — while a repo switch drops
    /// the creator entirely.
    #[gpui::test]
    fn creator_draft_survives_close_and_reopen_dies_on_repo_switch(cx: &mut TestAppContext) {
        use std::sync::Arc;

        use crate::auth::MockTokenStore;
        use crate::http::mock::MockHttp;
        use crate::http::{Http, SharedToken};
        use crate::service::{GithubCache, GithubState};
        use dashboard_core::nav::Navigator;
        use dashboard_core::paths::AppPaths;

        cx.update(gpui_component::init);
        cx.update(dashboard_core::status::init);
        let cx = cx.add_empty_window();
        cx.update(|window, cx| {
            // docs/0046: the creator fetches templates + config per open —
            // two lanes on the first open (new entity) and two more per
            // re-open — plus select_repo's two specs. All scripted
            // failures; results never read.
            let client: Arc<dyn Http> = Arc::new(MockHttp::new(vec![
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
            ]));
            let shared: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let state = cx.new(|_| {
                GithubState::new(
                    client,
                    Arc::new(MockTokenStore::with("t0k".into())),
                    shared,
                    refresh,
                    GithubCache::new(AppPaths::init()),
                    DraftStore::at_tmp("page-6"),
                )
            });
            state.update(cx, |s, _| s.selected = Some("acme/one".into()));
            let nav = cx.new(|_| Navigator::default());
            let page = cx.new(|cx| GithubPage::new(state.clone(), nav.clone(), window, cx));

            let mut r = repo("acme/one", None);
            r.node_id = Some("R_1".into());
            let r2 = repo("acme/two", None);

            page.update(cx, |p, cx| p.open_creator(&r, window, cx));
            assert!(page.read(cx).creator_open, "creator visible");
            assert!(
                page.read(cx).pr.is_none() && page.read(cx).issue.is_none(),
                "mutual exclusion: opening the creator closed any sub-page"
            );

            // Type a draft, then Cancel (close_creator): hidden, not dropped.
            page.update(cx, |p, cx| {
                let creator = p.creator.as_ref().unwrap();
                let title = creator.read(cx).title.clone();
                title.update(cx, |s, cx| s.set_value("My draft", window, cx));
            });
            page.update(cx, |p, cx| p.close_creator(cx));
            assert!(!page.read(cx).creator_open, "hidden");
            assert!(page.read(cx).creator.is_some(), "entity retained");

            // Reopen on the same repo: same entity, draft intact (D7).
            page.update(cx, |p, cx| p.open_creator(&r, window, cx));
            page.update(cx, |p, cx| {
                let creator = p.creator.as_ref().unwrap();
                let value = creator.read(cx).title.read(cx).value().to_string();
                assert_eq!(value, "My draft", "draft survives close/reopen");
            });

            // Switch repos: the creator drops (draft belongs to its repo).
            page.update(cx, |p, cx| p.select_repo("acme/two".into(), window, cx));
            assert!(page.read(cx).creator.is_none(), "creator dropped on switch");
            assert!(!page.read(cx).creator_open);

            // A different repo's creator is a fresh entity.
            page.update(cx, |p, cx| p.open_creator(&r2, window, cx));
            page.update(cx, |p, cx| {
                let creator = p.creator.as_ref().unwrap();
                let value = creator.read(cx).title.read(cx).value().to_string();
                assert_eq!(value, "", "fresh draft for the new repo");
            });
        });
    }
    // --- Breadcrumb sync (docs/0014) — pure fns, table-tested ---------

    #[test]
    fn repo_crumb_label_resolves_selected_repo_short_name() {
        let repos = vec![repo("acme/widget", None), repo("acme/gadget", None)];
        assert_eq!(
            repo_crumb_label(Some("acme/widget"), &repos, true),
            Some(SharedString::from("widget"))
        );
    }

    #[test]
    fn repo_crumb_label_none_without_selection() {
        let repos = vec![repo("acme/widget", None)];
        assert_eq!(repo_crumb_label(None, &repos, true), None);
    }

    #[test]
    fn create_gate_hides_until_every_condition_holds() {
        // docs/0024 D2: connected ∧ issues-enabled ∧ node id known —
        // any miss hides the button (hidden-not-disabled). No
        // viewerPermission term: any reader can file issues.
        let mut r = repo("acme/widget", None);
        r.node_id = Some("R_1".into());
        r.has_issues_enabled = true;
        assert!(create_allowed(&r, true));

        // Disconnected never offers creation.
        assert!(!create_allowed(&r, false));

        // Issues disabled → hidden.
        r.has_issues_enabled = false;
        assert!(!create_allowed(&r, true));
        r.has_issues_enabled = true;

        // Node id unknown (pre-0024 cache) → hidden until a fetch lands.
        r.node_id = None;
        assert!(!create_allowed(&r, true));
        r.node_id = Some("R_1".into());

        // Archived repo (#275) → hidden even with every other term true.
        r.is_archived = true;
        assert!(!create_allowed(&r, true));
    }

    #[test]
    fn create_pr_gate_hides_until_every_condition_holds() {
        // docs/0044 D2: connected ∧ node id known — no upstream creation
        // flag exists, so the gate is the mutation target's presence.
        let mut r = repo("acme/widget", None);
        r.node_id = Some("R_1".into());
        r.has_issues_enabled = true;
        assert!(create_pr_allowed(&r, true));

        // Disconnected never offers creation.
        assert!(!create_pr_allowed(&r, false));

        // Issues-disabled is irrelevant to PR creation — the node id
        // alone arms the button (docs/0044 §2: no PR analog of the flag).
        r.has_issues_enabled = false;
        assert!(create_pr_allowed(&r, true));

        // Node id unknown → hidden.
        r.node_id = None;
        assert!(!create_pr_allowed(&r, true));
        r.node_id = Some("R_1".into());

        // Archived repo (#275) → hidden even with the node id known.
        r.is_archived = true;
        assert!(!create_pr_allowed(&r, true));
    }

    #[test]
    fn more_hint_counts_only_past_the_cap() {
        // #87: `total − rendered` whenever the 100-row cap bit;
        // `None` otherwise — repos under the cap are untouched.
        assert_eq!(more_hint_count(137, 100), Some(37));
        assert_eq!(more_hint_count(101, 100), Some(1));
        assert_eq!(more_hint_count(100, 100), None);
        assert_eq!(more_hint_count(5, 100), None);
        assert_eq!(more_hint_count(0, 0), None);
    }

    #[test]
    fn repo_crumb_label_none_when_repo_missing_from_snapshot() {
        // Same lookup predicate as the right column (docs/0014 D2): a
        // dropped repo is "Select a repository" in the pane and no crumb
        // in the chrome.
        let repos = vec![repo("acme/widget", None)];
        assert_eq!(repo_crumb_label(Some("acme/ghost"), &repos, true), None);
    }

    #[test]
    fn repo_crumb_label_none_when_disconnected() {
        let repos = vec![repo("acme/widget", None)];
        assert_eq!(repo_crumb_label(Some("acme/widget"), &repos, false), None);
    }

    #[test]
    fn reconcile_noop_when_nothing_selected_and_no_crumbs() {
        assert_eq!(reconcile_repo_crumb(&[], &[]), None);
    }

    #[test]
    fn reconcile_clears_all_when_repo_lost() {
        // A suffix without its repo is meaningless — drop it too.
        let current = vec![SharedString::from("widget"), SharedString::from("#12")];
        assert_eq!(reconcile_repo_crumb(&[], &current), Some(Vec::new()));
    }

    #[test]
    fn reconcile_reports_when_crumbs_empty() {
        // First report, or re-entry after `go` cleared the crumbs.
        assert_eq!(
            reconcile_repo_crumb(&["widget".into()], &[]),
            Some(vec![SharedString::from("widget")])
        );
    }

    #[test]
    fn reconcile_noop_when_chain_matches() {
        let current = vec![SharedString::from("widget")];
        assert_eq!(reconcile_repo_crumb(&["widget".into()], &current), None);
    }

    #[test]
    fn reconcile_preserves_subpage_suffix() {
        // docs/0016 D1: while the PR page is open, the derived chain
        // carries the `#n` suffix — a matching navigator chain is a no-op,
        // so background ticks cannot flatten it back to [repo].
        let current = vec![SharedString::from("widget"), SharedString::from("#482")];
        assert_eq!(
            reconcile_repo_crumb(&["widget".into(), "#482".into()], &current),
            None
        );
    }

    #[test]
    fn reconcile_rewrites_when_repo_changed() {
        let current = vec![SharedString::from("widget")];
        assert_eq!(
            reconcile_repo_crumb(&["gadget".into()], &current),
            Some(vec![SharedString::from("gadget")])
        );
    }

    #[test]
    fn reconcile_appends_and_clears_the_pr_suffix() {
        // Open: [repo] → [repo, #n]. Close: [repo, #n] → [repo].
        let repo = [SharedString::from("widget")];
        let open = [SharedString::from("widget"), SharedString::from("#482")];
        assert_eq!(
            reconcile_repo_crumb(&open, &repo),
            Some(vec![
                SharedString::from("widget"),
                SharedString::from("#482")
            ])
        );
        assert_eq!(
            reconcile_repo_crumb(&repo, &open),
            Some(vec![SharedString::from("widget")])
        );
    }

    /// Mini inbox server for the row-level done round trip (docs/0026
    /// D8): GET answers the current inbox (whatever the DELETEs have
    /// left), DELETE pops the thread out of it — after its scripted
    /// failures — and PATCH answers 205. Order-independent, like the
    /// notification page's `LaneMock`.
    struct InboxMock {
        inbox: std::sync::Mutex<Vec<String>>,
        delete_failures: std::sync::atomic::AtomicUsize,
        requests: std::sync::Mutex<Vec<(crate::http::Verb, String)>>,
    }

    impl InboxMock {
        fn requests(&self) -> Vec<(crate::http::Verb, String)> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl crate::http::Http for InboxMock {
        fn request(
            &self,
            verb: crate::http::Verb,
            url: &str,
            _: Option<&str>,
            _: Option<&str>,
        ) -> Result<crate::http::HttpResult, crate::http::HttpError> {
            self.requests.lock().unwrap().push((verb, url.to_string()));
            let (status, body) = match verb {
                crate::http::Verb::Get => {
                    let inbox = self.inbox.lock().unwrap();
                    (200, Some(format!("[{}]", inbox.join(","))))
                }
                crate::http::Verb::Delete => {
                    let left = self
                        .delete_failures
                        .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    if left > 0 {
                        (500, None)
                    } else {
                        let id = url.rsplit('/').next().unwrap_or_default();
                        self.inbox
                            .lock()
                            .unwrap()
                            .retain(|t| !t.contains(&format!("\"id\":\"{id}\"")));
                        (204, None)
                    }
                }
                crate::http::Verb::Patch => (205, None),
                _ => (200, Some("[]".into())),
            };
            Ok(crate::http::HttpResult {
                status,
                etag: None,
                rate: None,
                body,
            })
        }

        fn post_graphql(
            &self,
            _: &str,
            _: &serde_json::Value,
        ) -> Result<crate::http::HttpResult, crate::http::HttpError> {
            Err(crate::http::HttpError("probe: no graphql".into()))
        }
    }

    /// The repo view over the mini inbox: state + connected page, the
    /// thread seeded straight into `repo_notifications` (the map the
    /// rows render from — docs/0026 D2).
    fn probe_repo_view(
        cx: &mut TestAppContext,
        mock: std::sync::Arc<InboxMock>,
    ) -> (gpui::Entity<GithubPage>, &mut gpui::VisualTestContext) {
        use crate::auth::MockTokenStore;
        use crate::service::{AuthStatus, GithubCache, GithubState};
        use dashboard_core::nav::Navigator;
        use dashboard_core::paths::AppPaths;

        use std::sync::Arc;
        cx.update(gpui_component::init);
        cx.update(dashboard_core::status::init);
        let state = cx.update(|cx| {
            let shared: crate::http::SharedToken = Arc::new(std::sync::RwLock::new(None));
            let refresh: crate::http::SharedToken = Arc::new(std::sync::RwLock::new(None));
            let entity = cx.new(|_| {
                GithubState::new(
                    mock,
                    Arc::new(MockTokenStore::with("t0k".into())),
                    shared,
                    refresh,
                    GithubCache::new(AppPaths::init()),
                    DraftStore::at_tmp("page-7"),
                )
            });
            entity.update(cx, |s, cx| {
                s.auth = AuthStatus::Connected;
                s.repos = vec![repo("acme/one", None)];
                s.selected = Some("acme/one".into());
                let thread: crate::api::Notification = serde_json::from_value(serde_json::json!({
                    "id": "19", "unread": true,
                    "subject": {"title": "Fix the flume", "type": "PullRequest"},
                    "repository": {"full_name": "acme/one"},
                    "reason": "review_requested",
                    "updated_at": "2026-09-01T00:00:00Z"
                }))
                .unwrap();
                s.repo_notifications.insert("acme/one".into(), vec![thread]);
                cx.notify();
            });
            entity
        });
        let nav = cx.update(|cx| cx.new(|_| Navigator::default()));
        let (page, vcx) = cx.add_window_view(|window, cx| GithubPage::new(state, nav, window, cx));
        // The row-done surface lives on the Notifications tab
        // (docs/0077 D6).
        let _ = vcx.debug_bounds("flush");
        vcx.update(|_, cx| {
            page.update(cx, |p, cx| {
                p.tab = RepoTab::Notifications;
                cx.notify();
            });
        });
        (page, vcx)
    }

    // #275 — archived repo: both creation buttons are hidden even with
    // every other gate term armed (connected is forced by the fixture;
    // node id + issues-enabled are set on the repo). Hidden-not-disabled.
    #[gpui::test]
    fn archived_repo_hides_creation_buttons(cx: &mut TestAppContext) {
        use crate::auth::MockTokenStore;
        use crate::http::mock::MockHttp;
        use crate::http::{Http, SharedToken};
        use crate::service::{AuthStatus, GithubCache, GithubState};
        use dashboard_core::nav::Navigator;
        use dashboard_core::paths::AppPaths;
        use std::sync::Arc;

        cx.update(gpui_component::init);
        cx.update(dashboard_core::status::init);
        let state = cx.update(|cx| {
            // Two scripted failures: attaching the page + the select sync
            // may fire specs; results are never read.
            let client: Arc<dyn Http> = Arc::new(MockHttp::new(vec![
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
            ]));
            let shared: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let refresh: crate::http::SharedToken = Arc::new(std::sync::RwLock::new(None));
            let entity = cx.new(|_| {
                GithubState::new(
                    client,
                    Arc::new(MockTokenStore::with("t0k".into())),
                    shared,
                    refresh,
                    GithubCache::new(AppPaths::init()),
                    DraftStore::at_tmp("page-archived"),
                )
            });
            entity.update(cx, |s, cx| {
                s.auth = AuthStatus::Connected;
                let mut r = repo("acme/one", None);
                r.node_id = Some("R_1".into());
                r.has_issues_enabled = true;
                r.is_archived = true;
                s.repos = vec![r];
                s.selected = Some("acme/one".into());
                cx.notify();
            });
            entity
        });
        let nav = cx.update(|cx| cx.new(|_| Navigator::default()));
        let (_, vcx) = cx.add_window_view(|window, cx| GithubPage::new(state, nav, window, cx));

        assert!(
            vcx.debug_bounds("new-issue-open").is_none(),
            "New issue hidden on an archived repo"
        );
        assert!(
            vcx.debug_bounds("new-pr-open").is_none(),
            "New pull request hidden on an archived repo"
        );
    }

    #[gpui::test]
    fn row_done_button_dones_without_opening_the_page(cx: &mut TestAppContext) {
        // docs/0026 D8 (#153, owner decision §4.5): the row's icon button
        // fires the DELETE directly — no page takeover — and the
        // post-write refetch (global + repo slice) drops the row from
        // the unread list.
        let thread = serde_json::json!({
            "id": "19", "unread": true,
            "subject": {"title": "Fix the flume", "type": "PullRequest"},
            "repository": {"full_name": "acme/one"},
            "reason": "review_requested",
            "updated_at": "2026-09-01T00:00:00Z"
        })
        .to_string();
        let mock = std::sync::Arc::new(InboxMock {
            inbox: std::sync::Mutex::new(vec![thread]),
            delete_failures: std::sync::atomic::AtomicUsize::new(0),
            requests: std::sync::Mutex::new(Vec::new()),
        });
        let (_, vcx) = probe_repo_view(cx, mock.clone());
        vcx.run_until_parked();
        let _ = vcx.debug_bounds("notif-19"); // flush frames
        assert!(vcx.debug_bounds("notif-19").is_some(), "row renders");
        let button = vcx
            .debug_bounds("notif-done-19")
            .expect("row done button renders");
        vcx.simulate_click(button.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
        // The DELETE, then the two refetch GETs (global list + the
        // selected repo's slice).
        let requests = mock.requests();
        let delete_at = requests
            .iter()
            .position(|(v, url)| {
                matches!(v, crate::http::Verb::Delete) && url.ends_with("/notifications/threads/19")
            })
            .expect("DELETE fired");
        let gets_after = requests[delete_at + 1..]
            .iter()
            .filter(|(v, _)| matches!(v, crate::http::Verb::Get))
            .count();
        assert!(
            gets_after >= 2,
            "post-done refetch covers global + repo lists, had {gets_after}"
        );
        // The refetch dropped the row, and the click never opened the
        // notification page (the guard div swallowed the bubble).
        assert!(
            vcx.debug_bounds("notif-19").is_none(),
            "done row leaves the unread list"
        );
        assert!(
            vcx.debug_bounds("notification-detail").is_none(),
            "the done click must not open the notification page"
        );
    }

    #[gpui::test]
    fn failed_row_done_keeps_the_row_and_re_arms(cx: &mut TestAppContext) {
        // docs/0026 D8: a failed DELETE keeps the row (the refetch
        // still sees the thread) and clears the in-flight guard — the
        // button is clickable again.
        let thread = serde_json::json!({
            "id": "19", "unread": true,
            "subject": {"title": "Fix the flume", "type": "PullRequest"},
            "repository": {"full_name": "acme/one"},
            "reason": "review_requested",
            "updated_at": "2026-09-01T00:00:00Z"
        })
        .to_string();
        let mock = std::sync::Arc::new(InboxMock {
            inbox: std::sync::Mutex::new(vec![thread]),
            delete_failures: std::sync::atomic::AtomicUsize::new(1),
            requests: std::sync::Mutex::new(Vec::new()),
        });
        let (_, vcx) = probe_repo_view(cx, mock.clone());
        vcx.run_until_parked();
        let button = vcx
            .debug_bounds("notif-done-19")
            .expect("row done button renders");
        vcx.simulate_click(button.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
        assert!(
            vcx.debug_bounds("notif-19").is_some(),
            "a failed done keeps the row"
        );
        // Retry round: the guard cleared, this click lands.
        let retry = vcx
            .debug_bounds("notif-done-19")
            .expect("row done button still renders");
        vcx.simulate_click(retry.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
        let deletes = mock
            .requests()
            .iter()
            .filter(|(v, _)| matches!(v, crate::http::Verb::Delete))
            .count();
        assert_eq!(deletes, 2, "failure re-arms the button");
        assert!(
            vcx.debug_bounds("notif-19").is_none(),
            "the retried done drops the row"
        );
    }

    // --- home view (docs/0047 §5) -------------------------------------

    /// A search-node JSON fragment for the home lane's fixtures — the
    /// same field set the ACCOUNT_PULL_REQUESTS_QUERY selects.
    fn search_pr_node(number: i64, updated: &str) -> String {
        format!(
            r##"{{"number":{number},"title":"pr {number}","isDraft":false,"state":"OPEN","author":{{"login":"alice","avatarUrl":""}},"updatedAt":"{updated}","url":"u","statusCheckRollup":{{"state":"SUCCESS"}},"labels":{{"totalCount":1,"nodes":[{{"name":"bug","color":"#f00"}}]}},"reviewRequests":{{"totalCount":0,"nodes":[]}},"repository":{{"nameWithOwner":"acme/one"}}}}"##
        )
    }

    /// A full home-lane response body: `issueCount` total, paging state,
    /// and the given node numbers.
    fn search_body(total: i64, has_next: bool, cursor: Option<&str>, numbers: &[i64]) -> String {
        let nodes: Vec<String> = numbers
            .iter()
            .enumerate()
            .map(|(ix, n)| search_pr_node(*n, &format!("2026-01-{:02}T00:00:00Z", ix + 1)))
            .collect();
        let cursor_json = match cursor {
            Some(c) => format!("\"{c}\""),
            None => "null".to_string(),
        };
        format!(
            r#"{{"data":{{"rateLimit":{{"remaining":4990,"resetAt":"2026-01-01T00:00:00Z"}},"search":{{"issueCount":{total},"pageInfo":{{"hasNextPage":{has_next},"endCursor":{cursor_json}}},"nodes":[{}]}}}}}}"#,
            nodes.join(",")
        )
    }

    /// A landed home SpecResult for one account.
    fn home_result(
        login: &str,
        scope: &str,
        body: String,
        continuation: bool,
    ) -> crate::service::SpecResult {
        crate::service::SpecResult {
            kind: crate::service::Kind::AccountPullRequests,
            ident: Some(format!("{scope}:{login}")),
            status: 200,
            etag: None,
            rate: None,
            body: Some(body),
            network: None,
            cursor: continuation.then(|| "c1".to_string()),
        }
    }

    /// docs/0047 §5 fixture: connected page, personal account "me" first
    /// (Q10), the "acme" org, acme/one on the state, and no repo selected
    /// — the home view is the module's landing surface. The scripted
    /// MockHttp answers the construction-time home fetch (orgs resolve on
    /// the first sync, docs/0047 D3).
    fn home_probe_page(
        cx: &mut gpui::TestAppContext,
        script: Vec<Result<crate::http::HttpResult, crate::http::HttpError>>,
        with_repos: bool,
    ) -> (
        gpui::Entity<GithubPage>,
        gpui::Entity<crate::service::GithubState>,
        &mut gpui::VisualTestContext,
    ) {
        use crate::auth::MockTokenStore;
        use crate::http::mock::MockHttp;
        use crate::http::{Http, SharedToken};
        use crate::service::{GithubCache, GithubState};
        use dashboard_core::nav::Navigator;
        use dashboard_core::paths::AppPaths;

        cx.update(gpui_component::init);
        cx.update(dashboard_core::status::init);
        let state = cx.update(|cx| {
            let client: Arc<dyn Http> = Arc::new(MockHttp::new(script));
            let shared: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
            cx.new(|_| {
                GithubState::new(
                    client,
                    Arc::new(MockTokenStore::with("t0k".into())),
                    shared,
                    refresh,
                    GithubCache::new(AppPaths::init()),
                    DraftStore::at_tmp("home"),
                )
            })
        });
        cx.update(|cx| {
            state.update(cx, |s, cx| {
                s.auth = crate::service::AuthStatus::Connected;
                s.user = Some(gql::User {
                    login: "me".into(),
                    avatar_url: String::new(),
                    id: String::new(),
                });
                s.orgs = crate::gql::orgs_from_values(&[serde_json::json!({"login": "acme"})]);
                if with_repos {
                    let mut r = repo("acme/one", None);
                    r.prs.total_count = 5;
                    r.prs.nodes = vec![pr(1, "union one", Some("alice")), pr(2, "union two", None)];
                    s.repos = vec![r];
                }
                cx.notify();
            });
        });
        let nav = cx.update(|cx| cx.new(|_| Navigator::default()));
        let (page, vcx) = cx
            .add_window_view(|window, cx| GithubPage::new(state.clone(), nav.clone(), window, cx));
        (page, state, vcx)
    }

    #[gpui::test]
    fn card_fires_totals_on_home_shown(cx: &mut TestAppContext) {
        // docs/0051 D4: the card constructs on the home page (nav's
        // initial page) while connected → the totals lane fires once,
        // lands, and re-showing home (a later edge) refetches through
        // the same dedup key.
        use crate::auth::MockTokenStore;
        use crate::card::GithubCard;
        use crate::http::mock::MockHttp;
        use crate::http::{Http, HttpResult, SharedToken};
        use crate::service::{CardTotals, GithubCache, GithubState};
        use dashboard_core::nav::{Navigator, Page};
        use std::sync::Arc;

        cx.update(gpui_component::init);
        cx.update(dashboard_core::status::init);
        let totals_body = || HttpResult {
            status: 200,
            etag: None,
            rate: None,
            body: Some(
                r#"{"data":{"viewer":{"repositories":{"pageInfo":{"hasNextPage":false},
                        "nodes":[{"pullRequests":{"totalCount":4},"issues":{"totalCount":7}}]}}}}"#
                    .to_string(),
            ),
        };
        // docs/0052 D5: the re-show edge also fires the bell's
        // notifications lane (never landed this session) — one REST
        // page lands the empty list.
        let notifications_body = || HttpResult {
            status: 200,
            etag: None,
            rate: None,
            body: Some("[]".to_string()),
        };
        let http = Arc::new(MockHttp::new(vec![
            Ok(totals_body()),
            Ok(totals_body()),
            Ok(notifications_body()),
        ]));
        let (state, nav) = cx.update(|cx| {
            let shared: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let state = cx.new(|_| {
                GithubState::new(
                    http.clone() as Arc<dyn Http>,
                    Arc::new(MockTokenStore::with("t0k".into())),
                    shared,
                    refresh,
                    GithubCache::at_tmp("card-lane"),
                    DraftStore::at_tmp("card-lane-drafts"),
                )
            });
            state.update(cx, |s, _| {
                s.auth = crate::service::AuthStatus::Connected;
            });
            let nav = cx.new(|_| Navigator::default());
            (state, nav)
        });
        assert!(
            cx.update(|cx| nav.read(cx).page.clone()) == Page::Home,
            "nav starts on home"
        );

        let (_card, vcx) = cx
            .add_window_view(|window, cx| GithubCard::new(state.clone(), nav.clone(), window, cx));
        flush(vcx);
        let totals = vcx.update(|_, cx| state.read(cx).card_totals.clone());
        assert!(
            matches!(
                &totals,
                Some(CardTotals {
                    open_prs: 4,
                    open_issues: 7,
                    ..
                })
            ),
            "construction on home fired and landed the totals lane: {totals:?}"
        );

        // Leaving and re-showing home fires the refresh edge.
        vcx.update(|_, cx| {
            nav.update(cx, |nav, cx| nav.go(Page::Settings, cx));
        });
        flush(vcx);
        vcx.update(|_, cx| {
            nav.update(cx, |nav, cx| nav.go(Page::Home, cx));
        });
        flush(vcx);
        assert_eq!(
            http.recorded().len(),
            3,
            "the re-show edge refetched totals and fired the bell's lane"
        );
    }

    fn flush(vcx: &mut gpui::VisualTestContext) {
        vcx.run_until_parked();
        let _ = vcx.debug_bounds("flush");
    }

    #[gpui::test]
    fn home_renders_search_rows_hint_and_load_more(cx: &mut TestAppContext) {
        // Construction fetch (personal account, empty) then the org page-1
        // and its load-more page land through the real transport.
        let ok_body = |body: String| {
            Ok(crate::http::HttpResult {
                status: 200,
                etag: None,
                rate: None,
                body: Some(body),
            })
        };
        let script = vec![
            ok_body(search_body(0, false, None, &[])), // open fetch (me)
            ok_body(search_body(4, true, Some("c1"), &[5, 6])), // acme page 1
            ok_body(search_body(4, false, None, &[7, 8])), // acme page 2
        ];
        let (page, _state, vcx) = home_probe_page(cx, script, true);
        flush(vcx);
        vcx.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.org = Some("acme".into());
                page.fetch_home_prs(Origin::BACKGROUND, window, cx);
            });
        });
        flush(vcx);
        assert!(
            vcx.debug_bounds("pr-acme/one#5").is_some()
                && vcx.debug_bounds("pr-acme/one#6").is_some(),
            "search rows render"
        );
        assert!(
            vcx.debug_bounds("home-prs-hint").is_some(),
            "truncated: hint shows"
        );
        assert!(
            vcx.debug_bounds("home-prs-more").is_some(),
            "truncated: load more shows"
        );
        // Load more appends page 2 and completes the chain.
        let more = vcx.debug_bounds("home-prs-more").unwrap();
        vcx.simulate_click(more.center(), gpui::Modifiers::default());
        flush(vcx);
        assert!(
            vcx.debug_bounds("pr-acme/one#7").is_some()
                && vcx.debug_bounds("pr-acme/one#8").is_some(),
            "page-2 rows append (not replace)"
        );
        assert!(
            vcx.debug_bounds("pr-acme/one#5").is_some(),
            "page-1 rows survive"
        );
        assert!(
            vcx.debug_bounds("home-prs-more").is_none()
                && vcx.debug_bounds("home-prs-hint").is_none(),
            "complete: hint and button retire"
        );
    }

    #[gpui::test]
    fn home_renders_skeletons_until_the_lane_lands(cx: &mut TestAppContext) {
        // docs/0051 D5: with no landed lane and no error the row area is
        // skeletons — never a false "No open pull requests", and the
        // deleted D6 union means seeded repo nodes never render either.
        let script = vec![Err(crate::http::HttpError("probe".into()))];
        let (page, state, vcx) = home_probe_page(cx, script, true);
        flush(vcx);
        // The seeded repo-snapshot rows (the old union source) must NOT
        // back the home view.
        assert!(
            vcx.debug_bounds("pr-acme/one#1").is_none(),
            "D6 union is gone — repo rows never render on home"
        );
        // Simulate the in-flight state: the attempt failed but no error
        // is showing (retry pending), lane still absent → skeletons.
        vcx.update(|_, cx| {
            state.update(cx, |s, cx| {
                s.home_error = None;
                cx.notify();
            });
        });
        flush(vcx);
        assert!(vcx.debug_bounds("pr-acme/one#1").is_none());
        assert!(vcx.debug_bounds("home-prs-retry").is_none());
        assert!(
            vcx.debug_bounds("home-prs-empty").is_none(),
            "no lane and no error is the skeleton branch, not a false empty state"
        );

        // A landed lane replaces the skeletons with real rows.
        vcx.update(|_, cx| {
            state.update(cx, |s, cx| {
                s.apply(home_result(
                    "acme",
                    "org",
                    search_body(6, false, None, &[5]),
                    false,
                ));
                cx.notify();
            });
        });
        flush(vcx);
        assert!(
            vcx.debug_bounds("pr-acme/one#5").is_some(),
            "lane rows land"
        );
        drop(page);
    }

    #[gpui::test]
    fn home_first_fetch_failure_shows_retry_then_recovers(cx: &mut TestAppContext) {
        // No repos for the personal account: a failed first fetch has
        // nothing to fall back on — error + Retry (docs/0047 D3).
        let script = vec![
            Err(crate::http::HttpError("down".into())),
            Ok(crate::http::HttpResult {
                status: 200,
                etag: None,
                rate: None,
                body: Some(search_body(0, false, None, &[])),
            }),
        ];
        let (_page, _state, vcx) = home_probe_page(cx, script, false);
        flush(vcx);
        assert!(
            vcx.debug_bounds("home-prs-error").is_some()
                && vcx.debug_bounds("home-prs-retry").is_some(),
            "failure with no fallback shows error + Retry"
        );

        // Retry rides the same kind; success clears the error and shows
        // the honest empty state (D8).
        let retry = vcx.debug_bounds("home-prs-retry").unwrap();
        vcx.simulate_click(retry.center(), gpui::Modifiers::default());
        flush(vcx);
        assert!(
            vcx.debug_bounds("home-prs-retry").is_none(),
            "recovered: retry retires"
        );
        assert!(
            vcx.debug_bounds("home-prs-empty").is_some(),
            "an empty account is a real result"
        );
    }

    #[gpui::test]
    fn home_account_switch_renders_cached_snapshots(cx: &mut TestAppContext) {
        // The orgs fixture resolves to "acme" (orgs[0]); its snapshot lands
        // through the construction fetch, a "me" snapshot lands manually.
        // Switching paints each cached list immediately (docs/0047 D9)
        // with no extra request.
        let script = vec![Ok(crate::http::HttpResult {
            status: 200,
            etag: None,
            rate: None,
            body: Some(search_body(1, false, None, &[3])),
        })];
        let (page, state, vcx) = home_probe_page(cx, script, true);
        flush(vcx);
        assert!(
            vcx.debug_bounds("pr-acme/one#3").is_some(),
            "acme rows landed at construction"
        );
        vcx.update(|_, cx| {
            state.update(cx, |s, cx| {
                s.apply(home_result(
                    "me",
                    "user",
                    search_body(2, false, None, &[5]),
                    false,
                ));
                cx.notify();
            });
        });
        flush(vcx);
        assert!(
            vcx.debug_bounds("pr-acme/one#3").is_some()
                && vcx.debug_bounds("pr-acme/one#5").is_none(),
            "me snapshot caches without disturbing the view"
        );

        // Switch to the personal account: its cached snapshot paints.
        vcx.update(|_, cx| {
            page.update(cx, |page, cx| {
                page.org = Some("me".into());
                cx.notify();
            });
        });
        flush(vcx);
        assert!(
            vcx.debug_bounds("pr-acme/one#5").is_some()
                && vcx.debug_bounds("pr-acme/one#3").is_none(),
            "me rows replace acme rows from cache"
        );

        // Switch back: acme's snapshot is still cached.
        vcx.update(|_, cx| {
            page.update(cx, |page, cx| {
                page.org = Some("acme".into());
                cx.notify();
            });
        });
        flush(vcx);
        assert!(
            vcx.debug_bounds("pr-acme/one#3").is_some(),
            "acme rows from cache"
        );
    }

    #[test]
    fn home_prs_hint_table() {
        // docs/0047 D4: three truthful states.
        assert_eq!(
            home_prs_hint(2, 5).as_deref(),
            Some("first 2 of 5 open pull requests")
        );
        assert_eq!(home_prs_hint(5, 5), None, "complete: no hint");
        assert_eq!(
            home_prs_hint(1_000, 1_500).as_deref(),
            Some("first 1000 of 1500 — search caps at 1,000"),
            "at the cap the copy names GitHub's ceiling"
        );
        assert_eq!(home_prs_hint(1_000, 1_000), None, "exactly full: complete");
        assert_eq!(home_prs_hint(0, 0), None);
    }

    /// History round-trip (docs/0048): `record_place` trails the visits,
    /// `GithubNav::restore` re-clicks them, `collapse` is the existing
    /// return-to-root, and a foreign payload is ignored — never a panic.
    #[gpui::test]
    fn history_restore_and_collapse_round_trip(cx: &mut TestAppContext) {
        use std::sync::Arc;

        use crate::auth::MockTokenStore;
        use crate::http::mock::MockHttp;
        use crate::http::{Http, SharedToken};
        use crate::service::{GithubCache, GithubState};
        use dashboard_core::module::ModuleNav;
        use dashboard_core::nav::{Navigator, Page};
        use dashboard_core::paths::AppPaths;
        use gpui::Focusable;

        cx.update(gpui_component::init);
        cx.update(dashboard_core::status::init);
        let cx = cx.add_empty_window();
        cx.update(|window, cx| {
            // Scripted failures: select_repo's two specs + the PR page's
            // eager detail fetch. The probe never depends on results.
            let client: Arc<dyn Http> = Arc::new(MockHttp::new(vec![
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
            ]));
            let shared: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let state = cx.new(|_| {
                GithubState::new(
                    client,
                    Arc::new(MockTokenStore::with("t0k".into())),
                    shared,
                    refresh,
                    GithubCache::new(AppPaths::init()),
                    DraftStore::at_tmp("page-6"),
                )
            });
            let nav = cx.new(|_| Navigator::default());
            let page = cx.new(|cx| GithubPage::new(state.clone(), nav.clone(), window, cx));
            let history = crate::page::GithubNav {
                page: page.downgrade(),
            };
            let summary = pr(7, "probe", None);

            // Repo visit recorded.
            page.update(cx, |p, cx| {
                p.select_repo("acme/one".into(), window, cx);
            });
            nav.read_with(cx, |nav, _| {
                assert_eq!(nav.history_entries_last_key(), "github:repo:acme/one:pulls");
            });

            // PR visit recorded; the sub-page is up.
            page.update(cx, |p, cx| {
                p.open_pr("acme/one", &summary, None, window, cx)
            });
            assert!(page.read(cx).pr.is_some());
            nav.read_with(cx, |nav, _| {
                assert_eq!(nav.history_entries_last_key(), "github:pr:acme/one#7");
            });

            // Dismissal (Esc path) records nothing (docs/0048 D4): the
            // trail head stays the PR place.
            page.update(cx, |p, cx| p.close_pr(cx));
            assert!(page.read(cx).pr.is_none());

            // The shell's cross-branch jump (docs/0048 D6): collapse the
            // abandoned module, record Settings. Back then lands on the
            // deep leaf — and restore re-opens it, exactly re-clicking.
            history.collapse(window, cx);
            assert_eq!(state.read(cx).selected, None, "collapse: root");
            nav.update(cx, |nav, cx| nav.go(Page::Settings, cx));
            let route = nav.update(cx, |nav, cx| nav.back(cx)).unwrap();
            assert_eq!(
                route.key(),
                "github:pr:acme/one#7",
                "Back across the branch jump lands on the deep leaf"
            );
            history.restore(route.payload(), window, cx);
            assert!(page.read(cx).pr.is_some(), "Back restored the PR page");
            assert_eq!(state.read(cx).selected.as_deref(), Some("acme/one"));
            // The shell refocuses its root before the restore; the PR
            // page steals focus back (its Esc affordance) — same as a
            // fresh click-through.
            let focused = page
                .read(cx)
                .pr
                .as_ref()
                .map(|pr| pr.read(cx).focus_handle(cx).is_focused(window));
            assert_eq!(focused, Some(true), "restored PR page owns focus");

            // A foreign payload is ignored — never a panic.
            let foreign: Arc<dyn std::any::Any + Send + Sync> = Arc::new(7i32);
            history.restore(&foreign, window, cx);
            assert!(page.read(cx).pr.is_some(), "state untouched");
        });
    }

    /// Back to the repo place (docs/0048 D5) lands ON the repo view: the
    /// open issue/PR sub-page closes with the restore (owner report — the
    /// sub-page stayed mounted, so Back appeared dead). The restore's
    /// re-select still dedups against the landed trail head.
    #[gpui::test]
    fn back_to_the_repo_place_closes_the_open_sub_page(cx: &mut TestAppContext) {
        use std::sync::Arc;

        use crate::auth::MockTokenStore;
        use crate::http::mock::MockHttp;
        use crate::http::{Http, SharedToken};
        use crate::service::{GithubCache, GithubState};
        use dashboard_core::module::ModuleNav;
        use dashboard_core::nav::Navigator;
        use dashboard_core::paths::AppPaths;

        cx.update(gpui_component::init);
        cx.update(dashboard_core::status::init);
        let cx = cx.add_empty_window();
        cx.update(|window, cx| {
            // Scripted failures: select_repo's specs on each activation and
            // the sub-pages' eager detail fetches. The probe never depends
            // on results.
            let client: Arc<dyn Http> = Arc::new(MockHttp::new(vec![
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
            ]));
            let shared: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let state = cx.new(|_| {
                GithubState::new(
                    client,
                    Arc::new(MockTokenStore::with("t0k".into())),
                    shared,
                    refresh,
                    GithubCache::new(AppPaths::init()),
                    DraftStore::at_tmp("page-back-restore"),
                )
            });
            let nav = cx.new(|_| Navigator::default());
            let page = cx.new(|cx| GithubPage::new(state.clone(), nav.clone(), window, cx));
            let history = crate::page::GithubNav {
                page: page.downgrade(),
            };
            let issue = gql::Issue {
                key: "acme/one#9".into(),
                database_id: None,
                number: 9,
                state: None,
                title: "probe".into(),
                author: Some(gql::Author {
                    login: "you".into(),
                    avatar_url: String::new(),
                }),
                updated_at: None,
                url: "https://probe".into(),
                labels: None,
            };
            let summary = pr(7, "probe", None);

            // repo → issue → Back: the restore lands on the repo place and
            // the issue page closes with it.
            page.update(cx, |p, cx| {
                p.select_repo("acme/one".into(), window, cx);
            });
            page.update(cx, |p, cx| {
                p.open_issue("acme/one", &issue, None, window, cx)
            });
            assert!(page.read(cx).issue.is_some());
            let route = nav.update(cx, |nav, cx| nav.back(cx)).unwrap();
            assert_eq!(route.key(), "github:repo:acme/one:pulls");
            history.restore(route.payload(), window, cx);
            let landed = page.read(cx);
            assert!(landed.issue.is_none(), "Back closed the issue page");
            assert!(landed.pr.is_none());
            assert_eq!(
                state.read(cx).selected.as_deref(),
                Some("acme/one"),
                "the repo selection survives the restore"
            );
            nav.read_with(cx, |nav, _| {
                assert_eq!(
                    nav.history_entries_last_key(),
                    "github:repo:acme/one:pulls",
                    "the restore's re-select dedups against the landed head"
                );
            });

            // The same round trip from the PR page.
            page.update(cx, |p, cx| {
                p.open_pr("acme/one", &summary, None, window, cx)
            });
            assert!(page.read(cx).pr.is_some());
            let route = nav.update(cx, |nav, cx| nav.back(cx)).unwrap();
            assert_eq!(route.key(), "github:repo:acme/one:pulls");
            history.restore(route.payload(), window, cx);
            let landed = page.read(cx);
            assert!(landed.pr.is_none(), "Back closed the PR page");
            assert!(landed.issue.is_none());
        });
    }

    #[gpui::test]
    fn repo_tab_switches_are_trail_places(cx: &mut TestAppContext) {
        use std::sync::Arc;

        use crate::auth::MockTokenStore;
        use crate::http::mock::MockHttp;
        use crate::http::{Http, SharedToken};
        use crate::service::{GithubCache, GithubState};
        use dashboard_core::module::ModuleNav;
        use dashboard_core::nav::Navigator;
        use dashboard_core::paths::AppPaths;

        cx.update(gpui_component::init);
        cx.update(dashboard_core::status::init);
        let cx = cx.add_empty_window();
        cx.update(|window, cx| {
            // Scripted failures: the tab-switch lane offers never land.
            // The probe is about the trail, not the rows.
            let client: Arc<dyn Http> = Arc::new(MockHttp::new(vec![
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
                Err(crate::http::HttpError("probe".into())),
            ]));
            let shared: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let refresh: SharedToken = Arc::new(std::sync::RwLock::new(None));
            let state = cx.new(|_| {
                GithubState::new(
                    client,
                    Arc::new(MockTokenStore::with("t0k".into())),
                    shared,
                    refresh,
                    GithubCache::new(AppPaths::init()),
                    DraftStore::at_tmp("page-tab-nav"),
                )
            });
            let nav = cx.new(|_| Navigator::default());
            let page = cx.new(|cx| GithubPage::new(state.clone(), nav.clone(), window, cx));
            let history = crate::page::GithubNav {
                page: page.downgrade(),
            };

            // Selection records the pulls place (the reset lands first).
            page.update(cx, |p, cx| {
                p.select_repo("acme/one".into(), window, cx);
            });
            nav.read_with(cx, |nav, _| {
                assert_eq!(nav.history_entries_last_key(), "github:repo:acme/one:pulls");
            });

            // Each switch is its own visit, keyed by the tab.
            page.update(cx, |p, cx| p.switch_repo_tab(RepoTab::Issues, window, cx));
            page.update(cx, |p, cx| {
                p.switch_repo_tab(RepoTab::Notifications, window, cx)
            });
            nav.read_with(cx, |nav, _| {
                assert_eq!(
                    nav.history_entries_last_key(),
                    "github:repo:acme/one:notifs"
                );
            });

            // Back re-enters the exact lane through the live switch path.
            let route = nav.update(cx, |nav, cx| nav.back(cx)).unwrap();
            assert_eq!(route.key(), "github:repo:acme/one:issues");
            history.restore(route.payload(), window, cx);
            assert_eq!(page.read(cx).tab, RepoTab::Issues);
            nav.read_with(cx, |nav, _| {
                assert_eq!(
                    nav.history_entries_last_key(),
                    "github:repo:acme/one:issues",
                    "the restore's switch record dedups against the landed head"
                );
            });

            // Forward returns to notifications; the browser rule holds.
            let route = nav.update(cx, |nav, cx| nav.forward(cx)).unwrap();
            assert_eq!(route.key(), "github:repo:acme/one:notifs");
            history.restore(route.payload(), window, cx);
            assert_eq!(page.read(cx).tab, RepoTab::Notifications);

            // A fresh visit after Back still truncates the forward tail.
            let route = nav.update(cx, |nav, cx| nav.back(cx)).unwrap();
            assert_eq!(route.key(), "github:repo:acme/one:issues");
            page.update(cx, |p, cx| p.switch_repo_tab(RepoTab::Pulls, window, cx));
            nav.read_with(cx, |nav, _| {
                assert_eq!(nav.history_entries_last_key(), "github:repo:acme/one:pulls");
                assert!(
                    !nav.can_forward(),
                    "the fresh visit killed the notifs forward branch"
                );
            });
        });
    }
}
