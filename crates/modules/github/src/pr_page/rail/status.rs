//! The pinned status block (docs/0083 D1–D6): verdict, sub-line, gate
//! rows and the action row on a `muted` surface, words untinted and tone
//! on icons only (D15). It answers one question first: can this pull
//! request merge, and if not, why?

use dashboard_core::freshness::relative_age;
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, accesskit, div,
    prelude::FluentBuilder as _,
};
use gpui_component::{ActiveTheme, h_flex, tooltip::Tooltip, v_flex};

use super::gates::{self, Kind, Trailing};
use super::{action_row, gate_row, glyph, terminal, update_branch, verdict};
use crate::gql::PrDetail;
use crate::pr_page::PrDetailPage;

pub(super) fn status_block(
    page: &PrDetailPage,
    d: &PrDetail,
    cx: &Context<PrDetailPage>,
) -> AnyElement {
    let verdict = verdict::verdict(d);
    // docs/0066 D3: the sweep's waiting runs join the rollup's counts.
    let waiting = page.gated.data().map(|rows| rows.len()).unwrap_or(0);
    let rows = gates::gates(d, waiting);
    let reasons: Vec<String> = rows.iter().filter_map(|g| g.blocking_reason(d)).collect();
    let tooltip: SharedString = verdict.blocked_tooltip(&reasons).into();
    let sub = terminal::sub_line(
        verdict.row,
        d,
        &relative_age(d.ending.merged_at.as_deref()),
        &relative_age(d.ending.closed_at.as_deref()),
    );
    let busy = page.writes_busy(cx);
    let words = verdict.word;
    let sentence = verdict.sentence;
    let hover = SharedString::from(sentence);
    v_flex()
        .id("pr-rail-status")
        .debug_selector(|| "pr-rail-status".to_string())
        .role(accesskit::Role::Group)
        .aria_label("Merge status")
        .flex_none()
        .w_full()
        .gap_2()
        .p_3()
        .rounded_md()
        .bg(cx.theme().muted)
        .child(
            h_flex()
                .id("pr-rail-verdict")
                .debug_selector(|| "pr-rail-verdict".to_string())
                .role(accesskit::Role::Label)
                .aria_label(format!("{words}. {sentence}"))
                .gap_2()
                .items_center()
                .tooltip(move |window, cx| Tooltip::new(hover.clone()).build(window, cx))
                .child(glyph::glyph(verdict.glyph, verdict.tone, cx))
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cx.theme().foreground)
                        .child(words),
                ),
        )
        .when_some(sub, |col, spans| {
            col.child(
                h_flex()
                    .id("pr-rail-verdict-sub")
                    .debug_selector(|| "pr-rail-verdict-sub".to_string())
                    .pl_6()
                    .flex_wrap()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .children(spans.into_iter().map(|s| {
                        div()
                            .when(s.mono, |t| {
                                t.font_family(cx.theme().mono_font_family.clone())
                            })
                            .child(s.text)
                    })),
            )
        })
        .child(v_flex().gap_1().children(rows.iter().map(|gate| {
            let trailing = (gate.trailing == Trailing::Update).then(|| {
                let usable =
                    !d.id.is_empty() && d.head_oid.as_deref().is_some_and(|oid| !oid.is_empty());
                // While the rebase confirm is open it replaces the button
                // (docs/0067 D4).
                (usable && !page.update_rebase_confirm)
                    .then(|| update_branch::update_split(page, busy, cx))
            });
            let row = gate_row::gate_row(gate, &d.url, trailing.flatten(), cx);
            // The rebase confirm opens directly under its row (D3).
            if gate.kind == Kind::Merge && page.update_rebase_confirm {
                v_flex()
                    .gap_2()
                    .child(row)
                    .child(update_branch::rebase_confirm(d, cx))
                    .into_any_element()
            } else {
                row
            }
        })))
        .children(action_row::action_row(page, d, tooltip, cx))
        .when_some(page.panel_error(cx), |col, e| {
            col.child(
                div()
                    .id("pr-rail-panel-error")
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(e.message.clone()),
            )
        })
        .into_any_element()
}
