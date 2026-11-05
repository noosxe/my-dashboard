//! D10's query bar: the `Transport` facet (segmented, one Tab stop),
//! `Module` and `Status class`, then `Group by`, `Then by` and
//! `Measure`. `cost` is offered only for a single quota transport
//! (Q7): otherwise its option is disabled with a one-line reason, so a
//! trend never pools REST requests and GraphQL points (D2).

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{App, Context, Entity, Focusable as _, SharedString, Window, div, px};
use gpui_component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_component::table::TableState;
use gpui_component::{ActiveTheme, IndexPath, Sizable as _, Size, h_flex, v_flex};

use super::format;
use super::log_table::LogTable;
use super::ranking::RankingTable;
use crate::usage_analytics::Dimension;
use crate::usage_analytics::page::UsagePage;
use crate::usage_analytics::page::derive::RequestQuery;
use crate::usage_analytics::page::header::segmented_stop;
use crate::usage_analytics::surfaces::{Measure, StatusClass};
use crate::usage_records::Transport;

/// A select's slot width (docs/0002 D10). Upstream's `Select` wrapper is
/// `size_full()`, so the parent bounds it.
const PICKER_W: f32 = 150.0;
/// "Every value" in the facets, "no second level" in `Then by`.
const ALL: &str = "All";
const NONE: &str = "(none)";

/// The Transport facet's segments (D10, D16: all four transports).
pub(super) const TRANSPORTS: [Option<Transport>; 5] = [
    None,
    Some(Transport::Rest),
    Some(Transport::Graphql),
    Some(Transport::Auth),
    Some(Transport::Local),
];

const STATUS_CLASSES: [StatusClass; 5] = [
    StatusClass::Success,
    StatusClass::NotModified,
    StatusClass::ClientError,
    StatusClass::ServerError,
    StatusClass::Network,
];

const MEASURES: [Measure; 6] = [
    Measure::Count,
    Measure::Cost,
    Measure::P50,
    Measure::P90,
    Measure::Max,
    Measure::ErrorRate,
];

/// Q7's reason, on the disabled `Cost` option.
pub(super) const COST_REASON: &str = "Pick REST or GraphQL: requests and points never pool";

/// One picker option: its value, its label and, when it can't be picked,
/// why.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Choice<V> {
    value: V,
    title: SharedString,
    reason: Option<&'static str>,
}

impl<V: Clone + PartialEq> Choice<V> {
    fn new(value: V, title: impl Into<SharedString>) -> Self {
        Choice {
            value,
            title: title.into(),
            reason: None,
        }
    }
}

impl<V: Clone + PartialEq + 'static> SelectItem for Choice<V> {
    type Value = V;

    fn title(&self) -> SharedString {
        // What assistive technology reads stands on its own (D19).
        match self.reason {
            Some(r) => format!("{}, unavailable: {r}", self.title).into(),
            None => self.title.clone(),
        }
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        v_flex()
            .child(self.title.clone())
            .when_some(self.reason, |this, r| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(r),
                )
            })
    }

    fn value(&self) -> &V {
        &self.value
    }

    fn disabled(&self) -> bool {
        self.reason.is_some()
    }
}

type Picker<V> = Entity<SelectState<Vec<Choice<V>>>>;

pub(super) fn module_choices(modules: &[String]) -> Vec<Choice<Option<String>>> {
    std::iter::once(Choice::new(None, ALL))
        .chain(
            modules
                .iter()
                .map(|m| Choice::new(Some(m.clone()), m.clone())),
        )
        .collect()
}

fn status_choices() -> Vec<Choice<Option<StatusClass>>> {
    std::iter::once(Choice::new(None, ALL))
        .chain(
            STATUS_CLASSES
                .into_iter()
                .map(|c| Choice::new(Some(c), format::status_class(c))),
        )
        .collect()
}

fn group_choices() -> Vec<Choice<Dimension>> {
    Dimension::all()
        .into_iter()
        .map(|d| Choice::new(d, d.label()))
        .collect()
}

/// `Then by`: none, then every dimension but `Group by`'s.
pub(super) fn then_choices(group_by: Dimension) -> Vec<Choice<Option<Dimension>>> {
    std::iter::once(Choice::new(None, NONE))
        .chain(
            Dimension::all()
                .into_iter()
                .filter(|d| *d != group_by)
                .map(|d| Choice::new(Some(d), d.label())),
        )
        .collect()
}

/// `Measure`, with `Cost` disabled unless a quota transport is picked.
pub(super) fn measure_choices(cost_allowed: bool) -> Vec<Choice<Measure>> {
    MEASURES
        .into_iter()
        .map(|m| Choice {
            value: m,
            title: format::measure(m).into(),
            reason: (m == Measure::Cost && !cost_allowed).then_some(COST_REASON),
        })
        .collect()
}

/// The query after a Transport pick: a `Cost` measure that the new
/// transport can't offer falls back to `Count`, the other bar measure.
pub(super) fn with_transport(q: &RequestQuery, transport: Option<Transport>) -> RequestQuery {
    let mut next = q.clone();
    next.filters.transport = transport;
    if next.measure == Measure::Cost && !next.filters.cost_allowed() {
        next.measure = Measure::Count;
    }
    next
}

fn picker<V: Clone + PartialEq + 'static>(
    items: Vec<Choice<V>>,
    selected: &V,
    window: &mut Window,
    cx: &mut Context<UsagePage>,
) -> Picker<V> {
    let ix = items.iter().position(|c| &c.value == selected).unwrap_or(0);
    cx.new(|cx| SelectState::new(items, Some(IndexPath::new(ix)), window, cx))
}

/// The Requests view's own controls, created with the page because
/// they need a window.
pub(in crate::usage_analytics::page) struct Controls {
    module: Picker<Option<String>>,
    /// The module list the Module picker shows.
    modules: Option<Arc<Vec<String>>>,
    status: Picker<Option<StatusClass>>,
    group_by: Picker<Dimension>,
    then_by: Picker<Option<Dimension>>,
    measure: Picker<Measure>,
    /// Whether the Measure picker currently offers `Cost`.
    cost_offered: bool,
    pub(super) ranking: Entity<TableState<RankingTable>>,
    pub(super) log: Entity<TableState<LogTable>>,
}

impl Controls {
    pub(in crate::usage_analytics::page) fn new(
        q: &RequestQuery,
        window: &mut Window,
        cx: &mut Context<UsagePage>,
    ) -> Self {
        let module = picker(module_choices(&[]), &None, window, cx);
        let status = picker(status_choices(), &q.filters.status_class, window, cx);
        let group_by = picker(group_choices(), &q.group_by, window, cx);
        let then_by = picker(then_choices(q.group_by), &q.then_by, window, cx);
        let cost_offered = q.filters.cost_allowed();
        let measure = picker(measure_choices(cost_offered), &q.measure, window, cx);
        let ranking = cx.new(|cx| TableState::new(RankingTable::empty(), window, cx));
        let page = cx.entity().downgrade();
        let log = cx.new(|cx| TableState::new(LogTable::new(page), window, cx));
        // Upstream's `DataTable` binds Tab to "next column" and never
        // lets focus leave (#711): as a Tab stop it would trap the walk
        // (D19). Both tables stay sortable by pointer, and every cell
        // exposes its text to assistive technology.
        for focus in [
            ranking.read(cx).focus_handle(cx),
            log.read(cx).focus_handle(cx),
        ] {
            let _ = focus.tab_stop(false);
        }

        cx.subscribe_in(&module, window, |this, _, ev, _, cx| {
            let SelectEvent::Confirm(Some(m)) = ev else {
                return;
            };
            let mut q = this.requests.clone();
            q.filters.module = m.clone();
            this.set_request_query(q, cx);
        })
        .detach();
        cx.subscribe_in(&status, window, |this, _, ev, _, cx| {
            let SelectEvent::Confirm(Some(c)) = ev else {
                return;
            };
            let mut q = this.requests.clone();
            q.filters.status_class = *c;
            this.set_request_query(q, cx);
        })
        .detach();
        cx.subscribe_in(&group_by, window, |this, _, ev, window, cx| {
            let SelectEvent::Confirm(Some(dim)) = ev else {
                return;
            };
            let mut q = this.requests.clone();
            q.group_by = *dim;
            if q.then_by == Some(*dim) {
                q.then_by = None;
            }
            if let Some(c) = &this.requests_controls {
                let then_by = q.then_by;
                c.then_by.update(cx, |s, cx| {
                    s.set_items(then_choices(*dim), window, cx);
                    s.set_selected_value(&then_by, window, cx);
                });
            }
            this.set_request_query(q, cx);
        })
        .detach();
        cx.subscribe_in(&then_by, window, |this, _, ev, _, cx| {
            let SelectEvent::Confirm(Some(dim)) = ev else {
                return;
            };
            let mut q = this.requests.clone();
            q.then_by = *dim;
            this.set_request_query(q, cx);
        })
        .detach();
        cx.subscribe_in(&measure, window, |this, _, ev, _, cx| {
            let SelectEvent::Confirm(Some(m)) = ev else {
                return;
            };
            let mut q = this.requests.clone();
            q.measure = *m;
            this.set_request_query(q, cx);
        })
        .detach();

        Controls {
            module,
            modules: None,
            status,
            group_by,
            then_by,
            measure,
            cost_offered,
            ranking,
            log,
        }
    }

    /// Bring the pickers in line with what landed or changed outside
    /// them: the read's module list, and whether `Cost` is on offer.
    /// Called from render, where the window is at hand; cheap when
    /// nothing changed.
    pub(super) fn sync(
        &mut self,
        q: &RequestQuery,
        modules: Option<&Arc<Vec<String>>>,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(m) = modules
            && !self.modules.as_ref().is_some_and(|s| Arc::ptr_eq(s, m))
        {
            let selected = q.filters.module.clone();
            self.module.update(cx, |s, cx| {
                s.set_items(module_choices(m), window, cx);
                s.set_selected_value(&selected, window, cx);
            });
            self.modules = Some(m.clone());
        }
        let allowed = q.filters.cost_allowed();
        if allowed != self.cost_offered {
            let measure = q.measure;
            self.measure.update(cx, |s, cx| {
                s.set_items(measure_choices(allowed), window, cx);
                s.set_selected_value(&measure, window, cx);
            });
            self.cost_offered = allowed;
        }
    }
}

/// A select in its fixed-width slot.
fn slot<V: Clone + PartialEq + 'static>(
    id: &'static str,
    name: &'static str,
    el: Select<Vec<Choice<V>>>,
) -> impl IntoElement {
    div()
        .w(px(PICKER_W))
        .flex_none()
        .child(el.id(id).with_size(Size::Small).accessibility_label(name))
}

impl UsagePage {
    /// D10's query bar, two rows: the facets, then the grouping and the
    /// measure.
    pub(super) fn requests_query_bar(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let selected = TRANSPORTS
            .iter()
            .position(|t| *t == self.requests.filters.transport)
            .unwrap_or(0);
        let transport = segmented_stop(
            "usage-transport",
            "Transport",
            self.focus.as_ref().map(|f| &f.transport),
            TRANSPORTS
                .iter()
                .map(|t| t.map_or(ALL, format::transport))
                .collect(),
            selected,
            |this, ix, cx| {
                if let Some(t) = TRANSPORTS.get(ix) {
                    let q = with_transport(&this.requests, *t);
                    this.set_request_query(q, cx);
                }
            },
            window,
            cx,
        );
        let muted = cx.theme().muted_foreground;
        let label = move |t: &'static str| div().text_sm().text_color(muted).child(t);
        let row = || h_flex().items_center().gap_2().flex_wrap();
        let Some(c) = &self.requests_controls else {
            return v_flex().id("usage-requests-query");
        };
        v_flex()
            .id("usage-requests-query")
            .gap_2()
            .child(
                row()
                    .child(label("Transport"))
                    .child(transport)
                    .child(div().w_2())
                    .child(label("Module"))
                    .child(slot("usage-module", "Module", Select::new(&c.module)))
                    .child(label("Status"))
                    .child(slot("usage-status", "Status class", Select::new(&c.status))),
            )
            .child(
                row()
                    .child(label("Group by"))
                    .child(slot(
                        "usage-requests-group-by",
                        "Group by",
                        Select::new(&c.group_by),
                    ))
                    .child(label("Then by"))
                    .child(slot(
                        "usage-requests-then-by",
                        "Then by",
                        Select::new(&c.then_by),
                    ))
                    .child(label("Measure"))
                    .child(slot("usage-measure", "Measure", Select::new(&c.measure))),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_is_offered_only_for_a_single_quota_transport() {
        let disabled = |allowed| {
            measure_choices(allowed)
                .into_iter()
                .filter(|c| c.disabled())
                .map(|c| c.value)
                .collect::<Vec<_>>()
        };
        assert_eq!(disabled(false), [Measure::Cost]);
        assert!(disabled(true).is_empty());
        let off = measure_choices(false);
        assert_eq!(off[1].title(), format!("Cost, unavailable: {COST_REASON}"));
    }

    #[test]
    fn leaving_a_quota_transport_drops_the_cost_measure_to_count() {
        let q = RequestQuery {
            measure: Measure::Cost,
            ..Default::default()
        };
        let rest = with_transport(&q, Some(Transport::Rest));
        assert_eq!(rest.measure, Measure::Cost);
        let all = with_transport(&rest, None);
        assert_eq!((all.measure, all.filters.transport), (Measure::Count, None));
        let p90 = with_transport(&RequestQuery::default(), Some(Transport::Auth));
        assert_eq!(p90.measure, Measure::P90, "other measures stay");
    }

    #[test]
    fn pickers_list_all_then_values_and_never_self_nest() {
        let m = module_choices(&["favicon".into(), "github".into()]);
        let titles: Vec<_> = m.iter().map(|c| c.title()).collect();
        assert_eq!(titles, ["All", "favicon", "github"]);
        assert_eq!(status_choices().len(), 6);
        let then = then_choices(Dimension::Operation);
        assert_eq!(then[0].value, None);
        assert!(then.iter().all(|c| c.value != Some(Dimension::Operation)));
    }
}
