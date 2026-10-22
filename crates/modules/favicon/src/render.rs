//! The render pipeline (docs/0064 D2/D4/D6): emit the canonical SVG
//! from a [`DesignSpec`], then rasterize **that exact string** — the
//! preview frames and every exported PNG/ICO frame come from the same
//! emitted artifact, so preview and export can never drift.
//!
//! Export rasters always render the light palette: the dark palette
//! travels inside the SVG as a `prefers-color-scheme` media block (one
//! file, two looks, D4) and the browser resolves it per OS setting.
//! resvg never resolves that query, so the stage's dark preview
//! (§12 L5) rasterizes [`emit_preview_svg`] instead — the same design
//! with the dark palette inlined. Export never calls it.

use std::sync::Arc;

use base64::Engine as _;

use crate::spec::{DesignSpec, Source, VIEW};

/// The SVG title-level namespace — always emitted, never inherited.
const SVGNS: &str = "http://www.w3.org/2000/svg";

/// The emoji font stack (D12): platform color-emoji fonts in fallback
/// order; browsers and fontdb each resolve what they have.
const EMOJI_STACK: &str = "Apple Color Emoji, Segoe UI Emoji, Noto Color Emoji";

/// XML-escape a text node / attribute value.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// Emit the canonical SVG (D4): `viewBox="0 0 512 512"`, a two-stop
/// gradient background clipped to the shape, the glyph layer, and —
/// when a dark palette exists — the media block that swaps the stops
/// and glyph fill.
pub fn emit_svg(spec: &DesignSpec) -> String {
    let light = &spec.light;
    let radius = if spec.shape.circle {
        VIEW / 2.0
    } else {
        (VIEW * spec.shape.radius_frac.clamp(0.0, 0.5)).min(VIEW / 2.0)
    };

    let mut svg = String::with_capacity(512);
    svg.push_str(&format!(
        "<svg xmlns=\"{SVGNS}\" viewBox=\"0 0 {V} {V}\">",
        V = VIEW as u32
    ));

    // Dark palette (D4): presentation attributes lose to CSS, so the
    // stops/glyph carry light values and this block re-paints them in
    // dark environments. No dark palette — no block.
    if let Some(dark) = &spec.dark {
        let mut style = String::from("<style>@media (prefers-color-scheme: dark){");
        style.push_str(&format!(".bg0{{stop-color:{}}}", esc(&dark.bg_start)));
        style.push_str(&format!(".bg1{{stop-color:{}}}", esc(&dark.bg_end)));
        style.push_str(&format!(".fg{{fill:{}}}", esc(&dark.fg)));
        style.push_str("}</style>");
        svg.push_str(&style);
    }

    svg.push_str(&format!(
        "<defs><linearGradient id=\"bg\" x1=\"0\" y1=\"0\" x2=\"0\" y2=\"1\">\
<stop offset=\"0\" class=\"bg0\" stop-color=\"{start}\"/>\
<stop offset=\"1\" class=\"bg1\" stop-color=\"{end}\"/>\
</linearGradient>\
<clipPath id=\"shape\"><rect width=\"{V}\" height=\"{V}\" rx=\"{radius:.2}\"/></clipPath></defs>",
        start = esc(&light.bg_start),
        end = esc(&light.bg_end),
        V = VIEW as u32,
    ));

    svg.push_str(&format!(
        "<g clip-path=\"url(#shape)\">\
<rect width=\"{V}\" height=\"{V}\" fill=\"url(#bg)\"/>{glyph}</g>",
        glyph = glyph_layer(spec, &light.fg),
        V = VIEW as u32,
    ));

    svg.push_str("</svg>");
    svg
}

/// Which palette a stage preview shows (docs/0064 §12 L5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum PreviewTheme {
    #[default]
    Light,
    Dark,
}

/// The preview-only emission (§12 L5): [`emit_svg`] for `Light`; for
/// `Dark`, the same design with the dark palette inlined as the
/// presentation colors and no media block — what favicon.svg looks
/// like in a dark-mode browser, in a form resvg can draw. No dark
/// palette → the light rendering (the browser shows the same).
pub fn emit_preview_svg(spec: &DesignSpec, theme: PreviewTheme) -> String {
    match (theme, &spec.dark) {
        (PreviewTheme::Dark, Some(dark)) => emit_svg(&DesignSpec {
            light: dark.clone(),
            dark: None,
            ..spec.clone()
        }),
        _ => emit_svg(spec),
    }
}

/// The glyph layer for each [`Source`] (D4), positioned dead-center in
/// the 512 space.
fn glyph_layer(spec: &DesignSpec, fg: &str) -> String {
    match &spec.source {
        Source::Text {
            chars,
            family,
            weight,
            scale,
        } => {
            let size = VIEW * scale.clamp(0.05, 1.2);
            // No dominant-baseline (usvg's support is partial): the
            // baseline sits at center + ~0.35em — the classic cap-height
            // approximation for centered 1–2 letter marks.
            let y = VIEW / 2.0 + size * 0.35;
            format!(
                "<text x=\"{x:.2}\" y=\"{y:.2}\" text-anchor=\"middle\" \
class=\"fg\" fill=\"{fg}\" font-family=\"{family}\" font-weight=\"{weight}\" \
font-size=\"{size:.2}\">{chars}</text>",
                x = VIEW / 2.0,
                fg = esc(fg),
                family = esc(family),
                chars = esc(chars),
            )
        }
        Source::Emoji { glyph, scale } => {
            let size = VIEW * scale.clamp(0.05, 1.2);
            let y = VIEW / 2.0 + size * 0.35;
            format!(
                "<text x=\"{x:.2}\" y=\"{y:.2}\" text-anchor=\"middle\" \
class=\"fg\" font-family=\"{EMOJI_STACK}\" font-size=\"{size:.2}\">{glyph}</text>",
                x = VIEW / 2.0,
                glyph = esc(glyph),
            )
        }
        Source::Image { mime, data, w, h } => image_layer(mime.as_str(), data, *w, *h),
        Source::Svg { data, w, h } => image_layer("image/svg+xml", data, *w, *h),
    }
}

/// The shared `<image>` embed (D4): rasters and uploaded SVGs alike are
/// base64 data URIs, contained (aspect-preserved, centered) into the
/// 512 space. usvg resolves both natively (`ImageKind::PNG`/`SVG`).
fn image_layer(mime: &str, data: &str, w: u32, h: u32) -> String {
    if w == 0 || h == 0 {
        return String::new();
    }
    let scale = VIEW / w.max(h) as f32;
    let iw = w as f32 * scale;
    let ih = h as f32 * scale;
    let x = (VIEW - iw) / 2.0;
    let y = (VIEW - ih) / 2.0;
    format!(
        "<image href=\"data:{mime};base64,{data}\" x=\"{x:.2}\" y=\"{y:.2}\" \
width=\"{iw:.2}\" height=\"{ih:.2}\" preserveAspectRatio=\"xMidYMid meet\"/>",
    )
}

/// Emitted raster frames keyed by pixel size — one emit, N rasters.
pub type RasterFrames = Vec<(u32, Vec<u8>)>;

/// A system-font database for rasterization (D11). Built once per
/// process and shared by every raster call.
pub fn system_font_db() -> Arc<resvg::usvg::fontdb::Database> {
    let mut db = resvg::usvg::fontdb::Database::new();
    db.load_system_fonts();
    // Generic-family fallbacks resolve somewhere sane on every platform.
    db.set_sans_serif_family("Arial");
    db.set_serif_family("Georgia");
    db.set_monospace_family("Courier New");
    Arc::new(db)
}

/// One rasterization error class (never fatal to the app — the page
/// surfaces it next to the preview).
#[derive(Debug)]
pub enum RenderError {
    Parse(String),
    ZeroSize,
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderError::Parse(e) => write!(f, "SVG parse failed: {e}"),
            RenderError::ZeroSize => write!(f, "zero-size raster requested"),
        }
    }
}

/// Rasterize the **emitted** SVG at `size`×`size` (D2). The scale is a
/// plain multiple of the 512 viewBox — no re-emission, no second
/// layout.
pub fn rasterize(
    svg: &str,
    size: u32,
    db: &Arc<resvg::usvg::fontdb::Database>,
) -> Result<resvg::tiny_skia::Pixmap, RenderError> {
    let options = resvg::usvg::Options {
        fontdb: db.clone(),
        ..Default::default()
    };
    let tree = resvg::usvg::Tree::from_str(svg, &options)
        .map_err(|e| RenderError::Parse(e.to_string()))?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size).ok_or(RenderError::ZeroSize)?;
    let s = size as f32 / VIEW;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(s, s),
        &mut pixmap.as_mut(),
    );
    Ok(pixmap)
}

/// Encode a pixmap as PNG bytes (the export frame format).
pub fn png_bytes(pixmap: &resvg::tiny_skia::Pixmap) -> Result<Vec<u8>, RenderError> {
    pixmap
        .encode_png()
        .map_err(|e| RenderError::Parse(e.to_string()))
}

/// Build a gpui preview frame from a pixmap — the byte-for-byte
/// conversion gpui's own svg renderer uses (RGBA→BGRA, un-premultiply):
/// tiny-skia emits premultiplied RGBA, `RenderImage` wants straight
/// BGRA.
pub fn preview_frame(pixmap: resvg::tiny_skia::Pixmap) -> Arc<gpui::RenderImage> {
    let mut buffer = image::ImageBuffer::from_raw(pixmap.width(), pixmap.height(), pixmap.take())
        .expect("pixmap data length matches dims");
    for pixel in buffer.chunks_exact_mut(4) {
        pixel.swap(0, 2);
        if pixel[3] > 0 {
            let a = pixel[3] as f32 / 255.0;
            pixel[0] = (pixel[0] as f32 / a) as u8;
            pixel[1] = (pixel[1] as f32 / a) as u8;
            pixel[2] = (pixel[2] as f32 / a) as u8;
        }
    }
    Arc::new(gpui::RenderImage::new(smallvec::SmallVec::from_vec(vec![
        image::Frame::new(buffer),
    ])))
}

/// The hand-rolled ICO writer (D6): ICONDIR + one ICONDIRENTRY per
/// frame + embedded PNG payloads (the format modern browsers expect).
/// `frames` = (size, png bytes); sizes ≤ 255 encode directly, 256
/// encodes as 0 (the ICO convention).
pub fn ico_bytes(frames: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(6 + 16 * frames.len());
    out.extend_from_slice(&[0, 0]); // reserved
    out.extend_from_slice(&[1, 0]); // type: icon
    out.extend_from_slice(&(frames.len() as u16).to_le_bytes());
    let mut offset: u32 = 6 + 16 * frames.len() as u32;
    for (size, png) in frames {
        let dim = if *size >= 256 { 0u8 } else { *size as u8 };
        out.push(dim); // width
        out.push(dim); // height
        out.push(0); // palette
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bpp
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in frames {
        out.extend_from_slice(png);
    }
    out
}

/// Convenience: the emitted SVG + every raster frame an export needs,
/// keyed by size. One emit, N rasters (D2).
pub fn render_all(
    spec: &DesignSpec,
    sizes: &[u32],
    db: &Arc<resvg::usvg::fontdb::Database>,
) -> Result<(String, RasterFrames), RenderError> {
    let svg = emit_svg(spec);
    let mut frames = Vec::with_capacity(sizes.len());
    for size in sizes {
        let pixmap = rasterize(&svg, *size, db)?;
        frames.push((*size, png_bytes(&pixmap)?));
    }
    Ok((svg, frames))
}

/// The decoded glyph payload for a picked raster upload: canonical
/// PNG bytes + dims (JPEG re-encoded as PNG so the embed is one mime).
pub fn normalize_raster(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), RenderError> {
    let img = image::load_from_memory(bytes)
        .map_err(|e| RenderError::Parse(format!("image decode: {e}")))?;
    let (w, h) = (img.width(), img.height());
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| RenderError::Parse(format!("png encode: {e}")))?;
    Ok((png, w, h))
}

/// Validate + measure an uploaded/pasted SVG (D4): parsed with the same
/// usvg options the raster will use, so anything accepted here renders
/// there. Returns the intrinsic size.
pub fn validate_svg(
    markup: &str,
    db: &Arc<resvg::usvg::fontdb::Database>,
) -> Result<(f32, f32), RenderError> {
    let options = resvg::usvg::Options {
        fontdb: db.clone(),
        ..Default::default()
    };
    let tree = resvg::usvg::Tree::from_str(markup, &options)
        .map_err(|e| RenderError::Parse(e.to_string()))?;
    let size = tree.size();
    Ok((size.width(), size.height()))
}

/// Base64 for data URIs (the single engine the module uses).
pub fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{ImageMime, Platforms, Shape, ThemeColors};

    fn db() -> Arc<resvg::usvg::fontdb::Database> {
        system_font_db()
    }

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(name)
    }

    /// Golden compare with a re-write escape hatch: fixtures are the
    /// D4 contract, so changes are always reviewed as diffs.
    fn assert_golden(name: &str, actual: &str) {
        let path = fixture(name);
        if std::env::var("FAVICON_UPDATE_FIXTURES").is_ok() {
            std::fs::write(&path, actual).unwrap();
            return;
        }
        let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            panic!("fixture {name} missing — run with FAVICON_UPDATE_FIXTURES=1")
        });
        assert_eq!(expected, actual, "emitted SVG drift for {name}");
    }

    fn text_spec() -> DesignSpec {
        DesignSpec::default()
    }

    fn dark_spec() -> DesignSpec {
        DesignSpec {
            dark: Some(ThemeColors {
                fg: "#101014".into(),
                bg_start: "#0b0d12".into(),
                bg_end: "#1c2230".into(),
            }),
            ..DesignSpec::default()
        }
    }

    fn circle_spec() -> DesignSpec {
        DesignSpec {
            shape: Shape {
                circle: true,
                radius_frac: 0.5,
            },
            ..DesignSpec::default()
        }
    }

    fn emoji_spec() -> DesignSpec {
        DesignSpec {
            source: Source::Emoji {
                glyph: "🚀".into(),
                scale: 0.7,
            },
            ..DesignSpec::default()
        }
    }

    /// 2×2 red PNG — the smallest legal image embed.
    fn tiny_png() -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(2, 2, image::Rgba([200, 30, 30, 255]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        png
    }

    fn image_spec() -> DesignSpec {
        let png = tiny_png();
        DesignSpec {
            source: Source::Image {
                mime: ImageMime::Png,
                data: b64(&png),
                w: 2,
                h: 2,
            },
            ..DesignSpec::default()
        }
    }

    fn svg_source_spec() -> DesignSpec {
        let markup = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"24\" height=\"24\"><circle cx=\"12\" cy=\"12\" r=\"10\" fill=\"#335577\"/></svg>";
        DesignSpec {
            source: Source::Svg {
                data: b64(markup.as_bytes()),
                w: 24,
                h: 24,
            },
            ..DesignSpec::default()
        }
    }

    #[test]
    fn golden_text_light() {
        assert_golden("text-light.svg", &emit_svg(&text_spec()));
    }

    #[test]
    fn golden_text_dark() {
        assert_golden("text-dark.svg", &emit_svg(&dark_spec()));
    }

    #[test]
    fn golden_circle() {
        assert_golden("circle.svg", &emit_svg(&circle_spec()));
    }

    #[test]
    fn golden_emoji() {
        assert_golden("emoji.svg", &emit_svg(&emoji_spec()));
    }

    #[test]
    fn golden_image_embed() {
        assert_golden("image.svg", &emit_svg(&image_spec()));
    }

    #[test]
    fn golden_svg_embed() {
        assert_golden("svg-embed.svg", &emit_svg(&svg_source_spec()));
    }

    /// Every golden must itself rasterize — the parse tolerance gate
    /// for the dark media block (D4) and both embed kinds.
    #[test]
    fn goldens_rasterize_at_every_matrix_size() {
        let db = db();
        for spec in [
            text_spec(),
            dark_spec(),
            circle_spec(),
            emoji_spec(),
            image_spec(),
            svg_source_spec(),
        ] {
            let svg = emit_svg(&spec);
            for size in [16, 32, 96, 180, 192, 512, 1024] {
                let pixmap =
                    rasterize(&svg, size, &db).unwrap_or_else(|e| panic!("raster {size}: {e}"));
                assert_eq!((pixmap.width(), pixmap.height()), (size, size));
            }
        }
    }

    /// The raster round-trip contract (§5): light palette paints the
    /// gradient — the center pixel is neither stop but a blend, and
    /// definitely not the transparent default.
    #[test]
    fn raster_paints_background() {
        let db = db();
        let svg = emit_svg(&text_spec());
        let pixmap = rasterize(&svg, 64, &db).unwrap();
        let px = pixmap.pixel(32, 4).expect("pixel");
        assert!(px.alpha() > 230, "background must be opaque, got {px:?}");
    }

    /// The D2 WYSIWYG spine: exported PNG bytes decode back at the
    /// exact target size (§5).
    #[test]
    fn png_roundtrip_dims() {
        let db = db();
        let (svg, frames) = render_all(&text_spec(), &[16, 96], &db).unwrap();
        assert!(!svg.is_empty());
        for (size, png) in frames {
            let img = image::load_from_memory(&png).unwrap();
            assert_eq!((img.width(), img.height()), (size, size));
        }
    }

    /// S2 spike (D12): does resvg 0.46 rasterize color emoji on this
    /// platform? The test asserts a saturated pixel exists — a
    /// monochrome fallback would paint only the fg color.
    #[test]
    fn spike_emoji_raster_has_color() {
        let db = db();
        let svg = emit_svg(&emoji_spec());
        let pixmap = rasterize(&svg, 128, &db).unwrap();
        let data = pixmap.data();
        let mut saturated = 0;
        for px in data.chunks_exact(4) {
            let (r, g, b, a) = (px[0], px[1], px[2], px[3]);
            if a == 0 {
                continue;
            }
            let mx = r.max(g).max(b) as i32;
            let mn = r.min(g).min(b) as i32;
            if mx - mn > 40 {
                saturated += 1;
            }
        }
        assert!(
            saturated > 8,
            "no saturated pixels — emoji rasterized monochrome ({saturated} saturated)"
        );
    }

    #[test]
    fn ico_layout_two_frames() {
        let a = vec![0xAA; 10];
        let b = vec![0xBB; 4];
        let ico = ico_bytes(&[(16, a), (32, b)]);
        assert_eq!(&ico[0..6], &[0, 0, 1, 0, 2, 0], "ICONDIR");
        // Entry 0: 16px, 10 bytes at offset 6+32=38.
        assert_eq!(ico[6], 16);
        assert_eq!(ico[7], 16);
        assert_eq!(&ico[10..12], &1u16.to_le_bytes(), "planes");
        assert_eq!(&ico[12..14], &32u16.to_le_bytes(), "bpp");
        assert_eq!(&ico[14..18], &10u32.to_le_bytes(), "size");
        assert_eq!(&ico[18..22], &38u32.to_le_bytes(), "offset");
        // Entry 1: 32px, 4 bytes at offset 38+10=48.
        assert_eq!(ico[22], 32);
        assert_eq!(&ico[30..34], &4u32.to_le_bytes(), "size2");
        assert_eq!(&ico[34..38], &48u32.to_le_bytes(), "offset2");
        // Payloads follow in order.
        assert_eq!(&ico[38..48], &[0xAA; 10]);
        assert_eq!(&ico[48..52], &[0xBB; 4]);
    }

    #[test]
    fn escape_never_breaks_markup() {
        let spec = DesignSpec {
            source: Source::Text {
                chars: "<&\">".into(),
                family: "Fo\"nt & Co".into(),
                weight: 400,
                scale: 0.6,
            },
            ..DesignSpec::default()
        };
        let db = db();
        let svg = emit_svg(&spec);
        rasterize(&svg, 16, &db).expect("escaped markup still parses");
    }

    #[test]
    fn image_letterbox_math() {
        // A 2:1 landscape embed spans the full 512 width, half height.
        let layer = image_layer("image/png", "QQ", 200, 100);
        assert!(layer.contains("width=\"512.00\""), "{layer}");
        assert!(layer.contains("height=\"256.00\""), "{layer}");
        assert!(layer.contains("x=\"0.00\""));
        assert!(layer.contains("y=\"128.00\""));
    }

    #[test]
    fn validate_svg_measures_intrinsic_size() {
        let db = db();
        let (w, h) = validate_svg(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"24\" height=\"24\"/>",
            &db,
        )
        .unwrap();
        assert_eq!((w as u32, h as u32), (24, 24));
        assert!(validate_svg("not svg", &db).is_err());
    }

    /// §12 L5: the dark preview inlines the dark palette with no media
    /// block; the light preview is the export emission byte for byte
    /// (the export goldens above stay the D4 contract).
    #[test]
    fn preview_dark_inlines_palette() {
        let spec = dark_spec();
        let dark = emit_preview_svg(&spec, PreviewTheme::Dark);
        assert!(!dark.contains("@media"), "{dark}");
        assert!(!dark.contains("<style>"), "{dark}");
        assert!(dark.contains("stop-color=\"#0b0d12\""), "{dark}");
        assert!(dark.contains("stop-color=\"#1c2230\""), "{dark}");
        assert!(dark.contains("fill=\"#101014\""), "{dark}");
        assert!(!dark.contains(&spec.light.bg_start), "{dark}");
        assert_eq!(
            emit_preview_svg(&spec, PreviewTheme::Light),
            emit_svg(&spec)
        );
        // Export emission unchanged by the preview path.
        assert_golden("text-dark.svg", &emit_svg(&spec));
        rasterize(&dark, 16, &db()).expect("dark preview rasterizes");
    }

    /// No dark palette → Dark previews the light rendering.
    #[test]
    fn preview_dark_without_palette_is_light() {
        let spec = text_spec();
        assert_eq!(emit_preview_svg(&spec, PreviewTheme::Dark), emit_svg(&spec));
    }

    #[test]
    fn platforms_field_rides_along() {
        // The emit is independent of platform selection (that's the
        // export plan's job) — same SVG either way.
        let mut spec = text_spec();
        spec.platforms = Platforms {
            web: true,
            ios: true,
            ios_1024: true,
            pwa: true,
        };
        assert_eq!(emit_svg(&spec), emit_svg(&text_spec()));
    }
}
