//! The rail's named geometry (docs/0083 D15, docs/0065 Appendix A item
//! 19). Everything else on the rail sits on the gap/padding scale.

/// A person, milestone or other one-line row in a metadata section.
pub(super) const ROW_H: f32 = 26.0;

/// The one-line muted empty state of a section (D11).
pub(super) const EMPTY_ROW_H: f32 = 24.0;

/// The picker's checklist rows scroll inside this height (D12), so the
/// Save and Cancel row under them is always reachable.
pub(super) const PICKER_ROWS_MAX_H: f32 = 240.0;
