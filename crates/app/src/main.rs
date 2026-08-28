use gpui::*;
use gpui_component::{button::*, *};

pub struct HelloWorld;

impl Render for HelloWorld {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .v_flex()
            .gap_2()
            .size_full()
            .items_center()
            .justify_center()
            .child("Hello, World!")
            .child(
                Button::new("click-me")
                    .primary()
                    .label("Click me!")
                    .on_click(|_, _, _| println!("Clicked!")),
            )
    }
}

fn main() {
    gpui_platform::application()
        .with_assets(gpui_component_assets::Assets)
        .run(move |cx| {
            // MUST be called before using any gpui-component features.
            gpui_component::init(cx);

            cx.spawn(async move |cx| {
                cx.open_window(WindowOptions::default(), |window, cx| {
                    let view = cx.new(|_| HelloWorld);
                    // The first level in every window MUST be a Root: it hosts
                    // theme, dialogs, sheets, notifications, overlays.
                    cx.new(|cx| Root::new(view, window, cx).bg(cx.theme().background))
                })
                .expect("Failed to open window");
            })
            .detach();
        });
}
