//! Built-in theme presets (docs/0074): the pinned gpui-component
//! gallery, vendored under `assets/themes/presets/`.

use gpui::App;
use gpui_component::{ThemeRegistry, ThemeSet};

/// The vendored gallery (docs/0074 D1, as amended by docs/0078 D1) — the
/// kit's `themes/` at the pinned rev, verbatim: every file, `aurora.json`
/// included; no exclusions, no factory pair to protect. Synced by the
/// bump ritual (docs/0074 D8, docs/0078 D8): re-copy on every
/// gpui-component bump; the smoke test below catches shape drift.
pub(crate) const PRESETS: &[(&str, &str)] = &[
    (
        "adventure.json",
        include_str!("../assets/themes/presets/adventure.json"),
    ),
    (
        "alduin.json",
        include_str!("../assets/themes/presets/alduin.json"),
    ),
    (
        "asciinema.json",
        include_str!("../assets/themes/presets/asciinema.json"),
    ),
    (
        "aurora.json",
        include_str!("../assets/themes/presets/aurora.json"),
    ),
    (
        "ayu.json",
        include_str!("../assets/themes/presets/ayu.json"),
    ),
    (
        "catppuccin.json",
        include_str!("../assets/themes/presets/catppuccin.json"),
    ),
    (
        "everforest.json",
        include_str!("../assets/themes/presets/everforest.json"),
    ),
    (
        "fahrenheit.json",
        include_str!("../assets/themes/presets/fahrenheit.json"),
    ),
    (
        "flexoki.json",
        include_str!("../assets/themes/presets/flexoki.json"),
    ),
    (
        "gruvbox.json",
        include_str!("../assets/themes/presets/gruvbox.json"),
    ),
    (
        "harper.json",
        include_str!("../assets/themes/presets/harper.json"),
    ),
    (
        "hybrid.json",
        include_str!("../assets/themes/presets/hybrid.json"),
    ),
    (
        "jellybeans.json",
        include_str!("../assets/themes/presets/jellybeans.json"),
    ),
    (
        "kibble.json",
        include_str!("../assets/themes/presets/kibble.json"),
    ),
    (
        "macos-classic.json",
        include_str!("../assets/themes/presets/macos-classic.json"),
    ),
    (
        "mellifluous.json",
        include_str!("../assets/themes/presets/mellifluous.json"),
    ),
    (
        "molokai.json",
        include_str!("../assets/themes/presets/molokai.json"),
    ),
    (
        "solarized.json",
        include_str!("../assets/themes/presets/solarized.json"),
    ),
    (
        "spaceduck.json",
        include_str!("../assets/themes/presets/spaceduck.json"),
    ),
    (
        "tokyonight.json",
        include_str!("../assets/themes/presets/tokyonight.json"),
    ),
    (
        "twilight.json",
        include_str!("../assets/themes/presets/twilight.json"),
    ),
];

/// Strip `is_default` (docs/0074 D3): the mode-keyed fallback slots
/// stay factory-owned regardless of what a preset ships. The registry
/// never sees a `true` flag from this path.
fn sanitize(src: &str) -> Result<String, serde_json::Error> {
    let mut set: ThemeSet = serde_json::from_str(src)?;
    for theme in &mut set.themes {
        theme.is_default = false;
    }
    serde_json::to_string(&set)
}

/// Load every preset into the registry (docs/0074 D2). Insert-only —
/// `load_themes_from_str` skips names already registered — so the call
/// order in `rebuild_themes` is the authority rule: factory pair,
/// presets, user dir. A user file naming a preset (e.g. a
/// Whiskers-rendered Catppuccin flavor) wins.
pub(crate) fn load(cx: &mut App) {
    for (name, src) in PRESETS {
        match sanitize(src) {
            Ok(json) => {
                if let Err(err) = ThemeRegistry::global_mut(cx).load_themes_from_str(&json) {
                    log::warn!("theme preset rejected: {name}: {err}");
                }
            }
            Err(err) => log::warn!("theme preset ignored: {name}: {err}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PRESETS;
    use gpui_component::ThemeSet;

    /// docs/0074 §4 D4 smoke: every vendored file parses as a
    /// `ThemeSet`; 36 themes; each carries a mode, a non-empty colors
    /// map, and a non-empty `highlight` section (the property the Diff
    /// story depends on); none sets `is_default`; names are unique
    /// across files (a duplicate would be silently insert-only-skipped).
    #[test]
    fn presets_parse_and_carry_highlight() {
        let mut total = 0usize;
        let mut names = std::collections::BTreeSet::new();
        for (file, src) in PRESETS {
            let set: ThemeSet = serde_json::from_str(src)
                .unwrap_or_else(|e| panic!("{file} does not parse as a ThemeSet: {e}"));
            let value: serde_json::Value = serde_json::from_str(src).unwrap();
            let themes = value["themes"].as_array().expect("themes list");
            for theme in themes {
                assert!(theme.get("mode").is_some(), "{file}: mode missing");
                assert!(
                    theme["colors"].as_object().is_some_and(|c| !c.is_empty()),
                    "{file}: empty colors"
                );
                assert!(
                    theme["highlight"]
                        .as_object()
                        .is_some_and(|h| !h.is_empty()),
                    "{file}: empty highlight section"
                );
                assert_ne!(
                    theme.get("is_default"),
                    Some(&serde_json::Value::Bool(true)),
                    "{file}: preset sets is_default"
                );
                assert!(names.insert(theme["name"].as_str().unwrap().to_string()));
                total += 1;
            }
            assert_eq!(set.themes.len(), themes.len());
        }
        assert_eq!(total, 36, "gallery size drifted — re-sync per docs/0074 D8");
        assert_eq!(names.len(), total, "duplicate preset names");
    }

    /// docs/0074 §4 D3: `is_default` never survives the load path.
    #[test]
    fn preset_defaults_cannot_hijack_slots() {
        let tagged = r#"{"themes":[{"name":"T","mode":"dark","is_default":true}]}"#;
        let clean = super::sanitize(tagged).unwrap();
        let set: ThemeSet = serde_json::from_str(&clean).unwrap();
        assert!(set.themes.iter().all(|t| !t.is_default));
    }
}
