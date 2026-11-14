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
    // A quiet secondary (Disable auto-merge, Remove from queue) is sized to
    // its label, so `⋯` needs a spacer to keep the row's trailing edge.
    let quiet_only = !no_primary
        && primaries
            .iter()
            .all(|p| matches!(p, Primary::DisableAutoMerge | Primary::RemoveFromQueue));
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
                Primary::EnableAutoMerge => auto_merge_button(page, busy, pending, cx),
                Primary::MergeWhenReady | Primary::MergeWhenReadyBlocked => queue_button(
                    p == Primary::MergeWhenReady,
                    busy,
                    pending,
                    blocked_tooltip.clone(),
                    cx,
                ),
                Primary::DisableAutoMerge => state_button(
                    "pr-automerge-disable",
                    "Disable auto-merge",
                    "Turn auto-merge off for this pull request",
                    pending == Some(PendingKind::DisableAutoMerge),
                    false,
                    busy,
                    cx.listener(|this, _, window, cx| this.disable_auto_merge_pr(window, cx)),
                ),
                Primary::RemoveFromQueue => state_button(
                    "pr-dequeue",
                    "Remove from queue",
                    "Remove this pull request from the merge queue",
                    pending == Some(PendingKind::Dequeue),
                    false,
                    busy,
                    cx.listener(|this, _, window, cx| this.dequeue_pr(window, cx)),
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
                Primary::DeleteBranch => state_button(
                    "pr-delete-branch",
                    "Delete branch",
                    "Delete the head branch",
                    pending == Some(PendingKind::DeleteBranch),
                    false,
                    busy,
                    cx.listener(|this, _, window, cx| {
                        this.open_merge_confirm(ConfirmKind::DeleteBranch, window, cx)
                    }),
                ),
            }))
            .when(quiet_only && !menu.is_empty(), |row| {
                row.child(div().flex_1().min_w_0())
            })
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
            | PendingKind::DeleteBranch
            | PendingKind::EnableAutoMerge
            | PendingKind::DisableAutoMerge
            | PendingKind::Enqueue
            | PendingKind::Dequeue
    )
}

/// A one-click state flip. The accent one (Ready for review) fills the
/// row's free width. The quiet ones of a terminal PR (Reopen, Delete
/// branch) are small and sized to their label, side by side as in the
/// docs/0083 mockup: at half width each "Reopen pull request" truncated.
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
        .disabled(busy)
        .tooltip(if busy { "Working" } else { tooltip })
        .on_click(on_click);
    if primary {
        return div()
            .flex_1()
            .min_w_0()
            .child(button.small().primary().w_full())
            .into_any_element();
    }
    div()
        .flex_none()
        .child(button.xsmall().outline())
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
    let label: SharedString = if pending == Some(PendingKind::Merge) {
        "Working…".into()
    } else {
        page.selected_method()
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
    split_button(
        page,
        Split {
            id: "pr-merge",
            split_id: "pr-merge-split",
            label,
            tooltip,
            enabled: enabled && !busy,
            kind: ConfirmKind::Merge,
            caret: true,
        },
        busy,
        cx,
    )
}

/// "Enable auto-merge ▾" (#774): the caret picks the method, except on a
/// base with a merge queue, where the queue sets it.
fn auto_merge_button(
    page: &PrDetailPage,
    busy: bool,
    pending: Option<PendingKind>,
    cx: &Context<PrDetailPage>,
) -> AnyElement {
    let working = pending == Some(PendingKind::EnableAutoMerge);
    let queued_base = page.detail(cx).is_some_and(|d| d.queueing.queue_enabled);
    split_button(
        page,
        Split {
            id: "pr-automerge",
            split_id: "pr-automerge-split",
            label: if working {
                "Working…"
            } else {
                "Enable auto-merge"
            }
            .into(),
            tooltip: if busy {
                "Working"
            } else {
                "Merge this pull request once the requirements pass"
            }
            .into(),
            enabled: !busy,
            kind: ConfirmKind::EnableAutoMerge,
            caret: !queued_base,
        },
        busy,
        cx,
    )
}

/// "Merge when ready" (#775): the base branch has a merge queue, which sets
/// the method, so there is no caret. Disabled (tooltip: the verdict) when
/// the Merge gate doesn't pass and the viewer can't enable auto-merge.
fn queue_button(
    enabled: bool,
    busy: bool,
    pending: Option<PendingKind>,
    blocked_tooltip: SharedString,
    cx: &Context<PrDetailPage>,
) -> AnyElement {
    let label = if pending == Some(PendingKind::Enqueue) {
        "Working…"
    } else {
        "Merge when ready"
    };
    let tooltip: SharedString = if busy {
        "Working".into()
    } else if enabled {
        "Add this pull request to the merge queue".into()
    } else {
        blocked_tooltip
    };
    div()
        .flex_1()
        .min_w_0()
        .child(
            Button::new("pr-queue")
                .debug_selector(|| "pr-queue".to_string())
                .label(label)
                .primary()
                .small()
                .w_full()
                .disabled(!enabled || busy)
                .tooltip(tooltip)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.open_merge_confirm(ConfirmKind::MergeWhenReady, window, cx)
                })),
        )
        .into_any_element()
}

/// One split button: an action that opens its confirm, plus (with two or
/// more allowed methods and `caret`) the method menu.
struct Split {
    id: &'static str,
    split_id: &'static str,
    label: SharedString,
    tooltip: SharedString,
    enabled: bool,
    kind: ConfirmKind,
    caret: bool,
}

fn split_button(
    page: &PrDetailPage,
    split: Split,
    busy: bool,
    cx: &Context<PrDetailPage>,
) -> AnyElement {
    let Split {
        id,
        split_id,
        label,
        tooltip,
        enabled,
        kind,
        caret,
    } = split;
    let action = Button::new(id)
        .debug_selector(move || id.to_string())
        .label(label)
        .primary()
        .small()
        .disabled(!enabled)
        .tooltip(tooltip)
        .on_click(
            cx.listener(move |this, _, window, cx| this.open_merge_confirm(kind, window, cx)),
        );
    if page.methods.len() < 2 || !caret {
        return div()
            .flex_1()
            .min_w_0()
            .child(action.w_full())
            .into_any_element();
    }
    let weak = cx.entity().downgrade();
    let methods = page.methods.clone();
    let current = page.selected_method();
    div()
        .flex_1()
        .min_w_0()
        .debug_selector(|| "pr-merge-method".to_string())
        .child(
            DropdownButton::new(split_id)
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
