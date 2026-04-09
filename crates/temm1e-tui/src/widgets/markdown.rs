//! Markdown text to ratatui Span/Line renderer.
//!
//! Renders markdown into styled ratatui Lines with:
//! - Bordered code blocks with language labels and syntax highlighting
//! - Styled headings with underlines
//! - Bullet/numbered lists with proper indentation
//! - Blockquotes with left border
//! - Bold, italic, inline code, links

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

/// A rendered line of styled text.
#[derive(Debug, Clone)]
pub struct RenderedLine {
    pub spans: Vec<Span<'static>>,
    pub indent: u16,
}

impl RenderedLine {
    pub fn to_line(&self) -> Line<'static> {
        let mut spans = Vec::new();
        if self.indent > 0 {
            spans.push(Span::raw(" ".repeat(self.indent as usize)));
        }
        spans.extend(self.spans.clone());
        Line::from(spans)
    }
}

/// Extract code blocks from raw markdown text.
///
/// Returns `(lang, text, line_count)` tuples for each fenced code block
/// found in the input. Used by the yank picker to capture blocks as
/// they flow through the message list, without changing the rendering
/// signature.
pub fn extract_code_blocks(text: &str) -> Vec<(String, String, usize)> {
    let mut blocks = Vec::new();
    let mut in_block = false;
    let mut lang = String::new();
    let mut buf: Vec<&str> = Vec::new();

    for raw_line in text.lines() {
        if raw_line.trim_start().starts_with("```") {
            if in_block {
                let body = buf.join("\n");
                let count = buf.len();
                blocks.push((lang.clone(), body, count));
                buf.clear();
                lang.clear();
                in_block = false;
            } else {
                in_block = true;
                lang = raw_line
                    .trim_start()
                    .trim_start_matches('`')
                    .trim()
                    .to_string();
            }
            continue;
        }
        if in_block {
            buf.push(raw_line);
        }
    }

    // Unclosed final block — still capture it
    if in_block && !buf.is_empty() {
        let body = buf.join("\n");
        let count = buf.len();
        blocks.push((lang, body, count));
    }

    blocks
}

/// Render markdown text into styled lines, wrapping prose to `width`.
pub fn render_markdown(
    text: &str,
    base_style: Style,
    heading_style: Style,
    code_style: Style,
    link_style: Style,
    quote_style: Style,
) -> Vec<RenderedLine> {
    render_markdown_with_width(
        text,
        base_style,
        heading_style,
        code_style,
        link_style,
        quote_style,
        120,
    )
}

/// Render markdown with explicit terminal width for wrapping.
pub fn render_markdown_with_width(
    text: &str,
    base_style: Style,
    heading_style: Style,
    code_style: Style,
    link_style: Style,
    quote_style: Style,
    width: usize,
) -> Vec<RenderedLine> {
    let mut lines = Vec::new();
    let mut in_code_block = false;
    let mut code_lang = String::new();
    let mut code_lines: Vec<String> = Vec::new();

    for raw_line in text.lines() {
        // Code block fence
        if raw_line.trim_start().starts_with("```") {
            if in_code_block {
                // End code block — render with visual treatment
                render_code_block_with_width(
                    &code_lang,
                    &code_lines,
                    code_style,
                    &mut lines,
                    width,
                );
                code_lines.clear();
                in_code_block = false;
                code_lang.clear();
            } else {
                in_code_block = true;
                code_lang = raw_line
                    .trim_start()
                    .trim_start_matches('`')
                    .trim()
                    .to_string();
            }
            continue;
        }

        if in_code_block {
            code_lines.push(raw_line.to_string());
            continue;
        }

        // Heading 1
        if let Some(rest) = raw_line.strip_prefix("# ") {
            lines.push(RenderedLine {
                spans: vec![],
                indent: 0,
            });
            lines.push(RenderedLine {
                spans: vec![Span::styled(format!("\u{2588} {}", rest), heading_style)],
                indent: 0,
            });
            lines.push(RenderedLine {
                spans: vec![],
                indent: 0,
            });
            continue;
        }

        // Heading 2
        if let Some(rest) = raw_line.strip_prefix("## ") {
            lines.push(RenderedLine {
                spans: vec![Span::styled(format!("\u{258c} {}", rest), heading_style)],
                indent: 0,
            });
            continue;
        }

        // Heading 3
        if let Some(rest) = raw_line.strip_prefix("### ") {
            lines.push(RenderedLine {
                spans: vec![Span::styled(
                    format!("  {}", rest),
                    heading_style.add_modifier(Modifier::ITALIC),
                )],
                indent: 0,
            });
            continue;
        }

        // Horizontal rule
        let trimmed = raw_line.trim();
        if trimmed == "---" || trimmed == "***" || trimmed == "___" {
            lines.push(RenderedLine {
                spans: vec![Span::styled(
                    " \u{2500}".repeat(20),
                    base_style.add_modifier(Modifier::DIM),
                )],
                indent: 0,
            });
            continue;
        }

        // Blockquote
        if let Some(rest) = raw_line.strip_prefix("> ") {
            let inline = render_inline(rest, base_style, code_style, link_style);
            let mut spans = vec![Span::styled(
                " \u{2502} ".to_string(),
                quote_style.add_modifier(Modifier::DIM),
            )];
            spans.extend(inline);
            lines.push(RenderedLine { spans, indent: 0 });
            continue;
        }

        // Unordered list (with nested support)
        if let Some(rest) = raw_line
            .strip_prefix("  - ")
            .or_else(|| raw_line.strip_prefix("  * "))
        {
            let inline = render_inline(rest, base_style, code_style, link_style);
            let mut spans = vec![Span::styled("    \u{25e6} ".to_string(), base_style)];
            spans.extend(inline);
            lines.push(RenderedLine { spans, indent: 0 });
            continue;
        }
        if let Some(rest) = raw_line
            .strip_prefix("- ")
            .or_else(|| raw_line.strip_prefix("* "))
        {
            let inline = render_inline(rest, base_style, code_style, link_style);
            let mut spans = vec![Span::styled("  \u{2022} ".to_string(), base_style)];
            spans.extend(inline);
            lines.push(RenderedLine { spans, indent: 0 });
            continue;
        }

        // Numbered list
        if let Some(dot_pos) = raw_line.find(". ") {
            if dot_pos <= 3 && raw_line[..dot_pos].chars().all(|c| c.is_ascii_digit()) {
                let number = &raw_line[..dot_pos];
                let rest = &raw_line[dot_pos + 2..];
                let inline = render_inline(rest, base_style, code_style, link_style);
                let mut spans = vec![Span::styled(format!("  {}. ", number), base_style)];
                spans.extend(inline);
                lines.push(RenderedLine { spans, indent: 0 });
                continue;
            }
        }

        // Empty line
        if trimmed.is_empty() {
            lines.push(RenderedLine {
                spans: vec![],
                indent: 0,
            });
            continue;
        }

        // Regular paragraph line — wrap to terminal width
        if raw_line.len() > width && width > 10 {
            let wrapped = textwrap::wrap(raw_line, width.saturating_sub(2));
            for wrap_line in wrapped {
                let inline = render_inline(&wrap_line, base_style, code_style, link_style);
                lines.push(RenderedLine {
                    spans: inline,
                    indent: 0,
                });
            }
        } else {
            let inline = render_inline(raw_line, base_style, code_style, link_style);
            lines.push(RenderedLine {
                spans: inline,
                indent: 0,
            });
        }
    }

    // Handle unclosed code block
    if in_code_block {
        render_code_block_with_width(&code_lang, &code_lines, code_style, &mut lines, width);
    }

    lines
}

fn render_code_block_with_width(
    lang: &str,
    code_lines: &[String],
    code_style: Style,
    output: &mut Vec<RenderedLine>,
    _width: usize,
) {
    // Use syntect for highlighting if we can find the syntax
    let ps = SyntaxSet::load_defaults_newlines();
    let ts = ThemeSet::load_defaults();
    let theme = &ts.themes["Solarized (dark)"];

    let syntax = if !lang.is_empty() {
        ps.find_syntax_by_token(lang)
    } else {
        None
    }
    .unwrap_or_else(|| ps.find_syntax_plain_text());

    // Top border with language label
    let label = if !lang.is_empty() {
        format!(" \u{256d}\u{2500} {} \u{2500}", lang)
    } else {
        " \u{256d}\u{2500}\u{2500}\u{2500}".to_string()
    };
    output.push(RenderedLine {
        spans: vec![Span::styled(label, code_style.add_modifier(Modifier::DIM))],
        indent: 0,
    });

    // Highlighted code lines
    let mut highlighter = syntect::easy::HighlightLines::new(syntax, theme);
    for code_line in code_lines {
        let highlighted = highlighter
            .highlight_line(code_line, &ps)
            .unwrap_or_default();

        let mut spans = vec![Span::styled(
            " \u{2502} ".to_string(),
            code_style.add_modifier(Modifier::DIM),
        )];

        for (style, text) in highlighted {
            let (r, g, b) = (style.foreground.r, style.foreground.g, style.foreground.b);
            // Boost very dark colors so code is readable on dark terminals
            let brightness = (r as u16 + g as u16 + b as u16) / 3;
            let fg = if brightness < 60 {
                // Too dark — use a readable light gray instead
                Color::Rgb(200, 200, 210)
            } else {
                Color::Rgb(r, g, b)
            };
            let mut ratatui_style = Style::default().fg(fg);
            if style
                .font_style
                .contains(syntect::highlighting::FontStyle::BOLD)
            {
                ratatui_style = ratatui_style.add_modifier(Modifier::BOLD);
            }
            if style
                .font_style
                .contains(syntect::highlighting::FontStyle::ITALIC)
            {
                ratatui_style = ratatui_style.add_modifier(Modifier::ITALIC);
            }
            spans.push(Span::styled(text.to_string(), ratatui_style));
        }

        output.push(RenderedLine { spans, indent: 0 });
    }

    // Bottom border
    output.push(RenderedLine {
        spans: vec![Span::styled(
            " \u{2570}\u{2500}\u{2500}\u{2500}".to_string(),
            code_style.add_modifier(Modifier::DIM),
        )],
        indent: 0,
    });
}

/// Render inline markdown (bold, italic, code, links) into spans.
#[allow(clippy::while_let_on_iterator)]
fn render_inline(text: &str, base: Style, code: Style, link: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut chars = text.char_indices().peekable();
    let mut current = String::new();

    while let Some((_i, c)) = chars.next() {
        match c {
            '`' => {
                // Inline code — render with distinct background
                if !current.is_empty() {
                    spans.push(Span::styled(current.clone(), base));
                    current.clear();
                }
                let mut code_text = String::new();
                while let Some((_, ch)) = chars.next() {
                    if ch == '`' {
                        break;
                    }
                    code_text.push(ch);
                }
                spans.push(Span::styled(
                    format!(" {} ", code_text),
                    code.add_modifier(Modifier::BOLD),
                ));
            }
            '*' | '_' => {
                let next_same = chars.peek().map(|(_, nc)| *nc == c).unwrap_or(false);
                if next_same {
                    // Bold **text**
                    chars.next();
                    if !current.is_empty() {
                        spans.push(Span::styled(current.clone(), base));
                        current.clear();
                    }
                    let mut bold_text = String::new();
                    while let Some((_, ch)) = chars.next() {
                        if ch == c && chars.peek().map(|(_, nc)| *nc == c).unwrap_or(false) {
                            chars.next();
                            break;
                        }
                        bold_text.push(ch);
                    }
                    spans.push(Span::styled(bold_text, base.add_modifier(Modifier::BOLD)));
                } else {
                    // Italic *text*
                    if !current.is_empty() {
                        spans.push(Span::styled(current.clone(), base));
                        current.clear();
                    }
                    let mut italic_text = String::new();
                    while let Some((_, ch)) = chars.next() {
                        if ch == c {
                            break;
                        }
                        italic_text.push(ch);
                    }
                    spans.push(Span::styled(
                        italic_text,
                        base.add_modifier(Modifier::ITALIC),
                    ));
                }
            }
            '[' => {
                // Markdown link [text](url)
                if !current.is_empty() {
                    spans.push(Span::styled(current.clone(), base));
                    current.clear();
                }
                let mut link_text = String::new();
                let mut found_close = false;
                while let Some((_, ch)) = chars.next() {
                    if ch == ']' {
                        found_close = true;
                        break;
                    }
                    link_text.push(ch);
                }
                if found_close && chars.peek().map(|(_, nc)| *nc == '(').unwrap_or(false) {
                    chars.next();
                    let mut url = String::new();
                    while let Some((_, ch)) = chars.next() {
                        if ch == ')' {
                            break;
                        }
                        url.push(ch);
                    }
                    spans.push(Span::styled(
                        format!("{} \u{2197}", link_text),
                        link.add_modifier(Modifier::UNDERLINED),
                    ));
                } else {
                    spans.push(Span::styled(format!("[{}]", link_text), base));
                }
            }
            _ => {
                current.push(c);
            }
        }
    }

    if !current.is_empty() {
        // Detect file paths and URLs in accumulated text and style them
        spans.extend(highlight_paths(&current, base, link));
    }

    if spans.is_empty() {
        spans.push(Span::raw(""));
    }

    spans
}

/// Detect file paths and URLs in text and render them with link styling.
fn highlight_paths(text: &str, base: Style, link: Style) -> Vec<Span<'static>> {
    let mut result = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        // Find the next path-like token
        if let Some(pos) = remaining.find(['/', '~']) {
            // Check if this looks like a file path
            let before = &remaining[..pos];
            let from_path = &remaining[pos..];

            // Find end of path (space, comma, paren, bracket, or end of string)
            let path_end = from_path
                .find(|c: char| {
                    c == ' '
                        || c == ','
                        || c == ')'
                        || c == ']'
                        || c == '>'
                        || c == '\''
                        || c == '"'
                })
                .unwrap_or(from_path.len());
            let candidate = &from_path[..path_end];

            // Must look like a real path (has at least one slash after start, or starts with ~/)
            let is_path = (candidate.starts_with('/') || candidate.starts_with("~/"))
                && candidate.len() > 2
                && candidate.contains('/')
                && !candidate.starts_with("//"); // not a URL protocol

            if is_path {
                if !before.is_empty() {
                    result.push(Span::styled(before.to_string(), base));
                }
                result.push(Span::styled(
                    candidate.to_string(),
                    link.add_modifier(Modifier::UNDERLINED),
                ));
                remaining = &from_path[path_end..];
                continue;
            }
        }

        // Also detect http/https URLs
        if let Some(url_start) = remaining
            .find("http://")
            .or_else(|| remaining.find("https://"))
        {
            let before = &remaining[..url_start];
            let from_url = &remaining[url_start..];
            let url_end = from_url
                .find(|c: char| {
                    c == ' ' || c == ')' || c == ']' || c == '>' || c == '\'' || c == '"'
                })
                .unwrap_or(from_url.len());
            let url = &from_url[..url_end];

            if !before.is_empty() {
                result.push(Span::styled(before.to_string(), base));
            }
            result.push(Span::styled(
                format!("{} \u{2197}", url),
                link.add_modifier(Modifier::UNDERLINED),
            ));
            remaining = &from_url[url_end..];
            continue;
        }

        // Check for bare filenames with extensions (e.g. "file.txt", "script.py")
        if let Some(dot_pos) = remaining.find('.') {
            // Find the word containing the dot
            let word_start = remaining[..dot_pos]
                .rfind([' ', '(', '[', '"', '\''])
                .map(|p| p + 1)
                .unwrap_or(0);
            let after_dot = &remaining[dot_pos + 1..];
            let ext_end = after_dot
                .find(|c: char| !c.is_alphanumeric() && c != '_')
                .unwrap_or(after_dot.len());
            let ext = &after_dot[..ext_end];

            // Common file extensions
            let is_file = matches!(
                ext,
                "txt"
                    | "py"
                    | "rs"
                    | "js"
                    | "ts"
                    | "json"
                    | "toml"
                    | "yaml"
                    | "yml"
                    | "md"
                    | "html"
                    | "css"
                    | "sh"
                    | "bash"
                    | "zsh"
                    | "csv"
                    | "log"
                    | "xml"
                    | "sql"
                    | "rb"
                    | "go"
                    | "java"
                    | "c"
                    | "cpp"
                    | "h"
                    | "pdf"
                    | "png"
                    | "jpg"
                    | "jpeg"
                    | "gif"
                    | "svg"
                    | "zip"
                    | "tar"
            ) && dot_pos > word_start;

            if is_file {
                let filename = &remaining[word_start..dot_pos + 1 + ext_end];
                let before = &remaining[..word_start];
                if !before.is_empty() {
                    result.push(Span::styled(before.to_string(), base));
                }
                result.push(Span::styled(
                    filename.to_string(),
                    link.add_modifier(Modifier::UNDERLINED),
                ));
                remaining = &remaining[dot_pos + 1 + ext_end..];
                continue;
            }
        }

        // No more paths/URLs/files — push the rest
        result.push(Span::styled(remaining.to_string(), base));
        break;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s() -> Style {
        Style::default()
    }

    #[test]
    fn plain_text() {
        let lines = render_markdown("hello world", s(), s(), s(), s(), s());
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn heading_h1() {
        let lines = render_markdown("# Title", s(), s(), s(), s(), s());
        // H1: blank line + heading + blank line
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn code_block_with_border() {
        let md = "```rust\nfn main() {}\n```";
        let lines = render_markdown(md, s(), s(), s(), s(), s());
        // top border + 1 code line + bottom border = 3
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn list_items() {
        let md = "- item 1\n- item 2\n- item 3";
        let lines = render_markdown(md, s(), s(), s(), s(), s());
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn inline_code() {
        let lines = render_markdown("use `hello` world", s(), s(), s(), s(), s());
        assert_eq!(lines.len(), 1);
        assert!(lines[0].spans.len() >= 3); // "use ", " hello ", " world"
    }

    #[test]
    fn blockquote() {
        let lines = render_markdown("> quoted text", s(), s(), s(), s(), s());
        assert_eq!(lines.len(), 1);
    }
}
