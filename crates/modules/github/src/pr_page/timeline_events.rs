//! #787: the conversation's event rows — merged, closed, reopened and
//! force-pushed. Each is one muted line in the review-event style (#786):
//! glyph, "actor verb", age on the trailing edge. They interleave with the
//! comment and review cards by time (docs/0021 D3, for the PR page).

use gpui::prelude::*;
use gpui::{AnyElement, App, FontWeight, SharedString, div};
use gpui_component::{ActiveTheme as _, Sizable as _, h_flex};

mod sentence;

use self::sentence::{Sentence, sentence};
use super::{TimelineKind, conversation_window::visible_timeline};
use crate::gql::{self, PrEvent, PrEventKind};
use crate::state_pill::{Glyph, Tone, icon};
use dashboard_core::freshness::relative_age;

/// One conversation entry: a card (comment or review) or an event row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Entry {
    Card(TimelineKind, usize),
    Event(usize),
}

fn stamp(ts: Option<&str>) -> i64 {
    ts.and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map_or(0, |t| t.timestamp())
}

/// The visible cards and the events in one ascending order. Stable: at an
/// equal stamp the card comes first, as the issue page does (docs/0021 D3).
pub(super) fn entries(d: &gql::PrDetail) -> Vec<Entry> {
    let mut all: Vec<(i64, u8, Entry)> = visible_timeline(&d.comments.nodes, &d.reviews.nodes)
        .into_iter()
        .map(|(kind, ix)| {
            let at = match kind {
                TimelineKind::Comment => d.comments.nodes[ix].created_at.as_deref(),
                TimelineKind::Review => d.reviews.nodes[ix].submitted_at.as_deref(),
            };
            (stamp(at), 0, Entry::Card(kind, ix))
        })
        .collect();
    all.extend(
        d.events
            .nodes
            .iter()
            .enumerate()
            .map(|(ix, e)| (stamp(e.created_at.as_deref()), 1, Entry::Event(ix))),
    );
    all.sort_by_key(|e| (e.0, e.1));
    all.into_iter().map(|e| e.2).collect()
}

/// What the cap row adds: events the query cut.
pub(super) fn hidden(d: &gql::PrDetail) -> i64 {
    (d.events.total_count - d.events.nodes.len() as i64).max(0)
}

fn look(kind: &PrEventKind) -> (Tone, Glyph) {
    match kind {
        PrEventKind::Merged => (Tone::Merged, Glyph::Merged),
        PrEventKind::Closed => (Tone::Danger, Glyph::Cross),
        PrEventKind::Reopened => (Tone::Success, Glyph::Open),
        PrEventKind::ForcePushed { .. } => (Tone::Muted, Glyph::Right),
        PrEventKind::Labeled { .. } => (Tone::Muted, Glyph::Plus),
        PrEventKind::Unlabeled { .. } => (Tone::Muted, Glyph::Minus),
        PrEventKind::Assigned { .. } => (Tone::Muted, Glyph::Plus),
        PrEventKind::Unassigned { .. } => (Tone::Muted, Glyph::Minus),
        PrEventKind::ReviewRequested { .. } | PrEventKind::ReviewRequestRemoved { .. } => {
            (Tone::Muted, Glyph::Info)
        }
        PrEventKind::Renamed { .. } => (Tone::Muted, Glyph::Edit),
        PrEventKind::Milestoned { .. } => (Tone::Muted, Glyph::Plus),
        PrEventKind::Demilestoned { .. } => (Tone::Muted, Glyph::Minus),
    }
}

/// A name in the actor's emphasis, so it does not blend into the verb.
fn emphasis(text: &str, cx: &App) -> AnyElement {
    div()
        .min_w_0()
        .font_weight(FontWeight::MEDIUM)
        .text_color(cx.theme().foreground)
        .child(text.to_string())
        .into_any_element()
}

/// "lead [label chip] [old] [name] tail" as the row's own flex items, so a
/// long title wraps word by word beside the actor. The chip is the rail's, on
/// the page surface.
fn sentence_parts(s: &Sentence, cx: &App) -> Vec<AnyElement> {
    let mut parts = vec![div().child(s.lead.clone()).into_any_element()];
    if let Some(l) = &s.label {
        let surface = cx.theme().background;
        parts.push(
            crate::page::label_pill(&l.name, l.color.as_deref(), surface, cx).into_any_element(),
        );
    }
    if let Some(old) = &s.struck {
        parts.push(
            div()
                .min_w_0()
                .line_through()
                .child(old.clone())
                .into_any_element(),
        );
    }
    if let Some(who) = &s.subject {
        parts.push(emphasis(who, cx));
    }
    if !s.tail.is_empty() {
        parts.push(div().child(s.tail.clone()).into_any_element());
    }
    parts
}

pub(super) fn row(ix: usize, e: &PrEvent, cx: &App) -> AnyElement {
    let (tone, glyph) = look(&e.kind);
    let color = match tone {
        Tone::Merged => cx.theme().info,
        Tone::Success => cx.theme().success,
        Tone::Danger => cx.theme().danger,
        _ => cx.theme().muted_foreground,
    };
    let key = format!("pr-event-{ix}");
    let probe = key.clone();
    h_flex()
        .id(SharedString::from(key))
        .debug_selector(move || probe.clone())
        .w_full()
        .gap_2()
        .items_center()
        .px_3()
        .py_1()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(icon(glyph).xsmall().text_color(color))
        .child(
            // A long title wraps inside the row; the age keeps its edge.
            h_flex()
                .flex_1()
                .min_w_0()
                .flex_wrap()
                .gap_1()
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(cx.theme().foreground)
                        .child(e.actor.clone().unwrap_or_else(|| "Someone".into())),
                )
                .children(match &e.kind {
                    // The commit ids are mono, as the header's branch names.
                    PrEventKind::ForcePushed {
                        before: Some(b),
                        after: Some(a),
                    } => {
                        let mono = cx.theme().mono_font_family.clone();
                        vec![
                            div().child("force-pushed from").into_any_element(),
                            div()
                                .font_family(mono.clone())
                                .child(b.clone())
                                .into_any_element(),
                            div().child("to").into_any_element(),
                            div().font_family(mono).child(a.clone()).into_any_element(),
                        ]
                    }
                    kind => sentence_parts(&sentence(kind, e.actor.as_deref()), cx),
                }),
        )
        .child(
            div()
                .ml_auto()
                .flex_shrink_0()
                .text_xs()
                .child(relative_age(e.created_at.as_deref())),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(at: &str, kind: PrEventKind) -> PrEvent {
        PrEvent {
            id: at.into(),
            actor: Some("a".into()),
            created_at: Some(at.into()),
            kind,
        }
    }

    fn detail(comment_at: &str, events: Vec<PrEvent>) -> gql::PrDetail {
        let repo = serde_json::json!({"pullRequest": {"id": "PR", "number": 7,
            "title": "t", "url": "u",
            "comments": {"totalCount": 1, "nodes": [{"id": "C", "body": "x",
                "createdAt": comment_at}]}}});
        let mut d = gql::pr_detail_from_value(&repo).unwrap();
        d.events = gql::PrEvents {
            total_count: events.len() as i64,
            nodes: events,
        };
        d
    }

    #[test]
    fn events_interleave_with_cards_by_time() {
        let d = detail(
            "2026-10-02T00:00:00Z",
            vec![
                event("2026-10-03T00:00:00Z", PrEventKind::Merged),
                event("2026-10-01T00:00:00Z", PrEventKind::Reopened),
            ],
        );
        assert_eq!(
            entries(&d),
            vec![
                Entry::Event(1),
                Entry::Card(TimelineKind::Comment, 0),
                Entry::Event(0)
            ]
        );
    }

    #[test]
    fn a_card_precedes_an_event_at_the_same_stamp() {
        let d = detail(
            "2026-10-02T00:00:00Z",
            vec![event("2026-10-02T00:00:00Z", PrEventKind::Closed)],
        );
        assert_eq!(
            entries(&d),
            vec![Entry::Card(TimelineKind::Comment, 0), Entry::Event(0)]
        );
    }

    fn verb_of(kind: &PrEventKind) -> String {
        sentence(kind, None).text()
    }

    #[test]
    fn verbs_read_after_the_login() {
        assert_eq!(verb_of(&PrEventKind::Merged), "merged this");
        assert_eq!(verb_of(&PrEventKind::Closed), "closed this");
        assert_eq!(verb_of(&PrEventKind::Reopened), "reopened this");
        let push = PrEventKind::ForcePushed {
            before: Some("1111111".into()),
            after: Some("2222222".into()),
        };
        assert_eq!(verb_of(&push), "force-pushed from 1111111 to 2222222");
        let bare = PrEventKind::ForcePushed {
            before: None,
            after: Some("2222222".into()),
        };
        assert_eq!(verb_of(&bare), "force-pushed the branch");
    }

    #[test]
    fn the_cap_counts_what_the_query_cut() {
        let mut d = detail("2026-10-02T00:00:00Z", vec![]);
        d.events.total_count = 130;
        assert_eq!(hidden(&d), 130);
    }
}
