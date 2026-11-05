//! Band 1 (docs/0082 D10): the glance strip. One tile per transport
//! (REST, GraphQL, `auth`, `local`) with p90, p50 and the request count,
//! then "Slower than 1 s" with its share and the error count. Six tiles
//! in one row from about 900 px of content, three per row below. Every
//! figure is over the selection the query bar names.

use gpui::prelude::*;
use gpui::{AnyElement, App, FontWeight, SharedString, div, relative};
use gpui_component::{ActiveTheme, h_flex, v_flex};

use super::format;
use crate::usage_analytics::surfaces::RequestFilters;
use crate::usage_analytics::surfaces::diagnostics::{RequestGlance, TransportGlance};

/// D10: six tiles per row from this content width, three below.
pub(super) const SIX_UP_MIN_W: f32 = 900.0;

/// One tile's words: label, headline (with its muted suffix), sub-line.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Tile {
    pub id: &'static str,
    pub label: &'static str,
    pub value: String,
    pub suffix: Option<&'static str>,
    pub sub: String,
}

impl Tile {
    /// What assistive technology reads: the tile in one sentence.
    pub(super) fn spoken(&self) -> String {
        let value = match self.suffix {
            Some(s) => format!("{s} {}", self.value),
            None => self.value.clone(),
        };
        format!("{}: {value}, {}", self.label, self.sub)
    }
}

fn transport_tile(t: &TransportGlance, filters: &RequestFilters) -> Tile {
    let (id, label) = match t.transport {
        crate::usage_records::Transport::Rest => ("usage-glance-rest", "REST"),
        crate::usage_records::Transport::Graphql => ("usage-glance-graphql", "GraphQL"),
        crate::usage_records::Transport::Auth => ("usage-glance-auth", "auth"),
        crate::usage_records::Transport::Local => ("usage-glance-local", "local"),
    };
    match (t.p90_ms, t.p50_ms) {
        (Some(p90), Some(p50)) => Tile {
            id,
            label,
            value: format::duration(p90.into()),
            suffix: Some("p90"),
            sub: format!(
                "{} p50 · {}",
                format::duration(p50.into()),
                format::requests(t.requests)
            ),
        },
        _ => Tile {
            id,
            label,
            value: "—".into(),
            suffix: None,
            sub: if filters.transport.is_some_and(|f| f != t.transport) {
                "not in this selection".into()
            } else {
                "no requests".into()
            },
        },
    }
}

/// The six tiles in order (D10).
pub(super) fn tiles(g: &RequestGlance, filters: &RequestFilters) -> Vec<Tile> {
    let mut out: Vec<Tile> = g
        .transports
        .iter()
        .map(|t| transport_tile(t, filters))
        .collect();
    let share = (g.requests > 0).then(|| g.slow as f64 / g.requests as f64 * 100.0);
    out.push(Tile {
        id: "usage-glance-slow",
        label: "Slower than 1 s",
        value: format::count(g.slow),
        suffix: None,
        sub: format!("{} of requests", format::share(share)),
    });
    out.push(Tile {
        id: "usage-glance-errors",
        label: "Errors",
        value: format::count(g.errors),
        suffix: None,
        sub: "4xx, 5xx and network".into(),
    });
    out
}

/// The strip at `content_w` of content width.
pub(super) fn band(
    g: &RequestGlance,
    filters: &RequestFilters,
    content_w: f32,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let per_row = if content_w >= SIX_UP_MIN_W { 6 } else { 3 };
    let tiles = tiles(g, filters);
    let rows = tiles.chunks(per_row).enumerate().map(|(r, chunk)| {
        h_flex()
            .w_full()
            .when(r > 0, |row| row.border_t_1().border_color(theme.border))
            .children(chunk.iter().enumerate().map(|(i, t)| {
                let spoken = SharedString::from(t.spoken());
                v_flex()
                    .id(t.id)
                    .role(gpui::accesskit::Role::Group)
                    .aria_label(spoken)
                    .w(relative(1.0 / per_row as f32))
                    .min_w_0()
                    .px_3()
                    .py_2()
                    .gap_1()
                    .when(i > 0, |tile| tile.border_l_1().border_color(theme.border))
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(t.label),
                    )
                    .child(
                        h_flex()
                            .items_baseline()
                            .gap_1()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .when(t.value == "—", |v| {
                                        v.text_color(theme.muted_foreground)
                                    })
                                    .child(t.value.clone()),
                            )
                            .when_some(t.suffix, |row, s| {
                                row.child(
                                    div().text_sm().text_color(theme.muted_foreground).child(s),
                                )
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(t.sub.clone()),
                    )
            }))
    });
    v_flex()
        .id("usage-glance")
        .role(gpui::accesskit::Role::Group)
        .aria_label("At a glance")
        .w_full()
        .border_1()
        .border_color(theme.border)
        .rounded(theme.radius_lg)
        .children(rows)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage_analytics::surfaces::diagnostics::GLANCE_TRANSPORTS;
    use crate::usage_records::Transport;

    fn glance() -> RequestGlance {
        RequestGlance {
            transports: GLANCE_TRANSPORTS
                .iter()
                .map(|&t| TransportGlance {
                    transport: t,
                    requests: if t == Transport::Rest { 338 } else { 0 },
                    p50_ms: (t == Transport::Rest).then_some(279),
                    p90_ms: (t == Transport::Rest).then_some(707),
                })
                .collect(),
            requests: 338,
            slow: 81,
            errors: 2,
        }
    }

    #[test]
    fn tiles_read_p90_p50_and_count_and_a_dash_when_empty() {
        let t = tiles(&glance(), &RequestFilters::default());
        assert_eq!(t.len(), 6);
        assert_eq!(
            (t[0].value.as_str(), t[0].sub.as_str()),
            ("707 ms", "279 ms p50 · 338 requests")
        );
        assert_eq!(t[0].spoken(), "REST: p90 707 ms, 279 ms p50 · 338 requests");
        assert_eq!(
            (t[1].value.as_str(), t[1].sub.as_str()),
            ("—", "no requests")
        );
        assert_eq!(
            (t[4].value.as_str(), t[4].sub.as_str()),
            ("81", "24% of requests")
        );
        assert_eq!(t[5].sub, "4xx, 5xx and network");
    }

    #[test]
    fn a_transport_the_facet_excludes_says_so() {
        let only_rest = RequestFilters {
            transport: Some(Transport::Rest),
            ..Default::default()
        };
        let t = tiles(&glance(), &only_rest);
        assert_eq!(t[2].sub, "not in this selection");
        assert_eq!(t[0].value, "707 ms");
    }
}
