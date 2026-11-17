//! The PR Commits tab's list (#793): the repo Commits tab's day headers
//! and two-line row (docs/0081 D5), over the PR's newest-100 window,
//! oldest first like GitHub's PR Commits tab.

use gpui::*;
use gpui_component::{ActiveTheme as _, v_flex};

use crate::commit_row::{commit_row, day_header};
use crate::commits::day_sections;
use crate::gql;

use super::commits_window;

/// The scrollable list: the cap caption (when the query cut older
/// commits) leads, then one header per local day with its rows. Rows
/// open the commit on github.com, as the repo tab's do.
pub(super) fn commits_list(d: &gql::PrDetail, cx: &App) -> AnyElement {
    let rows: Vec<gql::CommitRow> = d.commits.nodes.iter().map(gql::PrCommit::to_row).collect();
    let mut out: Vec<AnyElement> = Vec::new();
    // #876: the query reads the newest 100, so what it cut is older than
    // every row — the cap row leads the list.
    if commits_window::hidden_older(d) > 0 {
        out.push(
            div()
                .id("pr-commits-truncated")
                .px_2()
                .pt_2()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(commits_window::cap_caption(
                    d.commits.nodes.len(),
                    d.commits.total_count,
                ))
                .into_any_element(),
        );
    }
    // The connection is oldest-first and the tab keeps that order, as
    // GitHub's PR Commits tab does: the PR reads as a story.
    for section in day_sections(&rows, &chrono::Local) {
        out.push(day_header(section.day, Some(section.start), cx).into_any_element());
        for row in &rows[section.start..section.start + section.len] {
            let url = row.url.clone();
            out.push(
                commit_row(row, cx)
                    .on_click(move |_, _, cx| {
                        if !url.is_empty() {
                            cx.open_url(&url);
                        }
                    })
                    .into_any_element(),
            );
        }
    }
    div()
        .id("pr-commits")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px_1()
        .child(v_flex().children(out))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use crate::gql::PrCommit;

    #[test]
    fn a_pr_commit_becomes_the_repo_row() {
        let c = PrCommit {
            oid: "abcdef1234567890".into(),
            headline: "Fix the parser".into(),
            committed_at: Some("2026-10-20T12:00:00Z".into()),
            author_name: Some("Ada".into()),
            author_login: None,
            author_avatar: None,
            url: "https://example.com/c/abcdef1".into(),
            signature: None,
            ci_state: Some("SUCCESS".into()),
        };
        let row = c.to_row();
        // The short SHA is seven characters; short oids don't panic.
        assert_eq!(row.abbreviated_oid, "abcdef1");
        assert_eq!(crate::commit_row::author_name(&row), "Ada");
        assert!(row.status_check_rollup.is_some());
        let short = PrCommit {
            oid: "abc".into(),
            ..c
        };
        assert_eq!(short.to_row().abbreviated_oid, "abc");
    }
}
