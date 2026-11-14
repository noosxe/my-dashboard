//! The confirm in front of Merge and Close (docs/0016 D5, #738).
//!
//! Clicking either rail button opens an inline confirm instead of
//! sending the mutation; only the confirming button sends it. The state
//! machine ([`MergeConfirm`]) is pure and independent of where the
//! buttons sit (the sidebar redesign moves Close into a `⋯` menu — its
//! handler just calls [`PrDetailPage::open_merge_confirm`]). The gates
//! in `merge_gate.rs` stay authoritative at both steps: a confirm only
//! opens while its gate is open, and closes the moment the gate shuts
//! (refetch, state change, busy op).

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window, accesskit, div,
};
use gpui_component::{
    ActiveTheme, Sizable as _,
    button::{Button, ButtonVariants},
    h_flex, v_flex,
};

use super::merge_gate::{
    close_enabled, close_visible, convert_enabled, merge_enabled, merge_visible,
};
use super::{MergeMethod, PrDetailPage};
use crate::gql;

/// Which action the open confirm guards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfirmKind {
    Merge,
    Close,
    /// docs/0083 D5 (#772): reverses one click away, but it quiets
    /// reviewers, so it asks first.
    ConvertToDraft,
}

/// Whether each action's gate is open right now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Gates {
    pub merge: bool,
    pub close: bool,
    pub convert: bool,
}

impl Gates {
    fn allows(self, kind: ConfirmKind) -> bool {
        match kind {
            ConfirmKind::Merge => self.merge,
            ConfirmKind::Close => self.close,
            ConfirmKind::ConvertToDraft => self.convert,
        }
    }
}

/// At most one Merge/Close confirm is open.
#[derive(Debug, Default)]
pub(crate) struct MergeConfirm {
    open: Option<ConfirmKind>,
}

impl MergeConfirm {
    pub(crate) fn current(&self) -> Option<ConfirmKind> {
        self.open
    }

    /// Open `kind`'s confirm (replacing any other) — only when its gate
    /// is open. Returns whether it opened.
    pub(crate) fn open(&mut self, kind: ConfirmKind, gates: Gates) -> bool {
        if !gates.allows(kind) {
            return false;
        }
        self.open = Some(kind);
        true
    }

    /// Cancel / Esc. Returns whether a confirm was open.
    pub(crate) fn cancel(&mut self) -> bool {
        self.open.take().is_some()
    }

    /// The confirming click: yields the action to send, or `None` when
    /// nothing is open or the gate shut meanwhile. Always closes.
    pub(crate) fn confirm(&mut self, gates: Gates) -> Option<ConfirmKind> {
        self.open.take().filter(|k| gates.allows(*k))
    }

    /// Auto-dismiss when the open confirm's gate shut. Returns whether
    /// it dismissed.
    pub(crate) fn sync(&mut self, gates: Gates) -> bool {
        match self.open {
            Some(k) if !gates.allows(k) => {
                self.open = None;
                true
            }
            _ => false,
        }
    }
}

/// The merge prompt: "Squash and merge feature/x into main?".
pub(crate) fn merge_prompt(method: MergeMethod, head: &str, base: &str) -> String {
    let verb = match method {
        MergeMethod::Merge => "Merge",
        MergeMethod::Squash => "Squash and merge",
        MergeMethod::Rebase => "Rebase and merge",
    };
    format!("{verb} {head} into {base}?")
}

pub(crate) const CONVERT_PROMPT: &str =
    "Convert to draft? Reviewers won't be notified of new pushes until it is ready again.";

pub(crate) const CLOSE_PROMPT: &str =
    "Close this pull request without merging? It can be reopened later.";

impl PrDetailPage {
    /// Both gates, as of the current detail and busy state.
    pub(super) fn merge_confirm_gates(&self, cx: &Context<Self>) -> Gates {
        let Some(d) = self.detail(cx) else {
            return Gates::default();
        };
        let busy = self.busy.is_some() || self.store_pending(cx).is_some();
        Gates {
            merge: merge_visible(d.viewer_permission.as_deref())
                && merge_enabled(
                    d.state.as_deref(),
                    d.draft,
                    d.mergeable.as_deref(),
                    d.merge_state_status.as_deref(),
                    busy,
                    self.methods.is_empty(),
                ),
            close: close_visible(d.viewer_can_close)
                && close_enabled(d.state.as_deref(), d.id.is_empty(), busy),
            convert: convert_enabled(
                d.state.as_deref(),
                d.draft,
                d.viewer_can_update,
                d.id.is_empty(),
                busy,
            ),
        }
    }

    /// Open the Merge/Close confirm. One confirm at a time — the rebase
    /// confirm included.
    pub(super) fn open_merge_confirm(
        &mut self,
        kind: ConfirmKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let gates = self.merge_confirm_gates(cx);
        if self.merge_confirm.open(kind, gates) {
            self.update_rebase_confirm = false;
            // The clicked button leaves the tree; keep Esc working.
            self.refocus_page(window, cx);
            cx.notify();
        }
    }

    pub(super) fn cancel_merge_confirm(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let was_open = self.merge_confirm.cancel();
        if was_open {
            self.refocus_page(window, cx);
            cx.notify();
        }
        was_open
    }

    /// The confirming click: re-checks the gates, then sends.
    pub(super) fn confirm_merge_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let gates = self.merge_confirm_gates(cx);
        let kind = self.merge_confirm.confirm(gates);
        self.refocus_page(window, cx);
        cx.notify();
        match kind {
            Some(ConfirmKind::Merge) => self.merge_pr(window, cx),
            Some(ConfirmKind::Close) => self.close_pr(window, cx),
            Some(ConfirmKind::ConvertToDraft) => self.convert_to_draft_pr(window, cx),
            None => {}
        }
    }

    /// Close the confirm when its gate shut; called every render so a
    /// refetch or busy op can't leave a stale confirm armed.
    pub(super) fn sync_merge_confirm(&mut self, cx: &Context<Self>) {
        let gates = self.merge_confirm_gates(cx);
        self.merge_confirm.sync(gates);
    }

    pub(super) fn render_merge_confirm(
        &self,
        kind: ConfirmKind,
        d: &gql::PrDetail,
        cx: &Context<Self>,
    ) -> AnyElement {
        let (prompt, go_label) = match kind {
            ConfirmKind::Merge => (
                merge_prompt(
                    self.selected_method().unwrap_or(MergeMethod::Merge),
                    d.head_ref.as_deref().unwrap_or_default(),
                    d.base_ref.as_deref().unwrap_or_default(),
                ),
                "Confirm merge",
            ),
            ConfirmKind::Close => (CLOSE_PROMPT.to_string(), "Close without merging"),
            ConfirmKind::ConvertToDraft => (CONVERT_PROMPT.to_string(), "Convert to draft"),
        };
        let (id, go_id, cancel_id) = match kind {
            ConfirmKind::Merge => (
                "pr-merge-confirm",
                "pr-merge-confirm-go",
                "pr-merge-confirm-cancel",
            ),
            ConfirmKind::Close => (
                "pr-close-confirm",
                "pr-close-confirm-go",
                "pr-close-confirm-cancel",
            ),
            ConfirmKind::ConvertToDraft => (
                "pr-convert-confirm",
                "pr-convert-confirm-go",
                "pr-convert-confirm-cancel",
            ),
        };
        let go = Button::new(go_id)
            .debug_selector(move || go_id.to_string())
            .label(go_label)
            .small()
            .on_click(cx.listener(|this, _, window, cx| this.confirm_merge_confirm(window, cx)));
        let go = match kind {
            ConfirmKind::Merge => go.primary(),
            ConfirmKind::Close => go.danger(),
            ConfirmKind::ConvertToDraft => go,
        };
        v_flex()
            .id(id)
            .debug_selector(move || id.to_string())
            .role(accesskit::Role::Group)
            .aria_label(prompt.clone())
            .gap_2()
            .child(div().text_sm().text_color(cx.theme().warning).child(prompt))
            .child(
                h_flex().gap_2().child(go).child(
                    Button::new(cancel_id)
                        .debug_selector(move || cancel_id.to_string())
                        .label("Cancel")
                        .ghost()
                        .small()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.cancel_merge_confirm(window, cx);
                        })),
                ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPEN: Gates = Gates {
        merge: true,
        close: true,
        convert: true,
    };
    const SHUT: Gates = Gates {
        merge: false,
        close: false,
        convert: false,
    };

    #[test]
    fn opens_only_through_an_open_gate() {
        let mut c = MergeConfirm::default();
        assert!(!c.open(ConfirmKind::Merge, SHUT));
        assert_eq!(c.current(), None);
        assert!(c.open(ConfirmKind::Merge, OPEN));
        assert_eq!(c.current(), Some(ConfirmKind::Merge));
        let mut c = MergeConfirm::default();
        let only_merge = Gates {
            merge: true,
            close: false,
            convert: true,
        };
        assert!(!c.open(ConfirmKind::Close, only_merge));
    }

    #[test]
    fn cancel_backs_out_without_sending() {
        let mut c = MergeConfirm::default();
        assert!(!c.cancel());
        c.open(ConfirmKind::Close, OPEN);
        assert!(c.cancel());
        assert_eq!(c.current(), None);
        assert_eq!(c.confirm(OPEN), None);
    }

    #[test]
    fn confirm_sends_once_and_closes() {
        let mut c = MergeConfirm::default();
        c.open(ConfirmKind::Merge, OPEN);
        assert_eq!(c.confirm(OPEN), Some(ConfirmKind::Merge));
        assert_eq!(c.current(), None);
        assert_eq!(c.confirm(OPEN), None);
    }

    #[test]
    fn confirm_rechecks_the_gate() {
        let mut c = MergeConfirm::default();
        c.open(ConfirmKind::Merge, OPEN);
        assert_eq!(c.confirm(SHUT), None);
        assert_eq!(c.current(), None);
    }

    #[test]
    fn gate_loss_dismisses_only_the_affected_confirm() {
        let mut c = MergeConfirm::default();
        c.open(ConfirmKind::Merge, OPEN);
        assert!(!c.sync(OPEN));
        assert!(!c.sync(Gates {
            merge: true,
            close: false,
            convert: true
        }));
        assert_eq!(c.current(), Some(ConfirmKind::Merge));
        assert!(c.sync(Gates {
            merge: false,
            close: true,
            convert: true
        }));
        assert_eq!(c.current(), None);
        assert!(!c.sync(SHUT));
    }

    #[test]
    fn opening_one_replaces_the_other() {
        let mut c = MergeConfirm::default();
        c.open(ConfirmKind::Merge, OPEN);
        c.open(ConfirmKind::Close, OPEN);
        assert_eq!(c.current(), Some(ConfirmKind::Close));
    }

    #[test]
    fn convert_to_draft_confirms_through_its_own_gate() {
        let mut c = MergeConfirm::default();
        let no_convert = Gates {
            convert: false,
            ..OPEN
        };
        assert!(!c.open(ConfirmKind::ConvertToDraft, no_convert));
        assert!(c.open(ConfirmKind::ConvertToDraft, OPEN));
        // Esc backs out without a send.
        assert!(c.cancel());
        assert_eq!(c.confirm(OPEN), None);
        // The gate shutting (a refetch made it a draft) closes it.
        c.open(ConfirmKind::ConvertToDraft, OPEN);
        assert!(c.sync(no_convert));
        assert_eq!(c.current(), None);
        // Confirming re-checks, then yields the kind once.
        c.open(ConfirmKind::ConvertToDraft, OPEN);
        assert_eq!(c.confirm(OPEN), Some(ConfirmKind::ConvertToDraft));
        assert_eq!(c.confirm(OPEN), None);
        assert!(CONVERT_PROMPT.starts_with("Convert to draft?"));
    }

    #[test]
    fn prompt_names_method_and_branches() {
        assert_eq!(
            merge_prompt(MergeMethod::Squash, "feature/x", "main"),
            "Squash and merge feature/x into main?"
        );
        assert_eq!(
            merge_prompt(MergeMethod::Merge, "a", "b"),
            "Merge a into b?"
        );
        assert_eq!(
            merge_prompt(MergeMethod::Rebase, "a", "b"),
            "Rebase and merge a into b?"
        );
        assert!(CLOSE_PROMPT.contains("without merging"));
    }
}
