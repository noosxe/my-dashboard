//! The contrast gate (docs/0065 D4): WCAG 2.1 ratios for every declared
//! semantic fg/bg pair in the bundled Aurora theme. The theme is the
//! design source of truth (`BUNDLED_DEFAULTS` embeds this exact file),
//! so gating it here gates what ships.
//!
//! Thresholds verified against primary W3C text at implementation time
//! (the research round could not reach WebAIM): SC 1.4.3 AA — text
//! ≥ 4.5:1, large-scale text ≥ 3:1; SC 1.4.11 AA — non-text UI
//! components ≥ 3:1, with incidental/decorative/logotype exemptions.
//!
//! Scope ruling (0065 D4 as amended by this PR): the *gate* covers
//! text-class pairs — the strongest, least contestable rule. Non-text
//! slots (hairline borders, quiet chrome) are deliberately exempt by
//! the list at the bottom: the Aurora/Zed-register aesthetic is
//! low-contrast chrome by design, and SC 1.4.11 exempts visual
//! information that is not required to identify components. Forcing
//! 3:1 borders would redesign the app's look — a decision for the
//! owner, recorded here, not smuggled through a test.

use serde_json::Value;

/// WCAG 2.1 relative luminance (sRGB, the spec's exact formula).
fn luminance(hex: &str) -> f64 {
    let hex = hex.trim_start_matches('#');
    let channel = |slice: &str| {
        let raw = u8::from_str_radix(slice, 16).expect("two hex digits") as f64 / 255.0;
        if raw <= 0.039_28 {
            raw / 12.92
        } else {
            ((raw + 0.055) / 1.055).powf(2.4)
        }
    };
    let r = channel(&hex[0..2]);
    let g = channel(&hex[2..4]);
    let b = channel(&hex[4..6]);
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// WCAG 2.1 contrast ratio: (L1 + 0.05) / (L2 + 0.05), lighter first.
fn ratio(foreground: &str, background: &str) -> f64 {
    let l1 = luminance(foreground);
    let l2 = luminance(background);
    (l1.max(l2) + 0.05) / (l1.min(l2) + 0.05)
}

/// The AA minimum for body text (SC 1.4.3).
const TEXT_AA: f64 = 4.5;

/// One gated pair: foreground slot, background slot, both must exist.
const TEXT_PAIRS: &[(&str, &str)] = &[
    // The badge families: text on saturated fills.
    ("danger.foreground", "danger.background"),
    ("success.foreground", "success.background"),
    ("info.foreground", "info.background"),
    ("warning.foreground", "warning.background"),
    ("primary.foreground", "primary.background"),
    ("secondary.foreground", "secondary.background"),
    // Global ink over every surface it is used on.
    ("foreground", "background"),
    ("foreground", "title_bar.background"),
    ("foreground", "status_bar.background"),
    ("foreground", "popover.background"),
    ("foreground", "list.background"),
    ("foreground", "table.background"),
    ("foreground", "tiles.background"),
    ("popover.foreground", "popover.background"),
    // Muted text is still text.
    ("muted.foreground", "background"),
    ("muted.foreground", "muted.background"),
    ("muted.foreground", "list.background"),
    ("muted.foreground", "title_bar.background"),
    // Sidebar / tab / table chrome text.
    ("sidebar.foreground", "sidebar.background"),
    ("sidebar.accent.foreground", "sidebar.accent.background"),
    ("sidebar.primary.foreground", "sidebar.primary.background"),
    ("tab.foreground", "tab_bar.background"),
    ("tab.active.foreground", "tab.active.background"),
    ("table.head.foreground", "table.head.background"),
    ("description_list_label.foreground", "background"),
    // Links are body text.
    ("link.foreground", "background"),
    ("link.hover.foreground", "background"),
    ("accent.foreground", "accent.background"),
];

/// Non-text slots, **exempt by this documented list** (see module doc):
/// hairline borders, bars, thumbs, tracks, fills — the quiet-chrome
/// register. Any future slot promoted INTO gating moves to TEXT_PAIRS
/// with a PR that says why.
const EXEMPT_NON_TEXT_SLOTS: &[&str] = &[
    "border",
    "input.border",
    "window.border",
    "sidebar.border",
    "title_bar.border",
    "status_bar.border",
    "table.row.border",
    "scrollbar.thumb.background",
    "slider.bar.background",
    "slider.thumb.background",
    "switch.background",
    "selection.background",
    "progress_bar.background",
    "tab_bar.segmented.background",
    "caret",
    "drag_border",
];

fn theme_colors() -> Vec<(String, Value)> {
    let raw = include_str!("../assets/themes/aurora.json");
    let parsed: Value = serde_json::from_str(raw).expect("aurora.json parses");
    let themes = parsed["themes"]
        .as_array()
        .expect("themes is a list")
        .clone();
    themes
        .into_iter()
        .map(|theme| {
            let name = theme["name"].as_str().unwrap_or("(unnamed)").to_string();
            (name, theme["colors"].clone())
        })
        .collect()
}

#[test]
fn every_text_pair_meets_aa() {
    for (name, colors) in theme_colors() {
        for (fg, bg) in TEXT_PAIRS {
            let fg_value = colors[fg].as_str().unwrap_or_else(|| {
                panic!("{name}: missing slot {fg} — the pair table is the contract")
            });
            let bg_value = colors[bg]
                .as_str()
                .unwrap_or_else(|| panic!("{name}: missing slot {bg}"));
            let measured = ratio(fg_value, bg_value);
            assert!(
                measured >= TEXT_AA,
                "{name}: {fg} ({fg_value}) on {bg} ({bg_value}) = {measured:.2}:1 \
                 — below AA 4.5:1 (SC 1.4.3)"
            );
        }
    }
}

#[test]
fn exempt_slots_exist_and_are_listed_nowhere_else() {
    // The exemption list is only honest if the slots it names exist and
    // stay out of the gated table.
    for (name, colors) in theme_colors() {
        for slot in EXEMPT_NON_TEXT_SLOTS {
            assert!(
                colors[slot].is_string(),
                "{name}: exempt slot {slot} missing — prune the list"
            );
        }
        for (fg, _bg) in TEXT_PAIRS {
            assert!(
                !EXEMPT_NON_TEXT_SLOTS.contains(fg),
                "{fg} is both gated and exempt — pick one"
            );
        }
    }
}

#[test]
fn luminance_math_pins_the_spec_examples() {
    // White vs black is exactly 21:1.
    assert!((ratio("#ffffff", "#000000") - 21.0).abs() < 0.01);
    assert!((ratio("#000000", "#ffffff") - 21.0).abs() < 0.01);
    // #767676 on white is the classic AA boundary (≈4.54) and #777777
    // sits just below it (≈4.47) — the boundary the gate enforces.
    assert!(ratio("#767676", "#ffffff") >= TEXT_AA);
    assert!(ratio("#777777", "#ffffff") < TEXT_AA);
    // Identical colors are 1:1 — the floor.
    assert!((ratio("#808080", "#808080") - 1.0).abs() < 0.001);
}

#[test]
fn both_themes_are_present() {
    let themes = theme_colors();
    assert_eq!(themes.len(), 2, "expected light + dark exactly");
}
