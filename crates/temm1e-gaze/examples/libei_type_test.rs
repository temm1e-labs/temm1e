//! Keyboard proof: click into a text field to focus it, then type a string — both
//! through the integrated `DesktopController` → libei path, in ONE portal session.
//!
//!   cargo run -p temm1e-gaze --example libei_type_test --features wayland-libei \
//!       -- <out.png> "<text>" <focus_x> <focus_y>

use std::time::Duration;
use temm1e_gaze::DesktopController;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "/tmp/type_test.png".into());
    let text = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "TEMM1E gaze 789".into());
    let fx: i32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(600);
    let fy: i32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(300);

    let ctrl = DesktopController::new(0).expect("DesktopController");
    eprintln!("click ({fx},{fy}) to focus, then type {text:?} via libei…");
    if let Err(e) = ctrl.click(fx, fy) {
        eprintln!("click FAIL: {e}");
        return;
    }
    std::thread::sleep(Duration::from_millis(500));
    if let Err(e) = ctrl.type_text(&text) {
        eprintln!("type FAIL: {e}");
        return;
    }
    std::thread::sleep(Duration::from_millis(500));

    if let Ok(shot) = ctrl.capture() {
        let gridded =
            temm1e_gaze::overlay::overlay_coordinate_grid(&shot.png_data, shot.scale_factor, 150)
                .unwrap_or(shot.png_data);
        if std::fs::write(&out, &gridded).is_ok() {
            eprintln!("saved {out}");
        }
    }
}
