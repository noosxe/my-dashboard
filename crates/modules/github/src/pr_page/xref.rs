//! The PR page's cross-reference rows (docs/0085): the shared row, wired to
//! this page's navigation. A tracked repo's source opens in the app through
//! the host (`PrPageEvent::OpenSubject`) after the leave guard; any other
//! source opens in the browser and the page stays.

use gpui::{AnyElement, Context, Window};

use super::{PrDetailPage, PrPageEvent, leave_guard::LeaveIntent};
use crate::gql;
use crate::xref_row::{self, PageKind, Place, Target};

/// An in-app destination waiting on the discard confirm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Subject {
    pub repo: String,
    pub number: i64,
    pub is_pr: bool,
}

impl PrDetailPage {
    /// The row for `d.references.nodes[ix]`.
    pub(super) fn xref_row(&self, d: &gql::PrDetail, ix: usize, cx: &Context<Self>) -> AnyElement {
        let e = &d.references.nodes[ix];
        let target = {
            let state = self.state.read(cx);
            xref_row::resolve(&e.source, |r| {
                xref_row::listed(state.repos.iter().map(|r| r.full_name.as_str()), r)
            })
        };
        let page = cx.entity();
        xref_row::row(
            Place {
                id_prefix: "pr-event",
                // After the event rows, so the ids stay unique.
                ix: d.events.nodes.len() + ix,
                page: PageKind::PullRequest,
                page_repo: &self.repo,
            },
            e,
            target,
            move |target, window, cx| {
                page.update(cx, |p, cx| p.open_xref(target, window, cx));
            },
            cx,
        )
    }

    /// Activate a row: the browser opens at once; an in-app page goes
    /// through the leave guard first so unsent review drafts are not dropped.
    pub(super) fn open_xref(
        &mut self,
        target: Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            Target::Browser(url) => cx.open_url(&url),
            Target::InApp {
                repo,
                number,
                is_pr,
            } => {
                self.pending_subject = Some(Subject {
                    repo,
                    number,
                    is_pr,
                });
                if self.guard_leave(LeaveIntent::Subject, window, cx) {
                    self.emit_pending_subject(cx);
                }
            }
        }
    }

    pub(super) fn emit_pending_subject(&mut self, cx: &mut Context<Self>) {
        if let Some(s) = self.pending_subject.take() {
            cx.emit(PrPageEvent::OpenSubject {
                repo: s.repo,
                number: s.number,
                is_pr: s.is_pr,
            });
        }
    }
}
