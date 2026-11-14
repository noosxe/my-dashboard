//! The action row (docs/0083 D4, D5): the primary control for the PR's
//! state and the `⋯` menu. The rules live in `actions.rs`; this draws
//! what they allow. An open confirm replaces the row.

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, Styled as _, Window, div, prelude::FluentBuilder as _,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _, DropdownButton},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    spinner::Spinner,
};

use super::actions::{Facts, MenuItem, Primary, shown_menu, shown_primaries};
use crate::gql::PrDetail;
use crate::pr_page::merge_confirm::ConfirmKind;
use crate::pr_page::{MergeMethod, PrDetailPage};
use crate::service::PendingKind;

/// The row, or the open confirm in its place; `None` when the viewer has
/// nothing to do (the verdict and gates still show).
pub(super) fn action_row(
    page: &PrDetailPage,
    d: &PrDetail,
    blocked_tooltip: SharedString,
    cx: &Context<PrDetailPage>,
) -> Option<AnyElement> {
    if let Some(kind) = page.merge_confirm.current() {
        return Some(page.render_merge_confirm(kind, d, cx));
    }
    let facts = Facts::of(d, page.methods.is_empty());
    let primaries = shown_primaries(&facts);
    let menu = shown_menu(&facts);
    if primaries.is_empty() && menu.is_empty() {
        return None;
    }
    let pending = page.store_pending(cx);
    let busy = page.writes_busy(cx);
    let no_primary = primaries.is_empty();
    Some(
        h_flex()
            .id("pr-rail-actions")
            .w_full()
            .gap_2()
            .items_center()
            .when(pending.is_some_and(shows_row_spinner), |row| {
                row.child(Spinner::new().small())
            })
            .children(primaries.into_iter().map(|p| match p {
                Primary::Merge | Primary::MergeBlocked => merge_button(
                    page,
                    p == Primary::Merge,
                    busy,
                    pending,
                    blocked_tooltip.clone(),
                    cx,
                ),
                Primary::ReadyForReview => state_button(
                    "pr-ready",
                    "Ready for review",
                    "Mark this pull request ready for review",
                    pending == Some(PendingKind::ReadyForReview),
                    true,
                    busy,
                    cx.listener(|this, _, window, cx| this.ready_for_review_pr(window, cx)),
                ),
                Primary::Reopen => state_button(
                    "pr-reopen",
                    "Reopen pull request",
                    "Reopen this pull request",
                    pending == Some(PendingKind::Reopen),
                    false,
                    busy,
                    cx.listener(|this, _, window, cx| this.reopen_pr(window, cx)),
                ),
                // Not wired yet (#773–#775): `shown_primaries` drops them.
                _ => div().into_any_element(),
            }))
            .when(!menu.is_empty(), |row| row.child(more_menu(menu, busy, cx)))
            // A row with no primary (a viewer who can't mark a draft ready)
            // keeps `⋯` at the trailing edge.
            .when(no_primary, |row| row.justify_end())
            .into_any_element(),
    )
}

/// Whether a pending write puts the spinner at the row's start.
fn shows_row_spinner(kind: PendingKind) -> bool {
    matches!(
        kind,
        PendingKind::Merge
            | PendingKind::Close
            | PendingKind::ReadyForReview
            | PendingKind::ConvertToDraft
            | PendingKind::Reopen
    )
}

/// A one-click state flip (Ready for review, Reopen): fills the row's
/// free width, so a second control (Delete branch, #773) can sit beside
/// it. `primary` picks the accent style; a closed PR's Reopen is quiet.
#[allow(clippy::too_many_arguments)]
fn state_button(
    id: &'static str,
    label: &'static str,
    tooltip: &'static str,
    working: bool,
    primary: bool,
    busy: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let button = Button::new(id)
        .debug_selector(move || id.to_string())
        .label(if working { "Working…" } else { label })
        .small()
        .disabled(busy)
        .tooltip(if busy { "Working" } else { tooltip })
        .on_click(on_click);
    let button = if primary {
        button.primary()
    } else {
        button.outline()
    };
    div()
        .flex_1()
        .min_w_0()
        .child(button.w_full())
        .into_any_element()
}

/// "Squash and merge ▾": the method lives in the caret (D4), which stays
/// usable while the action half is disabled. With one allowed method it is
/// a plain button.
fn merge_button(
    page: &PrDetailPage,
    enabled: bool,
    busy: bool,
    pending: Option<PendingKind>,
    blocked_tooltip: SharedString,
    cx: &Context<PrDetailPage>,
) -> AnyElement {
    let method = page.selected_method();
    let label: SharedString = if pending == Some(PendingKind::Merge) {
        "Working…".into()
    } else {
        method
            .map_or("Merge pull request", MergeMethod::verb)
            .into()
    };
    let tooltip: SharedString = if busy {
        "Working".into()
    } else if enabled {
        "Merge this pull request".into()
    } else {
        blocked_tooltip
    };
    let action = Button::new("pr-merge")
        .debug_selector(|| "pr-merge".to_string())
        .label(label)
        .primary()
        .small()
        .disabled(!enabled || busy)
        .tooltip(tooltip)
        .on_click(cx.listener(|this, _, window, cx| {
            this.open_merge_confirm(ConfirmKind::Merge, window, cx)
        }));
    if page.methods.len() < 2 {
        return div()
            .flex_1()
            .min_w_0()
            .child(action.w_full())
            .into_any_element();
    }
    let weak = cx.entity().downgrade();
    let methods = page.methods.clone();
    let current = method;
    div()
        .flex_1()
        .min_w_0()
        .debug_selector(|| "pr-merge-method".to_string())
        .child(
            DropdownButton::new("pr-merge-split")
                .w_full()
                .button(action.flex_1().min_w_0())
                .small()
                .disabled(busy)
                .menu_accessibility_label("Choose merge method")
                .dropdown_menu(move |menu, _, _| {
                    let mut next = menu;
                    for m in methods.iter().copied() {
                        let item_weak = weak.clone();
                        let probe = format!("pr-merge-method-{}", m.as_str().to_lowercase());
                        next = next.item(
                            PopupMenuItem::element(move |_, _| {
                                let key = probe.clone();
                                h_flex()
                                    .debug_selector(move || key.clone())
                                    .child(m.label())
                            })
                            .checked(current == Some(m))
                            .on_click(move |_, window, cx| {
                                let item_weak = item_weak.clone();
                                window.defer(cx, move |_, cx| {
                                    let _ = item_weak.update(cx, |page, cx| {
                                        page.merge_method = Some(m);
                                        cx.notify();
                                    });
                                });
                            }),
                        );
                    }
                    next
                }),
        )
        .into_any_element()
}

/// The `⋯` button and its menu (D5). Close keeps the `pr-close` key.
fn more_menu(items: Vec<MenuItem>, busy: bool, cx: &Context<PrDetailPage>) -> AnyElement {
    let weak = cx.entity().downgrade();
    div()
        .flex_none()
        .debug_selector(|| "pr-more".to_string())
        .child(
            Button::new("pr-more")
                .icon(Icon::new(IconName::Ellipsis))
                .ghost()
                .small()
                .accessibility_label("More pull request actions")
                .tooltip("More pull request actions")
                .disabled(busy)
                .dropdown_menu(move |menu, _, _| {
                    let mut next = menu;
                    for item in items.iter().copied() {
                        match item {
                            MenuItem::Separator => next = next.separator(),
                            MenuItem::Close => {
                                let item_weak = weak.clone();
                                next = next.item(
                                    PopupMenuItem::element(|_, cx| {
                                        // Probe-visible row (0010 D10). The upstream
                                        // MenuItem of a custom element has no a11y
                                        // name; the receipts click it by this key.
                                        h_flex()
                                            .debug_selector(|| "pr-close".to_string())
                                            .text_color(cx.theme().danger)
                                            .child("Close pull request")
                                    })
                                    .on_click(
                                        move |_, window, cx| {
                                            let item_weak = item_weak.clone();
                                            window.defer(cx, move |window, cx| {
                                                let _ = item_weak.update(cx, |page, cx| {
                                                    page.open_merge_confirm(
                                                        ConfirmKind::Close,
                                                        window,
                                                        cx,
                                                    )
                                                });
                                            });
                                        },
                                    ),
                                );
                            }
                            MenuItem::ConvertToDraft => {
                                let item_weak = weak.clone();
                                next = next.item(
                                    PopupMenuItem::element(|_, _| {
                                        h_flex()
                                            .debug_selector(|| "pr-more-draft".to_string())
                                            .child("Convert to draft")
                                    })
                                    .on_click(
                                        move |_, window, cx| {
                                            let item_weak = item_weak.clone();
                                            window.defer(cx, move |window, cx| {
                                                let _ = item_weak.update(cx, |page, cx| {
                                                    page.open_merge_confirm(
                                                        ConfirmKind::ConvertToDraft,
                                                        window,
                                                        cx,
                                                    )
                                                });
                                            });
                                        },
                                    ),
                                );
                            }
                        }
                    }
                    next
                }),
        )
        .into_any_element()
}
