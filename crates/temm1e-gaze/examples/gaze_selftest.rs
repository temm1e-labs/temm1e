//! Live self-test for Tem Gaze on the REAL desktop. Exercises the actual library
//! code paths (capture → grid overlay → frame diff → input) so the loop can be
//! *proven*, not asserted. Safe: only ever drives windows the caller launches.
//!
//!   cargo run -p temm1e-gaze --example gaze_selftest -- info
//!   cargo run -p temm1e-gaze --example gaze_selftest -- capture <out.png>
//!   cargo run -p temm1e-gaze --example gaze_selftest -- click <x> <y> <out.png>
//!   cargo run -p temm1e-gaze --example gaze_selftest -- type <text> <out.png>

use temm1e_gaze::desktop_controller::Screenshot;
use temm1e_gaze::{compare_frames, DesktopController};

fn grid_step(w: u32) -> u32 {
    if w <= 1500 {
        100
    } else if w <= 2600 {
        150
    } else {
        200
    }
}

fn grid_and_save(shot: &Screenshot, path: &str) {
    let g = temm1e_gaze::overlay::overlay_coordinate_grid(
        &shot.png_data,
        shot.scale_factor,
        grid_step(shot.width),
    )
    .unwrap_or_else(|_| shot.png_data.clone());
    std::fs::write(path, &g).expect("write png");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("info");

    let ctrl = match DesktopController::new(0) {
        Ok(c) => c,
        Err(e) => {
            println!("FAIL DesktopController::new: {e}");
            std::process::exit(2);
        }
    };
    println!("input route/status : {}", ctrl.input_status_note());
    println!("input_available    : {}", ctrl.input_available());

    match cmd {
        "info" => match ctrl.capture() {
            Ok(s) => println!(
                "capture OK: {}x{} logical, {}x{} physical, scale {}",
                s.width, s.height, s.physical_width, s.physical_height, s.scale_factor
            ),
            Err(e) => {
                println!("capture FAIL: {e}");
                std::process::exit(4);
            }
        },

        "capture" => {
            let out = args
                .get(2)
                .cloned()
                .unwrap_or_else(|| "gaze_capture.png".into());
            let a = ctrl.capture().expect("capture A");
            println!(
                "captured {}x{} logical (scale {}), grid step {}px",
                a.width,
                a.height,
                a.scale_factor,
                grid_step(a.width)
            );
            std::thread::sleep(std::time::Duration::from_millis(200));
            let b = ctrl.capture().expect("capture B");
            let base = compare_frames(&a.png_data, &b.png_data, None).expect("diff");
            println!(
                "BASELINE static diff (no input): global_ratio={:.5} changed={}  (want ~0 / false)",
                base.global_ratio, base.changed
            );
            grid_and_save(&b, &out);
            println!("gridded screenshot saved: {out}");
        }

        "click" => {
            let x: i32 = args.get(2).expect("x").parse().expect("x int");
            let y: i32 = args.get(3).expect("y").parse().expect("y int");
            let out = args
                .get(4)
                .cloned()
                .unwrap_or_else(|| "gaze_click.png".into());
            let pre = ctrl.capture().expect("pre");
            grid_and_save(&pre, &format!("{out}.pre.png"));
            match ctrl.click(x, y) {
                Ok(()) => println!("click({x},{y}) delivered via {}", ctrl.input_status_note()),
                Err(e) => {
                    println!("click FAIL: {e}");
                    std::process::exit(3);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(450));
            let post = ctrl.capture().expect("post");
            let s = post.scale_factor.max(1.0);
            let half = (90.0 * s) as u32;
            let (cx, cy) = ((x as f32 * s) as u32, (y as f32 * s) as u32);
            let region = Some((
                cx.saturating_sub(half),
                cy.saturating_sub(half),
                half * 2,
                half * 2,
            ));
            let r = compare_frames(&pre.png_data, &post.png_data, region).expect("diff");
            println!(
                "CHANGE after click: global={:.5} region={:.5} changed={}",
                r.global_ratio, r.region_ratio, r.changed
            );
            println!("--> {}", r.describe(true));
            grid_and_save(&post, &out);
            println!("post-click gridded saved: {out}");
        }

        "type" => {
            let text = args
                .get(2)
                .cloned()
                .unwrap_or_else(|| "TEMM1E GAZE OK".into());
            let out = args
                .get(3)
                .cloned()
                .unwrap_or_else(|| "gaze_type.png".into());
            let pre = ctrl.capture().expect("pre");
            match ctrl.type_text(&text) {
                Ok(()) => println!("type({text:?}) delivered"),
                Err(e) => {
                    println!("type FAIL: {e}");
                    std::process::exit(3);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(450));
            let post = ctrl.capture().expect("post");
            let r = compare_frames(&pre.png_data, &post.png_data, None).expect("diff");
            println!(
                "CHANGE after type: global={:.5} changed={}",
                r.global_ratio, r.changed
            );
            println!("--> {}", r.describe(false));
            grid_and_save(&post, &out);
            println!("post-type gridded saved: {out}");
        }

        other => println!("unknown cmd: {other}"),
    }
}
