//! Data-colored chips — the sanctioned token-discipline exception for
//! GitHub label colors (docs/0022 Q2, issue #119 decision).
//!
//! docs/0002 D5 forbids literal color constructors: *interface* colors
//! come from semantic theme tokens. A GitHub label's `color` is user
//! data — an author-chosen hex arrives over the API next to the name.
//! Reproducing user data in its own color is not theming, so this module
//! is the single place in the workspace where colors may be constructed
//! from data; every other surface stays on tokens.
//!
//! The treatment follows GitHub's dark-mode label rendering: the exact
//! hue is kept, the fill is a faint tint of it over the chip surface,
//! and the text carries the hue with its lightness clamped just enough
//! to hold WCAG-AA (4.5:1) contrast against the composite fill. Pure
//! lightness correction preserves hue and saturation — the label reads
//! as *its* color, only made legible on our background.

use gpui::{Hsla, Rgba};

/// Chip colors derived from a data hex (GitHub label `color`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DataChip {
    /// The exact hue, tinted over the chip surface.
    pub bg: Hsla,
    /// The exact hue, lightness-corrected for AA contrast over `bg`.
    pub fg: Hsla,
    /// The exact hue, faint — the chip edge.
    pub border: Hsla,
}

/// Fill tint over the chip surface (GitHub dark-mode label proportion).
const BG_ALPHA: f32 = 0.15;
/// Edge tint.
const BORDER_ALPHA: f32 = 0.40;
/// WCAG AA for normal text.
const AA_RATIO: f32 = 4.5;
/// Lightness step for the contrast walk.
const L_STEP: f32 = 0.02;

/// `RRGGBB` (with or without `#`) → chip colors over `surface`.
///
/// Anything else — empty, shorthand, non-hex, 8-digit — is `None`; the
/// caller falls back to the muted token chip (the same degrade as
/// labels that omit `color` altogether).
pub fn data_chip(hex: &str, surface: Rgba) -> Option<DataChip> {
    let base = parse_hex6(hex)?;
    let hsla = Hsla::from(base);
    // The fill the text actually sits on: surface under the tint.
    let composite = surface.blend(Rgba {
        r: base.r,
        g: base.g,
        b: base.b,
        a: BG_ALPHA,
    });
    Some(DataChip {
        bg: Hsla {
            h: hsla.h,
            s: hsla.s,
            l: hsla.l,
            a: BG_ALPHA,
        },
        fg: aa_fg(hsla, composite),
        border: Hsla {
            h: hsla.h,
            s: hsla.s,
            l: hsla.l,
            a: BORDER_ALPHA,
        },
    })
}

/// The exact color, lightness-walked until it holds AA over `bg`.
///
/// Which pole to walk toward follows the fill, not the color: on a
/// dark composite (our theme) contrast only improves toward white —
/// darkening caps at `(Yfill + 0.05) / 0.05`, hopeless for a 4.5
/// target — and vice versa on a light fill. Sixty steps cover the full
/// lightness range; the color is only touched as far as the ratio
/// demands.
fn aa_fg(hsla: Hsla, bg: Rgba) -> Hsla {
    let lighten = rel_luma(bg) < 0.5;
    let mut l = hsla.l;
    let mut fg;
    for _ in 0..60 {
        fg = Hsla {
            h: hsla.h,
            s: hsla.s,
            l,
            a: 1.0,
        };
        if contrast(Rgba::from(fg), bg) >= AA_RATIO {
            return fg;
        }
        l += if lighten { L_STEP } else { -L_STEP };
        if !(0.0..=1.0).contains(&l) {
            break;
        }
    }
    // Range exhausted — the extreme is the best the hue can do.
    Hsla {
        h: hsla.h,
        s: hsla.s,
        l: l.clamp(0.0, 1.0),
        a: 1.0,
    }
}

fn parse_hex6(hex: &str) -> Option<Rgba> {
    let t = hex.strip_prefix('#').unwrap_or(hex);
    if t.len() != 6 || !t.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let [_, r, g, b] = u32::from_str_radix(t, 16).ok()?.to_be_bytes();
    Some(Rgba {
        r: f32::from(r) / 255.0,
        g: f32::from(g) / 255.0,
        b: f32::from(b) / 255.0,
        a: 1.0,
    })
}

/// WCAG relative luminance.
fn rel_luma(c: Rgba) -> f32 {
    fn lin(ch: f32) -> f32 {
        if ch <= 0.03928 {
            ch / 12.92
        } else {
            ((ch + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
}

fn contrast(a: Rgba, b: Rgba) -> f32 {
    let (ya, yb) = (rel_luma(a), rel_luma(b));
    let (hi, lo) = if ya >= yb { (ya, yb) } else { (yb, ya) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A representative dark panel fill (the app's dark theme surface);
    // the guarantee under test is the math, the render site passes the
    // live theme token.
    fn dark_surface() -> Rgba {
        Rgba {
            r: 0.11,
            g: 0.11,
            b: 0.12,
            a: 1.0,
        }
    }

    fn chip(hex: &str) -> DataChip {
        data_chip(hex, dark_surface()).expect("valid hex")
    }

    #[test]
    fn hex_parse_degrades() {
        assert!(data_chip("", dark_surface()).is_none());
        assert!(data_chip("d73a4", dark_surface()).is_none());
        assert!(data_chip("d73a4aa", dark_surface()).is_none()); // 7 chars
        assert!(data_chip("ff0000ff", dark_surface()).is_none()); // 8-digit
        assert!(data_chip("zzzzzz", dark_surface()).is_none());
        assert!(data_chip("#d73a4a", dark_surface()).is_some());
        assert!(data_chip("D73A4A", dark_surface()).is_some()); // case
    }

    #[test]
    fn chip_tints_and_edge_carry_the_exact_hue() {
        // GitHub's default red label.
        let red = chip("d73a4a");
        let hsla = Hsla::from(parse_hex6("d73a4a").unwrap());
        assert_eq!(red.bg.h, hsla.h);
        assert_eq!(red.fg.h, hsla.h);
        assert_eq!(red.border.h, hsla.h);
        assert_eq!(red.bg.a, BG_ALPHA);
        assert_eq!(red.border.a, BORDER_ALPHA);
        assert_eq!(red.fg.a, 1.0);
    }

    #[test]
    fn fg_holds_aa_over_the_composite_fill() {
        // A spread of GitHub's default label palette — bright yellow,
        // deep blue, near-black, near-white, mid gray.
        for hex in [
            "d73a4a", // red
            "0e8a16", // green
            "1d76db", // blue
            "5319e7", // purple
            "d93f0b", // orange
            "fbca04", // yellow
            "fef2c0", // pale yellow
            "c2e0c6", // pale green
            "bfd4f2", // pale blue
            "f9d0c4", // pale red
            "d4c5f9", // pale purple
            "e99695", // salmon
            "006b75", // teal
            "0052cc", // deep blue
            "000000", // black
            "ffffff", // white
            "808080", // gray
        ] {
            let chip = chip(hex);
            let fill = dark_surface().blend(Rgba::from(chip.bg));
            let ratio = contrast(Rgba::from(chip.fg), fill);
            assert!(
                ratio >= AA_RATIO,
                "{hex}: fg contrast {ratio:.2} under 4.5 over its own fill"
            );
        }
    }

    #[test]
    fn dark_fill_lightens_rather_than_darkens() {
        // On our dark fill, mid-tone colors must come UP in lightness —
        // darkening walks them into the background.
        for hex in ["d73a4a", "1d76db", "006b75", "5319e7", "808080"] {
            let chip = chip(hex);
            let base = Hsla::from(parse_hex6(hex).unwrap());
            assert!(chip.fg.l > base.l, "{hex}: not lightened");
        }
        // Black is the extreme case.
        let chip = chip("000000");
        let fg = Rgba::from(chip.fg);
        let fill = dark_surface().blend(Rgba::from(chip.bg));
        assert!(contrast(fg, fill) >= AA_RATIO);
        // One step back stays under — the walk stopped at the edge.
        let relaxed = Hsla {
            l: (chip.fg.l - L_STEP * 1.5).max(0.0),
            ..chip.fg
        };
        assert!(contrast(Rgba::from(relaxed), fill) < AA_RATIO);
    }

    #[test]
    fn legible_colors_untouched() {
        // Pale yellow already clears AA over its own faint fill —
        // correction must not move it at all.
        let chip = chip("fef2c0");
        let base = Hsla::from(parse_hex6("fef2c0").unwrap());
        assert_eq!(chip.fg.l, base.l);
    }
}
