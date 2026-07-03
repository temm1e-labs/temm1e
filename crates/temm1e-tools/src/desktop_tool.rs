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
         - screenshot: Capture the entire screen\n\
         - click: Click at (x, y) coordinates\n\
         - double_click: Double-click at (x, y)\n\
         - right_click: Right-click at (x, y)\n\
         - type: Type a text string\n\
         - key: Press a key combination (e.g. 'cmd+c', 'ctrl+shift+a', 'enter')\n\
         - scroll: Scroll at (x, y) with dx/dy amounts\n\
         - drag: Drag from (x1,y1) to (x2,y2)\n\
         - zoom_region: Crop a region from the last screenshot for detailed analysis\n\n\
         Coordinates are in logical pixels (not physical). On Retina displays, \
         the screen resolution is halved (e.g. 1470x956 logical for a 2940x1912 physical display).\n\n\
         Vision workflow: screenshot → analyze image → click at coordinates → screenshot → verify.";
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
                let screenshot = self.controller.capture()?;

                // Store as base64 for vision pipeline
                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(&screenshot.png_data);
                if let Ok(mut img) = self.last_image.lock() {
                    *img = Some(ToolOutputImage {
                        media_type: "image/png".to_string(),
                        data: b64,
                    });
                }

                Ok(ToolOutput {
                    content: format!(
                        "Desktop screenshot captured: {}x{} logical ({}x{} physical, scale={}). \
                         {} bytes. The image is now visible for analysis. \
                         Use click with x,y coordinates to interact with elements you see.",
                        screenshot.width,
                        screenshot.height,
                        screenshot.physical_width,
                        screenshot.physical_height,
                        screenshot.scale_factor,
                        screenshot.png_data.len()
                    ),
                    is_error: false,
                })
            }

            "click" => {
                let x = get_coord(&input, "x")?;
                let y = get_coord(&input, "y")?;
                self.controller.click(x, y)?;
                // Brief sync sleep for UI to settle (enigo is sync anyway)
                std::thread::sleep(std::time::Duration::from_millis(300));
                let screenshot = self.controller.capture()?;
                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(&screenshot.png_data);
                if let Ok(mut img) = self.last_image.lock() {
                    *img = Some(ToolOutputImage {
                        media_type: "image/png".to_string(),
                        data: b64,
                    });
                }
                Ok(ToolOutput {
                    content: format!(
                        "Clicked at ({}, {}). Post-click screenshot captured for verification.",
                        x, y
                    ),
                    is_error: false,
                })
            }

            "double_click" => {
                let x = get_coord(&input, "x")?;
                let y = get_coord(&input, "y")?;
                self.controller.double_click(x, y)?;
                Ok(ToolOutput {
                    content: format!("Double-clicked at ({}, {})", x, y),
                    is_error: false,
                })
            }

            "right_click" => {
                let x = get_coord(&input, "x")?;
                let y = get_coord(&input, "y")?;
                self.controller.right_click(x, y)?;
                Ok(ToolOutput {
                    content: format!("Right-clicked at ({}, {})", x, y),
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
                Ok(ToolOutput {
                    content: format!("Typed {} characters", text.len()),
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
                Ok(ToolOutput {
                    content: format!("Pressed key combo: {}", combo),
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
                Ok(ToolOutput {
                    content: format!("Scrolled at ({}, {}) dx={} dy={}", x, y, dx, dy),
                    is_error: false,
                })
            }

            "drag" => {
                let x1 = get_coord(&input, "x1")?;
                let y1 = get_coord(&input, "y1")?;
                let x2 = get_coord(&input, "x2")?;
                let y2 = get_coord(&input, "y2")?;
                self.controller.drag(x1, y1, x2, y2)?;
                Ok(ToolOutput {
                    content: format!("Dragged from ({},{}) to ({},{})", x1, y1, x2, y2),
                    is_error: false,
                })
            }

            "zoom_region" => {
                let x1 = get_coord(&input, "x1")? as u32;
                let y1 = get_coord(&input, "y1")? as u32;
                let x2 = get_coord(&input, "x2")? as u32;
                let y2 = get_coord(&input, "y2")? as u32;

                // Capture current screen and crop the region
                let screenshot = self.controller.capture()?;
                // Scale coordinates from logical to physical for cropping
                let s = screenshot.scale_factor;
                let px1 = (x1 as f32 * s) as u32;
                let py1 = (y1 as f32 * s) as u32;
                let px2 = (x2 as f32 * s) as u32;
                let py2 = (y2 as f32 * s) as u32;

                let cropped = self
                    .controller
                    .crop_region(&screenshot, px1, py1, px2, py2)?;

                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(&cropped);
                if let Ok(mut img) = self.last_image.lock() {
                    *img = Some(ToolOutputImage {
                        media_type: "image/png".to_string(),
                        data: b64,
                    });
                }

                Ok(ToolOutput {
                    content: format!(
                        "Zoomed into desktop region ({},{})->({},{}) ({} bytes). \
                         Use click with coordinates from the FULL screen (not this zoomed view).",
                        x1,
                        y1,
                        x2,
                        y2,
                        cropped.len()
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
