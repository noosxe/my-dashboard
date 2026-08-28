//! Clock sample module (docs/0003 §5.5).
//!
//! Proves the module contract's async path: a 1 s service timer writes to a
//! single state entity; the overview and main-page views both observe it and
//! stay live (and both survive page switches — the views are kept alive).

use chrono::{DateTime, Local};
use gpui::*;

use gpui_component::{ActiveTheme, h_flex, v_flex};

use dashboard_core::Module;
use dashboard_core::module::{ModuleEntry, ModuleMeta};
use dashboard_core::nav::Navigator;

pub struct ClockModule;

/// The single state entity (owned by the views, written by the service).
struct ClockState {
    now: DateTime<Local>,
}

impl Module for ClockModule {
    fn meta() -> ModuleMeta {
        ModuleMeta {
            id: "clock".into(),
            title: "Clock".into(),
            icon: None,
        }
    }

    fn init(_nav: &Entity<Navigator>, _window: &mut Window, cx: &mut App) -> ModuleEntry {
        // 1. State entity.
        let state = cx.new(|_| ClockState { now: Local::now() });

        // 2. Service: 1 s background timer — the module's whole backend.
        let timer_state = state.clone();
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                cx.update(|cx| timer_state.update(cx, |s, _| s.now = Local::now()));
            }
        })
        .detach();

        // 3. Two views over the same entity.
        ModuleEntry {
            meta: Self::meta(),
            overview: cx.new(|cx| ClockOverview::new(state.clone(), cx)).into(),
            main_page: cx.new(|cx| ClockPage::new(state.clone(), cx)).into(),
        }
    }
}

/// Home-card content: compact time.
struct ClockOverview {
    state: Entity<ClockState>,
}

impl ClockOverview {
    fn new(state: Entity<ClockState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self { state }
    }
}

impl Render for ClockOverview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = self.state.read(cx).now;
        h_flex().size_full().items_center().child(
            div()
                .text_size(px(28.))
                .child(now.format("%H:%M:%S").to_string()),
        )
    }
}

/// Main page: large time + date.
struct ClockPage {
    state: Entity<ClockState>,
}

impl ClockPage {
    fn new(state: Entity<ClockState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self { state }
    }
}

impl Render for ClockPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = self.state.read(cx).now;
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                div()
                    .text_size(px(64.))
                    .child(now.format("%H:%M:%S").to_string()),
            )
            .child(
                div()
                    .text_size(px(16.))
                    .text_color(cx.theme().muted_foreground)
                    .child(now.format("%A, %e %B %Y").to_string()),
            )
    }
}
