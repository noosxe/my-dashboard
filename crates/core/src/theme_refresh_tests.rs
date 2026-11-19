//! docs/0086 §3/§4.4: the theme switch ends in `cx.refresh_windows()`.
//! Checked on the gpui-fast stack: it re-runs the render of a *cached*
//! view, so views that read the theme in `render` restyle without a
//! per-view notify. If a gpui-fast bump breaks this, the theme path
//! must notify instead.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, StyleRefinement,
    Styled, Window, div,
};
use gpui_component::ActiveTheme as _;

/// Records the theme mode each time its render runs.
struct Probe {
    renders: Rc<Cell<usize>>,
    dark: Rc<Cell<bool>>,
}

impl Render for Probe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.renders.set(self.renders.get() + 1);
        self.dark.set(cx.theme().mode.is_dark());
        div().size_full()
    }
}

/// Hosts the probe the way retained mode caches a view.
struct Host(Entity<Probe>);

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(
            self.0
                .clone()
                .cached(StyleRefinement::default().size_full()),
        )
    }
}

#[gpui::test]
fn theme_switch_restyles_a_cached_view(cx: &mut gpui::TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(|cx| crate::settings::init(None, None, cx));
    let renders = Rc::new(Cell::new(0));
    let dark = Rc::new(Cell::new(false));
    let (r, d) = (renders.clone(), dark.clone());
    let (_host, vcx) = cx.add_window_view(move |_, cx| {
        Host(cx.new(|_| Probe {
            renders: r,
            dark: d,
        }))
    });
    vcx.run_until_parked();
    let first = renders.get();
    vcx.run_until_parked();
    assert_eq!(renders.get(), first, "an idle cached view is not re-run");

    vcx.update(|_, cx| crate::settings::set_theme_mode("dark", cx));
    vcx.run_until_parked();
    assert!(dark.get(), "the cached view re-rendered against dark");
    vcx.update(|_, cx| crate::settings::set_theme_mode("light", cx));
    vcx.run_until_parked();
    assert!(!dark.get(), "and back to light");
    assert!(renders.get() > first);
}
