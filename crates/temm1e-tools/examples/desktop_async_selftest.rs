//! Async-context self-test for the desktop tool.
//!
//! This is the check that was MISSING when v5.8.0 shipped: a screenshot that
//! panicked ("Cannot start a runtime from within a runtime") ONLY when the desktop
//! tool ran inside a tokio runtime — because `xcap`'s Wayland ScreenCast capture
//! drives a nested `block_on`, illegal on a runtime worker thread. Standalone SYNC
//! examples (plain `fn main`, e.g. `gaze_selftest`) never have an ambient runtime, so
//! they cannot catch it. This drives the REAL `DesktopTool` inside a MULTI-THREAD
//! tokio runtime — exactly the context the agent (`chat`/`start`/`tui`) uses.
//!
//! Requires a real display (X11 or Wayland). It performs ONLY a screenshot
//! (read-only) — it never clicks or types, so it is safe to run on a live desktop.
//!
//!   cargo run -p temm1e-tools --features desktop-control --example desktop_async_selftest
//!
//! (Set `TEMM1E_INPUT_BACKEND=ydotool` to skip the Wayland libei portal prompt during
//! automated runs — capture does not need it.)

use temm1e_core::{Tool, ToolContext, ToolInput};
use temm1e_tools::desktop_tool::DesktopTool;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let tool = DesktopTool::new(0).expect("DesktopTool::new failed (no monitor / no display?)");

    let ctx = ToolContext {
        workspace_path: std::env::temp_dir(),
        session_id: "async-selftest".to_string(),
        chat_id: "async-selftest".to_string(),
        read_tracker: None,
    };
    let input = ToolInput {
        name: "desktop".to_string(),
        arguments: serde_json::json!({ "action": "screenshot" }),
    };

    println!("Calling desktop.execute(screenshot) inside a multi-thread tokio runtime…");
    let out = tool.execute(input, &ctx).await.expect(
        "screenshot must succeed inside a tokio runtime \
         (regression guard: the v5.8.0 'runtime within a runtime' panic)",
    );
    assert!(
        !out.is_error,
        "screenshot returned is_error=true: {}",
        out.content
    );

    let img = tool
        .take_last_image()
        .expect("desktop screenshot must produce a gridded image");
    assert!(!img.data.is_empty(), "screenshot image data is empty");

    println!("tool result: {}", out.content);
    println!(
        "gridded image: {} bytes base64, media_type={}",
        img.data.len(),
        img.media_type
    );
    println!(
        "\nASYNC-CONTEXT SELF-TEST PASSED — no runtime-in-runtime panic; \
         capture works from inside the tokio runtime."
    );
}
