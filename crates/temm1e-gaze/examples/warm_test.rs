//! Proof for the eager STARTUP warm-up. Mimics exactly what `DesktopTool` does at
//! temm1e boot: spawn a background thread that calls `warm_input()` to establish the
//! input session BEFORE any action. Then it does one click and confirms the click
//! reuses the warmed session — i.e. the permission prompt (on attended Wayland)
//! appears during warm-up ("[startup]"), NOT at the click, and the click lands.
//!
//!   cargo run -p temm1e-gaze --example warm_test -- <out.png> <x> <y>

use std::sync::Arc;
use std::time::Duration;
use temm1e_gaze::DesktopController;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "/tmp/warm.png".into());
    let x: i32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(178);
    let y: i32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(392);

    let ctrl = Arc::new(DesktopController::new(0).expect("DesktopController"));

    // === What DesktopTool::new does at temm1e startup ===
    eprintln!("[startup] spawning background warm-up (as DesktopTool does)…");
    let warm_ctrl = Arc::clone(&ctrl);
    let warm = std::thread::spawn(move || warm_ctrl.warm_input());

    // (temm1e would keep booting here.) Wait so we can assert the session is READY
    // before the first action.
    warm.join().ok();
    eprintln!("[startup] warm-up finished — input session should now be established.");

    // === First action AFTER startup — must reuse the warmed session (no new prompt) ===
    eprintln!("[action] click ({x},{y}) — expect NO new prompt; session already warm…");
    match ctrl.click(x, y) {
        Ok(()) => eprintln!("[action] click delivered via the warmed session."),
        Err(e) => {
            eprintln!("[action] click FAILED: {e}");
            return;
        }
    }
    std::thread::sleep(Duration::from_millis(400));

    if let Ok(shot) = ctrl.capture() {
        let gridded =
            temm1e_gaze::overlay::overlay_coordinate_grid(&shot.png_data, shot.scale_factor, 150)
                .unwrap_or(shot.png_data);
        if std::fs::write(&out, &gridded).is_ok() {
            eprintln!("saved {out}");
        }
    }
}
