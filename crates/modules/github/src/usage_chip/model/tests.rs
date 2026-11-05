use super::*;

const NOW: u64 = 1_000_000;

fn live(remaining: u32, resets_in_secs: u64) -> BucketDisplay {
    BucketDisplay {
        remaining,
        limit: 5_000,
        resets_in_secs,
        stale: false,
        snapshot_at: Some(NOW),
    }
}

fn model(
    rest: Option<BucketDisplay>,
    graphql: Option<BucketDisplay>,
    pause: RateLimitPause,
) -> ChipModel {
    ChipModel::of(&UsageDisplay::Budget { rest, graphql }, &pause, NOW)
}

fn texts(m: &ChipModel) -> Vec<(Option<ThresholdState>, String, bool)> {
    m.budgets()
        .into_iter()
        .map(|(lane, chip)| {
            let s = segment(lane, chip);
            (s.icon, s.text, s.marked)
        })
        .collect()
}

#[test]
fn normal_budgets_read_words_and_numbers_muted() {
    let m = model(
        Some(live(3_712, 780)),
        Some(live(4_950, 570)),
        RateLimitPause::default(),
    );
    assert_eq!(
        texts(&m),
        vec![
            (None, "REST 3,712".into(), false),
            (None, "GraphQL 4,950".into(), false),
        ]
    );
}

#[test]
fn placeholders_read_dashes() {
    let m = ChipModel::of(&UsageDisplay::Pending, &RateLimitPause::default(), NOW);
    assert_eq!(
        texts(&m),
        vec![
            (None, "REST —".into(), false),
            (None, "GraphQL —".into(), false),
        ]
    );
    let m = model(Some(live(3_712, 780)), None, RateLimitPause::default());
    assert_eq!(texts(&m)[1], (None, "GraphQL —".into(), false));
}

#[test]
fn each_budget_takes_its_own_ladder_state_and_word() {
    // 1,140 of 5,000 is 22.8 %: Low. 412 is 8.2 %: Critical.
    let m = model(
        Some(live(412, 780)),
        Some(live(1_140, 570)),
        RateLimitPause::default(),
    );
    assert_eq!(
        texts(&m),
        vec![
            (
                Some(ThresholdState::Critical),
                "REST 412 Critical".into(),
                true
            ),
            (Some(ThresholdState::Low), "GraphQL 1,140 Low".into(), true),
        ]
    );
}

#[test]
fn a_stale_window_draws_no_state() {
    let stale = BucketDisplay {
        stale: true,
        resets_in_secs: 0,
        ..live(100, 0)
    };
    let m = model(Some(stale), None, RateLimitPause::default());
    assert_eq!(m.rest.state(), None);
    assert_eq!(texts(&m)[0], (None, "REST 100".into(), false));
}

#[test]
fn a_paused_budget_reads_resumes_and_the_other_stays_live() {
    let until = NOW + 37 * 60;
    let pause = RateLimitPause {
        rest: None,
        graphql: Some(until),
    };
    let m = model(Some(live(4_990, 780)), Some(live(0, 37 * 60)), pause);
    assert_eq!(
        texts(&m),
        vec![
            (None, "REST 4,990".into(), false),
            (
                Some(ThresholdState::Exhausted),
                format!("GraphQL paused · resumes {}", local_hhmm(until)),
                true
            ),
        ]
    );
    let spoken = label(&m, (3, 2, 1, 0));
    assert!(
        spoken.contains(&format!(
            "GraphQL: paused, resumes at {} in 37 minutes",
            local_hhmm(until)
        )),
        "{spoken}"
    );
    assert!(spoken.contains("REST: 4,990 of 5,000 requests remaining, resets in 13 minutes"));
    // A pause the clock has passed is no pause.
    let lapsed = RateLimitPause {
        rest: None,
        graphql: Some(NOW),
    };
    let m = model(None, Some(live(5_000, 3_600)), lapsed);
    assert_eq!(texts(&m)[1], (None, "GraphQL 5,000".into(), false));
}

#[test]
fn the_label_speaks_state_words_and_session() {
    let m = model(
        Some(live(3_712, 780)),
        Some(live(1_140, 570)),
        RateLimitPause::default(),
    );
    assert_eq!(
        label(&m, (5, 2, 1, 1)),
        "GitHub API: REST: 3,712 of 5,000 requests remaining, resets in 13 minutes; \
         GraphQL: 1,140 of 5,000 points remaining, Low, resets in 9 minutes; \
         session: 5 requests (2 GraphQL, 1 REST, 1 auth)"
    );
    let m = ChipModel::of(&UsageDisplay::Pending, &RateLimitPause::default(), NOW);
    assert!(label(&m, (0, 0, 0, 0)).starts_with("GitHub API: REST: no response yet; GraphQL"));
}

#[test]
fn the_tooltip_gives_limits_and_resets() {
    let m = model(Some(live(3_712, 780)), None, RateLimitPause::default());
    assert_eq!(
        tooltip(&m),
        "REST 3,712/5,000 resets in 13m\nGraphQL — no response yet\nOpens the Budget view"
    );
    let stale = BucketDisplay {
        stale: true,
        ..live(4_950, 0)
    };
    let m = model(None, Some(stale), RateLimitPause::default());
    assert!(tooltip(&m).contains(&format!("GraphQL 4,950/5,000 · as of {}", local_hhmm(NOW))));
}

#[test]
fn formatters() {
    assert_eq!(group_thousands(0), "0");
    assert_eq!(group_thousands(5000), "5,000");
    assert_eq!(group_thousands(4821), "4,821");
    assert_eq!(compact_duration(41 * 60), "41m");
    assert_eq!(compact_duration(65 * 60), "1h 05m");
    assert_eq!(spoken_duration(41 * 60), "41 minutes");
    assert_eq!(spoken_duration(65 * 60), "1 hours 5 minutes");
}
