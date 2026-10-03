//! The export pipeline (docs/0064 D5/D7): the platform/component
//! matrix as descriptors, the pre-flight plan (exact files +
//! collisions), and one background pass that renders, encodes and
//! writes each component **atomically**.

use std::path::Path;

use crate::render::{self, RenderError};
use crate::spec::DesignSpec;

/// One exportable component (D5). Descriptors — not a hardcoded loop —
/// so #429 (Windows tiles) and #430 (adaptive icons) later grow the
/// matrix without redesign.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Component {
    /// The emitted SVG verbatim.
    FaviconSvg,
    /// Hand-rolled ICO, 16+32 PNG frames (D6).
    FaviconIco,
    Png96,
    AppleTouch180,
    Icon1024,
    Icon192,
    Icon512,
    SiteWebmanifest,
    /// The `favicon.html` snippet — the copy-paste head block.
    Snippet,
}

impl Component {
    /// The plan-time file name (relative to the export dir).
    pub fn file_name(self) -> &'static str {
        match self {
            Component::FaviconSvg => "favicon.svg",
            Component::FaviconIco => "favicon.ico",
            Component::Png96 => "favicon-96x96.png",
            Component::AppleTouch180 => "apple-touch-icon.png",
            Component::Icon1024 => "icon-1024.png",
            Component::Icon192 => "icon-192.png",
            Component::Icon512 => "icon-512.png",
            Component::SiteWebmanifest => "site.webmanifest",
            Component::Snippet => "favicon.html",
        }
    }
}

/// The files the current platform selection produces, in write order
/// (SVG first — it's also the raster source — then rasters, then the
/// derived manifest/snippet).
pub fn plan(spec: &DesignSpec) -> Vec<Component> {
    let mut out = Vec::new();
    let p = &spec.platforms;
    if p.web {
        out.extend([
            Component::FaviconSvg,
            Component::FaviconIco,
            Component::Png96,
            Component::Snippet,
        ]);
    }
    if p.ios {
        out.push(Component::AppleTouch180);
        if p.ios_1024 {
            out.push(Component::Icon1024);
        }
    }
    if p.pwa {
        out.extend([
            Component::Icon192,
            Component::Icon512,
            Component::SiteWebmanifest,
        ]);
    }
    out
}

/// Pre-flight (D7): which planned files already exist in `dir`.
pub fn collisions(dir: &Path, plan: &[Component]) -> Vec<String> {
    plan.iter()
        .filter(|c| dir.join(c.file_name()).exists())
        .map(|c| c.file_name().to_string())
        .collect()
}

/// The `site.webmanifest` body (D5): exactly the selected icons.
pub fn manifest_json(spec: &DesignSpec) -> String {
    let p = &spec.platforms;
    let mut icons = Vec::new();
    if p.pwa {
        icons.push(icon_entry("/icon-192.png", "192x192"));
        icons.push(icon_entry("/icon-512.png", "512x512"));
    }
    if p.ios {
        if p.ios_1024 {
            icons.push(icon_entry("/icon-1024.png", "1024x1024"));
        }
        icons.push(icon_entry("/apple-touch-icon.png", "180x180"));
    }
    format!(
        "{{\n  \"name\": \"{name}\",\n  \"icons\": [\n{icons}\n  ]\n}}",
        name = escape_json(&effective_app_name(spec)),
        icons = icons.join(",\n"),
    )
}

fn icon_entry(src: &str, sizes: &str) -> String {
    format!("    {{ \"src\": \"{src}\", \"sizes\": \"{sizes}\", \"type\": \"image/png\" }}")
}

/// The `favicon.html` snippet (D5): links for exactly the selected
/// files, plus per-theme `theme-color` metas when the dark palette is
/// on. Comment header says what to do with it.
pub fn snippet_html(spec: &DesignSpec) -> String {
    let p = &spec.platforms;
    let mut lines = vec!["<!-- Favicon links — paste into <head> (docs/0064 D5) -->".to_string()];
    if p.web {
        lines.push("<link rel=\"icon\" href=\"/favicon.svg\" type=\"image/svg+xml\">".into());
        lines.push("<link rel=\"icon\" href=\"/favicon.ico\" sizes=\"32x32\">".into());
        lines.push(
            "<link rel=\"icon\" href=\"/favicon-96x96.png\" type=\"image/png\" sizes=\"96x96\">"
                .into(),
        );
    }
    if p.ios {
        lines.push("<link rel=\"apple-touch-icon\" href=\"/apple-touch-icon.png\">".into());
    }
    if p.pwa {
        lines.push("<link rel=\"manifest\" href=\"/site.webmanifest\">".into());
    }
    if let Some(dark) = &spec.dark {
        lines.push(format!(
            "<meta name=\"theme-color\" media=\"(prefers-color-scheme: light)\" content=\"{}\">",
            spec.light.bg_end
        ));
        lines.push(format!(
            "<meta name=\"theme-color\" media=\"(prefers-color-scheme: dark)\" content=\"{}\">",
            dark.bg_end
        ));
    } else {
        lines.push(format!(
            "<meta name=\"theme-color\" content=\"{}\">",
            spec.light.bg_end
        ));
    }
    lines.join("\n")
}

/// The manifest name (D5): the spec's `app_name`, `"My App"` when
/// empty.
pub fn effective_app_name(spec: &DesignSpec) -> String {
    let name = spec.app_name.trim();
    if name.is_empty() {
        "My App".to_string()
    } else {
        name.to_string()
    }
}

fn escape_json(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"My App\"".into())
}

/// What one export run produced (D7): per-file outcomes, never a
/// process crash — a failing component is reported, earlier writes
/// stay.
#[derive(Debug, Default)]
pub struct ExportOutcome {
    pub written: Vec<String>,
    pub failed: Vec<(String, String)>,
}

impl ExportOutcome {
    pub fn ok(&self) -> bool {
        self.failed.is_empty() && !self.written.is_empty()
    }
}

/// Write `bytes` atomically: sibling `.tmp` file, then rename over the
/// target (the `save_json_atomic` precedent) — a failed export never
/// leaves a half-written file behind.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("favicon-tmp");
    std::fs::write(&tmp, bytes)?;
    // Same-directory rename is atomic on every supported platform.
    std::fs::rename(&tmp, path)
}

/// Run the whole plan (D7): one emit, all rasters, all files. Blocking
/// by design — the page calls this on the background executor.
pub fn export(
    spec: &DesignSpec,
    dir: &Path,
    db: &std::sync::Arc<resvg::usvg::fontdb::Database>,
) -> ExportOutcome {
    let mut outcome = ExportOutcome::default();
    let plan = plan(spec);
    if let Err(err) = std::fs::create_dir_all(dir) {
        outcome
            .failed
            .push((dir.display().to_string(), format!("create dir: {err}")));
        return outcome;
    }

    let svg = render::emit_svg(spec);
    // The union of raster sizes the plan needs — one raster each.
    let mut sizes: Vec<u32> = Vec::new();
    for c in &plan {
        match c {
            Component::FaviconIco => {
                sizes.push(16);
                sizes.push(32);
            }
            Component::Png96 => sizes.push(96),
            Component::AppleTouch180 => sizes.push(180),
            Component::Icon1024 => sizes.push(1024),
            Component::Icon192 => sizes.push(192),
            Component::Icon512 => sizes.push(512),
            _ => {}
        }
    }
    let frames: Vec<(u32, Vec<u8>)> = match render::render_all(spec, &sizes, db) {
        Ok((_, frames)) => frames,
        Err(err) => {
            outcome.failed.push(("render".into(), err.to_string()));
            return outcome;
        }
    };
    let frame =
        |size: u32| -> Option<&Vec<u8>> { frames.iter().find(|(s, _)| *s == size).map(|(_, p)| p) };

    for component in &plan {
        let path = dir.join(component.file_name());
        let result: Result<Vec<u8>, RenderError> = match component {
            Component::FaviconSvg => Ok(svg.clone().into_bytes()),
            Component::FaviconIco => match (frame(16), frame(32)) {
                (Some(a), Some(b)) => Ok(render::ico_bytes(&[(16, a.clone()), (32, b.clone())])),
                _ => Err(RenderError::Parse("missing ICO frames".into())),
            },
            Component::Png96 => frame(96)
                .cloned()
                .ok_or(RenderError::Parse("no 96 frame".into())),
            Component::AppleTouch180 => frame(180)
                .cloned()
                .ok_or(RenderError::Parse("no 180 frame".into())),
            Component::Icon1024 => frame(1024)
                .cloned()
                .ok_or(RenderError::Parse("no 1024 frame".into())),
            Component::Icon192 => frame(192)
                .cloned()
                .ok_or(RenderError::Parse("no 192 frame".into())),
            Component::Icon512 => frame(512)
                .cloned()
                .ok_or(RenderError::Parse("no 512 frame".into())),
            Component::SiteWebmanifest => Ok(manifest_json(spec).into_bytes()),
            Component::Snippet => Ok(snippet_html(spec).into_bytes()),
        };
        match result.and_then(|bytes| {
            write_atomic(&path, &bytes).map_err(|e| RenderError::Parse(format!("write: {e}")))
        }) {
            Ok(()) => outcome.written.push(component.file_name().to_string()),
            Err(err) => outcome
                .failed
                .push((component.file_name().to_string(), err.to_string())),
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::Platforms;
    use std::path::PathBuf;

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("favicon-export-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn plan_follows_platform_selection() {
        let spec = DesignSpec::default(); // web only
        assert_eq!(
            plan(&spec),
            vec![
                Component::FaviconSvg,
                Component::FaviconIco,
                Component::Png96,
                Component::Snippet,
            ]
        );
        let mut all = spec.clone();
        all.platforms = Platforms {
            web: true,
            ios: true,
            ios_1024: true,
            pwa: true,
        };
        assert_eq!(plan(&all).len(), 9);
        let mut none = spec.clone();
        none.platforms = Platforms::default();
        none.platforms.web = false;
        assert!(plan(&none).is_empty());
    }

    #[test]
    fn collisions_list_existing_only() {
        let dir = tmp_dir("collisions");
        std::fs::write(dir.join("favicon.svg"), b"x").unwrap();
        let spec = DesignSpec::default();
        assert_eq!(collisions(&dir, &plan(&spec)), vec!["favicon.svg"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_lists_selected_icons_only() {
        let mut spec = DesignSpec::default();
        let json = manifest_json(&spec);
        assert!(
            !json.contains("icon-192"),
            "web-only has no manifest icons: {json}"
        );
        spec.platforms = Platforms {
            web: false,
            ios: true,
            ios_1024: true,
            pwa: true,
        };
        let json = manifest_json(&spec);
        assert!(json.contains("/icon-192.png"));
        assert!(json.contains("/icon-512.png"));
        assert!(json.contains("/icon-1024.png"));
        assert!(json.contains("/apple-touch-icon.png"));
        assert!(json.contains("\"My App\""));
        spec.app_name = "Dash \"HQ\"".into();
        assert!(manifest_json(&spec).contains("Dash \\\"HQ\\\""));
    }

    #[test]
    fn snippet_follows_selection_and_dark() {
        let mut spec = DesignSpec::default();
        let html = snippet_html(&spec);
        assert!(html.contains("favicon.svg"));
        assert!(!html.contains("manifest"), "web-only: {html}");
        assert!(html.contains("theme-color\" content=\"#8b5cf6\""));
        spec.platforms = Platforms {
            web: true,
            ios: true,
            ios_1024: false,
            pwa: true,
        };
        spec.dark = Some(crate::spec::ThemeColors {
            bg_end: "#0b0d12".into(),
            ..Default::default()
        });
        let html = snippet_html(&spec);
        assert!(html.contains("rel=\"manifest\""));
        assert!(html.contains("apple-touch-icon"));
        assert!(html.contains("media=\"(prefers-color-scheme: light)\""));
        assert!(html.contains("media=\"(prefers-color-scheme: dark)\""));
    }

    #[test]
    fn write_atomic_never_leaves_tmp() {
        let dir = tmp_dir("atomic");
        let path = dir.join("favicon.svg");
        write_atomic(&path, b"<svg/>").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"<svg/>");
        assert!(!path.with_extension("favicon-tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The full run against a real dir (§5): every planned file lands,
    /// decodes, and nothing else appears.
    #[test]
    fn export_writes_the_full_matrix() {
        let dir = tmp_dir("full");
        let spec = DesignSpec {
            platforms: Platforms {
                web: true,
                ios: true,
                ios_1024: true,
                pwa: true,
            },
            app_name: "Test App".into(),
            ..DesignSpec::default()
        };
        let db = render::system_font_db();
        let outcome = export(&spec, &dir, &db);
        assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
        let mut landed: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        landed.sort();
        assert_eq!(landed.len(), 9, "{landed:?}");
        // Sizes decode at target dims (the D2 round-trip, on disk).
        for (name, size) in [
            ("favicon-96x96.png", 96u32),
            ("apple-touch-icon.png", 180),
            ("icon-192.png", 192),
            ("icon-512.png", 512),
            ("icon-1024.png", 1024),
        ] {
            let img = image::load_from_memory(&std::fs::read(dir.join(name)).unwrap()).unwrap();
            assert_eq!((img.width(), img.height()), (size, size), "{name}");
        }
        // The ICO parses with our own reader shape: 2 entries.
        let ico = std::fs::read(dir.join("favicon.ico")).unwrap();
        assert_eq!(&ico[0..6], &[0, 0, 1, 0, 2, 0]);
        let svg = std::fs::read_to_string(dir.join("favicon.svg")).unwrap();
        assert!(svg.starts_with("<svg "), "{svg}");
        let manifest = std::fs::read_to_string(dir.join("site.webmanifest")).unwrap();
        assert!(manifest.contains("Test App"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A failing component is reported, not fatal (D7): a read-only
    /// target dir fails every write but the outcome still returns.
    #[test]
    fn export_reports_failures_per_file() {
        let dir = tmp_dir("fail");
        let spec = DesignSpec::default();
        let db = render::system_font_db();
        // A directory squatting on the target path makes the rename
        // fail on every component — the plan collapses to failures.
        for c in plan(&spec) {
            std::fs::create_dir_all(dir.join(c.file_name())).unwrap();
        }
        let outcome = export(&spec, &dir, &db);
        assert!(!outcome.failed.is_empty());
        assert!(outcome.written.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
