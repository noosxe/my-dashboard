//! Pixel-golden comparison (0065 D6): Playwright semantics, in-house
//! implementation. Baselines live and die on the Linux runner — this
//! module is pure math over decoded buffers so the same rules are
//! unit-testable everywhere.

use image::RgbaImage;

/// Default per-channel tolerance (0–255). Anything a human can see is
/// far above 2; anti-aliasing jitter between identical frames is not.
/// Chosen over 0 deliberately: captures are codec-fed rasters, and a
/// zero-tolerance gate would turn font hinting noise into drift.
pub const DEFAULT_TOLERANCE: u8 = 2;

#[derive(Debug, PartialEq, Eq)]
pub enum PixelsError {
    /// Dimensions differ — never a drift verdict, always an error:
    /// comparing different frames of reference is meaningless.
    SizeMismatch { a: (u32, u32), b: (u32, u32) },
}

impl std::fmt::Display for PixelsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PixelsError::SizeMismatch { a, b } => write!(
                f,
                "size mismatch: baseline {}×{} vs capture {}×{} — recapture \
                 the baseline at the session size (--update), never compare \
                 across frame sizes",
                a.0, a.1, b.0, b.1
            ),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct PixelDiff {
    pub width: u32,
    pub height: u32,
    /// Pixels where any channel delta exceeds the tolerance.
    pub over_tolerance: u64,
    /// Largest single-channel delta observed (0–255).
    pub max_channel_delta: u8,
    /// First differing pixel, for the human report.
    pub first_over: Option<(u32, u32)>,
}

impl PixelDiff {
    /// Playwright-style verdict: any pixel over tolerance is drift.
    /// The count and worst delta are reported either way — a "1 pixel
    /// at 251,44" line is triageable, a bare FAIL is not.
    pub fn is_drift(&self) -> bool {
        self.over_tolerance > 0
    }
}

/// Compare two RGBA buffers with a per-channel tolerance. Direct
/// RGBA comparison — session captures are opaque composites, and the
/// rule stays uniform (alpha is a channel like the rest).
pub fn diff_rgba(a: &RgbaImage, b: &RgbaImage, tolerance: u8) -> Result<PixelDiff, PixelsError> {
    let (aw, ah) = a.dimensions();
    let (bw, bh) = b.dimensions();
    if (aw, ah) != (bw, bh) {
        return Err(PixelsError::SizeMismatch {
            a: (aw, ah),
            b: (bw, bh),
        });
    }
    let mut diff = PixelDiff {
        width: aw,
        height: ah,
        over_tolerance: 0,
        max_channel_delta: 0,
        first_over: None,
    };
    for y in 0..ah {
        for x in 0..aw {
            let pa = a.get_pixel(x, y).0;
            let pb = b.get_pixel(x, y).0;
            let mut over = false;
            for (ca, cb) in pa.iter().zip(pb.iter()) {
                let delta = ca.abs_diff(*cb);
                if delta > diff.max_channel_delta {
                    diff.max_channel_delta = delta;
                }
                if delta > tolerance {
                    over = true;
                }
            }
            if over {
                diff.over_tolerance += 1;
                if diff.first_over.is_none() {
                    diff.first_over = Some((x, y));
                }
            }
        }
    }
    Ok(diff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn solid(w: u32, h: u32, c: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba(c))
    }

    #[test]
    fn identical_buffers_pass() {
        let a = solid(16, 8, [10, 20, 30, 255]);
        let b = solid(16, 8, [10, 20, 30, 255]);
        let d = diff_rgba(&a, &b, DEFAULT_TOLERANCE).unwrap();
        assert!(!d.is_drift());
        assert_eq!(d.over_tolerance, 0);
        assert_eq!(d.max_channel_delta, 0);
        assert_eq!(d.first_over, None);
    }

    #[test]
    fn delta_at_exactly_tolerance_is_not_drift() {
        let mut b = solid(4, 4, [10, 10, 10, 255]);
        b.put_pixel(1, 2, Rgba([12, 10, 10, 255]));
        let a = solid(4, 4, [10, 10, 10, 255]);
        let d = diff_rgba(&a, &b, 2).unwrap();
        assert!(!d.is_drift(), "delta == tolerance is inside the gate");
        assert_eq!(d.max_channel_delta, 2);
        assert_eq!(d.over_tolerance, 0);
    }

    #[test]
    fn one_pixel_over_tolerance_reports_position() {
        let mut b = solid(8, 8, [0, 0, 0, 255]);
        b.put_pixel(5, 3, Rgba([0, 0, 9, 255]));
        let a = solid(8, 8, [0, 0, 0, 255]);
        let d = diff_rgba(&a, &b, 2).unwrap();
        assert!(d.is_drift());
        assert_eq!(d.over_tolerance, 1);
        assert_eq!(d.max_channel_delta, 9);
        assert_eq!(d.first_over, Some((5, 3)));
    }

    #[test]
    fn alpha_is_a_channel_like_the_rest() {
        let a = solid(2, 2, [0, 0, 0, 255]);
        let b = solid(2, 2, [0, 0, 0, 200]);
        let d = diff_rgba(&a, &b, DEFAULT_TOLERANCE).unwrap();
        assert!(d.is_drift());
        assert_eq!(d.max_channel_delta, 55);
    }

    #[test]
    fn size_mismatch_is_an_error_not_a_verdict() {
        let a = solid(4, 4, [0, 0, 0, 255]);
        let b = solid(8, 4, [0, 0, 0, 255]);
        assert_eq!(
            diff_rgba(&a, &b, DEFAULT_TOLERANCE),
            Err(PixelsError::SizeMismatch {
                a: (4, 4),
                b: (8, 4)
            })
        );
        // The message must carry the fix (recapture, never stretch).
        let msg = format!(
            "{}",
            PixelsError::SizeMismatch {
                a: (4, 4),
                b: (8, 4)
            }
        );
        assert!(msg.contains("--update"));
    }

    #[test]
    fn many_pixels_sum_correctly() {
        let mut b = solid(10, 10, [50, 50, 50, 255]);
        for x in 0..10 {
            b.put_pixel(x, 0, Rgba([90, 50, 50, 255]));
        }
        let a = solid(10, 10, [50, 50, 50, 255]);
        let d = diff_rgba(&a, &b, DEFAULT_TOLERANCE).unwrap();
        assert_eq!(d.over_tolerance, 10);
        assert_eq!(d.first_over, Some((0, 0)));
    }
}
