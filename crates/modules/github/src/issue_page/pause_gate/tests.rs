//! #887: the issue page obeys the GraphQL pause — no request goes out,
//! a document that never landed says why, a landed one keeps its data,
//! and a held write fails with the pause note rather than a bare status.

use gpui::TestAppContext;

use crate::issue_page::test_rig::{NUMBER, REPO, Reply, Rig, detail_at_server};
use crate::rate_pause::hold::held_result;
use crate::service::{FetchSpec, IssuePendingKind, Kind};
use crate::usage::{Origin, Surface, Trigger, budget_now};

const NOTE: &str = "GraphQL paused · resumes ";

#[gpui::test]
fn a_cold_document_shows_the_pause_instead_of_a_fetch_error(cx: &mut TestAppContext) {
    let rig = Rig::new(cx, vec![detail_at_server(0, "OPEN")], false);
    rig.pause_graphql(cx, budget_now() + 3600);
    let (_page, vcx) = rig.open(cx);
    assert_eq!(rig.detail_reads(), 0, "no request goes out");
    let error = rig.with_entry(vcx, |e| e.books.last_error.clone());
    assert!(error.is_some_and(|e| e.starts_with(NOTE)), "the pause note");
    assert!(!rig.in_flight(vcx));
}

#[gpui::test]
fn a_landed_document_keeps_its_data_and_shows_no_error(cx: &mut TestAppContext) {
    let rig = Rig::new(cx, vec![Reply::Status(500)], true);
    let (page, vcx) = rig.open(cx);
    rig.pause_graphql(vcx, budget_now() + 3600);
    rig.fly_detail(vcx, &page);
    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 0);
    assert!(rig.has_detail(vcx), "the landed detail stays");
    assert!(rig.with_entry(vcx, |e| e.books.last_error.is_none()));
}

#[gpui::test]
fn a_held_write_fails_with_the_pause_note(cx: &mut TestAppContext) {
    let rig = Rig::new(cx, vec![detail_at_server(0, "OPEN")], true);
    rig.pause_graphql(cx, budget_now() + 3600);
    let spec = FetchSpec {
        origin: Origin::new(Surface::IssuePage, Trigger::MutationRefetch),
        kind: Kind::PrMutation(crate::service::Mutation::Comment),
        url: String::new(),
        etag: None,
        ident: None,
        max_pages: 1,
        variables: None,
    };
    cx.update(|cx| {
        rig.state.update(cx, |s, _| {
            let token = s
                .begin_issue_pending(REPO, NUMBER, IssuePendingKind::Comment)
                .expect("no write pending");
            assert!(!s.apply_issue_mutation_result(
                REPO,
                NUMBER,
                IssuePendingKind::Comment,
                token,
                held_result(spec),
            ));
        })
    });
    let message = rig.with_entry(cx, |e| e.error.as_ref().map(|e| e.message.clone()));
    assert!(message.is_some_and(|m| m.starts_with(NOTE)), "not HTTP 429");
}
