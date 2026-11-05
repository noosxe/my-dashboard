//! The Requests view (docs/0082 §6, D10): the owner as the dashboard's
//! developer. Every figure comes from this app's records (source
//! follows audience, 0082 §4), and it is the only place `auth` and
//! `local` records appear (D16). Under the [`query`] bar, five bands in
//! one scroll: [`glance`], [`trend`], [`ranking`], [`diagnostics`]
//! (collapsed) and the [`log`], last and filling the viewport (Q8).
//! Every band reads the data the page derived off the UI thread (D11);
//! a band whose inputs changed shows its skeleton until its derivation
//! lands (D18). Rendering only lays out what was derived.

use gpui::prelude::*;
use gpui::{AnyElement, Context, Window, div, px};
use gpui_component::table::DataTable;
use gpui_component::{Sizable as _, Size, v_flex};

use super::derive::{self, RequestBand, RequestQuery, RequestsData, request_span};
use super::layout::CONTENT_MAX_W;
use super::{UsagePage, band_frame};
use crate::usage_analytics::surfaces::{LogSort, RequestFilters, Span};

mod diagnostics;
mod format;
mod glance;
mod grid;
mod log;
mod log_table;
mod query;
mod ranking;
mod trend;

pub(super) use query::Controls;

/// The skeletons' heights per band (docs/0002 D10: canvas sizes).
const GLANCE_SKELETON_H: f32 = 84.0;
const TREND_SKELETON_H: f32 = 240.0;
const RANKING_SKELETON_H: f32 = 200.0;
const DIAGNOSTICS_SKELETON_H: f32 = 320.0;
const LOG_SKELETON_H: f32 = 400.0;
/// The page canvas's horizontal inset (`p_4`), to measure content width.
const CANVAS_INSET_W: f32 = 32.0;

/// The view's Empty (D18) when the selection holds no request: one
/// message under the query bar, not five band empties. A facet that
/// narrows the selection is named as the next step.
pub(super) fn empty_copy(filters: &RequestFilters, span: Span) -> String {
    let next = if *filters == RequestFilters::default() {
        "Widen the range or open a GitHub page."
    } else {
        "Widen the range or clear a facet."
    };
    format!(
        "No requests in this selection {}. {next}",
        format::span_phrase(span)
    )
}

impl UsagePage {
    /// D10's controls changed: re-derive the bands they feed (D11).
    pub(super) fn set_request_query(&mut self, q: RequestQuery, cx: &mut Context<Self>) {
        self.requests = q;
        self.rederive_views(cx);
        cx.notify();
    }

    /// The log's header asked for a new order (D10): the log alone
    /// re-derives.
    pub(super) fn set_log_sort(&mut self, sort: LogSort, cx: &mut Context<Self>) {
        let mut q = self.requests.clone();
        q.log_sort = sort;
        self.set_request_query(q, cx);
    }

    /// Whether `band`'s newest data matches the controls; `false` shows
    /// its skeleton (D18).
    pub(super) fn request_band_ready(&self, band: RequestBand) -> bool {
        let target = self.view_key();
        self.views
            .as_ref()
            .and_then(|v| v.requests.as_ref())
            .is_some_and(|r| r.ready(band, &target))
    }

    /// The Requests view's body (D10, D18).
    pub(super) fn render_requests(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let data: Option<RequestsData> = self.views.as_ref().and_then(|v| v.requests.clone());
        if let (Some(c), Some(d)) = (self.requests_controls.as_mut(), &data) {
            c.sync(&self.requests, Some(&d.modules), window, cx);
        }
        let ready = |band: RequestBand| self.request_band_ready(band);
        let span = request_span(self.effective_preset());
        let tz = derive::local_offset_ms(self.as_of_ms);
        let viewport = self.body_scroll.bounds().size;
        let content_w = (viewport.width.as_f32() - CANVAS_INSET_W).min(CONTENT_MAX_W);
        let q = self.requests.clone();

        let body = v_flex()
            .id("usage-requests-body")
            .w_full()
            .gap_6()
            .child(self.requests_query_bar(window, cx));

        let glance = match &data {
            Some(d) if ready(RequestBand::Glance) => {
                if d.glance.requests == 0 {
                    return body
                        .child(band_frame::empty(
                            "usage-requests-empty",
                            empty_copy(&q.filters, span),
                            cx,
                        ))
                        .into_any_element();
                }
                glance::band(&d.glance, &q.filters, content_w, cx)
            }
            _ => band_frame::skeleton("usage-glance-loading", GLANCE_SKELETON_H),
        };

        let trend_body = match &data {
            Some(d) if ready(RequestBand::Trend) => v_flex()
                .gap_3()
                .child(band_frame::summary(
                    trend::summary(&d.trend, span, q.filters.transport),
                    cx,
                ))
                .child(trend::chart(&d.trend, span, tz, cx))
                .into_any_element(),
            _ => band_frame::skeleton("usage-trend-loading", TREND_SKELETON_H),
        };
        let trend_band = band_frame::section(
            "usage-trend",
            trend::title(q.measure),
            None::<gpui::Div>,
            trend_body,
        );

        let ranking = self.ranking_band(data.as_ref().filter(|_| ready(RequestBand::Trend)), cx);

        let diagnostics = self.diagnostics_band(
            data.as_ref()
                .filter(|_| ready(RequestBand::Diagnostics))
                .map(|d| {
                    (
                        d.diagnostics.as_ref(),
                        diagnostics::Frame {
                            span,
                            as_of_ms: d.key.as_of_ms,
                            tz_offset_ms: tz,
                        },
                    )
                }),
            window,
            cx,
        );

        let (log_meta, log_body) = match (&data, &self.requests_controls) {
            (Some(d), Some(c)) if ready(RequestBand::Log) => {
                log::sync_table(c, &d.log, d.key.as_of_ms, tz, cx);
                (
                    Some(log::meta_line(log::meta(&d.log), cx)),
                    log::table(c, viewport.height),
                )
            }
            _ => (
                None,
                band_frame::skeleton("usage-log-loading", LOG_SKELETON_H),
            ),
        };
        let log_band = band_frame::section("usage-log", "Request log", log_meta, log_body);

        body.child(glance)
            .child(trend_band)
            .child(ranking)
            .child(diagnostics)
            .child(log_band)
            .into_any_element()
    }

    /// Band 3: the ranking table, titled by its grouping ("By
    /// operation").
    fn ranking_band(&self, data: Option<&RequestsData>, cx: &mut Context<Self>) -> AnyElement {
        let title = format!("By {}", self.requests.group_by.label().to_lowercase());
        let body = match (data, &self.requests_controls) {
            (Some(d), Some(c)) => {
                if !c.ranking.read(cx).delegate().shows(&d.trend) {
                    let (group_by, then_by) = (self.requests.group_by, self.requests.then_by);
                    let trend = d.trend.clone();
                    c.ranking.update(cx, |t, cx| {
                        t.delegate_mut().set(trend, group_by, then_by);
                        t.refresh(cx);
                    });
                }
                let rows = c
                    .ranking
                    .read(cx)
                    .delegate()
                    .len()
                    .min(ranking::MAX_VISIBLE_ROWS);
                // Header plus rows at the small size's row height: the
                // table sizes to its content up to the cap (D20).
                let row_h = Size::Small.table_row_height();
                div()
                    .id("usage-ranking-table")
                    .w_full()
                    .h(row_h * (rows + 1) as f32 + px(2.))
                    .child(DataTable::new(&c.ranking).small())
                    .into_any_element()
            }
            _ => band_frame::skeleton("usage-ranking-loading", RANKING_SKELETON_H),
        };
        band_frame::section("usage-ranking", title, None::<gpui::Div>, body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage_records::Transport;

    #[test]
    fn the_empty_selection_names_its_next_step() {
        assert_eq!(
            empty_copy(&RequestFilters::default(), Span::H24),
            "No requests in this selection in the last 24 hours. Widen the range or open a GitHub page."
        );
        let narrowed = RequestFilters {
            transport: Some(Transport::Auth),
            ..Default::default()
        };
        assert_eq!(
            empty_copy(&narrowed, Span::H1),
            "No requests in this selection in the last hour. Widen the range or clear a facet."
        );
    }
}
