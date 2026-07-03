//! Desktop control tool — captures the screen and simulates mouse/keyboard input
//! at the OS level. This is Tem Gaze's desktop computer use implementation.

use async_trait::async_trait;
use std::sync::Arc;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::{Tool, ToolContext, ToolDeclarations, ToolInput, ToolOutput, ToolOutputImage};
use temm1e_gaze::DesktopController;

/// Desktop control tool — full computer use via screen capture + input simulation.
pub struct DesktopTool {
    controller: Arc<DesktopController>,
    /// Full tool description incl. the runtime input-backend status.
    description: String,
    last_image: std::sync::Mutex<Option<ToolOutputImage>>,
}

impl DesktopTool {
    /// Create a new desktop tool for the given monitor.
    pub fn new(monitor_index: usize) -> Result<Self, Temm1eError> {
        let controller = DesktopController::new(monitor_index)?;
        let status_note = controller.input_status_note();
        tracing::info!(
            monitor = monitor_index,
            input = %status_note,
            "Desktop tool initialized"
        );
        let description = format!(
            "{}\n\nInput backend status on this host: {}",
            Self::BASE_DESCRIPTION,
            status_note
        );

        Ok(Self {
            controller: Arc::new(controller),
            description,
            last_image: std::sync::Mutex::new(None),
        })
    }

    /// Base description; `description()` appends the runtime input-backend status so
    /// the model knows up front whether desktop input actually works on this host.
    const BASE_DESCRIPTION: &'static str = "Control the computer desktop — capture screenshots, click at coordinates, \
         type text, press key combinations, scroll, and drag. Works at the OS level \
         on any application (not just the browser).\n\n\
         Actions:\n\
         - screenshot: Capture the entire screen (with coordinate grid)\n\
         - click: Click at (x, y) coordinates\n\
         - double_click: Double-click at (x, y)\n\
         - right_click: Right-click at (x, y)\n\
         - type: Type a text string\n\
         - key: Press a key combination (e.g. 'cmd+c', 'ctrl+shift+a', 'enter')\n\
         - scroll: Scroll at (x, y) with dx/dy amounts\n\
         - drag: Drag from (x1,y1) to (x2,y2)\n\
         - zoom_region: Crop+magnify a screen region for detailed analysis\n\n\
         COORDINATE GRID (Set-of-Mark): every screenshot this tool returns has a magenta \
         coordinate grid composited on top — grid lines at fixed intervals, with the LOGICAL \
         x-values labelled along the TOP edge, y-values down the LEFT edge, and 'x,y' anchor \
         labels at intersections. Read your target's coordinates directly off this grid instead \
         of guessing raw pixels. All coordinates are LOGICAL pixels (not physical); on Retina/HiDPI \
         the logical resolution is the physical divided by the scale factor (e.g. 1470x956 logical \
         for a 2940x1912 physical display).\n\n\
         AIRTIGHT LOOP — every mutating action auto-captures a FRESH gridded screenshot so you can \
         VERIFY its effect: screenshot → find target on grid → click/type at grid coords → (tool \
         returns a new gridded screenshot) → confirm the expected change is visible → continue or \
         correct. The tool ALSO measures how much the screen changed after each action and reports \
         it: if it says 'NO visible change detected', the action had no effect (missed target / dead \
         space / wrong coordinates) — retry with adjusted coordinates, do NOT claim success. NEVER \
         report an action as done unless BOTH the screenshot shows the change AND the change signal \
         confirms it; do not claim success you cannot see.";

    /// Overlay the coordinate grid onto a captured screenshot, stash it as the
    /// tool's `last_image` (the runtime feeds that back to the model as vision),
    /// and return a short note describing it for the tool-result text.
    fn store_gridded(&self, shot: &temm1e_gaze::desktop_controller::Screenshot) -> String {
        let step = grid_step_for(shot.width);
        let gridded = match temm1e_gaze::overlay::overlay_coordinate_grid(
            &shot.png_data,
            shot.scale_factor,
            step,
        ) {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!(error = %e, "Grid overlay failed; falling back to raw screenshot");
                shot.png_data.clone()
            }
        };

        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&gridded);
        if let Ok(mut img) = self.last_image.lock() {
            *img = Some(ToolOutputImage {
                media_type: "image/png".to_string(),
                data: b64,
            });
        }

        format!(
            "{}x{} logical (scale {}); a {}px labelled coordinate grid is overlaid — read X off \
             the top axis and Y off the left axis (LOGICAL pixels, exactly what click/type use).",
            shot.width, shot.height, shot.scale_factor, step
        )
    }

    /// Capture a fresh screenshot and grid it (for the `screenshot` action).
    fn capture_gridded(&self) -> Result<String, Temm1eError> {
        let shot = self.controller.capture()?;
        Ok(self.store_gridded(&shot))
    }

    /// Run a mutating action inside the airtight verify loop: capture a pre-frame,
    /// perform `act`, settle, capture a post-frame (gridded + stored as last_image),
    /// diff the two, and fold an OBJECTIVE change report into the result. `roi_center`
    /// is the LOGICAL action point that focuses the diff (None = whole screen).
    ///
    /// The change report is the load-bearing honesty signal: if nothing moved after
    /// the action, the result says so loudly so the model cannot truthfully claim
    /// success it did not cause. If `act` fails, its error propagates unchanged — the
    /// low-level primitives never fabricate success, and neither does this.
    fn act_and_verify(
        &self,
        label: String,
        roi_center: Option<(i32, i32)>,
        settle_ms: u64,
        act: impl FnOnce() -> Result<(), Temm1eError>,
    ) -> Result<ToolOutput, Temm1eError> {
        let pre = self.controller.capture()?;
        act()?;
        std::thread::sleep(std::time::Duration::from_millis(settle_ms));
        let post = self.controller.capture()?;

        // Region of interest, in PHYSICAL pixels, around the action point.
        let region =
            roi_center.map(|(lx, ly)| roi_physical(lx, ly, post.scale_factor, ROI_HALF_LOGICAL));

        // Fail-open: a diff error must never block a legitimately-performed action.
        let report = temm1e_gaze::compare_frames(&pre.png_data, &post.png_data, region).unwrap_or(
            temm1e_gaze::ChangeReport {
                global_ratio: 0.0,
                region_ratio: 0.0,
                changed: true,
            },
        );

        let grid_note = self.store_gridded(&post);
        Ok(ToolOutput {
            content: format!(
                "{label}. {}\nFresh screenshot captured: {grid_note}",
                report.describe(region.is_some())
            ),
            is_error: false,
        })
    }
}

#[async_trait]
impl Tool for DesktopTool {
    fn name(&self) -> &str {
        "desktop"
    }

    fn description(&self) -> &str {
        self.description.as_str()
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["screenshot", "click", "double_click", "right_click",
                             "type", "key", "scroll", "drag", "zoom_region"],
                    "description": "The desktop action to perform"
                },
                "x": {
                    "type": "number",
                    "description": "X coordinate in logical pixels (for click, double_click, right_click, scroll)"
                },
                "y": {
                    "type": "number",
                    "description": "Y coordinate in logical pixels (for click, double_click, right_click, scroll)"
                },
                "text": {
                    "type": "string",
                    "description": "Text to type (for 'type' action) or key combo (for 'key' action, e.g. 'cmd+c')"
                },
                "dx": {
                    "type": "number",
                    "description": "Horizontal scroll amount (for 'scroll' action)"
                },
                "dy": {
                    "type": "number",
                    "description": "Vertical scroll amount (for 'scroll' action, positive=down)"
                },
                "x1": { "type": "number", "description": "Start/left X (for drag, zoom_region)" },
                "y1": { "type": "number", "description": "Start/top Y (for drag, zoom_region)" },
                "x2": { "type": "number", "description": "End/right X (for drag, zoom_region)" },
                "y2": { "type": "number", "description": "End/bottom Y (for drag, zoom_region)" }
            },
            "required": ["action"]
        })
    }

    fn declarations(&self) -> ToolDeclarations {
        ToolDeclarations {
            file_access: vec![],
            network_access: vec![],
            shell_access: false,
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, Temm1eError> {
        let action = input
            .arguments
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Temm1eError::Tool("Missing required parameter: action".into()))?;

        match action {
            "screenshot" => {
                let note = self.capture_gridded()?;
                Ok(ToolOutput {
                    content: format!(
                        "Desktop screenshot captured: {note} Locate your target on the grid, then \
                         use click/type/key with those LOGICAL coordinates."
                    ),
                    is_error: false,
                })
            }

            "click" => {
                let x = get_coord(&input, "x")?;
                let y = get_coord(&input, "y")?;
                self.act_and_verify(format!("Clicked at ({x}, {y})"), Some((x, y)), 350, || {
                    self.controller.click(x, y)
                })
            }

            "double_click" => {
                let x = get_coord(&input, "x")?;
                let y = get_coord(&input, "y")?;
                self.act_and_verify(
                    format!("Double-clicked at ({x}, {y})"),
                    Some((x, y)),
                    350,
                    || self.controller.double_click(x, y),
                )
            }

            "right_click" => {
                let x = get_coord(&input, "x")?;
                let y = get_coord(&input, "y")?;
                self.act_and_verify(
                    format!("Right-clicked at ({x}, {y})"),
                    Some((x, y)),
                    350,
                    || self.controller.right_click(x, y),
                )
            }

            "type" => {
                let text = input
                    .arguments
                    .get("text")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        Temm1eError::Tool("'type' action requires 'text' parameter".into())
                    })?;
                let n = text.chars().count();
                self.act_and_verify(format!("Typed {n} characters"), None, 250, || {
                    self.controller.type_text(text)
                })
            }

            "key" => {
                let combo = input
                    .arguments
                    .get("text")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        Temm1eError::Tool(
                            "'key' action requires 'text' parameter (e.g. 'cmd+c', 'enter')".into(),
                        )
                    })?;
                self.act_and_verify(format!("Pressed key combo: {combo}"), None, 250, || {
                    self.controller.key_combo(combo)
                })
            }

            "scroll" => {
                let x = get_coord(&input, "x")?;
                let y = get_coord(&input, "y")?;
                let dx = input
                    .arguments
                    .get("dx")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0) as i32;
                let dy = input
                    .arguments
                    .get("dy")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0) as i32;
                self.act_and_verify(
                    format!("Scrolled at ({x}, {y}) dx={dx} dy={dy}"),
                    Some((x, y)),
                    300,
                    || self.controller.scroll(x, y, dx, dy),
                )
            }

            "drag" => {
                let x1 = get_coord(&input, "x1")?;
                let y1 = get_coord(&input, "y1")?;
                let x2 = get_coord(&input, "x2")?;
                let y2 = get_coord(&input, "y2")?;
                self.act_and_verify(
                    format!("Dragged from ({x1},{y1}) to ({x2},{y2})"),
                    Some((x2, y2)),
                    350,
                    || self.controller.drag(x1, y1, x2, y2),
                )
            }

            "zoom_region" => {
                let x1 = get_coord(&input, "x1")? as u32;
                let y1 = get_coord(&input, "y1")? as u32;
                let x2 = get_coord(&input, "x2")? as u32;
                let y2 = get_coord(&input, "y2")? as u32;

                // Capture current screen and crop the region.
                let screenshot = self.controller.capture()?;
                // Scale coordinates from logical to physical for cropping.
                let s = screenshot.scale_factor;
                let px1 = (x1 as f32 * s) as u32;
                let py1 = (y1 as f32 * s) as u32;
                let px2 = (x2 as f32 * s) as u32;
                let py2 = (y2 as f32 * s) as u32;

                let cropped = self
                    .controller
                    .crop_region(&screenshot, px1, py1, px2, py2)?;

                // Overlay a finer grid whose labels stay in FULL-screen logical
                // coordinates (origin = crop's top-left), so clicks computed off the
                // zoom still target the real screen position.
                let crop_w = x2.saturating_sub(x1);
                let step = if crop_w <= 400 { 50 } else { 100 };
                let gridded = match temm1e_gaze::overlay::overlay_coordinate_grid_with_origin(
                    &cropped, s, step, x1, y1,
                ) {
                    Ok(g) => g,
                    Err(_) => cropped,
                };

                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(&gridded);
                if let Ok(mut img) = self.last_image.lock() {
                    *img = Some(ToolOutputImage {
                        media_type: "image/png".to_string(),
                        data: b64,
                    });
                }

                Ok(ToolOutput {
                    content: format!(
                        "Zoomed into region ({x1},{y1})->({x2},{y2}) with a {step}px grid whose \
                         labels are FULL-screen LOGICAL coordinates. Click using those coordinates \
                         (not positions within this magnified view)."
                    ),
                    is_error: false,
                })
            }

            other => Err(Temm1eError::Tool(format!(
                "Unknown desktop action: '{}'. Valid: screenshot, click, double_click, \
                 right_click, type, key, scroll, drag, zoom_region",
                other
            ))),
        }
    }

    fn take_last_image(&self) -> Option<ToolOutputImage> {
        self.last_image.lock().ok().and_then(|mut img| img.take())
    }
}

fn get_coord(input: &ToolInput, name: &str) -> Result<i32, Temm1eError> {
    input
        .arguments
        .get(name)
        .and_then(|v| v.as_f64())
        .map(|v| v as i32)
        .ok_or_else(|| Temm1eError::Tool(format!("Missing required parameter: '{}'", name)))
}

/// Half-width (in LOGICAL pixels) of the square region-of-interest centred on an
/// action point, used to focus post-action change detection near where the click/
/// drag landed. A 180px box catches the button/menu that should react without being
/// swamped by unrelated screen motion (cursor blink, clock).
const ROI_HALF_LOGICAL: i32 = 90;

/// Choose a coordinate-grid spacing (in LOGICAL pixels) that yields a readable
/// number of gridlines for the given logical screen width — finer on small
/// screens, coarser on large ones so labels never crowd.
fn grid_step_for(logical_w: u32) -> u32 {
    match logical_w {
        0..=1500 => 100,
        1501..=2600 => 150,
        _ => 200,
    }
}

/// Compute the PHYSICAL-pixel ROI rect `(x, y, w, h)` centred on a LOGICAL action
/// point, clamped at the top-left origin. `scale` maps logical→physical (the
/// screenshot is physical, but the model works in logical coordinates).
fn roi_physical(lx: i32, ly: i32, scale: f32, half_logical: i32) -> (u32, u32, u32, u32) {
    let s = if scale > 0.0 { scale } else { 1.0 };
    let half = (half_logical as f32 * s).max(1.0) as i32;
    let cx = (lx as f32 * s) as i32;
    let cy = (ly as f32 * s) as i32;
    let x0 = (cx - half).max(0) as u32;
    let y0 = (cy - half).max(0) as u32;
    let side = (half * 2).max(1) as u32;
    (x0, y0, side, side)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_step_scales_with_width() {
        assert_eq!(grid_step_for(1366), 100);
        assert_eq!(grid_step_for(1920), 150);
        assert_eq!(grid_step_for(3840), 200);
    }

    #[test]
    fn roi_physical_at_1x_centers_on_point() {
        // half=90 logical → 180px box around (500, 400) at scale 1.0.
        assert_eq!(roi_physical(500, 400, 1.0, 90), (410, 310, 180, 180));
    }

    #[test]
    fn roi_physical_scales_to_physical_on_hidpi() {
        // 2x: logical (500,400) → physical center (1000,800); half 90→180.
        assert_eq!(roi_physical(500, 400, 2.0, 90), (820, 620, 360, 360));
    }

    #[test]
    fn roi_physical_clamps_at_origin() {
        let (x, y, _, _) = roi_physical(10, 10, 1.0, 90);
        assert_eq!((x, y), (0, 0)); // 10-90 < 0 → clamped to 0
    }
}
