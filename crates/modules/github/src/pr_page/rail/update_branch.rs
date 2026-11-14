//! The Update split button in the Mergeability gate row and its rebase
//! confirm (docs/0067 D1, D4, docs/0083 D3). The main click updates with
//! a merge commit; the caret offers both methods, and rebase confirms
//! first. Built from the upstream `DropdownButton`, which gives the main
//! half its own click and the caret a name.

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    Styled as _, div, prelude::FluentBuilder as _,
};
use gpui_component::{
    ActiveTheme, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _, DropdownButton},
    h_flex,
    menu::PopupMenuItem,
    spinner::Spinner,
    v_flex,
};

use crate::gql::PrDetail;
use crate::pr_page::{PrDetailPage, UpdateBranchMethod};
use crate::service::PendingKind;

/// The split button; `busy` disables both halves.
pub(super) fn update_split(
    page: &PrDetailPage,
    busy: bool,
    cx: &Context<PrDetailPage>,
) -> AnyElement {
    let weak = cx.entity().downgrade();
    h_flex()
        .id("pr-update-branch-row")
        .debug_selector(|| "pr-update-branch-row".to_string())
        .flex_none()
        .gap_1()
        .items_center()
        .when(
            page.store_pending(cx) == Some(PendingKind::UpdateBranch),
            |row| row.child(Spinner::new().small()),
        )
        .child(
            DropdownButton::new("pr-update-split")
                .button(
                    Button::new("pr-update-branch")
                        .debug_selector(|| "pr-update-branch".to_string())
                        .label("Update")
                        .tooltip("Bring this branch up to date with the base branch")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.update_branch_pr(UpdateBranchMethod::MergeCommit, window, cx)
                        })),
                )
                .small()
                .disabled(busy)
                .menu_accessibility_label("Choose how to update the branch")
                .dropdown_menu(move |menu, _, _| {
                    let mut next = menu;
                    for method in [UpdateBranchMethod::MergeCommit, UpdateBranchMethod::Rebase] {
                        let item_weak = weak.clone();
                        let probe = format!("pr-update-branch-{}", method.slug());
                        next = next.item(
                            PopupMenuItem::element(move |_, _| {
                                // Probe-visible row (0010 D10): the plain
                                // Item variant is not.
                                let key = probe.clone();
                                h_flex()
                                    .debug_selector(move || key.clone())
                                    .child(method.menu_label())
                            })
                            .disabled(busy)
                            .on_click(move |_, window, cx| {
                                // The click dispatches inside the window's
                                // update: defer to the end of the effect
                                // cycle, then post from the live context.
                                let item_weak = item_weak.clone();
                                window.defer(cx, move |window, cx| {
                                    let _ = item_weak.update(cx, |page, cx| match method {
                                        UpdateBranchMethod::MergeCommit => {
                                            page.update_branch_pr(method, window, cx)
                                        }
                                        UpdateBranchMethod::Rebase => {
                                            page.arm_update_rebase(window, cx)
                                        }
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

/// The rebase confirm (docs/0067 D4), opened under the Mergeability row:
/// the force-push warning with Rebase branch / Cancel.
pub(super) fn rebase_confirm(d: &PrDetail, cx: &Context<PrDetailPage>) -> AnyElement {
    v_flex()
        .id("pr-update-rebase-confirm")
        .debug_selector(|| "pr-update-rebase-confirm".to_string())
        .gap_2()
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().warning)
                .child(format!(
                    "Rebase {} onto {} and force-push the branch? Anyone \
                     working off it must re-pull.",
                    d.head_ref.as_deref().unwrap_or_default(),
                    d.base_ref.as_deref().unwrap_or_default()
                )),
        )
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("pr-update-rebase-go")
                        .debug_selector(|| "pr-update-rebase-go".to_string())
                        .label("Rebase branch")
                        .danger()
                        .small()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.update_branch_pr(UpdateBranchMethod::Rebase, window, cx)
                        })),
                )
                .child(
                    Button::new("pr-update-rebase-cancel")
                        .debug_selector(|| "pr-update-rebase-cancel".to_string())
                        .label("Cancel")
                        .ghost()
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_update_rebase(cx))),
                ),
        )
        .into_any_element()
}
