//! Tem Gaze — vision-primary desktop control for TEMM1E.
//!
//! Provides OS-level screen capture and input simulation for full computer
//! control. Uses `xcap` for cross-platform screen capture and `enigo` for
//! mouse/keyboard input simulation, with a `ydotool`/uinput fallback on Linux
//! Wayland (see [`input`]).
//!
//! ## Input permissions by platform
//!
//! - **macOS** — input simulation requires the Accessibility permission
//!   (System Settings → Privacy & Security → Accessibility) for the terminal or
//!   binary running TEMM1E.
//! - **Linux / X11** — works via enigo's XTEST backend.
//! - **Linux / Wayland** — GNOME and KDE ignore XTEST, so input is routed through
//!   `ydotool` (needs `ydotoold` running with access to `/dev/uinput`). Without a
//!   working backend, input methods return a clear error instead of silently doing
//!   nothing.
//! - **Windows** — works via enigo's SendInput backend.

pub mod desktop_controller;
pub mod input;
pub mod overlay;
pub mod platform;

pub use desktop_controller::DesktopController;
