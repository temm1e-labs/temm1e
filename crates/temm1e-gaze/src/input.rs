//! Desktop input routing.
//!
//! `enigo` drives input on macOS (CoreGraphics), Windows (SendInput) and
//! Linux/**X11** (XTEST). On a Linux **Wayland** session, enigo's XTEST backend is
//! silently ignored by GNOME/KDE — the compositor drops synthetic X11 pointer and
//! keyboard events for security — so `move_mouse`/`button`/`key` return `Ok` while
//! doing nothing. Reporting that as success makes the agent believe it clicked when
//! it did not.
//!
//! To keep the desktop tool truthful, this module picks an [`InputRoute`] at
//! startup:
//! - **macOS / Windows / Linux-X11** → [`InputRoute::Enigo`] (the historical path,
//!   unchanged).
//! - **Linux-Wayland with `ydotool` installed** → [`InputRoute::Ydotool`], which
//!   injects input through the kernel `uinput` device (compositor-agnostic, works on
//!   GNOME/KDE/wlroots alike).
//! - **Linux-Wayland without `ydotool`** → [`InputRoute::Unavailable`], whose input
//!   methods return a clear, actionable error instead of fabricating success.

use std::process::Command;
use temm1e_core::types::error::Temm1eError;

/// How desktop input is delivered on this host.
#[derive(Debug, Clone)]
pub enum InputRoute {
    /// enigo: macOS / Windows / Linux-X11. The original, unchanged backend.
    Enigo,
    /// `ydotool` CLI over the kernel `uinput` device — used on Linux/Wayland where
    /// enigo's XTEST backend is ignored by the compositor.
    Ydotool,
    /// No working input path. The message explains why and how to fix it.
    Unavailable(String),
}

impl InputRoute {
    /// Whether this route can actually deliver input.
    pub fn is_available(&self) -> bool {
        !matches!(self, InputRoute::Unavailable(_))
    }

    /// A short human-readable status line for the tool description / logs.
    pub fn status_note(&self) -> String {
        match self {
            InputRoute::Enigo => "input simulation available (enigo)".to_string(),
            InputRoute::Ydotool => {
                if ydotoold_socket_present() {
                    "input simulation available (ydotool/uinput — Wayland)".to_string()
                } else {
                    "input via ydotool/uinput (Wayland) — but NO ydotoold socket detected, the \
                     daemon may not be running; if clicks/keys fail, start it with `sudo ydotoold` \
                     (or `systemctl --user start ydotool`)"
                        .to_string()
                }
            }
            InputRoute::Unavailable(msg) => format!("input simulation UNAVAILABLE — {msg}"),
        }
    }
}

/// Best-effort check for a live `ydotoold` daemon socket. `ydotool` talks to
/// `ydotoold` over a unix socket; if none exists the daemon is almost certainly
/// not running and input will fail. Used ONLY to enrich the status note — input is
/// still attempted and errors honestly at action time, so this never fabricates
/// availability, it only warns earlier. (The original incident had `ydotoold`
/// crashed while the tool still reported "available".)
pub fn ydotoold_socket_present() -> bool {
    if let Some(sock) = std::env::var_os("YDOTOOL_SOCKET") {
        return std::path::Path::new(&sock).exists();
    }
    if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        if std::path::Path::new(&runtime_dir)
            .join(".ydotool_socket")
            .exists()
        {
            return true;
        }
    }
    std::path::Path::new("/tmp/.ydotool_socket").exists()
}

/// Runtime environment facts used to choose an [`InputRoute`].
#[derive(Debug, Clone)]
pub struct InputEnv {
    /// Whether the target OS is Linux.
    pub is_linux: bool,
    /// `XDG_SESSION_TYPE` (e.g. "wayland", "x11"), if set.
    pub session_type: Option<String>,
    /// `WAYLAND_DISPLAY`, if set and non-empty.
    pub wayland_display: Option<String>,
    /// Whether a `ydotool` executable is on `PATH`.
    pub ydotool_available: bool,
}

impl InputEnv {
    /// Detect the current environment.
    pub fn detect() -> Self {
        Self {
            is_linux: cfg!(target_os = "linux"),
            session_type: std::env::var("XDG_SESSION_TYPE").ok(),
            wayland_display: std::env::var("WAYLAND_DISPLAY")
                .ok()
                .filter(|s| !s.is_empty()),
            ydotool_available: ydotool_on_path(),
        }
    }

    /// Whether this is a Wayland session.
    pub fn is_wayland(&self) -> bool {
        self.session_type.as_deref() == Some("wayland") || self.wayland_display.is_some()
    }
}

/// Which desktop input backend to prefer. Set via the `TEMM1E_INPUT_BACKEND`
/// environment variable (`auto` | `enigo` | `ydotool` | `libei`).
///
/// - `Auto` (default) — right for INTERACTIVE use: libei on Wayland (exact, but a
///   one-time portal permission prompt), enigo on X11/macOS/Windows.
/// - `Ydotool` — for UNATTENDED Wayland (e.g. a cloud VPS with no one to answer the
///   portal dialog): needs a one-time `/dev/uinput` setup but NEVER prompts.
/// - `Enigo` — force enigo (X11 / macOS / Windows).
/// - `Libei` — force the Wayland RemoteDesktop portal even off Wayland detection.
///
/// On an X11 host, `Auto` already selects enigo/XTEST (exact + promptless), so an
/// X11 VPS needs no setting at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputBackendPref {
    #[default]
    Auto,
    Enigo,
    Ydotool,
    Libei,
}

impl InputBackendPref {
    /// Read the preference from `TEMM1E_INPUT_BACKEND`; unknown/empty → `Auto`.
    pub fn from_env() -> Self {
        match std::env::var("TEMM1E_INPUT_BACKEND")
            .ok()
            .map(|s| s.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("enigo") => Self::Enigo,
            Some("ydotool") => Self::Ydotool,
            Some("libei") => Self::Libei,
            _ => Self::Auto,
        }
    }

    /// Whether the libei/RemoteDesktop-portal backend should be attempted. It shows
    /// a one-time permission prompt and blocks until answered, so `Enigo`/`Ydotool`
    /// return false — unattended hosts must never hang on that dialog.
    pub fn attempt_libei(self, is_wayland: bool) -> bool {
        match self {
            Self::Libei => true,
            Self::Auto => is_wayland,
            Self::Enigo | Self::Ydotool => false,
        }
    }
}

/// Choose the input route from the environment and whether enigo initialized.
///
/// Pure function (no I/O) so the full decision matrix is unit-testable regardless of
/// the host it runs on.
pub fn resolve_route(enigo_ok: bool, env: &InputEnv) -> InputRoute {
    if !env.is_linux {
        // macOS / Windows: enigo is the backend, exactly as before.
        return if enigo_ok {
            InputRoute::Enigo
        } else {
            InputRoute::Unavailable(enigo_init_failed_msg())
        };
    }

    // Linux.
    if env.is_wayland() {
        // XTEST/XWayland input is ignored by GNOME/KDE, so enigo is NOT trustworthy
        // here even though `Enigo::new()` succeeds. Prefer ydotool; otherwise be honest.
        if env.ydotool_available {
            InputRoute::Ydotool
        } else {
            InputRoute::Unavailable(wayland_no_backend_msg())
        }
    } else if enigo_ok {
        // X11 (or no display protocol reported): enigo's XTEST works.
        InputRoute::Enigo
    } else {
        InputRoute::Unavailable(enigo_init_failed_msg())
    }
}

/// Message when running under Wayland with no usable input backend.
fn wayland_no_backend_msg() -> String {
    "desktop input is unavailable on this Wayland session. temm1e-gaze drives input via \
     enigo's XTEST/X11 backend, which GNOME and KDE Wayland compositors ignore — mouse and \
     keyboard actions would silently do nothing. To enable real desktop input, install \
     ydotool and run its daemon with access to /dev/uinput:\n  \
     sudo apt install ydotool\n  \
     sudo ydotoold            # or: sudo systemctl enable --now ydotool\n\
     then restart temm1e. For web pages (Teams, etc.) prefer the `browser` tool, which does \
     not need this."
        .to_string()
}

/// Message when enigo itself failed to initialize.
fn enigo_init_failed_msg() -> String {
    "desktop input simulation could not be initialized. On macOS, grant Accessibility \
     permission (System Settings → Privacy & Security → Accessibility) to the terminal or \
     binary running temm1e. On Linux, ensure a display server is reachable."
        .to_string()
}

/// Whether an executable named `ydotool` exists on `PATH`.
fn ydotool_on_path() -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join("ydotool").is_file())
}

// --- ydotool operations (Linux/Wayland) ---------------------------------------

/// Left-button click code for `ydotool click` (0x40 down | 0x80 up, button 0 = left).
const YD_LEFT_CLICK: &str = "0xC0";
/// Right-button click code (button 1 = right).
const YD_RIGHT_CLICK: &str = "0xC1";
/// Left-button press (down only).
const YD_LEFT_DOWN: &str = "0x40";
/// Left-button release (up only).
const YD_LEFT_UP: &str = "0x80";

/// Run `ydotool` with the given args, mapping spawn/exit failures to `Temm1eError`.
fn run_ydotool(args: &[String]) -> Result<(), Temm1eError> {
    let output = Command::new("ydotool").args(args).output().map_err(|e| {
        Temm1eError::Tool(format!(
            "failed to run ydotool ({e}). Install it and start its daemon: \
             `sudo apt install ydotool && sudo ydotoold` (needs /dev/uinput access)."
        ))
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Temm1eError::Tool(format!(
            "ydotool {} failed: {}. Is ydotoold running with access to /dev/uinput?",
            args.join(" "),
            stderr.trim()
        )));
    }
    Ok(())
}

/// Move the pointer to absolute logical coordinates.
pub fn yd_move(x: i32, y: i32) -> Result<(), Temm1eError> {
    run_ydotool(&[
        "mousemove".to_string(),
        "--absolute".to_string(),
        "-x".to_string(),
        x.to_string(),
        "-y".to_string(),
        y.to_string(),
    ])
}

/// Move to `(x, y)` and left-click.
pub fn yd_click(x: i32, y: i32) -> Result<(), Temm1eError> {
    yd_move(x, y)?;
    run_ydotool(&["click".to_string(), YD_LEFT_CLICK.to_string()])
}

/// Move to `(x, y)` and double left-click.
pub fn yd_double_click(x: i32, y: i32) -> Result<(), Temm1eError> {
    yd_move(x, y)?;
    run_ydotool(&["click".to_string(), YD_LEFT_CLICK.to_string()])?;
    run_ydotool(&["click".to_string(), YD_LEFT_CLICK.to_string()])
}

/// Move to `(x, y)` and right-click.
pub fn yd_right_click(x: i32, y: i32) -> Result<(), Temm1eError> {
    yd_move(x, y)?;
    run_ydotool(&["click".to_string(), YD_RIGHT_CLICK.to_string()])
}

/// Press-drag from `(x1, y1)` to `(x2, y2)` with the left button.
pub fn yd_drag(x1: i32, y1: i32, x2: i32, y2: i32) -> Result<(), Temm1eError> {
    yd_move(x1, y1)?;
    run_ydotool(&["click".to_string(), YD_LEFT_DOWN.to_string()])?;
    yd_move(x2, y2)?;
    run_ydotool(&["click".to_string(), YD_LEFT_UP.to_string()])
}

/// Type a text string.
pub fn yd_type(text: &str) -> Result<(), Temm1eError> {
    run_ydotool(&["type".to_string(), text.to_string()])
}

/// Press a key combination (e.g. "ctrl+c", "enter").
pub fn yd_key(combo: &str) -> Result<(), Temm1eError> {
    let codes = parse_key_combo_evdev(combo)?;
    let mut args = Vec::with_capacity(codes.len() * 2 + 1);
    args.push("key".to_string());
    // Press modifiers/keys in order, then release in reverse (mirrors enigo path).
    for code in &codes {
        args.push(format!("{code}:1"));
    }
    for code in codes.iter().rev() {
        args.push(format!("{code}:0"));
    }
    run_ydotool(&args)
}

/// Parse a "ctrl+shift+a" style combo into evdev key codes (modifiers first).
pub fn parse_key_combo_evdev(combo: &str) -> Result<Vec<u16>, Temm1eError> {
    let mut codes = Vec::new();
    for part in combo.split('+') {
        let name = part.trim().to_lowercase();
        if name.is_empty() {
            continue;
        }
        codes.push(map_key_evdev(&name)?);
    }
    if codes.is_empty() {
        return Err(Temm1eError::Tool(format!("empty key combo: '{combo}'")));
    }
    Ok(codes)
}

/// Map a human-readable key name to a Linux evdev key code.
///
/// Mirrors the names accepted by [`crate::platform::parse_key_combo`].
fn map_key_evdev(name: &str) -> Result<u16, Temm1eError> {
    // Values from linux/input-event-codes.h.
    let code = match name {
        // Modifiers.
        "cmd" | "command" | "meta" | "super" | "win" => 125, // KEY_LEFTMETA
        "ctrl" | "control" => 29,                            // KEY_LEFTCTRL
        "alt" | "option" | "opt" => 56,                      // KEY_LEFTALT
        "shift" => 42,                                       // KEY_LEFTSHIFT

        // Special keys.
        "enter" | "return" => 28,
        "tab" => 15,
        "escape" | "esc" => 1,
        "backspace" | "delete" => 14,
        "del" | "forwarddelete" => 111,
        "space" => 57,
        "up" => 103,
        "down" => 108,
        "left" => 105,
        "right" => 106,
        "home" => 102,
        "end" => 107,
        "pageup" => 104,
        "pagedown" => 109,

        // Function keys (F11/F12 are not contiguous with F1..F10).
        "f1" => 59,
        "f2" => 60,
        "f3" => 61,
        "f4" => 62,
        "f5" => 63,
        "f6" => 64,
        "f7" => 65,
        "f8" => 66,
        "f9" => 67,
        "f10" => 68,
        "f11" => 87,
        "f12" => 88,

        // Single character.
        s if s.chars().count() == 1 => {
            let ch = s.chars().next().unwrap_or(' ');
            char_to_evdev(ch).ok_or_else(|| {
                Temm1eError::Tool(format!(
                    "key '{ch}' is not supported by the ydotool backend (letters a-z and \
                     digits 0-9 only); use the 'type' action for arbitrary text"
                ))
            })?
        }

        other => {
            return Err(Temm1eError::Tool(format!(
                "unknown key name: '{other}'. Supported: cmd, ctrl, alt, shift, enter, tab, \
                 escape, backspace, del, space, up/down/left/right, home, end, \
                 pageup/pagedown, f1-f12, or a single letter/digit."
            )));
        }
    };
    Ok(code)
}

/// Map a single `a-z`/`0-9` character to its evdev key code.
fn char_to_evdev(ch: char) -> Option<u16> {
    let code = match ch.to_ascii_lowercase() {
        'a' => 30,
        'b' => 48,
        'c' => 46,
        'd' => 32,
        'e' => 18,
        'f' => 33,
        'g' => 34,
        'h' => 35,
        'i' => 23,
        'j' => 36,
        'k' => 37,
        'l' => 38,
        'm' => 50,
        'n' => 49,
        'o' => 24,
        'p' => 25,
        'q' => 16,
        'r' => 19,
        's' => 31,
        't' => 20,
        'u' => 22,
        'v' => 47,
        'w' => 17,
        'x' => 45,
        'y' => 21,
        'z' => 44,
        '1' => 2,
        '2' => 3,
        '3' => 4,
        '4' => 5,
        '5' => 6,
        '6' => 7,
        '7' => 8,
        '8' => 9,
        '9' => 10,
        '0' => 11,
        _ => return None,
    };
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(is_linux: bool, session: Option<&str>, wl: Option<&str>, ydotool: bool) -> InputEnv {
        InputEnv {
            is_linux,
            session_type: session.map(String::from),
            wayland_display: wl.map(String::from),
            ydotool_available: ydotool,
        }
    }

    #[test]
    fn macos_uses_enigo_when_ok() {
        let e = env(false, None, None, false);
        assert!(matches!(resolve_route(true, &e), InputRoute::Enigo));
    }

    #[test]
    fn macos_unavailable_when_enigo_fails() {
        let e = env(false, None, None, false);
        assert!(matches!(
            resolve_route(false, &e),
            InputRoute::Unavailable(_)
        ));
    }

    #[test]
    fn linux_x11_uses_enigo() {
        let e = env(true, Some("x11"), None, false);
        assert!(matches!(resolve_route(true, &e), InputRoute::Enigo));
    }

    #[test]
    fn linux_wayland_with_ydotool_uses_ydotool() {
        // Even though enigo "initialized" (true), Wayland must NOT trust XTEST.
        let e = env(true, Some("wayland"), Some("wayland-0"), true);
        assert!(matches!(resolve_route(true, &e), InputRoute::Ydotool));
    }

    #[test]
    fn linux_wayland_without_ydotool_is_unavailable_not_fake_success() {
        let e = env(true, Some("wayland"), Some("wayland-0"), false);
        match resolve_route(true, &e) {
            InputRoute::Unavailable(msg) => {
                assert!(msg.contains("ydotool"), "message should guide the fix");
                assert!(msg.contains("Wayland") || msg.contains("wayland"));
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    #[test]
    fn wayland_detected_via_display_even_without_session_type() {
        let e = env(true, None, Some("wayland-0"), false);
        assert!(e.is_wayland());
    }

    #[test]
    fn x11_session_is_not_wayland() {
        let e = env(true, Some("x11"), None, false);
        assert!(!e.is_wayland());
    }

    #[test]
    fn parse_single_key() {
        assert_eq!(parse_key_combo_evdev("enter").unwrap(), vec![28]);
    }

    #[test]
    fn parse_modifier_combo_order_preserved() {
        // ctrl=29, c=46
        assert_eq!(parse_key_combo_evdev("ctrl+c").unwrap(), vec![29, 46]);
    }

    #[test]
    fn parse_triple_combo() {
        // cmd=125, shift=42, a=30
        assert_eq!(
            parse_key_combo_evdev("cmd+shift+a").unwrap(),
            vec![125, 42, 30]
        );
    }

    #[test]
    fn parse_case_insensitive_and_spaces() {
        assert_eq!(parse_key_combo_evdev("CTRL + C").unwrap(), vec![29, 46]);
    }

    #[test]
    fn parse_unknown_key_fails() {
        assert!(parse_key_combo_evdev("nonexistent").is_err());
    }

    #[test]
    fn parse_empty_fails() {
        assert!(parse_key_combo_evdev("").is_err());
    }

    #[test]
    fn function_keys_map_correctly() {
        assert_eq!(parse_key_combo_evdev("f1").unwrap(), vec![59]);
        assert_eq!(parse_key_combo_evdev("f10").unwrap(), vec![68]);
        assert_eq!(parse_key_combo_evdev("f11").unwrap(), vec![87]);
        assert_eq!(parse_key_combo_evdev("f12").unwrap(), vec![88]);
    }

    #[test]
    fn digits_map_correctly() {
        assert_eq!(parse_key_combo_evdev("0").unwrap(), vec![11]);
        assert_eq!(parse_key_combo_evdev("1").unwrap(), vec![2]);
    }

    #[test]
    fn status_note_reflects_route() {
        assert!(InputRoute::Enigo.status_note().contains("available"));
        assert!(InputRoute::Ydotool.status_note().contains("ydotool"));
        assert!(InputRoute::Unavailable("x".into())
            .status_note()
            .contains("UNAVAILABLE"));
    }

    #[test]
    fn backend_pref_gates_libei_prompt() {
        // Auto only attempts libei on Wayland (interactive, exact).
        assert!(InputBackendPref::Auto.attempt_libei(true));
        assert!(!InputBackendPref::Auto.attempt_libei(false));
        // Ydotool/Enigo NEVER attempt libei — unattended hosts must not block on
        // the portal prompt, even on Wayland.
        assert!(!InputBackendPref::Ydotool.attempt_libei(true));
        assert!(!InputBackendPref::Enigo.attempt_libei(true));
        // Explicit libei forces it.
        assert!(InputBackendPref::Libei.attempt_libei(false));
    }
}
