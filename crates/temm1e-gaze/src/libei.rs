//! Native Wayland input via the XDG RemoteDesktop portal (ashpd).
//!
//! This is the correct backend for GNOME/Wayland: `notify_pointer_motion_absolute`
//! places the pointer at an EXACT logical coordinate with no acceleration — unlike
//! the ydotool fallback, whose relative-emulated "absolute" motion is distorted by
//! libinput pointer acceleration (see issue #70 / docs/GAZE_NATIVE_COMPUTER_USE.md).
//!
//! The portal is async (ashpd/zbus) and the session must stay alive for its whole
//! lifetime, but `DesktopController`'s input API is synchronous. So we run the
//! session on a dedicated thread with a current-thread tokio runtime and drive it
//! through a command channel: the session is created ONCE (one permission prompt
//! per process), then every click/type reuses it.

use ashpd::desktop::{
    remote_desktop::{DeviceType, KeyState, RemoteDesktop},
    screencast::{CursorMode, Screencast, SourceType},
    PersistMode,
};
use std::sync::mpsc as smpsc;
use std::time::Duration;
use temm1e_core::types::error::Temm1eError;
use tokio::sync::mpsc as tmpsc;

/// Linux evdev button codes (linux/input-event-codes.h).
const BTN_LEFT: i32 = 0x110;
const BTN_RIGHT: i32 = 0x111;

/// Reply channel for a single command (Ok, or a stringified portal error).
type Reply = smpsc::Sender<Result<(), String>>;

enum Cmd {
    /// Move the pointer to an absolute GLOBAL logical coordinate.
    Move(f64, f64, Reply),
    /// Press or release a pointer button at the current position.
    Button(i32, bool, Reply),
    /// Move to (x, y) then press+release `button` — a full click.
    MoveClick(f64, f64, i32, Reply),
    /// Press or release an evdev keycode.
    Keycode(i32, bool, Reply),
    /// Press or release an X keysym (used for typing text).
    Keysym(i32, bool, Reply),
    Shutdown,
}

/// Handle to a live RemoteDesktop portal session running on its own thread.
pub struct LibeiController {
    tx: tmpsc::UnboundedSender<Cmd>,
    _thread: std::thread::JoinHandle<()>,
}

impl LibeiController {
    /// Establish a portal session. Shows a one-time permission dialog (remote
    /// control + screen share, the latter only to define the coordinate space).
    /// Returns `Err` if the portal is unavailable or the user denies access.
    pub fn new() -> Result<Self, Temm1eError> {
        let (tx, rx) = tmpsc::unbounded_channel::<Cmd>();
        let (ready_tx, ready_rx) = smpsc::channel::<Result<(), String>>();

        let thread = std::thread::Builder::new()
            .name("temm1e-libei".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = ready_tx.send(Err(format!("tokio runtime: {e}")));
                        return;
                    }
                };
                rt.block_on(session_loop(rx, ready_tx));
            })
            .map_err(|e| Temm1eError::Tool(format!("failed to spawn libei thread: {e}")))?;

        // Block until the session is established (this is where the prompt is answered).
        match ready_rx.recv() {
            Ok(Ok(())) => {
                tracing::info!("libei RemoteDesktop portal session established");
                Ok(Self {
                    tx,
                    _thread: thread,
                })
            }
            Ok(Err(e)) => Err(Temm1eError::Tool(format!("libei portal setup failed: {e}"))),
            Err(_) => Err(Temm1eError::Tool(
                "libei thread exited before establishing a session".into(),
            )),
        }
    }

    /// Send one command and block for its result.
    fn call(&self, make: impl FnOnce(Reply) -> Cmd) -> Result<(), Temm1eError> {
        let (rtx, rrx) = smpsc::channel();
        self.tx
            .send(make(rtx))
            .map_err(|_| Temm1eError::Tool("libei session thread is gone".into()))?;
        match rrx.recv() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(Temm1eError::Tool(format!("libei input failed: {e}"))),
            Err(_) => Err(Temm1eError::Tool("libei command got no reply".into())),
        }
    }

    /// Move the pointer to absolute logical (x, y).
    pub fn move_to(&self, x: i32, y: i32) -> Result<(), Temm1eError> {
        self.call(|r| Cmd::Move(x as f64, y as f64, r))
    }

    /// Left-click at (x, y).
    pub fn click(&self, x: i32, y: i32) -> Result<(), Temm1eError> {
        self.call(|r| Cmd::MoveClick(x as f64, y as f64, BTN_LEFT, r))
    }

    /// Right-click at (x, y).
    pub fn right_click(&self, x: i32, y: i32) -> Result<(), Temm1eError> {
        self.call(|r| Cmd::MoveClick(x as f64, y as f64, BTN_RIGHT, r))
    }

    /// Double left-click at (x, y).
    pub fn double_click(&self, x: i32, y: i32) -> Result<(), Temm1eError> {
        self.click(x, y)?;
        self.click(x, y)
    }

    /// Press-drag with the left button from (x1, y1) to (x2, y2).
    pub fn drag(&self, x1: i32, y1: i32, x2: i32, y2: i32) -> Result<(), Temm1eError> {
        self.move_to(x1, y1)?;
        self.call(|r| Cmd::Button(BTN_LEFT, true, r))?;
        self.move_to(x2, y2)?;
        self.call(|r| Cmd::Button(BTN_LEFT, false, r))
    }

    /// Type a text string (each char as a keysym press+release).
    pub fn type_text(&self, text: &str) -> Result<(), Temm1eError> {
        for ch in text.chars() {
            let sym = char_to_keysym(ch);
            self.call(|r| Cmd::Keysym(sym, true, r))?;
            self.call(|r| Cmd::Keysym(sym, false, r))?;
        }
        Ok(())
    }

    /// Press a key combination (e.g. "ctrl+c", "enter"); reuses the evdev keymap.
    pub fn key_combo(&self, combo: &str) -> Result<(), Temm1eError> {
        let codes = crate::input::parse_key_combo_evdev(combo)?;
        for &c in &codes {
            self.call(|r| Cmd::Keycode(c as i32, true, r))?;
        }
        for &c in codes.iter().rev() {
            self.call(|r| Cmd::Keycode(c as i32, false, r))?;
        }
        Ok(())
    }
}

impl Drop for LibeiController {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
    }
}

/// Map a character to an X11 keysym. ASCII 0x20–0x7e keysyms equal their codepoint;
/// other Unicode uses the `0x01000000 | codepoint` keysym range.
fn char_to_keysym(ch: char) -> i32 {
    let cp = ch as u32;
    if (0x20..0x7f).contains(&cp) {
        cp as i32
    } else {
        (0x0100_0000 | cp) as i32
    }
}

/// Establish the session, signal readiness, then serve input commands until the
/// channel closes. Keeps `remote_desktop` and `session` alive as locals for the
/// whole loop (the session must not be dropped while injecting).
async fn session_loop(
    mut rx: tmpsc::UnboundedReceiver<Cmd>,
    ready: smpsc::Sender<Result<(), String>>,
) {
    let remote_desktop = match RemoteDesktop::new().await {
        Ok(v) => v,
        Err(e) => {
            let _ = ready.send(Err(e.to_string()));
            return;
        }
    };
    let screencast = match Screencast::new().await {
        Ok(v) => v,
        Err(e) => {
            let _ = ready.send(Err(e.to_string()));
            return;
        }
    };
    let session = match remote_desktop.create_session().await {
        Ok(v) => v,
        Err(e) => {
            let _ = ready.send(Err(e.to_string()));
            return;
        }
    };

    // Configure + start. A monitor screencast stream defines the coordinate space.
    let configured: Result<(u32, (i32, i32)), String> = async {
        remote_desktop
            .select_devices(
                &session,
                DeviceType::Keyboard | DeviceType::Pointer,
                None,
                PersistMode::DoNot,
            )
            .await
            .map_err(|e| e.to_string())?;
        screencast
            .select_sources(
                &session,
                CursorMode::Metadata,
                SourceType::Monitor.into(),
                false,
                None,
                PersistMode::DoNot,
            )
            .await
            .map_err(|e| e.to_string())?;
        let response = remote_desktop
            .start(&session, None)
            .await
            .map_err(|e| e.to_string())?
            .response()
            .map_err(|e| e.to_string())?;
        let stream = response
            .streams()
            .and_then(|s| s.first())
            .ok_or_else(|| "no screencast stream returned".to_string())?;
        Ok((
            stream.pipe_wire_node_id(),
            stream.position().unwrap_or((0, 0)),
        ))
    }
    .await;

    let (node, (ox, oy)) = match configured {
        Ok(v) => v,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = ready.send(Ok(()));

    while let Some(cmd) = rx.recv().await {
        match cmd {
            Cmd::Shutdown => break,
            Cmd::Move(x, y, reply) => {
                let res = remote_desktop
                    .notify_pointer_motion_absolute(&session, node, x - ox as f64, y - oy as f64)
                    .await
                    .map_err(|e| e.to_string());
                let _ = reply.send(res);
            }
            Cmd::Button(button, press, reply) => {
                let state = key_state(press);
                let res = remote_desktop
                    .notify_pointer_button(&session, button, state)
                    .await
                    .map_err(|e| e.to_string());
                let _ = reply.send(res);
            }
            Cmd::MoveClick(x, y, button, reply) => {
                let res = async {
                    remote_desktop
                        .notify_pointer_motion_absolute(
                            &session,
                            node,
                            x - ox as f64,
                            y - oy as f64,
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    remote_desktop
                        .notify_pointer_button(&session, button, KeyState::Pressed)
                        .await
                        .map_err(|e| e.to_string())?;
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    remote_desktop
                        .notify_pointer_button(&session, button, KeyState::Released)
                        .await
                        .map_err(|e| e.to_string())?;
                    Ok::<(), String>(())
                }
                .await;
                let _ = reply.send(res);
            }
            Cmd::Keycode(code, press, reply) => {
                let res = remote_desktop
                    .notify_keyboard_keycode(&session, code, key_state(press))
                    .await
                    .map_err(|e| e.to_string());
                let _ = reply.send(res);
            }
            Cmd::Keysym(sym, press, reply) => {
                let res = remote_desktop
                    .notify_keyboard_keysym(&session, sym, key_state(press))
                    .await
                    .map_err(|e| e.to_string());
                let _ = reply.send(res);
            }
        }
    }
}

fn key_state(press: bool) -> KeyState {
    if press {
        KeyState::Pressed
    } else {
        KeyState::Released
    }
}
