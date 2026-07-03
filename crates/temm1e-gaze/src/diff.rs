//! Frame-difference utilities for verifying that a desktop action actually
//! changed the screen.
//!
//! This is the objective half of the airtight loop: instead of asking the VLM to
//! "please verify" (which it can ignore or misread), we measure how much of the
//! screen — and of the region right around where the action happened — changed
//! between the pre-action and post-action screenshots. A click that lands on a
//! real, reactive target moves pixels; a click into dead space moves nothing. The
//! tool reports that fact so the model (and the user) get a truthful signal rather
//! than a confident guess.

use temm1e_core::types::error::Temm1eError;

/// Per-pixel summed-channel-delta above which a pixel counts as "changed".
/// ~24 tolerates anti-aliasing / subpixel noise while catching real UI changes.
const PIXEL_DELTA_THRESHOLD: u32 = 24;

/// A pixel-change measurement between two frames.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChangeReport {
    /// Fraction (0.0–1.0) of all pixels that changed across the whole frame.
    pub global_ratio: f32,
    /// Fraction (0.0–1.0) of pixels that changed within the region of interest
    /// (equals `global_ratio` when no region was supplied).
    pub region_ratio: f32,
    /// Whether a non-trivial change was detected (see [`compare_frames`]).
    pub changed: bool,
}

impl ChangeReport {
    /// A compact, model-facing one-liner describing the measured change, with an
    /// explicit warning when nothing moved (the load-bearing honesty signal).
    pub fn describe(&self, had_region: bool) -> String {
        let g = self.global_ratio * 100.0;
        if !self.changed {
            return format!(
                "⚠ NO visible change detected after this action (screen {g:.2}% changed) — it \
                 most likely had NO effect (missed target / dead space / wrong coordinates). Do \
                 NOT report success; re-check the screenshot and retry with adjusted coordinates."
            );
        }
        if had_region {
            let r = self.region_ratio * 100.0;
            format!(
                "Change detected: {r:.1}% of the region around the action point changed ({g:.2}% \
                 of the whole screen). Confirm in the screenshot that this is the change you intended."
            )
        } else {
            format!(
                "Change detected: {g:.2}% of the screen changed. Confirm in the screenshot that \
                 this is the change you intended."
            )
        }
    }
}

/// Compare two PNG frames and report how much changed globally and within an
/// optional region of interest (a physical-pixel rect `(x, y, w, h)`).
///
/// Frames of differing dimensions are treated as fully changed (resolution or
/// layout shifted). Empty frames report no change.
pub fn compare_frames(
    pre_png: &[u8],
    post_png: &[u8],
    region: Option<(u32, u32, u32, u32)>,
) -> Result<ChangeReport, Temm1eError> {
    let a = image::load_from_memory(pre_png)
        .map_err(|e| Temm1eError::Tool(format!("diff: load pre-frame: {e}")))?
        .to_rgba8();
    let b = image::load_from_memory(post_png)
        .map_err(|e| Temm1eError::Tool(format!("diff: load post-frame: {e}")))?
        .to_rgba8();

    if a.dimensions() != b.dimensions() {
        return Ok(ChangeReport {
            global_ratio: 1.0,
            region_ratio: 1.0,
            changed: true,
        });
    }

    let (w, h) = a.dimensions();
    if w == 0 || h == 0 {
        return Ok(ChangeReport {
            global_ratio: 0.0,
            region_ratio: 0.0,
            changed: false,
        });
    }

    let pixel_changed = |p: &image::Rgba<u8>, q: &image::Rgba<u8>| -> bool {
        let d = (p.0[0] as i32 - q.0[0] as i32).unsigned_abs()
            + (p.0[1] as i32 - q.0[1] as i32).unsigned_abs()
            + (p.0[2] as i32 - q.0[2] as i32).unsigned_abs();
        d > PIXEL_DELTA_THRESHOLD
    };

    let mut global_changed = 0u64;
    for (pa, pb) in a.pixels().zip(b.pixels()) {
        if pixel_changed(pa, pb) {
            global_changed += 1;
        }
    }
    let total = (w as u64) * (h as u64);
    let global_ratio = global_changed as f32 / total as f32;

    let region_ratio = match region {
        Some((rx, ry, rw, rh)) => {
            let x0 = rx.min(w);
            let y0 = ry.min(h);
            let x1 = rx.saturating_add(rw).min(w);
            let y1 = ry.saturating_add(rh).min(h);
            if x1 <= x0 || y1 <= y0 {
                global_ratio
            } else {
                let mut region_changed = 0u64;
                for y in y0..y1 {
                    for x in x0..x1 {
                        if pixel_changed(a.get_pixel(x, y), b.get_pixel(x, y)) {
                            region_changed += 1;
                        }
                    }
                }
                let region_total = ((x1 - x0) as u64) * ((y1 - y0) as u64);
                region_changed as f32 / region_total as f32
            }
        }
        None => global_ratio,
    };

    // "Changed" if a non-trivial fraction moved anywhere, OR a clear change
    // happened in the region of interest. 0.1% of a 1080p screen ≈ a small
    // button; 1% of a focused region ≈ a menu opening or selection highlight.
    let changed = global_ratio > 0.001 || region_ratio > 0.01;

    Ok(ChangeReport {
        global_ratio,
        region_ratio,
        changed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};
    use std::io::Cursor;

    fn png(img: &RgbaImage) -> Vec<u8> {
        let mut buf = Vec::new();
        image::DynamicImage::ImageRgba8(img.clone())
            .write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    fn solid(w: u32, h: u32, c: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba(c))
    }

    #[test]
    fn identical_frames_report_no_change() {
        let a = png(&solid(200, 200, [128, 128, 128, 255]));
        let r = compare_frames(&a, &a, None).unwrap();
        assert!(!r.changed, "identical frames must not report change");
        assert_eq!(r.global_ratio, 0.0);
    }

    #[test]
    fn different_dimensions_report_full_change() {
        let a = png(&solid(200, 200, [0, 0, 0, 255]));
        let b = png(&solid(300, 200, [0, 0, 0, 255]));
        let r = compare_frames(&a, &b, None).unwrap();
        assert!(r.changed);
        assert_eq!(r.global_ratio, 1.0);
    }

    #[test]
    fn small_local_change_is_detected_in_region() {
        let mut pre = solid(400, 400, [30, 30, 30, 255]);
        let mut post = pre.clone();
        // Flip a 40x40 block near (100,100) — a "button reacted" style change.
        for y in 80..120 {
            for x in 80..120 {
                post.put_pixel(x, y, Rgba([240, 240, 240, 255]));
            }
        }
        let pre_p = png(&pre);
        let post_p = png(&post);
        // Region around the action point catches it strongly.
        let r = compare_frames(&pre_p, &post_p, Some((60, 60, 80, 80))).unwrap();
        assert!(r.changed);
        assert!(
            r.region_ratio > 0.1,
            "region ratio should be high, got {}",
            r.region_ratio
        );
        // silence unused-mut on pre in case of edits
        pre.put_pixel(0, 0, Rgba([30, 30, 30, 255]));
    }

    #[test]
    fn region_outside_bounds_falls_back_to_global() {
        let a = png(&solid(100, 100, [10, 10, 10, 255]));
        let mut b = solid(100, 100, [10, 10, 10, 255]);
        b.put_pixel(50, 50, Rgba([200, 200, 200, 255]));
        let b = png(&b);
        // Region entirely off-image → region_ratio == global_ratio.
        let r = compare_frames(&a, &b, Some((999, 999, 50, 50))).unwrap();
        assert_eq!(r.region_ratio, r.global_ratio);
    }

    #[test]
    fn describe_warns_loudly_on_no_change() {
        let r = ChangeReport {
            global_ratio: 0.0,
            region_ratio: 0.0,
            changed: false,
        };
        let msg = r.describe(true);
        assert!(msg.contains("NO visible change"));
        assert!(msg.to_lowercase().contains("do not report success"));
    }
}
