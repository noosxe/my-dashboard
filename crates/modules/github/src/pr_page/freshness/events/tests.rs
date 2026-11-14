//! docs/0084 D2 triggers over the fake clock: a stamp backdated by a
//! known age stands for time passing, and the transport counts what
//! leaves.

use std::time::{Duration, Instant};

use gpui::TestAppContext;

use super::{TabLanes, tab_lanes};
use crate::pr_page::PrTab;
use crate::pr_page::test_rig::{NUMBER, REPO, Rig, server};
use crate::service::{GithubState, Kind, PrKey, SpecResult};

fn age_detail(rig: &Rig, cx: &mut TestAppContext, age: Duration) {
    cx.update(|cx| {
        rig.state.update(cx, |s, _| {
            s.pulls
                .get_mut(&PrKey::new(REPO, NUMBER))
                .expect("the document")
                .detail_at = Some(Instant::now() - age);
        })
    });
}

fn rig(cx: &mut TestAppContext) -> Rig {
    Rig::new(
        cx,
        server("aaa", "MERGEABLE"),
        vec![server("bbb", "MERGEABLE")],
    )
}

#[test]
fn tab_entry_names_each_tabs_lanes() {
    let detail = TabLanes {
        detail: true,
        files: false,
        threads: false,
    };
    for tab in [PrTab::Conversation, PrTab::Commits, PrTab::Checks] {
        assert_eq!(tab_lanes(tab), detail, "{tab:?}");
    }
    assert_eq!(
        tab_lanes(PrTab::Files),
        TabLanes {
            detail: false,
            files: true,
            threads: true
        }
    );
}

#[gpui::test]
fn a_detail_tab_entry_at_29_s_is_quiet_and_at_31_s_refetches(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let (page, vcx) = rig.open(cx);
    for tab in [PrTab::Commits, PrTab::Checks, PrTab::Conversation] {
        age_detail(&rig, vcx, Duration::from_secs(29));
        page.update(vcx, |p, cx| p.enter_tab(tab, true, cx));
        vcx.run_until_parked();
        assert_eq!(rig.detail_reads(), 0, "{tab:?} at 29 s");
    }
    age_detail(&rig, vcx, Duration::from_secs(31));
    page.update(vcx, |p, cx| p.enter_tab(PrTab::Commits, true, cx));
    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 1, "Commits at 31 s");
    assert_eq!(rig.head(vcx).as_deref(), Some("bbb"));
}

#[gpui::test]
fn reselecting_the_open_tab_is_not_an_entry(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let (page, vcx) = rig.open(cx);
    age_detail(&rig, vcx, Duration::from_secs(31));
    page.update(vcx, |p, cx| p.enter_tab(PrTab::Conversation, false, cx));
    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 0);
}

#[gpui::test]
fn a_files_entry_refetches_its_loaded_lanes_past_the_floor(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let (page, vcx) = rig.open(cx);
    // First entry starts both lanes, whatever their age.
    page.update(vcx, |p, cx| p.enter_tab(PrTab::Files, true, cx));
    vcx.run_until_parked();
    assert_eq!((rig.files_reads(), rig.threads_reads()), (1, 1));

    page.update(vcx, |p, cx| {
        p.files.backdate(Duration::from_secs(29));
        p.threads.backdate(Duration::from_secs(29));
        p.enter_tab(PrTab::Files, true, cx);
    });
    vcx.run_until_parked();
    assert_eq!((rig.files_reads(), rig.threads_reads()), (1, 1), "29 s");

    page.update(vcx, |p, cx| {
        p.files.backdate(Duration::from_secs(31));
        p.threads.backdate(Duration::from_secs(31));
        p.enter_tab(PrTab::Files, true, cx);
    });
    vcx.run_until_parked();
    assert_eq!((rig.files_reads(), rig.threads_reads()), (2, 2), "31 s");
    assert_eq!(rig.detail_reads(), 0, "Files reads no detail");
}

#[gpui::test]
fn a_tab_entry_stays_behind_the_pause_gate(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let (page, vcx) = rig.open(cx);
    age_detail(&rig, vcx, Duration::from_secs(31));
    vcx.update(|_, cx| {
        rig.state.update(cx, |s, _| {
            s.rate_limited_until.graphql = Some(u64::MAX);
        })
    });
    page.update(vcx, |p, cx| p.enter_tab(PrTab::Checks, true, cx));
    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 0, "a paused budget holds the entry");
}

#[gpui::test]
fn module_reshown_follows_the_15_s_floor(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let (page, vcx) = rig.open(cx);
    age_detail(&rig, vcx, Duration::from_secs(14));
    page.update(vcx, |p, cx| p.module_reshown(cx));
    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 0, "14 s");
    age_detail(&rig, vcx, Duration::from_secs(16));
    page.update(vcx, |p, cx| p.module_reshown(cx));
    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 1, "16 s");
}

fn land_notification(rig: &Rig, cx: &mut TestAppContext, number: i64, updated: &str) {
    let body = serde_json::json!([{
        "id": "1", "unread": true, "updated_at": updated,
        "subject": { "title": "t", "type": "PullRequest",
            "url": format!("https://api.github.com/repos/{REPO}/pulls/{number}") },
        "repository": { "full_name": REPO },
    }])
    .to_string();
    cx.update(|cx| {
        rig.state.update(cx, |s: &mut GithubState, cx| {
            s.apply(SpecResult {
                kind: Kind::Notifications,
                ident: None,
                status: 200,
                etag: None,
                rate: None,
                body: Some(body),
                network: None,
                cursor: None,
            });
            cx.notify();
        })
    });
}

#[gpui::test]
fn a_notification_naming_the_open_pr_refetches_once_per_landing(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let (_page, vcx) = rig.open(cx);
    // The seeding landing is silent.
    land_notification(&rig, vcx, NUMBER, "t1");
    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 0, "seed");

    land_notification(&rig, vcx, NUMBER, "t2");
    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 1, "the landing refetches");

    // The same thread replayed, and an unrelated store notification,
    // refetch nothing more.
    land_notification(&rig, vcx, NUMBER, "t2");
    vcx.update(|_, cx| rig.state.update(cx, |_, cx| cx.notify()));
    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 1, "once per landing");
}

#[gpui::test]
fn a_notification_naming_another_pr_refetches_nothing(cx: &mut TestAppContext) {
    let rig = rig(cx);
    let (_page, vcx) = rig.open(cx);
    land_notification(&rig, vcx, NUMBER + 1, "t1");
    land_notification(&rig, vcx, NUMBER + 1, "t2");
    vcx.run_until_parked();
    assert_eq!(rig.detail_reads(), 0);
}
