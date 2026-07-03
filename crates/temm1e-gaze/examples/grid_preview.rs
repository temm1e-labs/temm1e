//! Dev preview: render a synthetic Teams-like UI and overlay the coordinate grid,
//! so the desktop Set-of-Mark grid can be eyeballed for readability/usefulness.
//!
//! Run: cargo run -p temm1e-gaze --example grid_preview -- <out.png>

use image::{Rgba, RgbaImage};
use std::io::Cursor;

fn fill(img: &mut RgbaImage, x0: u32, y0: u32, x1: u32, y1: u32, c: Rgba<u8>) {
    for y in y0..y1.min(img.height()) {
        for x in x0..x1.min(img.width()) {
            img.put_pixel(x, y, c);
        }
    }
}

fn border(img: &mut RgbaImage, x0: u32, y0: u32, x1: u32, y1: u32, c: Rgba<u8>) {
    for x in x0..x1.min(img.width()) {
        img.put_pixel(x, y0, c);
        img.put_pixel(x, (y1 - 1).min(img.height() - 1), c);
    }
    for y in y0..y1.min(img.height()) {
        img.put_pixel(x0, y, c);
        img.put_pixel((x1 - 1).min(img.width() - 1), y, c);
    }
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "grid_preview.png".to_string());

    let (w, h) = (1400u32, 900u32);
    let mut img = RgbaImage::from_pixel(w, h, Rgba([245, 246, 248, 255])); // light content bg

    fill(&mut img, 0, 0, 260, h, Rgba([32, 34, 40, 255])); // dark left rail (chat list)
    fill(&mut img, 260, 0, w, 64, Rgba([255, 255, 255, 255])); // top bar
    fill(&mut img, 40, 120, 236, 172, Rgba([90, 120, 220, 255])); // selected group "mún work hard"
    fill(&mut img, 40, 188, 236, 240, Rgba([70, 72, 82, 255])); // another chat
    fill(&mut img, 40, 256, 236, 308, Rgba([70, 72, 82, 255])); // another chat
    fill(&mut img, 300, 820, w - 40, 872, Rgba([255, 255, 255, 255])); // message input
    border(&mut img, 300, 820, w - 40, 872, Rgba([170, 172, 180, 255]));
    fill(
        &mut img,
        w - 120,
        828,
        w - 52,
        864,
        Rgba([40, 170, 90, 255]),
    ); // green send button

    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();

    let gridded = temm1e_gaze::overlay::overlay_coordinate_grid(&png, 1.0, 100).unwrap();
    std::fs::write(&out, &gridded).unwrap();
    println!("wrote {} ({} bytes)", out, gridded.len());
}
