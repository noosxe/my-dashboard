//! macOS feature gates (docs/0010 D4; #401 interim).
//!
//! The pinned gpui stack never populates the a11y tree on macOS — the
//! platform's `a11y_init` is the trait no-op, so the activation callback
//! that flips the population flag never runs (#401). Tree-backed bridge
//! features are therefore *completely broken* there, not degraded, and
//! fail fast with the tracking-issue link instead of a multi-second poll
//! ending in an opaque error.
//!
//! `DASHBOARD_DEBUG_A11Y=1` is the escape hatch for the planned local
//! force-patch (#401 route A): setting it skips every gate, so the patch
//! can land without re-gating.

/// Pure decision: gate when on macOS and the escape hatch is unset.
/// Shared by the server (`tree.dump`, `input.click_node`,
/// `input.focus_node`) and the CLI (`tree`, `wait-frame`, `click`,
/// `focus`), so both sides fail with the same reason text.
pub fn macos_gate(is_macos: bool, escape_hatch_set: bool) -> Option<&'static str> {
    if is_macos && !escape_hatch_set {
        Some(GATE_REASON)
    } else {
        None
    }
}

/// The live decision for this process, from the platform and the
/// `DASHBOARD_DEBUG_A11Y` environment variable (any non-empty value).
pub fn gate_reason() -> Option<&'static str> {
    let escape_hatch_set = std::env::var("DASHBOARD_DEBUG_A11Y")
        .map(|value| !value.is_empty())
        .unwrap_or(false);
    macos_gate(cfg!(target_os = "macos"), escape_hatch_set)
}

pub const GATE_REASON: &str = "not available on this platform: the a11y \
     tree is never populated on macOS \
     (#401: https://github.com/noosxe/my-dashboard/issues/401)";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_fires_only_on_macos_without_escape_hatch() {
        let reason = macos_gate(true, false).expect("gated on macOS");
        assert!(reason.contains("#401"), "links the tracking issue");
        assert_eq!(macos_gate(true, true), None, "escape hatch skips the gate");
        assert_eq!(macos_gate(false, false), None, "Linux never gated");
        assert_eq!(macos_gate(false, true), None);
    }
}
