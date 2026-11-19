//! docs/0086 §4.3: the issue page's "synced Nm ago" label has its own
//! render wake, so it advances while nothing else notifies the page
//! (paused, gated or offline ticks).

use super::test_rig::Rig;
use crate::render_wake::testing::notifies_over;

#[gpui::test]
fn the_page_re_renders_when_the_clock_advances(cx: &mut gpui::TestAppContext) {
    let rig = Rig::new(cx, Vec::new(), true);
    let (page, vcx) = rig.open(cx);
    assert_eq!(notifies_over(&page, vcx, 1), 1, "one wake, one re-render");
    assert_eq!(notifies_over(&page, vcx, 2), 2, "it keeps waking");
}
