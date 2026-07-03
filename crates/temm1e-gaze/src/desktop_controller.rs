//! Desktop screen controller — capture screenshots and simulate input at the OS level.

use crate::input::{InputEnv, InputRoute};
use crate::platform;
use enigo::{Axis, Button, Coordinate, Direction, Enigo, Keyboard, Mouse, Settings};
use image::ImageFormat;
use std::io::Cursor;
use temm1e_core::types::error::Temm1eError;

/// Screenshot captured from the desktop.
pub struct Screenshot {
    /// Raw PNG bytes.
    pub png_data: Vec<u8>,
    /// Logical width (physical / scale_factor).
    pub width: u32,
    /// Logical height (physical / scale_factor).
    pub height: u32,
    /// Physical width (actual pixels in the image).
    pub physical_width: u32,
    /// Physical height (actual pixels in the image).
    pub physical_height: u32,
    /// DPI scale factor (2.0 for Retina, 1.0 for standard).
    pub scale_factor: f32,
}

/// Controls the desktop — captures screenshots and simulates mouse/keyboard input.
///
/// Uses `xcap` for screen capture and `enigo` for input simulation.
/// On macOS, input simulation requires Accessibility permission.
///
/// `Enigo` is created fresh per input operation to avoid `Send/Sync` issues
/// with macOS Core Graphics pointers. The initialization overhead is negligible.
pub struct DesktopController {
    monitor_index: usize,
    /// Chosen input backend for this host (enigo / ydotool / unavailable).
    route: InputRoute,
    /// Native Wayland input via the RemoteDesktop portal. Lazily established on the
    /// FIRST input use (one permission prompt), then reused for the process
    /// lifetime. Preferred over ydotool on Wayland because it positions the pointer
    /// EXACTLY (ydotool's absolute motion is distorted by pointer acceleration).
    #[cfg(feature = "wayland-libei")]
    libei: std::sync::OnceLock<Option<crate::libei::LibeiController>>,
    /// Whether to attempt libei at all (true only on Wayland).
    #[cfg(feature = "wayland-libei")]
    prefer_libei: bool,
}

impl DesktopController {
    /// Create a new DesktopController for the given monitor index.
    ///
    /// Screen capture is always available. Input simulation may fail on macOS
    /// if Accessibility permission has not been granted — in that case,
    /// `input_available()` returns false and input methods return clear errors.
    pub fn new(monitor_index: usize) -> Result<Self, Temm1eError> {
        // Verify the monitor exists
        let monitors = xcap::Monitor::all()
            .map_err(|e| Temm1eError::Tool(format!("Failed to enumerate monitors: {}", e)))?;
        if monitors.is_empty() {
            return Err(Temm1eError::Tool("No monitors found".into()));
        }
        if monitor_index >= monitors.len() {
            return Err(Temm1eError::Tool(format!(
                "Monitor index {} out of range (found {} monitors)",
                monitor_index,
                monitors.len()
            )));
        }

        // Probe enigo, then choose an input backend. On Linux/Wayland enigo's XTEST
        // backend is silently ignored by the compositor, so a successful probe there
        // does NOT mean input works — `resolve_route` accounts for that.
        let settings = Settings::default();
        let enigo_ok = Enigo::new(&settings).is_ok();
        let route = crate::input::resolve_route(enigo_ok, &InputEnv::detect());
        match &route {
            InputRoute::Unavailable(reason) => {
                tracing::warn!(reason = %reason, "Desktop input simulation unavailable");
            }
            ready => {
                tracing::info!(status = %ready.status_note(), "Desktop input ready");
            }
        }

        #[cfg(feature = "wayland-libei")]
        let prefer_libei = InputEnv::detect().is_wayland();

        Ok(Self {
            monitor_index,
            route,
            #[cfg(feature = "wayland-libei")]
            libei: std::sync::OnceLock::new(),
            #[cfg(feature = "wayland-libei")]
            prefer_libei,
        })
    }

    /// Lazily establish (once) and return the libei portal controller, if this is a
    /// Wayland host and the portal is available. The first call blocks on the
    /// permission prompt; failures cache as `None` so we fall back to ydotool/enigo
    /// and never re-prompt.
    #[cfg(feature = "wayland-libei")]
    fn libei(&self) -> Option<&crate::libei::LibeiController> {
        if !self.prefer_libei {
            return None;
        }
        self.libei
            .get_or_init(|| match crate::libei::LibeiController::new() {
                Ok(controller) => {
                    tracing::info!(
                        "Desktop input via libei (RemoteDesktop portal) — exact positioning"
                    );
                    Some(controller)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "libei unavailable; falling back to ydotool/enigo");
                    None
                }
            })
            .as_ref()
    }

    /// Whether input simulation (click, type, key) is available.
    /// False on Wayland without a working backend, or macOS without Accessibility.
    pub fn input_available(&self) -> bool {
        self.route.is_available()
    }

    /// Human-readable status of the chosen input backend (for the tool description/logs).
    pub fn input_status_note(&self) -> String {
        self.route.status_note()
    }

    /// Capture the current screen as a PNG screenshot.
    pub fn capture(&self) -> Result<Screenshot, Temm1eError> {
        let monitors = xcap::Monitor::all()
            .map_err(|e| Temm1eError::Tool(format!("Failed to enumerate monitors: {}", e)))?;
        let monitor = monitors.get(self.monitor_index).ok_or_else(|| {
            Temm1eError::Tool(format!("Monitor {} not found", self.monitor_index))
        })?;

        let img = monitor
            .capture_image()
            .map_err(|e| Temm1eError::Tool(format!("Screen capture failed: {}", e)))?;

        let physical_width = img.width();
        let physical_height = img.height();
        let scale_factor = monitor.scale_factor().unwrap_or(1.0);
        let logical_width = (physical_width as f32 / scale_factor) as u32;
        let logical_height = (physical_height as f32 / scale_factor) as u32;

        let mut png_data = Vec::new();
        img.write_to(&mut Cursor::new(&mut png_data), ImageFormat::Png)
            .map_err(|e| Temm1eError::Tool(format!("PNG encoding failed: {}", e)))?;

        tracing::debug!(
            physical_w = physical_width,
            physical_h = physical_height,
            logical_w = logical_width,
            logical_h = logical_height,
            scale = scale_factor,
            png_bytes = png_data.len(),
            "Desktop screenshot captured"
        );

        Ok(Screenshot {
            png_data,
            width: logical_width,
            height: logical_height,
            physical_width,
            physical_height,
            scale_factor,
        })
    }

    /// Crop a region from a screenshot and return at higher detail.
    /// Coordinates are in the physical pixel space of the captured image.
    pub fn crop_region(
        &self,
        screenshot: &Screenshot,
        x1: u32,
        y1: u32,
        x2: u32,
        y2: u32,
    ) -> Result<Vec<u8>, Temm1eError> {
        if x2 <= x1 || y2 <= y1 {
            return Err(Temm1eError::Tool(format!(
                "Invalid crop region: ({},{})->({},{})",
                x1, y1, x2, y2
            )));
        }

        let img = image::load_from_memory(&screenshot.png_data)
            .map_err(|e| Temm1eError::Tool(format!("Failed to load screenshot: {}", e)))?;

        let crop_x = x1.min(img.width().saturating_sub(1));
        let crop_y = y1.min(img.height().saturating_sub(1));
        let crop_w = (x2 - x1).min(img.width() - crop_x);
        let crop_h = (y2 - y1).min(img.height() - crop_y);

        let cropped = img.crop_imm(crop_x, crop_y, crop_w, crop_h);

        let mut png_data = Vec::new();
        cropped
            .write_to(&mut Cursor::new(&mut png_data), ImageFormat::Png)
            .map_err(|e| Temm1eError::Tool(format!("Crop PNG encoding failed: {}", e)))?;

        Ok(png_data)
    }

    /// Get monitor dimensions in logical coordinates.
    pub fn dimensions(&self) -> Result<(u32, u32), Temm1eError> {
        let monitors = xcap::Monitor::all()
            .map_err(|e| Temm1eError::Tool(format!("Monitor query failed: {}", e)))?;
        let monitor = monitors
            .get(self.monitor_index)
            .ok_or_else(|| Temm1eError::Tool("Monitor not found".into()))?;
        let w = monitor.width().unwrap_or(0);
        let h = monitor.height().unwrap_or(0);
        Ok((w, h))
    }

    /// Get DPI scale factor.
    pub fn scale_factor(&self) -> Result<f32, Temm1eError> {
        let monitors = xcap::Monitor::all()
            .map_err(|e| Temm1eError::Tool(format!("Monitor query failed: {}", e)))?;
        let monitor = monitors
            .get(self.monitor_index)
            .ok_or_else(|| Temm1eError::Tool("Monitor not found".into()))?;
        Ok(monitor.scale_factor().unwrap_or(1.0))
    }

    // --- Input simulation methods ---
    // All require Accessibility permission on macOS.

    /// Guard for input methods: returns the active route, or an error if input is
    /// unavailable — so callers never fabricate success.
    fn input_route(&self) -> Result<&InputRoute, Temm1eError> {
        match &self.route {
            InputRoute::Unavailable(msg) => Err(Temm1eError::Tool(msg.clone())),
            route => Ok(route),
        }
    }

    /// Create a fresh Enigo instance for input simulation (Enigo route only).
    fn new_enigo(&self) -> Result<Enigo, Temm1eError> {
        let settings = Settings::default();
        Enigo::new(&settings)
            .map_err(|e| Temm1eError::Tool(format!("Failed to initialize input simulation: {}", e)))
    }

    /// Convert a LOGICAL coordinate into what the enigo backend consumes on this OS.
    ///
    /// On Linux, enigo drives X11 via XTEST, whose coordinate space is the PHYSICAL
    /// framebuffer — so a logical coordinate must be scaled by the DPI factor (a
    /// no-op at scale 1.0). macOS (Core Graphics points) and Windows (DPI-unaware
    /// SendInput) already accept logical coordinates and pass through unchanged. The
    /// ydotool/Wayland path never calls this: it consumes logical layout coordinates
    /// directly, so its callers pass logical values straight through.
    fn enigo_coords(&self, x_logical: i32, y_logical: i32) -> (i32, i32) {
        if cfg!(target_os = "linux") {
            let scale = self.scale_factor().unwrap_or(1.0);
            scale_point(x_logical, y_logical, scale)
        } else {
            (x_logical, y_logical)
        }
    }

    /// Move mouse to logical coordinates and click.
    pub fn click(&self, x: i32, y: i32) -> Result<(), Temm1eError> {
        #[cfg(feature = "wayland-libei")]
        if let Some(libei) = self.libei() {
            return libei.click(x, y);
        }
        if let InputRoute::Ydotool = self.input_route()? {
            return crate::input::yd_click(x, y);
        }
        let (x, y) = self.enigo_coords(x, y);
        let mut enigo = self.new_enigo()?;

        enigo
            .move_mouse(x, y, Coordinate::Abs)
            .map_err(|e| Temm1eError::Tool(format!("Mouse move failed: {}", e)))?;
        enigo
            .button(Button::Left, Direction::Click)
            .map_err(|e| Temm1eError::Tool(format!("Click failed: {}", e)))?;

        tracing::debug!(x, y, "Desktop click");
        Ok(())
    }

    /// Double-click at logical coordinates.
    pub fn double_click(&self, x: i32, y: i32) -> Result<(), Temm1eError> {
        #[cfg(feature = "wayland-libei")]
        if let Some(libei) = self.libei() {
            return libei.double_click(x, y);
        }
        if let InputRoute::Ydotool = self.input_route()? {
            return crate::input::yd_double_click(x, y);
        }
        let (x, y) = self.enigo_coords(x, y);
        let mut enigo = self.new_enigo()?;

        enigo
            .move_mouse(x, y, Coordinate::Abs)
            .map_err(|e| Temm1eError::Tool(format!("Mouse move failed: {}", e)))?;
        enigo
            .button(Button::Left, Direction::Click)
            .map_err(|e| Temm1eError::Tool(format!("Double-click 1 failed: {}", e)))?;
        enigo
            .button(Button::Left, Direction::Click)
            .map_err(|e| Temm1eError::Tool(format!("Double-click 2 failed: {}", e)))?;

        tracing::debug!(x, y, "Desktop double-click");
        Ok(())
    }

    /// Right-click at logical coordinates.
    pub fn right_click(&self, x: i32, y: i32) -> Result<(), Temm1eError> {
        #[cfg(feature = "wayland-libei")]
        if let Some(libei) = self.libei() {
            return libei.right_click(x, y);
        }
        if let InputRoute::Ydotool = self.input_route()? {
            return crate::input::yd_right_click(x, y);
        }
        let (x, y) = self.enigo_coords(x, y);
        let mut enigo = self.new_enigo()?;

        enigo
            .move_mouse(x, y, Coordinate::Abs)
            .map_err(|e| Temm1eError::Tool(format!("Mouse move failed: {}", e)))?;
        enigo
            .button(Button::Right, Direction::Click)
            .map_err(|e| Temm1eError::Tool(format!("Right-click failed: {}", e)))?;

        tracing::debug!(x, y, "Desktop right-click");
        Ok(())
    }

    /// Type a text string.
    pub fn type_text(&self, text: &str) -> Result<(), Temm1eError> {
        #[cfg(feature = "wayland-libei")]
        if let Some(libei) = self.libei() {
            return libei.type_text(text);
        }
        if let InputRoute::Ydotool = self.input_route()? {
            return crate::input::yd_type(text);
        }
        let mut enigo = self.new_enigo()?;

        enigo
            .text(text)
            .map_err(|e| Temm1eError::Tool(format!("Type text failed: {}", e)))?;

        tracing::debug!(len = text.len(), "Desktop type_text");
        Ok(())
    }

    /// Press a key combination (e.g., "cmd+c", "ctrl+shift+a", "enter", "tab").
    pub fn key_combo(&self, combo: &str) -> Result<(), Temm1eError> {
        #[cfg(feature = "wayland-libei")]
        if let Some(libei) = self.libei() {
            return libei.key_combo(combo);
        }
        if let InputRoute::Ydotool = self.input_route()? {
            return crate::input::yd_key(combo);
        }
        let mut enigo = self.new_enigo()?;

        let keys = platform::parse_key_combo(combo)?;

        // Press all modifier keys down, then the main key, then release all
        for key in &keys {
            enigo
                .key(*key, Direction::Press)
                .map_err(|e| Temm1eError::Tool(format!("Key press failed: {}", e)))?;
        }
        for key in keys.iter().rev() {
            enigo
                .key(*key, Direction::Release)
                .map_err(|e| Temm1eError::Tool(format!("Key release failed: {}", e)))?;
        }

        tracing::debug!(combo, "Desktop key_combo");
        Ok(())
    }

    /// Scroll at the current mouse position.
    pub fn scroll(&self, x: i32, y: i32, dx: i32, dy: i32) -> Result<(), Temm1eError> {
        if let InputRoute::Ydotool = self.input_route()? {
            return Err(Temm1eError::Tool(
                "scroll is not yet supported by the ydotool (Wayland) backend; \
                 use the browser tool for scrollable web content."
                    .into(),
            ));
        }
        let (x, y) = self.enigo_coords(x, y);
        let mut enigo = self.new_enigo()?;

        enigo
            .move_mouse(x, y, Coordinate::Abs)
            .map_err(|e| Temm1eError::Tool(format!("Mouse move failed: {}", e)))?;

        if dy != 0 {
            enigo
                .scroll(dy, Axis::Vertical)
                .map_err(|e| Temm1eError::Tool(format!("Vertical scroll failed: {}", e)))?;
        }
        if dx != 0 {
            enigo
                .scroll(dx, Axis::Horizontal)
                .map_err(|e| Temm1eError::Tool(format!("Horizontal scroll failed: {}", e)))?;
        }

        tracing::debug!(x, y, dx, dy, "Desktop scroll");
        Ok(())
    }

    /// Drag from (x1, y1) to (x2, y2).
    pub fn drag(&self, x1: i32, y1: i32, x2: i32, y2: i32) -> Result<(), Temm1eError> {
        #[cfg(feature = "wayland-libei")]
        if let Some(libei) = self.libei() {
            return libei.drag(x1, y1, x2, y2);
        }
        if let InputRoute::Ydotool = self.input_route()? {
            return crate::input::yd_drag(x1, y1, x2, y2);
        }
        let (x1, y1) = self.enigo_coords(x1, y1);
        let (x2, y2) = self.enigo_coords(x2, y2);
        let mut enigo = self.new_enigo()?;

        enigo
            .move_mouse(x1, y1, Coordinate::Abs)
            .map_err(|e| Temm1eError::Tool(format!("Drag start move failed: {}", e)))?;
        enigo
            .button(Button::Left, Direction::Press)
            .map_err(|e| Temm1eError::Tool(format!("Drag press failed: {}", e)))?;
        enigo
            .move_mouse(x2, y2, Coordinate::Abs)
            .map_err(|e| Temm1eError::Tool(format!("Drag end move failed: {}", e)))?;
        enigo
            .button(Button::Left, Direction::Release)
            .map_err(|e| Temm1eError::Tool(format!("Drag release failed: {}", e)))?;

        tracing::debug!(x1, y1, x2, y2, "Desktop drag");
        Ok(())
    }
}

/// Scale a LOGICAL point to PHYSICAL pixels (nearest integer), for backends whose
/// coordinate space is the physical framebuffer (enigo/X11). Identity at scale 1.0
/// or non-positive scale, so it never perturbs the common unscaled case.
fn scale_point(x_logical: i32, y_logical: i32, scale: f32) -> (i32, i32) {
    if scale <= 0.0 || (scale - 1.0).abs() < f32::EPSILON {
        return (x_logical, y_logical);
    }
    (
        (x_logical as f32 * scale).round() as i32,
        (y_logical as f32 * scale).round() as i32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore] // Requires display — skip in headless CI
    fn test_new_primary_monitor() {
        let result = DesktopController::new(0);
        assert!(result.is_ok(), "Primary monitor should be available");
    }

    #[test]
    #[ignore] // Requires display — skip in headless CI
    fn test_new_invalid_monitor() {
        let result = DesktopController::new(99);
        assert!(result.is_err(), "Monitor 99 should not exist");
    }

    #[test]
    #[ignore] // Requires display — skip in headless CI
    fn test_capture_screenshot() {
        let ctrl = DesktopController::new(0).expect("Primary monitor");
        let screenshot = ctrl.capture().expect("Capture should succeed");
        assert!(screenshot.png_data.len() > 1000, "PNG should have data");
        assert!(screenshot.width > 0, "Width should be positive");
        assert!(screenshot.height > 0, "Height should be positive");
        assert!(
            screenshot.physical_width >= screenshot.width,
            "Physical >= logical"
        );
        assert!(screenshot.scale_factor >= 1.0, "Scale factor >= 1.0");
    }

    #[test]
    #[ignore] // Requires display — skip in headless CI
    fn test_crop_region() {
        let ctrl = DesktopController::new(0).expect("Primary monitor");
        let screenshot = ctrl.capture().expect("Capture");
        let cropped = ctrl.crop_region(&screenshot, 0, 0, 200, 200);
        assert!(cropped.is_ok(), "Crop should succeed");
        let cropped = cropped.unwrap();
        assert!(cropped.len() > 100, "Cropped PNG should have data");
        assert!(
            cropped.len() < screenshot.png_data.len(),
            "Cropped should be smaller than full"
        );
    }

    #[test]
    #[ignore] // Requires display — skip in headless CI
    fn test_crop_invalid_region() {
        let ctrl = DesktopController::new(0).expect("Primary monitor");
        let screenshot = ctrl.capture().expect("Capture");
        let result = ctrl.crop_region(&screenshot, 500, 500, 100, 100);
        assert!(result.is_err(), "Reversed coords should fail");
    }

    #[test]
    #[ignore] // Requires display — skip in headless CI
    fn test_dimensions() {
        let ctrl = DesktopController::new(0).expect("Primary monitor");
        let (w, h) = ctrl.dimensions().expect("Dimensions");
        assert!(w > 0 && h > 0, "Dimensions should be positive");
    }

    #[test]
    #[ignore] // Requires display — skip in headless CI
    fn test_scale_factor() {
        let ctrl = DesktopController::new(0).expect("Primary monitor");
        let s = ctrl.scale_factor().expect("Scale factor");
        assert!(s >= 1.0, "Scale factor should be >= 1.0");
    }

    #[test]
    fn scale_point_is_identity_at_1x_and_scales_on_hidpi() {
        assert_eq!(scale_point(100, 200, 1.0), (100, 200), "1.0 is a no-op");
        assert_eq!(scale_point(0, 0, 2.0), (0, 0));
        assert_eq!(scale_point(100, 200, 2.0), (200, 400), "2x → physical");
        assert_eq!(scale_point(100, 200, 1.5), (150, 300), "fractional scale");
        assert_eq!(
            scale_point(50, 50, 0.0),
            (50, 50),
            "non-positive scale guarded"
        );
    }
}
