//! #721 and #751: a paused budget holds the page's reads and writes, the
//! page says so, and a response's rate-limit signals arm the pause.

use gpui::{TestAppContext, VisualTestContext};

use super::super::BusyOp;
use super::super::test_rig::{NUMBER, REPO, Rig, server};
use crate::http::RateLimit;
use crate::rate_pause::Lane;
use crate::service::{Mutation, PrKey};
use crate::usage::{Origin, Surface, Trigger, budget_now};

fn rig(cx: &mut TestAppContext) -> Rig {
    Rig::new(
        cx,
        server("aaa", "MERGEABLE"),
        vec![server("aaa", "MERGEABLE")],
    )
}

fn pause(rig: &Rig, cx: &mut VisualTestContext, lane: Lane) {
    rig.state.update(cx, |s, _| {
        s.rate_limited_until.arm(lane, budget_now() + 3600);
    });
}

fn manual() -> Origin {
    Origin::new(Surface::PrPage, Trigger::Manual)
}

fn paused_until(rig: &Rig, cx: &mut VisualTestContext, lane: Lane) -> Option<u64> {
    rig.state.read_with(cx, |s, _| s.pause_in_force(lane))
}

fn detail_error(rig: &Rig, cx: &mut VisualTestContext) -> Option<String> {
    rig.state.read_with(cx, |s, _| {
        s.pr_entry(REPO, NUMBER)
            .and_then(|e| e.books.last_error.clone())
    })
}

fn empty(remaining: u32) -> Option<RateLimit> {
    Some(RateLimit {
        limit: 5000,
        remaining,
        reset_epoch: budget_now() + 3600,
        retry_after: None,
    })
}

#[gpui::test]
fn a_paused_graphql_budget_holds_the_detail_and_threads_reads(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let (page, vcx) = rig.open(cx);
    pause(&rig, vcx, Lane::Graphql);
    page.update(vcx, |p, cx| {
        p.fetch_detail(manual(), cx);
        p.fetch_threads(manual(), cx);
    });
    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 0, "no GraphQL request goes out");
    assert_eq!(rig.threads_reads(), 0);
    assert!(
        page.read_with(vcx, |p, cx| p.pause_note(cx))
            .is_some_and(|n| n.starts_with("GraphQL paused · resumes ")),
        "the page says why"
    );
}

#[gpui::test]
fn a_paused_rest_budget_holds_the_files_read_but_not_graphql(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let (page, vcx) = rig.open(cx);
    pause(&rig, vcx, Lane::Rest);
    page.update(vcx, |p, cx| {
        p.fetch_files(manual(), cx);
        p.fetch_detail(manual(), cx);
    });
    vcx.run_until_parked();
    assert_eq!(rig.files_reads(), 0);
    assert_eq!(rig.detail_reads(), 1, "the other budget is not held");
    assert!(
        page.read_with(vcx, |p, _| p.files.load_error().map(str::to_string))
            .is_some_and(|e| e.starts_with("REST paused · resumes ")),
        "a lane with nothing to show carries the note as its error"
    );
}

#[gpui::test]
fn a_cold_document_shows_the_pause_instead_of_a_fetch_error(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let (page, vcx) = rig.open(cx);
    rig.state.update(vcx, |s, _| {
        let e = s.pulls.get_mut(&PrKey::new(REPO, NUMBER)).unwrap();
        e.detail = None;
        e.detail_at = None;
    });
    pause(&rig, vcx, Lane::Graphql);
    page.update(vcx, |p, cx| p.fetch_detail(manual(), cx));
    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 0);
    assert!(
        detail_error(&rig, vcx).is_some_and(|e| e.starts_with("GraphQL paused · resumes ")),
        "{:?}",
        detail_error(&rig, vcx)
    );
}

#[gpui::test]
fn a_write_under_a_graphql_pause_is_refused_without_a_request(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let window = rig.open_droppable(cx);
    cx.run_until_parked();
    cx.update(|cx| {
        rig.state.update(cx, |s, _| {
            s.rate_limited_until.arm(Lane::Graphql, budget_now() + 3600)
        });
        window
            .update(cx, |p, window, cx| {
                p.busy = Some(BusyOp::PrSetLabels);
                p.spawn_mutation(
                    BusyOp::PrSetLabels,
                    Mutation::AddReaction,
                    serde_json::json!({}),
                    window,
                    cx,
                );
            })
            .unwrap();
    });
    cx.run_until_parked();
    assert_eq!(rig.mutations(), 0, "nothing was sent");
    let message = cx.update(|cx| {
        window
            .read_with(cx, |p, _| {
                p.mutation_error.as_ref().map(|e| e.message.clone())
            })
            .unwrap()
    });
    assert!(
        message.is_some_and(|m| m.starts_with("GraphQL paused · resumes ")),
        "the write's inline error names the pause"
    );
}

#[gpui::test]
fn a_rate_limited_200_arms_the_pause_and_says_so(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let (page, vcx) = rig.open(cx);
    rig.state.update(vcx, |s, _| {
        let e = s.pulls.get_mut(&PrKey::new(REPO, NUMBER)).unwrap();
        e.detail = None;
        e.detail_at = None;
    });
    rig.answer_details_with(
        200,
        empty(0),
        r#"{"data":{"repository":null},"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded"}]}"#,
    );
    page.update(vcx, |p, cx| p.fetch_detail(manual(), cx));
    vcx.run_until_parked();
    assert!(
        paused_until(&rig, vcx, Lane::Graphql).is_some(),
        "200 at remaining 0 arms"
    );
    let error = detail_error(&rig, vcx).unwrap_or_default();
    assert!(error.starts_with("GraphQL paused · resumes "), "{error}");
    assert!(!error.contains("not found"));
}

#[gpui::test]
fn a_forbidden_read_is_not_reported_as_removed(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let (page, vcx) = rig.open(cx);
    rig.state.update(vcx, |s, _| {
        let e = s.pulls.get_mut(&PrKey::new(REPO, NUMBER)).unwrap();
        e.detail = None;
        e.detail_at = None;
    });
    rig.answer_details_with(
        200,
        empty(4000),
        r#"{"data":{"repository":null},"errors":[{"type":"FORBIDDEN","message":"Resource protected by organization SAML enforcement."}]}"#,
    );
    page.update(vcx, |p, cx| p.fetch_detail(manual(), cx));
    vcx.run_until_parked();
    let error = detail_error(&rig, vcx).unwrap_or_default();
    assert!(error.contains("refused access"), "{error}");
    assert!(
        paused_until(&rig, vcx, Lane::Graphql).is_none(),
        "no pause for a refusal"
    );
}

#[gpui::test]
fn a_secondary_limit_with_retry_after_arms_the_pause(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let (page, vcx) = rig.open(cx);
    let rate = Some(RateLimit {
        limit: 5000,
        remaining: 4000,
        reset_epoch: budget_now() + 3000,
        retry_after: Some(90),
    });
    rig.answer_details_with(
        403,
        rate,
        r#"{"message":"You have exceeded a secondary rate limit."}"#,
    );
    let before = budget_now();
    page.update(vcx, |p, cx| p.fetch_detail(manual(), cx));
    vcx.run_until_parked();
    let until = paused_until(&rig, vcx, Lane::Graphql).expect("armed");
    assert!(
        (before + 90..=budget_now() + 90).contains(&until),
        "{until}"
    );
    assert!(
        page.read_with(vcx, |p, cx| p.pause_note(cx)).is_some(),
        "the note appears"
    );
}
