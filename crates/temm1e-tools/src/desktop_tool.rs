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
         correct. NEVER report an action as done unless the follow-up screenshot actually shows it \
         happened. If the screenshot does not show the change, the action FAILED — say so and \
         adjust; do not claim success you cannot see.";

    /// Capture the screen, composite the coordinate grid, stash it as the tool's
    /// `last_image` (the runtime feeds that back to the model as vision), and return
    /// a short note describing the capture for the tool-result text. Shared by the
    /// `screenshot` action and every mutating action so the model always gets fresh,
    /// gridded visual evidence to verify against — this is the "capture-to-validate"
    /// half of the airtight loop.
    fn capture_gridded(&self) -> Result<String, Temm1eError> {
        let shot = self.controller.capture()?;
        let step = grid_step_for(shot.width);
        let gridded = match temm1e_gaze::overlay::overlay_coordinate_grid(
            &shot.png_data,
            shot.scale_factor,
            step,
        ) {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!(error = %e, "Grid overlay failed; falling back to raw screenshot");
                shot.png_data
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

        Ok(format!(
            "{}x{} logical (scale {}); a {}px labelled coordinate grid is overlaid — read X off \
             the top axis and Y off the left axis (LOGICAL pixels, exactly what click/type use).",
            shot.width, shot.height, shot.scale_factor, step
        ))
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
                self.controller.click(x, y)?;
                // Brief settle so the UI can react before we re-capture.
                std::thread::sleep(std::time::Duration::from_millis(350));
                let note = self.capture_gridded()?;
                Ok(ToolOutput {
                    content: format!(
                        "Clicked at ({x}, {y}). Fresh post-action screenshot captured: {note} \
                         VERIFY the intended change is visible before proceeding — if it is not, \
                         the click missed its target; do not claim success."
                    ),
                    is_error: false,
                })
            }

            "double_click" => {
                let x = get_coord(&input, "x")?;
                let y = get_coord(&input, "y")?;
                self.controller.double_click(x, y)?;
                std::thread::sleep(std::time::Duration::from_millis(350));
                let note = self.capture_gridded()?;
                Ok(ToolOutput {
                    content: format!(
                        "Double-clicked at ({x}, {y}). Fresh screenshot captured: {note} Verify \
                         the result before proceeding."
                    ),
                    is_error: false,
                })
            }

            "right_click" => {
                let x = get_coord(&input, "x")?;
                let y = get_coord(&input, "y")?;
                self.controller.right_click(x, y)?;
                std::thread::sleep(std::time::Duration::from_millis(350));
                let note = self.capture_gridded()?;
                Ok(ToolOutput {
                    content: format!(
                        "Right-clicked at ({x}, {y}). Fresh screenshot captured: {note} Verify the \
                         context menu / result is visible."
                    ),
                    is_error: false,
                })
            }

            "type" => {
                let text = input
                    .arguments
                    .get("text")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        Temm1eError::Tool("'type' action requires 'text' parameter".into())
                    })?;
                self.controller.type_text(text)?;
                std::thread::sleep(std::time::Duration::from_millis(250));
                let note = self.capture_gridded()?;
                Ok(ToolOutput {
                    content: format!(
                        "Typed {} characters. Fresh screenshot captured: {note} Verify the text \
                         actually appears in the intended field before continuing.",
                        text.len()
                    ),
                    is_error: false,
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
                self.controller.key_combo(combo)?;
                std::thread::sleep(std::time::Duration::from_millis(250));
                let note = self.capture_gridded()?;
                Ok(ToolOutput {
                    content: format!(
                        "Pressed key combo: {combo}. Fresh screenshot captured: {note} Verify the \
                         expected result (e.g. message sent, dialog closed) is visible."
                    ),
                    is_error: false,
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
                self.controller.scroll(x, y, dx, dy)?;
                std::thread::sleep(std::time::Duration::from_millis(250));
                let note = self.capture_gridded()?;
                Ok(ToolOutput {
                    content: format!(
                        "Scrolled at ({x}, {y}) dx={dx} dy={dy}. Fresh screenshot captured: {note}"
                    ),
                    is_error: false,
                })
            }

            "drag" => {
                let x1 = get_coord(&input, "x1")?;
                let y1 = get_coord(&input, "y1")?;
                let x2 = get_coord(&input, "x2")?;
                let y2 = get_coord(&input, "y2")?;
                self.controller.drag(x1, y1, x2, y2)?;
                std::thread::sleep(std::time::Duration::from_millis(350));
                let note = self.capture_gridded()?;
                Ok(ToolOutput {
                    content: format!(
                        "Dragged from ({x1},{y1}) to ({x2},{y2}). Fresh screenshot captured: {note}"
                    ),
                    is_error: false,
                })
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
