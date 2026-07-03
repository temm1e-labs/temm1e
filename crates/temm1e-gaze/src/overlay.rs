//! SoM (Set-of-Mark) overlay compositing for desktop screenshots.
//!
//! Unlike browser SoM overlays (injected via JavaScript), desktop overlays
//! are composited directly onto the PNG image before sending to the VLM.

use image::{Rgba, RgbaImage};
use std::io::Cursor;
use temm1e_core::types::error::Temm1eError;

/// A candidate element for SoM labeling.
pub struct SomCandidate {
    /// The label index (1-based, matching accessibility tree if available).
    pub index: u32,
    /// Center X coordinate in the screenshot (physical pixels).
    pub center_x: u32,
    /// Center Y coordinate in the screenshot (physical pixels).
    pub center_y: u32,
}

const SOM_RADIUS: i32 = 14;
const SOM_COLOR: Rgba<u8> = Rgba([229, 62, 62, 255]); // #e53e3e red
const SOM_TEXT_COLOR: Rgba<u8> = Rgba([255, 255, 255, 255]); // white

/// Composite numbered SoM labels onto a screenshot PNG.
///
/// Draws red filled circles with white index numbers at each candidate's
/// position. Returns the modified PNG bytes.
///
/// If `candidates` is empty, returns the original PNG unchanged.
pub fn overlay_som_labels(
    screenshot_png: &[u8],
    candidates: &[SomCandidate],
) -> Result<Vec<u8>, Temm1eError> {
    if candidates.is_empty() {
        return Ok(screenshot_png.to_vec());
    }

    let img = image::load_from_memory(screenshot_png)
        .map_err(|e| Temm1eError::Tool(format!("Failed to load screenshot for overlay: {}", e)))?;

    let mut rgba = img.into_rgba8();

    for candidate in candidates {
        draw_som_label(&mut rgba, candidate);
    }

    let mut output = Vec::new();
    image::DynamicImage::ImageRgba8(rgba)
        .write_to(&mut Cursor::new(&mut output), image::ImageFormat::Png)
        .map_err(|e| Temm1eError::Tool(format!("Failed to encode overlay PNG: {}", e)))?;

    Ok(output)
}

/// Draw a single SoM label (red circle with index number) onto the image.
fn draw_som_label(img: &mut RgbaImage, candidate: &SomCandidate) {
    let cx = candidate.center_x as i32;
    let cy = candidate.center_y as i32;
    let w = img.width() as i32;
    let h = img.height() as i32;

    // Draw filled circle
    for dy in -SOM_RADIUS..=SOM_RADIUS {
        for dx in -SOM_RADIUS..=SOM_RADIUS {
            if dx * dx + dy * dy <= SOM_RADIUS * SOM_RADIUS {
                let px = cx + dx;
                let py = cy + dy;
                if px >= 0 && px < w && py >= 0 && py < h {
                    img.put_pixel(px as u32, py as u32, SOM_COLOR);
                }
            }
        }
    }

    // Draw index number using simple bitmap font (no external font dependency)
    let text = candidate.index.to_string();
    let char_w = 5;
    let char_h = 7;
    let text_w = text.len() as i32 * (char_w + 1);
    let start_x = cx - text_w / 2;
    let start_y = cy - char_h / 2;

    for (ci, ch) in text.chars().enumerate() {
        let bitmap = char_bitmap(ch);
        let offset_x = start_x + ci as i32 * (char_w + 1);
        for (row, bits) in bitmap.iter().enumerate() {
            for col in 0..char_w {
                if bits & (1 << (char_w - 1 - col)) != 0 {
                    let px = offset_x + col;
                    let py = start_y + row as i32;
                    if px >= 0 && px < w && py >= 0 && py < h {
                        img.put_pixel(px as u32, py as u32, SOM_TEXT_COLOR);
                    }
                }
            }
        }
    }
}

/// 5x7 bitmap font for digits 0-9. Each u8 encodes one row (5 bits used).
fn char_bitmap(ch: char) -> [u8; 7] {
    match ch {
        '0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        '2' => [
            0b01110, 0b10001, 0b00001, 0b00110, 0b01000, 0b10000, 0b11111,
        ],
        '3' => [
            0b01110, 0b10001, 0b00001, 0b00110, 0b00001, 0b10001, 0b01110,
        ],
        '4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
        ],
        '6' => [
            0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
        ],
        ',' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00110, 0b00100, 0b01000,
        ],
        _ => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000,
        ],
    }
}

// ─── Desktop coordinate-grid overlay (grid-based Set-of-Mark) ────────────────
//
// The desktop path has no accessibility tree to source element marks from (unlike
// the browser path, which numbers CDP a11y nodes). So instead of *element* SoM we
// composite a labelled coordinate grid onto the screenshot. This gives the VLM
// explicit, readable coordinate anchors — in the same LOGICAL pixel space the
// `click` action consumes — so it can target points by interpolation off the grid
// instead of eyeballing raw pixels (which SOTA VLMs are still unreliable at).

/// Grid line colour — magenta is rare in UI chrome, so it reads clearly.
const GRID_COLOR: Rgba<u8> = Rgba([255, 0, 200, 255]);
/// Alpha for blended grid lines (keep the underlying UI visible beneath them).
const GRID_ALPHA: f32 = 0.32;
/// Coordinate-label text colour.
const GRID_LABEL_FG: Rgba<u8> = Rgba([255, 255, 255, 255]);
/// Dark chip drawn behind labels so digits stay legible over any background.
const GRID_LABEL_BG: Rgba<u8> = Rgba([0, 0, 0, 255]);
const GRID_LABEL_BG_ALPHA: f32 = 0.58;

/// Composite a labelled coordinate grid onto a full-screen screenshot PNG.
///
/// `scale_factor` maps LOGICAL → physical pixels (e.g. 2.0 on Retina, 1.0 on a
/// standard display). Grid lines are spaced every `step_logical` LOGICAL pixels
/// and drawn at the matching physical position; each line is labelled with its
/// LOGICAL coordinate — exactly the value the `click` action expects. Returns the
/// original PNG unchanged if `step_logical` is 0.
pub fn overlay_coordinate_grid(
    screenshot_png: &[u8],
    scale_factor: f32,
    step_logical: u32,
) -> Result<Vec<u8>, Temm1eError> {
    overlay_coordinate_grid_with_origin(screenshot_png, scale_factor, step_logical, 0, 0)
}

/// Like [`overlay_coordinate_grid`], but the image's top-left corner corresponds
/// to LOGICAL coordinate (`origin_logical_x`, `origin_logical_y`). Used for zoomed
/// crops so the grid labels still read as FULL-screen coordinates (what `click`
/// consumes), not positions within the crop.
pub fn overlay_coordinate_grid_with_origin(
    screenshot_png: &[u8],
    scale_factor: f32,
    step_logical: u32,
    origin_logical_x: u32,
    origin_logical_y: u32,
) -> Result<Vec<u8>, Temm1eError> {
    if step_logical == 0 {
        return Ok(screenshot_png.to_vec());
    }
    let scale = if scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    let img = image::load_from_memory(screenshot_png)
        .map_err(|e| Temm1eError::Tool(format!("Failed to load screenshot for grid: {}", e)))?;
    let mut rgba = img.into_rgba8();
    let pw = rgba.width() as i32;
    let ph = rgba.height() as i32;
    let step = step_logical;

    // LOGICAL coordinate → physical pixel offset within this image.
    let phys_x = |xl: u32| (((xl - origin_logical_x) as f32) * scale).round() as i32;
    let phys_y = |yl: u32| (((yl - origin_logical_y) as f32) * scale).round() as i32;

    // First gridline at/after the origin (keeps labels on round logical values).
    let first_x = origin_logical_x.div_ceil(step) * step;
    let first_y = origin_logical_y.div_ceil(step) * step;

    // Vertical lines + x-axis labels along the top edge.
    let mut xl = first_x;
    while phys_x(xl) < pw {
        let px = phys_x(xl);
        draw_vline_blend(&mut rgba, px, GRID_COLOR, GRID_ALPHA);
        draw_grid_label(&mut rgba, &xl.to_string(), px + 2, 2);
        xl += step;
    }

    // Horizontal lines + y-axis labels down the left edge.
    let mut yl = first_y;
    while phys_y(yl) < ph {
        let py = phys_y(yl);
        draw_hline_blend(&mut rgba, py, GRID_COLOR, GRID_ALPHA);
        draw_grid_label(&mut rgba, &yl.to_string(), 2, py + 2);
        yl += step;
    }

    // Intersection anchors: a dot at every crossing, plus an "x,y" chip every 3rd
    // gridline so there are in-field coordinate anchors even where axis labels are
    // far away or the edges are occluded.
    let mut yy = first_y;
    while phys_y(yy) < ph {
        let py = phys_y(yy);
        let mut xx = first_x;
        while phys_x(xx) < pw {
            let px = phys_x(xx);
            draw_dot_blend(&mut rgba, px, py, GRID_COLOR, 1);
            if xx % (step * 3) == 0 && yy % (step * 3) == 0 {
                draw_grid_label(&mut rgba, &format!("{},{}", xx, yy), px + 3, py + 3);
            }
            xx += step;
        }
        yy += step;
    }

    let mut output = Vec::new();
    image::DynamicImage::ImageRgba8(rgba)
        .write_to(&mut Cursor::new(&mut output), image::ImageFormat::Png)
        .map_err(|e| Temm1eError::Tool(format!("Failed to encode grid PNG: {}", e)))?;
    Ok(output)
}

/// Alpha-blend `color` over the existing pixel at (x, y). Out-of-bounds is a no-op.
fn blend_pixel(img: &mut RgbaImage, x: i32, y: i32, color: Rgba<u8>, alpha: f32) {
    if x < 0 || y < 0 {
        return;
    }
    let (ux, uy) = (x as u32, y as u32);
    if ux >= img.width() || uy >= img.height() {
        return;
    }
    let a = alpha.clamp(0.0, 1.0);
    let bg = img.get_pixel(ux, uy).0;
    let mix = |c: u8, b: u8| ((c as f32 * a) + (b as f32 * (1.0 - a))).round() as u8;
    img.put_pixel(
        ux,
        uy,
        Rgba([
            mix(color.0[0], bg[0]),
            mix(color.0[1], bg[1]),
            mix(color.0[2], bg[2]),
            255,
        ]),
    );
}

/// Opaque put with bounds clipping (used for label glyphs).
fn put_pixel_clip(img: &mut RgbaImage, x: i32, y: i32, color: Rgba<u8>) {
    if x < 0 || y < 0 {
        return;
    }
    let (ux, uy) = (x as u32, y as u32);
    if ux < img.width() && uy < img.height() {
        img.put_pixel(ux, uy, color);
    }
}

fn draw_vline_blend(img: &mut RgbaImage, x: i32, color: Rgba<u8>, alpha: f32) {
    let h = img.height() as i32;
    for y in 0..h {
        blend_pixel(img, x, y, color, alpha);
    }
}

fn draw_hline_blend(img: &mut RgbaImage, y: i32, color: Rgba<u8>, alpha: f32) {
    let w = img.width() as i32;
    for x in 0..w {
        blend_pixel(img, x, y, color, alpha);
    }
}

fn draw_dot_blend(img: &mut RgbaImage, cx: i32, cy: i32, color: Rgba<u8>, r: i32) {
    for dy in -r..=r {
        for dx in -r..=r {
            if dx * dx + dy * dy <= r * r {
                blend_pixel(img, cx + dx, cy + dy, color, 0.95);
            }
        }
    }
}

/// Draw a short numeric label (digits and `,`) with a translucent dark chip behind
/// it for legibility over arbitrary UI content.
fn draw_grid_label(img: &mut RgbaImage, text: &str, ox: i32, oy: i32) {
    const CHAR_W: i32 = 5;
    const CHAR_H: i32 = 7;
    const GAP: i32 = 1;
    let text_w = text.chars().count() as i32 * (CHAR_W + GAP) + 1;

    // Chip.
    for by in (oy - 1)..=(oy + CHAR_H) {
        for bx in (ox - 1)..=(ox + text_w) {
            blend_pixel(img, bx, by, GRID_LABEL_BG, GRID_LABEL_BG_ALPHA);
        }
    }

    // Glyphs.
    for (ci, ch) in text.chars().enumerate() {
        let bitmap = char_bitmap(ch);
        let gx = ox + ci as i32 * (CHAR_W + GAP);
        for (row, bits) in bitmap.iter().enumerate() {
            for col in 0..CHAR_W {
                if bits & (1 << (CHAR_W - 1 - col)) != 0 {
                    put_pixel_clip(img, gx + col, oy + row as i32, GRID_LABEL_FG);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_png(w: u32, h: u32) -> Vec<u8> {
        let img = RgbaImage::from_pixel(w, h, Rgba([200, 200, 200, 255]));
        let mut buf = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    #[test]
    fn overlay_empty_candidates() {
        let png = make_test_png(100, 100);
        let result = overlay_som_labels(&png, &[]).unwrap();
        assert_eq!(result, png, "Empty candidates should return unchanged PNG");
    }

    #[test]
    fn overlay_single_label() {
        let png = make_test_png(200, 200);
        let candidates = vec![SomCandidate {
            index: 1,
            center_x: 100,
            center_y: 100,
        }];
        let result = overlay_som_labels(&png, &candidates).unwrap();
        assert_ne!(result, png, "Overlay should modify the image");
        assert!(result.len() > 100, "Result should be valid PNG");
    }

    #[test]
    fn overlay_multiple_labels() {
        let png = make_test_png(400, 400);
        let candidates: Vec<SomCandidate> = (1..=20)
            .map(|i| SomCandidate {
                index: i,
                center_x: (i * 18) % 380 + 20,
                center_y: (i * 15) % 380 + 20,
            })
            .collect();
        let result = overlay_som_labels(&png, &candidates).unwrap();
        assert!(
            result.len() > 100,
            "Result should be valid PNG with 20 labels"
        );
    }

    #[test]
    fn overlay_edge_coordinates() {
        let png = make_test_png(100, 100);
        let candidates = vec![
            SomCandidate {
                index: 1,
                center_x: 0,
                center_y: 0,
            },
            SomCandidate {
                index: 2,
                center_x: 99,
                center_y: 99,
            },
        ];
        let result = overlay_som_labels(&png, &candidates);
        assert!(result.is_ok(), "Edge coordinates should not crash");
    }

    #[test]
    fn char_bitmap_coverage() {
        for ch in '0'..='9' {
            let bm = char_bitmap(ch);
            let has_pixels = bm.iter().any(|row| *row != 0);
            assert!(has_pixels, "Digit {} should have non-zero bitmap", ch);
        }
    }

    #[test]
    fn grid_zero_step_returns_unchanged() {
        let png = make_test_png(200, 200);
        let out = overlay_coordinate_grid(&png, 1.0, 0).unwrap();
        assert_eq!(out, png, "step 0 should be a no-op");
    }

    #[test]
    fn grid_overlays_and_preserves_dimensions() {
        let png = make_test_png(400, 300);
        let out = overlay_coordinate_grid(&png, 1.0, 100).unwrap();
        assert_ne!(out, png, "grid should modify the image");
        let img = image::load_from_memory(&out).expect("valid PNG");
        assert_eq!(
            (img.width(), img.height()),
            (400, 300),
            "grid must not change image size"
        );
    }

    #[test]
    fn grid_with_retina_scale_is_valid() {
        // 800x600 physical from a 2x display → logical 400x300; labels use logical.
        let png = make_test_png(800, 600);
        let out = overlay_coordinate_grid(&png, 2.0, 100).unwrap();
        let img = image::load_from_memory(&out).expect("valid PNG");
        assert_eq!((img.width(), img.height()), (800, 600));
    }

    #[test]
    fn grid_origin_shifts_lines() {
        let png = make_test_png(400, 300);
        let a = overlay_coordinate_grid_with_origin(&png, 1.0, 100, 0, 0).unwrap();
        let b = overlay_coordinate_grid_with_origin(&png, 1.0, 100, 150, 150).unwrap();
        assert_ne!(a, b, "different origins should place gridlines differently");
    }

    #[test]
    fn grid_handles_tiny_image_without_panic() {
        let png = make_test_png(10, 10);
        let out = overlay_coordinate_grid(&png, 1.0, 100).unwrap();
        assert!(!out.is_empty());
    }

    #[test]
    fn comma_glyph_has_pixels() {
        assert!(
            char_bitmap(',').iter().any(|r| *r != 0),
            "comma glyph should render pixels for x,y anchor labels"
        );
    }
}
