//! The issue page's cross-reference rows (docs/0085): the shared row, wired
//! to this page's navigation. A tracked repo's source opens in the app
//! through the host (`IssuePageEvent::OpenSubject`); any other source opens
//! in the browser and the page stays. The issue page has no leave guard (Esc
//! closes it at once; its drafts persist in the draft store), so activation
//! emits straight away.

use gpui::{AnyElement, Context, Window};

use super::{IssueDetailPage, IssuePageEvent};
use crate::gql;
use crate::xref_row::{self, PageKind, Place, Target};

/// The cap row's share (docs/0085 D2): references the window cut, beyond
/// what the parser dropped or folded.
pub(super) fn hidden(d: &gql::IssueDetail) -> i64 {
    (d.references.total_count - d.references.nodes.len() as i64).max(0)
}

impl IssueDetailPage {
    /// The row for `d.references.nodes[ix]`.
    pub(super) fn xref_row(
        &self,
        d: &gql::IssueDetail,
        ix: usize,
        cx: &Context<Self>,
    ) -> AnyElement {
        let e = &d.references.nodes[ix];
        let target = {
            let state = self.state.read(cx);
            xref_row::resolve(&e.source, |r| {
                xref_row::listed(state.repos.iter().map(|r| r.full_name.as_str()), r)
            })
        };
        let page = cx.entity();
        let title = e.source.title.clone();
        xref_row::row(
            Place {
                id_prefix: "issue-event",
                // After the event rows, so the ids stay unique.
                ix: d.events.len() + ix,
                page: PageKind::Issue,
                page_repo: &self.repo,
            },
            e,
            target,
            move |target, window, cx| {
                page.update(cx, |p, cx| p.open_xref(target, &title, window, cx));
            },
            cx,
        )
    }

    fn open_xref(
        &mut self,
        target: Target,
        title: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            Target::Browser(url) => cx.open_url(&url),
            Target::InApp {
                repo,
                number,
                is_pr,
            } => cx.emit(IssuePageEvent::OpenSubject {
                repo,
                number,
                is_pr,
                title: title.to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests;
