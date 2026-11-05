//! The Requests view (docs/0082 §6, D10): the owner as the
//! dashboard's developer, from this app's records (0082 §4). Its
//! surfaces land with #698; until then the body is the neutral
//! placeholder.

use gpui::App;
use gpui::prelude::*;

use super::states;

pub(super) fn body(cx: &App) -> impl IntoElement {
    states::rebuilding("usage-requests-body", cx)
}
