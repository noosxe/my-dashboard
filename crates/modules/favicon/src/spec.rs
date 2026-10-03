//! `DesignSpec` (docs/0064 D3): the whole generator state as one serde
//! model. Everything the page edits lives here — the state file
//! (`state/favicon.json`, D8), the SVG emitter (D4) and the export plan
//! (D5) are pure functions of it.
//!
//! Compat: every field carries `#[serde(default)]` with a `Default`
//! impl (the 0062 `AppSettings` pattern), so a state file written by an
//! older v1 patch — missing later fields entirely — still loads, and
//! nothing ever hard-fails on a stale file.

use serde::{Deserialize, Serialize};

/// The canonical coordinate space (D4): every raster size is a scale of
/// `512×512`.
pub const VIEW: f32 = 512.0;

/// Curated family list (D11): common across macOS/Linux/Windows plus
/// the popular web fonts people usually have installed. The free-text
/// field is the real escape hatch — fontdb resolves any installed
/// family at raster time.
pub const FONT_FAMILIES: &[&str] = &[
    "System UI",
    "Helvetica",
    "Helvetica Neue",
    "Arial",
    "SF Pro",
    "Inter",
    "Roboto",
    "Verdana",
    "Tahoma",
    "Georgia",
    "Times New Roman",
    "Courier New",
    "Menlo",
    "Monaco",
    "Arial Black",
    "Impact",
];

/// Curated emoji grid (D12): common glyphs that read as brand marks at
/// favicon sizes. Free text covers the rest; raster color support is
/// the S2 spike question, not an input question.
pub const EMOJI_GRID: &[&str] = &[
    "🚀", "⭐", "🔥", "✨", "⚡", "🌈", "🎯", "🧭", "🛠️", "📦", //
    "🧩", "💡", "🔔", "📌", "📎", "🔒", "🔑", "🗝️", "🧠", "🤖", //
    "🐙", "🐝", "🦀", "🐧", "🦊", "🐢", "🦄", "🐳", "🦋", "🐞", //
    "🌱", "🌳", "🌵", "🌸", "🍁", "🌊", "❄️", "☀️", "🌙", "☁️", //
    "❤️", "💙", "💚", "💜", "🖤", "🧡", "💛", "💠", "♦️", "♠️", //
    "👍", "👏", "🙌", "🤝", "✌️", "💪", "🫶", "👀", "🧿", "🎨", //
    "📈", "📊", "🗂️", "📚", "✏️", "📝", "🗓️", "⏰", "🌐", "🔗", //
    "☕", "🍵", "🍺", "🍕", "🍩", "🍪", "🎂", "🎸", "🎮", "🎲", //
    "🟥", "🟧", "🟨", "🟩", "🟦", "🟪", "⬛", "⬜", "🔶", "🔷", //
    "🏁", "🚩", "🎌", "🏴", "🏆", "🥇", "🎖️", "🎗️", "🏅", "🎰", //
];

/// Where the glyph came from (D3/D4). `Image`/`Svg` embed their payload
/// as base64 so the emitted favicon.svg is self-contained and the
/// emit-then-raster rule (D2) has exactly one artifact to render.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Source {
    /// 1–2 characters, a font family, a weight, a glyph scale.
    Text {
        #[serde(default = "default_text")]
        chars: String,
        #[serde(default = "default_family")]
        family: String,
        #[serde(default = "default_weight")]
        weight: u32,
        /// Glyph height as a fraction of the 512 viewBox (D4).
        #[serde(default = "default_scale")]
        scale: f32,
    },
    /// One emoji glyph (D12): emitted as text over the platform emoji
    /// font stack; color raster is the S2 spike question.
    Emoji {
        #[serde(default = "default_emoji")]
        glyph: String,
        #[serde(default = "default_scale")]
        scale: f32,
    },
    /// An uploaded raster (PNG/JPEG), letterboxed onto the background.
    Image {
        #[serde(default)]
        mime: ImageMime,
        #[serde(default)]
        data: String,
        /// Pixel dims, captured at pick time — emit letterboxes from
        /// these without re-decoding the payload.
        #[serde(default)]
        w: u32,
        #[serde(default)]
        h: u32,
    },
    /// An uploaded/pasted SVG, validated via usvg and embedded as a
    /// data URI `<image>` (D4) — usvg resolves nested SVG trees natively
    /// (`ImageKind::SVG`), so the embed is the same code path as rasters.
    Svg {
        #[serde(default)]
        data: String,
        #[serde(default)]
        w: u32,
        #[serde(default)]
        h: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ImageMime {
    #[default]
    Png,
    Jpeg,
}

impl ImageMime {
    pub fn as_str(self) -> &'static str {
        match self {
            ImageMime::Png => "image/png",
            ImageMime::Jpeg => "image/jpeg",
        }
    }
}

fn default_text() -> String {
    "A".into()
}
fn default_family() -> String {
    "Helvetica".into()
}
fn default_weight() -> u32 {
    400
}
fn default_scale() -> f32 {
    0.6
}
fn default_emoji() -> String {
    "🚀".into()
}

impl Default for Source {
    fn default() -> Self {
        Source::Text {
            chars: default_text(),
            family: default_family(),
            weight: default_weight(),
            scale: default_scale(),
        }
    }
}

/// One theme's palette (D4): glyph fill + the two gradient stops, all
/// canonical `#rrggbb`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ThemeColors {
    pub fg: String,
    pub bg_start: String,
    pub bg_end: String,
}

impl Default for ThemeColors {
    fn default() -> Self {
        Self {
            fg: "#ffffff".into(),
            bg_start: "#6366f1".into(),
            bg_end: "#8b5cf6".into(),
        }
    }
}

/// Background shape (D4): a rounded square (`radius_frac` of the half
/// width, 0–0.5) or a full circle.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Shape {
    pub circle: bool,
    pub radius_frac: f32,
}

impl Default for Shape {
    fn default() -> Self {
        Self {
            circle: false,
            radius_frac: 0.18,
        }
    }
}

/// Platform selection (D5): three checkboxes; `ios_1024` is the
/// optional marketing icon inside the iOS row.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Platforms {
    pub web: bool,
    pub ios: bool,
    pub ios_1024: bool,
    pub pwa: bool,
}

impl Default for Platforms {
    fn default() -> Self {
        Self {
            web: true,
            ios: false,
            ios_1024: false,
            pwa: false,
        }
    }
}

/// The whole design (D3). `dark = None` disables the dark palette
/// (no media block in the emitted SVG, D4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct DesignSpec {
    pub source: Source,
    pub light: ThemeColors,
    pub dark: Option<ThemeColors>,
    pub shape: Shape,
    pub platforms: Platforms,
    /// Manifest `name` (D5).
    pub app_name: String,
    /// Last export target (D7) — remembered, never required.
    pub export_dir: Option<String>,
}

impl DesignSpec {
    /// Any platform selected? An empty matrix has nothing to export.
    pub fn any_platform(&self) -> bool {
        self.platforms.web || self.platforms.ios || self.platforms.pwa
    }

    /// The platform set as the usage `ident` code (D9), e.g.
    /// `"web+ios+pwa"` — stable order, only selected platforms.
    pub fn platform_code(&self) -> &'static str {
        match (self.platforms.web, self.platforms.ios, self.platforms.pwa) {
            (true, true, true) => "web+ios+pwa",
            (true, true, false) => "web+ios",
            (true, false, true) => "web+pwa",
            (false, true, true) => "ios+pwa",
            (true, false, false) => "web",
            (false, true, false) => "ios",
            (false, false, true) => "pwa",
            (false, false, false) => "none",
        }
    }
}

/// Canonicalize a user-typed color into `#rrggbb` (lowercase). Accepts
/// `#rgb`, `#rrggbb`, `rgb` and `rrggbb`; `None` = unparseable.
pub fn normalize_hex(input: &str) -> Option<String> {
    let hex = input.trim().trim_start_matches('#');
    let expanded = match hex.len() {
        3 => {
            let bytes = hex.as_bytes();
            if !bytes.iter().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            let (r, g, b) = (bytes[0], bytes[1], bytes[2]);
            format!(
                "{}{}{}{}{}{}",
                r as char, r as char, g as char, g as char, b as char, b as char
            )
            .to_ascii_lowercase()
        }
        6 => hex.to_ascii_lowercase(),
        _ => return None,
    };
    if !expanded.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("#{expanded}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_load_from_empty_json() {
        // A file missing every key (older patch, or `{}`) loads as the
        // default design — the 0062 compat pattern.
        let spec: DesignSpec = serde_json::from_str("{}").unwrap();
        assert_eq!(spec.source, Source::default());
        assert_eq!(spec.light, ThemeColors::default());
        assert!(spec.platforms.web);
        assert_eq!(spec.app_name, "");
        assert_eq!(spec.export_dir, None);
    }

    #[test]
    fn app_name_defaults_empty_but_ui_fills() {
        // Missing app_name loads as "" — the page substitutes "My App"
        // at export time (D5), the file keeps whatever was saved.
        let spec: DesignSpec = serde_json::from_str(r#"{"appName": "Dashboard"}"#).unwrap();
        assert_eq!(spec.app_name, "Dashboard");
    }

    #[test]
    fn roundtrip_camel_case() {
        let spec = DesignSpec {
            dark: Some(ThemeColors {
                fg: "#101014".into(),
                bg_start: "#111827".into(),
                bg_end: "#1f2937".into(),
            }),
            ..Default::default()
        };
        let json = serde_json::to_string(&spec).unwrap();
        assert!(json.contains("\"bgStart\""));
        let back: DesignSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(back, spec);
    }

    #[test]
    fn platform_codes() {
        let mut spec = DesignSpec::default();
        assert_eq!(spec.platform_code(), "web");
        spec.platforms = Platforms {
            web: true,
            ios: true,
            ios_1024: false,
            pwa: true,
        };
        assert_eq!(spec.platform_code(), "web+ios+pwa");
        spec.platforms = Platforms::default();
        spec.platforms.web = false;
        spec.platforms.pwa = true;
        assert_eq!(spec.platform_code(), "pwa");
        spec.platforms.pwa = false;
        assert!(!spec.any_platform());
        assert_eq!(spec.platform_code(), "none");
    }

    #[test]
    fn normalize_hex_forms() {
        assert_eq!(normalize_hex("#FFF"), Some("#ffffff".into()));
        assert_eq!(normalize_hex("6366F1"), Some("#6366f1".into()));
        assert_eq!(normalize_hex("#6366f1"), Some("#6366f1".into()));
        assert_eq!(normalize_hex("#12"), None);
        assert_eq!(normalize_hex("#zzzzzz"), None);
    }
}
