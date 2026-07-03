//! Integration + session-reuse proof: ONE `DesktopController`, multiple libei
//! clicks through the real input path, with a SINGLE permission prompt (the portal
//! session is established once and reused).
//!
//!   cargo run -p temm1e-gaze --example libei_ctrl_test --features wayland-libei \
//!       -- <out.png> <x1> <y1> <x2> <y2> ...

use temm1e_gaze::DesktopController;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "/tmp/ctrl_test.png".into());
    let coords: Vec<i32> = args.iter().skip(2).filter_map(|s| s.parse().ok()).collect();

    let ctrl = DesktopController::new(0).expect("DesktopController");
    eprintln!(
        "clicking {} point(s) via ONE session (first click shows the portal prompt once)…",
        coords.len() / 2
    );
    for pair in coords.chunks(2) {
        if let [x, y] = pair {
            match ctrl.click(*x, *y) {
                Ok(()) => eprintln!("  clicked ({x},{y})"),
                Err(e) => {
                    eprintln!("  FAIL ({x},{y}): {e}");
                    return;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(350));
        }
    }

    if let Ok(shot) = ctrl.capture() {
        let gridded =
            temm1e_gaze::overlay::overlay_coordinate_grid(&shot.png_data, shot.scale_factor, 150)
                .unwrap_or(shot.png_data);
        if std::fs::write(&out, &gridded).is_ok() {
            eprintln!("saved {out}");
        }
    }
}
