use gpui::{prelude::FluentBuilder as _, *};
use gpui_component::{
    IconName, TitleBar,
    button::{Button, ButtonVariants},
    h_flex, v_flex,
};

actions!(dashboard, [ToggleTitleBar]);

/// Default chrome state (D7): titlebar-less on Linux (owner runs Hyprland),
/// full title bar elsewhere. Persisted choice arrives in 0004.
#[cfg(target_os = "linux")]
const DEFAULT_TITLEBAR: bool = false;
#[cfg(not(target_os = "linux"))]
const DEFAULT_TITLEBAR: bool = true;

pub struct DashboardShell {
    show_titlebar: bool,
}

impl DashboardShell {
    pub fn new() -> Self {
        Self {
            show_titlebar: DEFAULT_TITLEBAR,
        }
    }

    fn toggle_titlebar(
        &mut self,
        _: &ToggleTitleBar,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_titlebar = !self.show_titlebar;
        cx.notify();
    }
}

impl Render for DashboardShell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .key_context("Dashboard")
            .on_action(cx.listener(Self::toggle_titlebar))
            .when(self.show_titlebar, |this| {
                this.child(
                    // Custom chrome; per-OS adaptive (traffic lights / buttons,
                    // drag, right-click menu, double-click maximize on Linux).
                    TitleBar::new().child(
                        h_flex()
                            .w_full()
                            .pr_2()
                            .justify_between()
                            .child("Dashboard")
                            // Reserved right-side slot (Q6) — fill in 0003+.
                            .child(div()),
                    ),
                )
            })
            .child(
                // Body placeholder — becomes the module host in 0003.
                h_flex()
                    .id("window-body")
                    .relative()
                    .flex_1()
                    .w_full()
                    .items_center()
                    .justify_center()
                    .child("Hello, World!")
                    // Titlebar-less mode (D7): ghost close button, top-right.
                    .when(!self.show_titlebar, |this| {
                        this.child(
                            div().absolute().top_1().right_1().child(
                                Button::new("close")
                                    .ghost()
                                    .icon(IconName::Close)
                                    .on_click(|_, window, _| window.remove_window()),
                            ),
                        )
                    }),
            )
    }
}
