//! Onboarding wizard view — multi-step provider setup.

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};

use crate::onboarding::steps::OnboardingStep;
use crate::theme::Theme;
use crate::widgets::ascii_art::tem_mascot;
use crate::widgets::select_list::SelectListWidget;

/// Render the onboarding wizard.
pub fn render_onboarding(step: &OnboardingStep, theme: &Theme, area: Rect, buf: &mut Buffer) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.accent)
        .title(" TEMM1E Setup ")
        .title_alignment(Alignment::Center);
    let inner = block.inner(area);
    block.render(area, buf);

    match step {
        OnboardingStep::Welcome => render_welcome(theme, inner, buf),
        OnboardingStep::SelectMode(state) => {
            let widget = SelectListWidget::new(state)
                .normal_style(theme.text)
                .selected_style(theme.accent.add_modifier(Modifier::REVERSED))
                .description_style(theme.secondary);
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(2),
                    Constraint::Min(1),
                    Constraint::Length(2),
                ])
                .split(inner);
            let title = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(" How should Tem behave?", theme.heading)),
            ]);
            title.render(chunks[0], buf);
            widget.render(chunks[1], buf);
            let hint = Paragraph::new(Line::from(Span::styled(
                "  \u{2191}\u{2193} select  Enter confirm  Esc back",
                theme.secondary,
            )));
            hint.render(chunks[2], buf);
        }
        OnboardingStep::SelectProvider(state) => {
            let widget = SelectListWidget::new(state)
                .normal_style(theme.text)
                .selected_style(theme.accent.add_modifier(Modifier::REVERSED))
                .description_style(theme.secondary);
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(2),
                    Constraint::Min(1),
                    Constraint::Length(2),
                ])
                .split(inner);
            let title = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(" Select your AI provider:", theme.heading)),
            ]);
            title.render(chunks[0], buf);
            widget.render(chunks[1], buf);
            let hint = Paragraph::new(Line::from(Span::styled(
                "  \u{2191}\u{2193} select  Enter confirm  Esc back",
                theme.secondary,
            )));
            hint.render(chunks[2], buf);
        }
        OnboardingStep::EnterApiKey {
            provider,
            input,
            error,
        } => {
            let mut lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    format!(" Enter your {} API key:", provider),
                    theme.heading,
                )),
                Line::from(""),
            ];
            let masked = if input.is_empty() {
                "\u{2588}".to_string() // cursor block
            } else if input.len() <= 8 {
                format!("{}\u{2588}", "*".repeat(input.len()))
            } else {
                format!(
                    "{}{}{}\u{2588}",
                    &input[..4],
                    "*".repeat(input.len() - 8),
                    &input[input.len() - 4..]
                )
            };
            lines.push(Line::from(vec![
                Span::styled("  > ", theme.prompt),
                Span::styled(masked, theme.text),
            ]));
            if let Some(err) = error {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!("  \u{2717} {}", err),
                    theme.error,
                )));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  Enter validate  Esc back",
                theme.secondary,
            )));
            let para = Paragraph::new(lines).wrap(Wrap { trim: false });
            para.render(inner, buf);
        }
        OnboardingStep::ValidatingKey { provider } => {
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    format!("  \u{25dc} Validating {} key...", provider),
                    theme.info,
                )),
            ];
            let para = Paragraph::new(lines);
            para.render(inner, buf);
        }
        OnboardingStep::SelectModel(state) => {
            let widget = SelectListWidget::new(state)
                .normal_style(theme.text)
                .selected_style(theme.accent.add_modifier(Modifier::REVERSED))
                .description_style(theme.secondary);
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(2),
                    Constraint::Min(1),
                    Constraint::Length(2),
                ])
                .split(inner);
            let title = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(" Select a model:", theme.heading)),
            ]);
            title.render(chunks[0], buf);
            widget.render(chunks[1], buf);
            let hint = Paragraph::new(Line::from(Span::styled(
                "  \u{2191}\u{2193} select  Enter confirm  Esc back",
                theme.secondary,
            )));
            hint.render(chunks[2], buf);
        }
        OnboardingStep::Confirm { provider, model } => {
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(" \u{2714} Setup Summary", theme.heading)),
                Line::from(""),
                Line::from(vec![
                    Span::styled("  Provider: ", theme.secondary),
                    Span::styled(provider.clone(), theme.text),
                ]),
                Line::from(vec![
                    Span::styled("  Model:    ", theme.secondary),
                    Span::styled(model.clone(), theme.text),
                ]),
                Line::from(""),
                Line::from(Span::styled("  Enter confirm  Esc back", theme.secondary)),
            ];
            let para = Paragraph::new(lines);
            para.render(inner, buf);
        }
        OnboardingStep::Saving => {
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled("  \u{25dc} Saving credentials...", theme.info)),
            ];
            let para = Paragraph::new(lines);
            para.render(inner, buf);
        }
        OnboardingStep::Done => {
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled("  \u{2714} Setup complete!", theme.info)),
                Line::from(Span::styled(
                    "  Press Enter to start chatting",
                    theme.secondary,
                )),
            ];
            let para = Paragraph::new(lines);
            para.render(inner, buf);
        }
    }
}

fn render_welcome(theme: &Theme, area: Rect, buf: &mut Buffer) {
    let mascot = tem_mascot(theme.accent, theme.secondary);
    let mascot_height = mascot.len() as u16;

    // For small terminals: skip mascot, just show title + prompt
    if area.height < mascot_height + 5 {
        let mut lines = Vec::new();
        lines.push(Line::from(""));

        // Compact: just show title
        let amber = ratatui::style::Style::default().fg(crate::theme::TemPalette::AMBER);
        lines.push(Line::from(vec![
            Span::styled("  T E M M", theme.accent.add_modifier(Modifier::BOLD)),
            Span::styled("1", amber.add_modifier(Modifier::BOLD)),
            Span::styled("E", theme.accent.add_modifier(Modifier::BOLD)),
        ]));
        lines.push(Line::from(Span::styled(
            "  Cloud-native AI Agent",
            theme.secondary,
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  Press Enter to get started",
            theme.info,
        )));

        let para = Paragraph::new(lines);
        para.render(area, buf);
        return;
    }

    // Full layout with mascot
    let top_pad = area.height.saturating_sub(mascot_height + 4) / 2;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(top_pad.max(1)),
            Constraint::Length(mascot_height),
            Constraint::Length(3),
            Constraint::Min(0),
        ])
        .split(area);

    // Render mascot centered
    for (i, line) in mascot.iter().enumerate() {
        let y = chunks[1].top() + i as u16;
        if y < chunks[1].bottom() {
            let x = chunks[1].left() + (chunks[1].width.saturating_sub(35)) / 2;
            buf.set_line(x, y, line, 35);
        }
    }

    // Instruction
    let hint = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled("Press Enter to get started", theme.info)),
    ])
    .alignment(Alignment::Center);
    hint.render(chunks[2], buf);
}
