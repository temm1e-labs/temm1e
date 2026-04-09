//! Code block yank picker — numbered overlay for copying code blocks.
//!
//! Pressing `Ctrl+Y` opens this picker showing the most recent 9 code
//! blocks from the message history. User presses 1-9 to copy that
//! block to the clipboard.
//!
//! # Clipboard backends
//!
//! - **Non-musl targets** (macOS, Windows, Linux/glibc): primary
//!   is `arboard` with native OS clipboard APIs. Falls back to
//!   OSC 52 if arboard fails (e.g., headless Linux without
//!   `$DISPLAY`).
//! - **musl targets** (static Linux binaries in containers, SSH,
//!   minimal base images): OSC 52 only. `arboard`'s X11 + Wayland
//!   backends pull in ~1 MB of transitive deps (x11rb,
//!   wl-clipboard-rs) which pushes the static binary over the
//!   30 MB CI size gate. OSC 52 works in every modern terminal
//!   that musl users actually target.

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap};

use crate::app::AppState;

pub fn render_copy_picker(state: &AppState, area: Rect, buf: &mut Buffer) {
    if state.code_blocks.is_empty() {
        // No blocks — degenerate case, should be handled by caller
        return;
    }

    let popup_width = 82.min(area.width.saturating_sub(4));
    let popup_height = 18.min(area.height.saturating_sub(4));
    let popup = centered_rect(popup_width, popup_height, area);

    Clear.render(popup, buf);

    let block = Block::default()
        .title(" Yank Code Block ")
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_style(state.theme.accent);
    let inner = block.inner(popup);
    block.render(popup, buf);

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(""));

    // Most recent first: iter().rev().take(9) gives us #1..#9 newest-to-oldest
    for (i, cb) in state.code_blocks.iter().rev().take(9).enumerate() {
        let num = i + 1;
        let lang_display: String = if cb.lang.is_empty() {
            "text".to_string()
        } else {
            cb.lang.clone()
        };
        let preview: String = cb
            .text
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .chars()
            .take(48)
            .collect();

        lines.push(Line::from(vec![
            Span::styled(format!("  {num}. "), state.theme.accent),
            Span::styled(format!("[{:<8}] ", lang_display), state.theme.secondary),
            Span::styled(preview, state.theme.text),
            Span::styled(
                format!("  ({} lines)", cb.line_count),
                state.theme.secondary,
            ),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Press 1-9 to copy · Esc to cancel",
        state.theme.secondary,
    )));

    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    para.render(inner, buf);
}

/// Copy text to the system clipboard. On non-musl targets uses
/// `arboard` first (native OS clipboard) and falls back to OSC 52.
/// On musl targets uses OSC 52 directly — `arboard`'s Linux
/// backends are excluded to keep the static binary under the
/// 30 MB size gate.
#[cfg(not(target_env = "musl"))]
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    match arboard::Clipboard::new() {
        Ok(mut cb) => cb.set_text(text.to_string()).map_err(|e| e.to_string()),
        Err(_) => write_osc52(text).map_err(|e| e.to_string()),
    }
}

/// Copy text to the terminal via the OSC 52 escape sequence.
/// Works in every modern terminal emulator (kitty, alacritty,
/// iTerm2, WezTerm, Windows Terminal, tmux with allow-passthrough).
/// This is the only clipboard path on musl builds.
#[cfg(target_env = "musl")]
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    write_osc52(text).map_err(|e| e.to_string())
}

fn write_osc52(text: &str) -> std::io::Result<()> {
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine as _;
    use std::io::Write;
    let encoded = B64.encode(text);
    let mut stdout = std::io::stdout();
    write!(stdout, "\x1b]52;c;{}\x07", encoded)?;
    stdout.flush()
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.left() + (area.width.saturating_sub(width)) / 2;
    let y = area.top() + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}
