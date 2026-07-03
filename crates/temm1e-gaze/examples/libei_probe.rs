//! Probe: native Wayland input via the XDG RemoteDesktop portal (ashpd), proving
//! EXACT absolute pointer positioning — no ydotool, no acceleration.
//!
//!   cargo run -p temm1e-gaze --example libei_probe --features wayland-libei -- info
//!   cargo run -p temm1e-gaze --example libei_probe --features wayland-libei -- move <x> <y>
//!   cargo run -p temm1e-gaze --example libei_probe --features wayland-libei -- click <x> <y>
//!   cargo run -p temm1e-gaze --example libei_probe --features wayland-libei -- rclick <x> <y>
//!
//! (x, y) are GLOBAL logical screen coordinates. On FIRST run the portal shows a
//! permission dialog — pick your monitor and click Share/Allow. The grant is saved
//! as a restore token so later runs don't prompt.

use ashpd::desktop::{
    remote_desktop::{DeviceType, KeyState, RemoteDesktop},
    screencast::{CursorMode, Screencast, SourceType},
    PersistMode,
};

// Linux evdev button codes.
const BTN_LEFT: i32 = 0x110;
const BTN_RIGHT: i32 = 0x111;

fn token_path() -> String {
    format!(
        "{}/.cache/temm1e/libei_restore_token",
        std::env::var("HOME").unwrap_or_else(|_| "/tmp".into())
    )
}

#[tokio::main]
async fn main() -> ashpd::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let action = args.get(1).map(String::as_str).unwrap_or("info");
    let x: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let y: f64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0.0);

    let saved_token = std::fs::read_to_string(token_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    eprintln!(
        "restore token: {}",
        if saved_token.is_some() {
            "present (should not re-prompt)"
        } else {
            "none (FIRST RUN — approve the dialog)"
        }
    );

    let remote_desktop = RemoteDesktop::new().await?;
    let screencast = Screencast::new().await?;
    let session = remote_desktop.create_session().await?;
    eprintln!("session created");

    remote_desktop
        .select_devices(
            &session,
            DeviceType::Keyboard | DeviceType::Pointer,
            saved_token.as_deref(),
            PersistMode::DoNot,
        )
        .await?;
    eprintln!("devices selected (pointer+keyboard)");

    // A monitor screencast stream defines the absolute coordinate space.
    screencast
        .select_sources(
            &session,
            CursorMode::Metadata,
            SourceType::Monitor.into(),
            false,
            saved_token.as_deref(),
            PersistMode::DoNot,
        )
        .await?;
    eprintln!("sources selected (monitor)");

    eprintln!(
        "→ calling Start(): APPROVE the portal dialog if it appears (pick monitor → Share/Allow)…"
    );
    let response = remote_desktop.start(&session, None).await?.response()?;
    eprintln!(
        "✓ session STARTED — devices granted: {:?}",
        response.devices()
    );

    if let Some(token) = response.restore_token() {
        let _ = std::fs::create_dir_all(format!(
            "{}/.cache/temm1e",
            std::env::var("HOME").unwrap_or_else(|_| "/tmp".into())
        ));
        if std::fs::write(token_path(), token).is_ok() {
            eprintln!(
                "✓ saved restore token ({} bytes) — no prompt next time",
                token.len()
            );
        }
    }

    let streams = response.streams().unwrap_or(&[]);
    if streams.is_empty() {
        eprintln!("no screencast streams — cannot do absolute positioning");
        return Ok(());
    }
    for s in streams {
        eprintln!(
            "stream node={} position={:?} size={:?}",
            s.pipe_wire_node_id(),
            s.position(),
            s.size()
        );
    }

    // Pick the stream whose region contains (x, y); fall back to the first.
    let stream = streams
        .iter()
        .find(|s| {
            let (sx, sy) = s.position().unwrap_or((0, 0));
            let (sw, sh) = s.size().unwrap_or((0, 0));
            let (xi, yi) = (x as i32, y as i32);
            xi >= sx && xi < sx + sw && yi >= sy && yi < sy + sh
        })
        .unwrap_or(&streams[0]);
    let node = stream.pipe_wire_node_id();
    let (sx, sy) = stream.position().unwrap_or((0, 0));
    let (lx, ly) = (x - sx as f64, y - sy as f64);

    match action {
        "info" => eprintln!("info only (session established, streams listed above)"),
        "move" => {
            remote_desktop
                .notify_pointer_motion_absolute(&session, node, lx, ly)
                .await?;
            eprintln!("moved to global ({x},{y}) → stream-local ({lx},{ly}) node {node}");
        }
        "click" | "rclick" => {
            let button = if action == "rclick" {
                BTN_RIGHT
            } else {
                BTN_LEFT
            };
            remote_desktop
                .notify_pointer_motion_absolute(&session, node, lx, ly)
                .await?;
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            remote_desktop
                .notify_pointer_button(&session, button, KeyState::Pressed)
                .await?;
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            remote_desktop
                .notify_pointer_button(&session, button, KeyState::Released)
                .await?;
            eprintln!("{action} at global ({x},{y}) → stream-local ({lx},{ly}) node {node}");
        }
        other => eprintln!("unknown action: {other}"),
    }

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    Ok(())
}
