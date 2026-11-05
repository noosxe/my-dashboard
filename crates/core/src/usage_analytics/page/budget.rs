//! The Budget view (docs/0082 §5, D4–D9): the owner as a user, from
//! GitHub's reporting (0082 §4). Its surfaces land with #697; until
//! then the body is the neutral placeholder.

use gpui::App;
use gpui::prelude::*;

use super::states;

pub(super) fn body(cx: &App) -> impl IntoElement {
    states::rebuilding("usage-budget-body", cx)
}
