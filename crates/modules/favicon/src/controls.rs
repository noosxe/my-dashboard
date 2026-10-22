//! The page's control mappings (docs/0064 §12), kept pure so the
//! layout-v3 contracts are unit-tested without a window: the shape
//! segmented control onto the unchanged [`Shape`] (L4), palette edits
//! with copy-on-first-dark-edit (L3), the font list (L2), and the
//! export rail's per-platform file lists (L6).

use std::collections::BTreeSet;

use gpui::SharedString;
use gpui_component::searchable_list::SearchableListItem;

use crate::export::{self, Component};
use crate::spec::{DesignSpec, FONT_FAMILIES, Shape, ThemeColors};

// ---- L4: shape ----

/// The radius a fresh Rounded choice restores to — the [`Shape`]
/// default.
pub const DEFAULT_RADIUS: f32 = 0.18;

/// The three shape choices (L4), in segmented-control order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeKind {
    Square,
    Rounded,
    Circle,
}

impl ShapeKind {
    pub const ALL: [ShapeKind; 3] = [ShapeKind::Square, ShapeKind::Rounded, ShapeKind::Circle];

    pub fn label(self) -> &'static str {
        match self {
            ShapeKind::Square => "Square",
            ShapeKind::Rounded => "Rounded",
            ShapeKind::Circle => "Circle",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|k| *k == self).unwrap_or(1)
    }

    /// Read the choice off the spec (the L4 table).
    pub fn of(shape: &Shape) -> Self {
        if shape.circle {
            ShapeKind::Circle
        } else if shape.radius_frac > 0.0 {
            ShapeKind::Rounded
        } else {
            ShapeKind::Square
        }
    }
}

/// Apply a shape choice (L4 table). `last_radius` is the model's held
/// non-zero radius; it is updated from the outgoing shape before a
/// Square choice zeroes it, and restored by Rounded.
pub fn apply_shape(shape: &mut Shape, kind: ShapeKind, last_radius: &mut f32) {
    if !shape.circle && shape.radius_frac > 0.0 {
        *last_radius = shape.radius_frac;
    }
    match kind {
        ShapeKind::Square => {
            shape.circle = false;
            shape.radius_frac = 0.0;
        }
        ShapeKind::Rounded => {
            shape.circle = false;
            shape.radius_frac = if *last_radius > 0.0 {
                *last_radius
            } else {
                DEFAULT_RADIUS
            };
        }
        ShapeKind::Circle => shape.circle = true,
    }
}

/// The held radius a model starts with: the stored one when Rounded,
/// else the default.
pub fn initial_radius(shape: &Shape) -> f32 {
    if shape.radius_frac > 0.0 {
        shape.radius_frac
    } else {
        DEFAULT_RADIUS
    }
}

// ---- L3: palette ----

/// The three palette slots the Colors fields edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Glyph,
    Start,
    End,
}

/// The palette the Colors fields show in `dark` mode: the dark palette
/// when it exists, else the light one it reuses.
pub fn shown_palette(spec: &DesignSpec, dark: bool) -> &ThemeColors {
    match (&spec.dark, dark) {
        (Some(palette), true) => palette,
        _ => &spec.light,
    }
}

/// One Colors-field edit (L3). The first edit in Dark mode creates the
/// dark palette as a copy of the light one, then applies the change.
pub fn set_color(spec: &mut DesignSpec, dark: bool, slot: Slot, hex: String) {
    let palette = if dark {
        let light = spec.light.clone();
        spec.dark.get_or_insert(light)
    } else {
        &mut spec.light
    };
    match slot {
        Slot::Glyph => palette.fg = hex,
        Slot::Start => palette.bg_start = hex,
        Slot::End => palette.bg_end = hex,
    }
}

/// "Use light colors" (L3): drop the dark palette.
pub fn reset_dark(spec: &mut DesignSpec) {
    spec.dark = None;
}

// ---- L2: fonts ----

/// One font-select row: the family value plus its display title — the
/// family itself, or "<family> (not installed)" for a stored family
/// this machine lacks (D11's fallback stays visible).
#[derive(Clone, Debug, PartialEq)]
pub struct FontChoice {
    pub family: SharedString,
    pub title: SharedString,
}

impl SearchableListItem for FontChoice {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &SharedString {
        &self.family
    }
}

/// Every installed family name (the primary, English-first name of
/// each face), de-duplicated and sorted.
pub fn installed_families(db: &resvg::usvg::fontdb::Database) -> BTreeSet<String> {
    db.faces()
        .filter_map(|face| face.families.first().map(|(name, _)| name.clone()))
        .collect()
}

/// The font list (L2): the curated families that are installed, then —
/// with `all` — every other installed family in sorted order. The
/// `selected` family is always listed (appended when the list lacks
/// it), titled "(not installed)" when this machine doesn't have it.
pub fn font_choices(installed: &BTreeSet<String>, all: bool, selected: &str) -> Vec<FontChoice> {
    let plain = |family: &str| FontChoice {
        family: SharedString::from(family.to_string()),
        title: SharedString::from(family.to_string()),
    };
    let mut out: Vec<FontChoice> = FONT_FAMILIES
        .iter()
        .filter(|f| installed.contains(**f))
        .map(|f| plain(f))
        .collect();
    if all {
        let curated: BTreeSet<&str> = FONT_FAMILIES.iter().copied().collect();
        out.extend(
            installed
                .iter()
                .filter(|f| !curated.contains(f.as_str()))
                .map(|f| plain(f)),
        );
    }
    if !selected.is_empty() && !out.iter().any(|c| c.family.as_ref() == selected) {
        out.push(if installed.contains(selected) {
            plain(selected)
        } else {
            FontChoice {
                family: SharedString::from(selected.to_string()),
                title: SharedString::from(format!("{selected} (not installed)")),
            }
        });
    }
    out
}

// ---- L6: export rail ----

/// The rail's platform rows, in checklist order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Web,
    Ios,
    Pwa,
}

impl Platform {
    pub const ALL: [Platform; 3] = [Platform::Web, Platform::Ios, Platform::Pwa];

    pub fn key(self) -> &'static str {
        match self {
            Platform::Web => "web",
            Platform::Ios => "ios",
            Platform::Pwa => "pwa",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Platform::Web => "Web",
            Platform::Ios => "iOS",
            Platform::Pwa => "Android / PWA",
        }
    }

    pub fn checked(self, spec: &DesignSpec) -> bool {
        match self {
            Platform::Web => spec.platforms.web,
            Platform::Ios => spec.platforms.ios,
            Platform::Pwa => spec.platforms.pwa,
        }
    }
}

/// The files one platform row writes (L6): [`export::plan`] of the spec
/// with only that platform selected (the iOS 1024 option kept), so the
/// rail can't drift from what an export writes. Listed whether or not
/// the row is checked — the row says what checking it would add.
pub fn platform_files(spec: &DesignSpec, platform: Platform) -> Vec<Component> {
    let mut only = spec.clone();
    only.platforms.web = platform == Platform::Web;
    only.platforms.ios = platform == Platform::Ios;
    only.platforms.pwa = platform == Platform::Pwa;
    only.platforms.ios_1024 = platform == Platform::Ios && spec.platforms.ios_1024;
    export::plan(&only)
}

/// "N file(s)" for counts and the commit button.
pub fn files_label(n: usize) -> String {
    format!("{n} {}", if n == 1 { "file" } else { "files" })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::Platforms;

    // ---- L4 ----

    #[test]
    fn shape_mapping_round_trips() {
        let mut shape = Shape::default();
        let mut held = initial_radius(&shape);
        assert_eq!(ShapeKind::of(&shape), ShapeKind::Rounded);

        apply_shape(&mut shape, ShapeKind::Square, &mut held);
        assert_eq!(
            shape,
            Shape {
                circle: false,
                radius_frac: 0.0
            }
        );
        assert_eq!(ShapeKind::of(&shape), ShapeKind::Square);

        apply_shape(&mut shape, ShapeKind::Circle, &mut held);
        assert!(shape.circle);
        assert_eq!(ShapeKind::of(&shape), ShapeKind::Circle);

        apply_shape(&mut shape, ShapeKind::Rounded, &mut held);
        assert_eq!(ShapeKind::of(&shape), ShapeKind::Rounded);
        assert_eq!(shape.radius_frac, DEFAULT_RADIUS);
    }

    #[test]
    fn rounded_restores_last_radius() {
        let mut shape = Shape {
            circle: false,
            radius_frac: 0.31,
        };
        let mut held = initial_radius(&shape);
        apply_shape(&mut shape, ShapeKind::Square, &mut held);
        apply_shape(&mut shape, ShapeKind::Rounded, &mut held);
        assert_eq!(shape.radius_frac, 0.31);

        // Through Circle too: the radius survives on the spec and the
        // held value.
        apply_shape(&mut shape, ShapeKind::Circle, &mut held);
        apply_shape(&mut shape, ShapeKind::Square, &mut held);
        apply_shape(&mut shape, ShapeKind::Rounded, &mut held);
        assert_eq!(shape.radius_frac, 0.31);
    }

    #[test]
    fn square_spec_starts_with_default_held_radius() {
        let mut shape = Shape {
            circle: false,
            radius_frac: 0.0,
        };
        let mut held = initial_radius(&shape);
        apply_shape(&mut shape, ShapeKind::Rounded, &mut held);
        assert_eq!(shape.radius_frac, DEFAULT_RADIUS);
    }

    // ---- L3 ----

    #[test]
    fn first_dark_edit_copies_light_palette() {
        let mut spec = DesignSpec::default();
        spec.light.fg = "#111111".into();
        assert!(spec.dark.is_none());
        assert_eq!(shown_palette(&spec, true), &spec.light);

        set_color(&mut spec, true, Slot::Start, "#000000".into());
        let dark = spec.dark.clone().expect("dark palette created");
        assert_eq!(dark.bg_start, "#000000");
        assert_eq!(dark.fg, "#111111", "copied from light, not defaulted");
        assert_eq!(dark.bg_end, spec.light.bg_end);
        assert_ne!(spec.light.bg_start, "#000000", "light untouched");

        // Later dark edits change only dark.
        set_color(&mut spec, true, Slot::Glyph, "#222222".into());
        assert_eq!(spec.dark.as_ref().unwrap().fg, "#222222");
        assert_eq!(spec.light.fg, "#111111");
    }

    #[test]
    fn light_edit_never_creates_dark() {
        let mut spec = DesignSpec::default();
        set_color(&mut spec, false, Slot::End, "#abcdef".into());
        assert_eq!(spec.light.bg_end, "#abcdef");
        assert!(spec.dark.is_none());
    }

    #[test]
    fn reset_clears_dark() {
        let mut spec = DesignSpec::default();
        set_color(&mut spec, true, Slot::Glyph, "#222222".into());
        reset_dark(&mut spec);
        assert!(spec.dark.is_none());
        assert_eq!(shown_palette(&spec, true), &spec.light);
    }

    // ---- L2 ----

    fn installed(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn families(choices: &[FontChoice]) -> Vec<&str> {
        choices.iter().map(|c| c.family.as_ref()).collect()
    }

    #[test]
    fn fonts_curated_by_default() {
        let fonts = installed(&["Zapfino", "Georgia", "Helvetica", "Arial"]);
        let list = font_choices(&fonts, false, "Helvetica");
        // Curated order, installed only; Zapfino isn't curated.
        assert_eq!(families(&list), ["Helvetica", "Arial", "Georgia"]);
    }

    #[test]
    fn all_fonts_lists_curated_first_then_sorted_rest() {
        let fonts = installed(&["Zapfino", "Georgia", "Helvetica", "Avenir", "Arial"]);
        let list = font_choices(&fonts, true, "Helvetica");
        assert_eq!(
            families(&list),
            ["Helvetica", "Arial", "Georgia", "Avenir", "Zapfino"]
        );
        // De-duplicated: no family twice.
        let unique: BTreeSet<&str> = families(&list).into_iter().collect();
        assert_eq!(unique.len(), list.len());
    }

    #[test]
    fn selection_stays_listed_when_toggle_off() {
        let fonts = installed(&["Zapfino", "Helvetica"]);
        let list = font_choices(&fonts, false, "Zapfino");
        assert_eq!(families(&list), ["Helvetica", "Zapfino"]);
        assert_eq!(list[1].title.as_ref(), "Zapfino");
        // Toggled on, it isn't listed twice.
        let all = font_choices(&fonts, true, "Zapfino");
        assert_eq!(families(&all), ["Helvetica", "Zapfino"]);
    }

    #[test]
    fn missing_stored_family_reads_not_installed() {
        let fonts = installed(&["Helvetica"]);
        let list = font_choices(&fonts, false, "Comic Mono");
        let last = list.last().unwrap();
        assert_eq!(last.family.as_ref(), "Comic Mono");
        assert_eq!(last.title.as_ref(), "Comic Mono (not installed)");
        // A curated family missing here is listed the same way.
        let curated = font_choices(&fonts, true, "Inter");
        assert_eq!(
            curated.last().unwrap().title.as_ref(),
            "Inter (not installed)"
        );
    }

    // ---- L6 ----

    #[test]
    fn rail_lists_equal_export_plan_for_every_combination() {
        for bits in 0..16u8 {
            let spec = DesignSpec {
                platforms: Platforms {
                    web: bits & 1 != 0,
                    ios: bits & 2 != 0,
                    ios_1024: bits & 4 != 0,
                    pwa: bits & 8 != 0,
                },
                ..DesignSpec::default()
            };
            let from_rail: Vec<Component> = Platform::ALL
                .iter()
                .filter(|p| p.checked(&spec))
                .flat_map(|p| platform_files(&spec, *p))
                .collect();
            assert_eq!(from_rail, export::plan(&spec), "platforms {bits:04b}");
            let total: usize = Platform::ALL
                .iter()
                .filter(|p| p.checked(&spec))
                .map(|p| platform_files(&spec, *p).len())
                .sum();
            assert_eq!(total, export::plan(&spec).len());
        }
    }

    #[test]
    fn rail_counts_per_platform() {
        let mut spec = DesignSpec::default();
        assert_eq!(platform_files(&spec, Platform::Web).len(), 4);
        assert_eq!(platform_files(&spec, Platform::Ios).len(), 1);
        assert_eq!(platform_files(&spec, Platform::Pwa).len(), 3);
        spec.platforms.ios_1024 = true;
        assert_eq!(platform_files(&spec, Platform::Ios).len(), 2);
        assert_eq!(files_label(1), "1 file");
        assert_eq!(files_label(7), "7 files");
    }
}
