//! Which actions the comment box offers (#755). GitHub refuses
//! Approve and Request changes on your own pull request ("Can not approve
//! your own pull request" / "Can not request changes on your own pull
//! request"), so the PR author only gets Comment. The PR state does not
//! matter: GitHub accepts reviews on merged and closed pull requests too.

use gpui::{Context, SharedString, Window};
use gpui_component::select::SearchableVec;

use super::{PrDetailPage, comment_box_viewer_is_author};

/// The action labels offered for the viewer's relation to the PR.
pub(crate) fn comment_actions(viewer_is_author: bool) -> Vec<&'static str> {
    if viewer_is_author {
        vec!["Comment"]
    } else {
        vec!["Comment", "Approve", "Request changes"]
    }
}

impl PrDetailPage {
    /// Re-seed the action Select when the viewer turns out to be the PR
    /// author (the detail lands after the page is built). Falls back to
    /// Comment when the selected action disappears.
    pub(super) fn sync_comment_actions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let author = comment_box_viewer_is_author(
            self.detail(cx)
                .and_then(|d| d.author.as_ref())
                .map(|a| a.login.as_str()),
            self.viewer_login(cx).as_deref(),
        );
        if self.comment_actions_author.get() == author {
            return;
        }
        self.comment_actions_author.set(author);
        let items: Vec<SharedString> = comment_actions(author)
            .into_iter()
            .map(SharedString::from)
            .collect();
        let selected = self.current_action(cx);
        let keep = items.contains(&selected);
        let fallback: SharedString = "Comment".into();
        self.action_select.update(cx, |select, cx| {
            select.set_items(SearchableVec::from(items), window, cx);
            if !keep {
                select.set_selected_value(&fallback, window, cx);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn author_only_comments() {
        assert_eq!(comment_actions(true), vec!["Comment"]);
        assert_eq!(
            comment_actions(false),
            vec!["Comment", "Approve", "Request changes"]
        );
    }
}
