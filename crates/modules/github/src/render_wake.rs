//! The render-only wake for clock-driven labels (docs/0086 §3 rule 3,
//! §4.3; docs/0052 D6). A retained-mode view re-renders only when its
//! entity is notified, so a label such as "synced 3m ago" needs its own
//! timer: nothing global repaints on its behalf. The wake only notifies;
//! it never dispatches a fetch (docs/0052 D1 forbids timer-driven
//! fetches, not repaints).

use gpui::{App, Context};

use crate::page::RENDER_WAKE;

/// Notify the view every [`RENDER_WAKE`] while `wanted` holds. The loop
/// ends with the entity.
pub(crate) fn spawn<T: 'static>(cx: &mut Context<T>, wanted: impl Fn(&T, &App) -> bool + 'static) {
    cx.spawn(async move |this, cx| {
        loop {
            cx.background_executor().timer(RENDER_WAKE).await;
            let alive = this.update(cx, |view, cx| {
                if wanted(view, cx) {
                    cx.notify();
                }
            });
            if alive.is_err() {
                break;
            }
        }
    })
    .detach();
}

/// Test helper: count the notifies a view receives while the clock
/// advances one wake period. Shared by the per-page wake tests.
#[cfg(test)]
pub(crate) mod testing {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::{Entity, VisualTestContext};

    use crate::page::RENDER_WAKE;

    /// Observe `entity`, advance the pinned clock by `periods` wakes and
    /// return how many notifies landed.
    pub(crate) fn notifies_over<T: 'static>(
        entity: &Entity<T>,
        vcx: &mut VisualTestContext,
        periods: u32,
    ) -> usize {
        let seen = Rc::new(Cell::new(0usize));
        let _sub = vcx.update(|_, cx| {
            let seen = seen.clone();
            cx.observe(entity, move |_, _| seen.set(seen.get() + 1))
        });
        for _ in 0..periods {
            vcx.executor()
                .advance_clock(RENDER_WAKE + std::time::Duration::from_millis(1));
            vcx.run_until_parked();
        }
        seen.get()
    }
}
