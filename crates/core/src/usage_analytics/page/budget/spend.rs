//! Band 2 (docs/0082 D7): where it went. A query bar (`Group by`, an
//! optional `Then by`), one horizontal bar chart of the top groups plus
//! "other", and the mirroring table. This is this app's recorded spend
//! (GitHub doesn't attribute spend), and the summary line says so (§4
//! principle).

use gpui::prelude::*;
use gpui::{App, Context, Entity, FocusHandle, Focusable as _, SharedString, Window, div, px};
use gpui_component::select::{Select, SelectEvent, SelectState};
use gpui_component::table::{DataTable, TableState};
use gpui_component::{ActiveTheme, Sizable as _, Size, h_flex, v_flex};

use super::spend_table::{MAX_VISIBLE_ROWS, SpendTable};
use super::{band_frame, format};
use crate::usage_analytics::page::UsagePage;
use crate::usage_analytics::page::chart::{self, hbars};
use crate::usage_analytics::page::choice::Choice;
use crate::usage_analytics::page::derive::SpendGroup;
use crate::usage_analytics::surfaces::{Preset, SpendDimension, WHERE_TOP_N, WhereItWent};

/// `Then by`'s "no second level" entry.
const THEN_BY_NONE: &str = "(none)";
/// A select's slot width (docs/0002 D10). Upstream's `Select` wrapper is
/// `size_full()`, so the parent bounds it.
const PICKER_W: f32 = 160.0;

/// The options, each its own named node (D19).
type Options = Vec<Choice<SharedString>>;
type Picker = Entity<SelectState<Options>>;

fn options(labels: Vec<SharedString>) -> Options {
    labels
        .into_iter()
        .map(|l| Choice::new(l.clone(), l))
        .collect()
}

/// The Budget view's own controls: D7's selects and table, created with
/// the page because they need a window.
pub(in crate::usage_analytics::page) struct Controls {
    group_by: Picker,
    then_by: Picker,
    pub(super) table: Entity<TableState<SpendTable>>,
}

fn dimension(label: &str) -> Option<SpendDimension> {
    SpendDimension::all()
        .into_iter()
        .find(|d| d.label() == label)
}

/// `Then by`'s options: none, then every dimension but `Group by`'s (a
/// self-nest is not a question).
fn then_by_labels(group_by: SpendDimension) -> Vec<SharedString> {
    std::iter::once(SharedString::from(THEN_BY_NONE))
        .chain(
            SpendDimension::all()
                .into_iter()
                .filter(|d| *d != group_by)
                .map(|d| SharedString::from(d.label())),
        )
        .collect()
}

impl Controls {
    pub(in crate::usage_analytics::page) fn new(
        spend: SpendGroup,
        window: &mut Window,
        cx: &mut Context<UsagePage>,
    ) -> Self {
        let labels: Vec<SharedString> = SpendDimension::all()
            .into_iter()
            .map(|d| SharedString::from(d.label()))
            .collect();
        let group_by = cx.new(|cx| {
            let mut s = SelectState::new(options(labels), None, window, cx);
            s.set_selected_value(&SharedString::from(spend.group_by.label()), window, cx);
            s
        });
        let then_by = cx.new(|cx| {
            let mut s = SelectState::new(options(then_by_labels(spend.group_by)), None, window, cx);
            let current = spend.then_by.map_or(THEN_BY_NONE, |d| d.label());
            s.set_selected_value(&SharedString::from(current), window, cx);
            s
        });
        let table = cx.new(|cx| TableState::new(SpendTable::empty(), window, cx));

        cx.subscribe_in(
            &group_by,
            window,
            |this, _, ev: &SelectEvent<Options>, window, cx| {
                let SelectEvent::Confirm(Some(label)) = ev else {
                    return;
                };
                let Some(dim) = dimension(label) else { return };
                let mut spend = this.spend;
                spend.group_by = dim;
                if spend.then_by == Some(dim) {
                    spend.then_by = None;
                }
                if let Some(c) = &this.budget_controls {
                    let none =
                        SharedString::from(spend.then_by.map_or(THEN_BY_NONE, |d| d.label()));
                    c.then_by.update(cx, |s, cx| {
                        s.set_items(options(then_by_labels(dim)), window, cx);
                        s.set_selected_value(&none, window, cx);
                    });
                }
                this.set_spend(spend, cx);
            },
        )
        .detach();
        cx.subscribe_in(
            &then_by,
            window,
            |this, _, ev: &SelectEvent<Options>, _, cx| {
                let SelectEvent::Confirm(Some(label)) = ev else {
                    return;
                };
                let mut spend = this.spend;
                spend.then_by = dimension(label);
                this.set_spend(spend, cx);
            },
        )
        .detach();
        Controls {
            group_by,
            then_by,
            table,
        }
    }

    /// Every focus handle these controls own (D19): what a view switch
    /// checks before it unmounts them.
    pub(in crate::usage_analytics::page) fn focus_handles(&self, cx: &App) -> Vec<FocusHandle> {
        vec![
            self.group_by.focus_handle(cx),
            self.then_by.focus_handle(cx),
            self.table.focus_handle(cx),
        ]
    }
}

/// The band's summary line: the unit, the range, the top group, and
/// that these are this app's records (§4 principle). `None` when the
/// budget recorded nothing in range.
pub(super) fn summary(w: &WhereItWent, preset: Preset, group_by: SpendDimension) -> Option<String> {
    if w.totals.requests == 0 {
        return None;
    }
    let mut s = format!(
        "This app's recorded spend: {} {}",
        format::cost_of_sent(w.totals.known_cost, w.totals.requests, w.budget, true),
        format::range_phrase(preset)
    );
    if let Some(top) = w.groups.first().filter(|g| g.cost > 0) {
        s.push_str(&format!(
            "; top {} {} with {} ({})",
            group_by.label().to_lowercase(),
            top.key,
            format::count(top.cost),
            format::share(top.share_pct)
        ));
    }
    s.push('.');
    if w.totals.unknown > 0 {
        s.push_str(&format!(
            " {} had no reported cost.",
            format::requests(w.totals.unknown)
        ));
    }
    Some(s)
}

/// The band's empty copy (D18): band-specific, with the next step.
pub(super) fn empty_copy(w: &WhereItWent, preset: Preset) -> String {
    format!(
        "No {} spend recorded {}. Widen the range or open a GitHub page.",
        super::budget_name(w.budget),
        format::range_phrase(preset)
    )
}

/// The chart's bars: the top groups, then "other" (D7). A compound key
/// draws its top level only; members live in the table.
pub(super) fn chart_rows(w: &WhereItWent) -> Vec<(String, f64)> {
    w.groups
        .iter()
        .take(WHERE_TOP_N)
        .chain(w.other.iter())
        .map(|g| (g.key.clone(), g.cost as f64))
        .collect()
}

/// The chart's a11y summary (D19): every bar's key and value.
fn chart_label(w: &WhereItWent, rows: &[(String, f64)]) -> String {
    let unit = w.budget.unit();
    let bars: Vec<String> = rows
        .iter()
        .map(|(k, v)| format!("{k} {}", format::count(*v as u64)))
        .collect();
    format!("Bar chart of recorded {unit} by group: {}", bars.join(", "))
}

/// Point the table at `w` if it isn't already (a new derivation landed).
pub(super) fn sync_table(
    c: &Controls,
    w: &std::sync::Arc<WhereItWent>,
    spend: SpendGroup,
    cx: &mut App,
) {
    if c.table.read(cx).delegate().shows(w) {
        return;
    }
    c.table.update(cx, |t, cx| {
        t.delegate_mut()
            .set(w.clone(), spend.group_by, spend.then_by);
        t.refresh(cx);
    });
}

/// The band's query bar (D7): `Group by` and `Then by`.
pub(super) fn query_bar(c: &Controls, cx: &App) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;
    let label = |t: &'static str| div().text_sm().text_color(muted).child(t);
    h_flex()
        .items_center()
        .gap_2()
        .child(label("Group by"))
        .child(
            div().w(px(PICKER_W)).flex_none().child(
                Select::new(&c.group_by)
                    .id("usage-group-by")
                    .with_size(Size::Small)
                    .accessibility_label("Group by"),
            ),
        )
        .child(label("Then by"))
        .child(
            div().w(px(PICKER_W)).flex_none().child(
                Select::new(&c.then_by)
                    .id("usage-then-by")
                    .with_size(Size::Small)
                    .accessibility_label("Then by"),
            ),
        )
}

/// Band 2's body under its header: summary, chart and table, or the
/// band's Empty.
pub(super) fn body(
    w: &WhereItWent,
    c: &Controls,
    preset: Preset,
    spend: SpendGroup,
    cx: &App,
) -> gpui::AnyElement {
    let Some(line) = summary(w, preset, spend.group_by) else {
        return band_frame::empty("usage-where-empty", empty_copy(w, preset), cx)
            .into_any_element();
    };
    let rows = chart_rows(w);
    let max = rows.iter().map(|(_, v)| *v).fold(0.0, f64::max);
    let label = chart_label(w, &rows);
    let height = hbars::height(rows.len());
    let table_rows = c.table.read(cx).delegate().len().min(MAX_VISIBLE_ROWS);
    // Header plus rows at the small size's row height: the table sizes
    // to its content up to the cap (D20).
    let row_h = Size::Small.table_row_height();
    let table_h = row_h * (table_rows + 1) as f32 + px(2.);
    v_flex()
        .gap_3()
        .child(band_frame::summary(line, cx))
        .child(
            div()
                .id("usage-where-chart")
                .role(gpui::accesskit::Role::Label)
                .aria_label(label)
                .w_full()
                .h(px(height))
                .child(hbars::HBarChart {
                    rows,
                    ticks: chart::value_ticks(max),
                    unit: w.budget.unit(),
                }),
        )
        .child(
            div()
                .id("usage-where-table")
                .w_full()
                .h(table_h)
                .child(DataTable::new(&c.table).small()),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage_analytics::surfaces::{Budget, SpendRow, SpendTotals};

    fn group(key: &str, cost: u64) -> SpendRow {
        SpendRow {
            key: key.into(),
            cost,
            share_pct: Some(cost as f64 / 100.0 * 100.0),
            requests: 1,
            avg_cost: Some(cost as f64),
            free_304: 0,
            errors: 0,
            unknown: 0,
            children: Vec::new(),
        }
    }

    fn where_(groups: Vec<SpendRow>, unknown: u64) -> WhereItWent {
        WhereItWent {
            budget: Budget::Graphql,
            unit: "points".into(),
            bounds: None,
            totals: SpendTotals {
                requests: 1_318,
                known_cost: 10_625,
                unknown,
            },
            groups,
            other: None,
        }
    }

    #[test]
    fn the_summary_says_it_is_this_apps_recorded_spend() {
        let w = where_(vec![group("pr-page", 62)], 0);
        assert_eq!(
            summary(
                &w,
                Preset::Trailing(crate::usage_analytics::surfaces::Span::H24),
                SpendDimension::Surface
            )
            .as_deref(),
            Some(
                "This app's recorded spend: 10,625 GraphQL points over 1,318 requests in the last 24 hours; top surface pr-page with 62 (62%)."
            )
        );
        let with_unknown = summary(
            &where_(vec![], 3),
            Preset::CurrentWindow,
            SpendDimension::Trigger,
        );
        assert_eq!(
            with_unknown.as_deref(),
            Some(
                "This app's recorded spend: 10,625 GraphQL points over 1,318 requests in this window. 3 requests had no reported cost."
            )
        );
    }

    #[test]
    fn a_rest_summary_tells_counted_requests_from_sent_ones() {
        let mut w = where_(vec![group("pr-page", 71)], 2);
        w.budget = Budget::Rest;
        w.totals.known_cost = 159;
        w.totals.requests = 347;
        assert_eq!(
            summary(
                &w,
                Preset::Trailing(crate::usage_analytics::surfaces::Span::H24),
                SpendDimension::Surface
            )
            .as_deref(),
            Some(
                "This app's recorded spend: 159 REST requests counted against the limit, of 347 sent in the last 24 hours; top surface pr-page with 71 (71%). 2 requests had no reported cost."
            )
        );
    }

    #[test]
    fn an_empty_budget_has_its_own_empty_and_no_summary() {
        let mut w = where_(vec![], 0);
        w.totals = SpendTotals::default();
        assert_eq!(
            summary(&w, Preset::CurrentWindow, SpendDimension::Surface),
            None
        );
        assert_eq!(
            empty_copy(&w, Preset::CurrentWindow),
            "No GraphQL spend recorded in this window. Widen the range or open a GitHub page."
        );
    }

    #[test]
    fn the_chart_draws_the_top_groups_then_other() {
        let mut w = where_((0..9).map(|i| group(&format!("g{i}"), 90 - i)).collect(), 0);
        w.other = Some(group("other", 5));
        let rows = chart_rows(&w);
        assert_eq!(rows.len(), WHERE_TOP_N + 1);
        assert_eq!(rows.last().map(|r| r.0.as_str()), Some("other"));
        assert_eq!(then_by_labels(SpendDimension::Surface)[0], THEN_BY_NONE);
        assert!(!then_by_labels(SpendDimension::Surface).contains(&"Surface".into()));
        assert_eq!(dimension("Operation"), Some(SpendDimension::Operation));
    }
}
