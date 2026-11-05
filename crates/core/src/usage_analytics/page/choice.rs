//! One select option for both views' pickers (docs/0082 D7, D10, D19):
//! a value, the label assistive technology reads, and, when it can't be
//! picked, why. Upstream's list row makes no named node (#661), so the
//! option draws its own `Label` node.

use gpui::prelude::*;
use gpui::{App, SharedString, Window, div};
use gpui_component::select::SelectItem;
use gpui_component::{ActiveTheme, h_flex};

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Choice<V> {
    pub(super) value: V,
    title: SharedString,
    /// Why the option can't be picked, and the short form drawn on its
    /// one line.
    unavailable: Option<(&'static str, &'static str)>,
}

impl<V: Clone + PartialEq> Choice<V> {
    pub(super) fn new(value: V, title: impl Into<SharedString>) -> Self {
        Choice {
            value,
            title: title.into(),
            unavailable: None,
        }
    }

    /// The option, disabled: `reason` is read in full, `hint` drawn.
    pub(super) fn unavailable(mut self, reason: &'static str, hint: &'static str) -> Self {
        self.unavailable = Some((reason, hint));
        self
    }
}

impl<V: Clone + PartialEq + 'static> SelectItem for Choice<V> {
    type Value = V;

    fn title(&self) -> SharedString {
        // What assistive technology reads stands on its own (D19).
        match self.unavailable {
            Some((reason, _)) => format!("{}, unavailable: {reason}", self.title).into(),
            None => self.title.clone(),
        }
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        // One line: the list lays its rows out at one uniform height, so
        // a second line would overdraw the next option. The node carries
        // the full title: upstream's list row makes no named node (#661).
        h_flex()
            .id(self.title.clone())
            .role(gpui::accesskit::Role::Label)
            .aria_label(self.title())
            .gap_1()
            .child(self.title.clone())
            .when_some(self.unavailable, |this, (_, hint)| {
                this.child(div().text_color(cx.theme().muted_foreground).child(hint))
            })
    }

    fn value(&self) -> &V {
        &self.value
    }

    fn disabled(&self) -> bool {
        self.unavailable.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unavailable_option_reads_its_reason_and_is_disabled() {
        let on = Choice::new(1, "Count");
        assert_eq!((on.title().as_ref(), on.disabled()), ("Count", false));
        let off = Choice::new(2, "Cost").unavailable("Pick one", "· one only");
        assert_eq!(off.title().as_ref(), "Cost, unavailable: Pick one");
        assert!(off.disabled());
    }
}
