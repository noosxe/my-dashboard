//! What a budget-owning module hands the usage page live (docs/0082
//! D13, §2.1 Q10). The page reads stored records; two things it needs
//! are not records:
//!
//! - **The per-budget pause** (`rate_limited_until`, docs/0050 D4 as
//!   amended by Q10): the module registers a reader, and the page calls
//!   it when it stamps its freeze, so the cards show a budget as
//!   Exhausted while its pause is in force.
//! - **"Open the Budget view"**: the status-bar chip is the button that
//!   opens the page on Budget (D13), even when the page is already open
//!   on Requests.

use std::sync::Arc;

use gpui::{App, Entity, Global};

use crate::nav::{Navigator, Page};

/// Each budget's pause deadline, in epoch ms; `None` while that budget
/// isn't paused.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct BudgetPauses {
    pub rest_until_ms: Option<u64>,
    pub graphql_until_ms: Option<u64>,
}

/// Reads the live pauses on the UI thread.
pub type PauseReader = Arc<dyn Fn(&App) -> BudgetPauses>;

struct Pauses(PauseReader);

impl Global for Pauses {}

/// Register the module's pause reader (the GitHub module, at init).
pub fn set_pause_reader(cx: &mut App, reader: PauseReader) {
    cx.set_global(Pauses(reader));
}

/// The pauses as they stand; none when no module registered a reader.
pub fn pauses(cx: &App) -> BudgetPauses {
    cx.try_global::<Pauses>()
        .map(|p| p.0.clone())
        .map_or_else(BudgetPauses::default, |read| read(cx))
}

/// Bumped by [`open_budget_view`]; the page observes it.
#[derive(Default)]
pub(super) struct BudgetViewRequest(u64);

impl Global for BudgetViewRequest {}

/// Navigate to the usage page on its Budget view (D13). A page created
/// by this navigation opens on Budget anyway; an existing one switches
/// to it, keeping its freeze and range (D1).
pub fn open_budget_view(nav: &Entity<Navigator>, cx: &mut App) {
    cx.default_global::<BudgetViewRequest>().0 += 1;
    nav.update(cx, |nav, cx| nav.go(Page::Usage, cx));
}
