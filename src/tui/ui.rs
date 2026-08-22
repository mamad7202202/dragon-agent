//! Rendering: one ember-lit terminal, three zones (header / transcript / prompt).

use crate::theme::*;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use super::App;
use super::Entry;

const SPINNER: [&str; 8] = ["|", "/", "-", "\\", "|", "/", "-", "\\"];

fn style_fg(c: ratatui::style::Color) -> Style {
    Style::new().fg(c)
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let [header, body, input, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(3),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .areas(f.area());

    draw_header(f, app, header);
    draw_transcript(f, app, body);
    draw_input(f, app, input);
    draw_footer(f, app, footer);
}

// ------------------------------------------------------------------- header

fn brand_lines() -> Vec<Line<'static>> {
    // Box-drawing display type: D R A G O N   A G E N T
    let d = ["╔═╗", "║ ║", "╚═╝"];
    let r = ["╦═╗", "╠═╝", "╩ ╩"];
    let a = ["┌─┐", "├─┤", "┴ ┴"];
    let g = ["┌─┐", "│ ┬", "┴─┘"];
    let o = ["┌─┐", "│ │", "└─┘"];
    let n = ["╔╦╗", "║║║", "╩ ╩"];
    let e = ["┌─┐", "├─┤", "└─┘"];
    let t = ["┌┬┐", " │ ", " ┴ "];
    let word1 = [d, r, a, g, o, n];
    let word2 = [a, g, e, n, t];
    let palette = [EMBER, FLAME, GOLD];

    let mut rows: [Vec<Span<'static>>; 3] = [vec![], vec![], vec![]];
    for (wi, word) in [word1.to_vec(), word2.to_vec()].iter().enumerate() {
        if wi > 0 {
            for row in rows.iter_mut() {
                row.push(Span::styled("  ", style_fg(ASH)));
            }
        }
        for (li, letter) in word.iter().enumerate() {
            let color = palette[(li + wi * 2) % palette.len()];
            for (row_i, part) in letter.iter().enumerate() {
                rows[row_i].push(Span::styled(
                    (*part).to_string(),
                    Style::new().fg(color).add_modifier(Modifier::BOLD),
                ));
                rows[row_i].push(Span::raw(" "));
            }
        }
    }
    rows.into_iter().map(Line::from).collect()
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered()
        .border_set(border::ROUNDED)
        .border_style(style_fg(SCALE))
        .style(Style::new().bg(NIGHT));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [left, right] =
        Layout::horizontal([Constraint::Min(20), Constraint::Length(26)]).areas(inner);

    // Right side info
    f.render_widget(
        Paragraph::new(vec![
            Line::from(app.model_spec.clone()).right_aligned(),
            Line::from(format!("session {}", app.session_label())).right_aligned(),
        ])
        .style(Style::new().fg(ASH)),
        right,
    );

    f.render_widget(Paragraph::new(brand_lines()), left);
}

// ---------------------------------------------------------------- transcript

pub(super) fn entry_lines(entry: &Entry, width: u16) -> Vec<Line<'static>> {
    match entry {
        Entry::User(text) => labeled_block("you", SKY, text, width),
        Entry::Assistant(text) => labeled_block("dragon", EMBER, text, width),
        Entry::Tool { name, detail } => vec![Line::from(vec![
            Span::styled("  » ".to_string(), style_fg(VIOLET)),
            Span::styled(name.clone(), style_fg(VIOLET).add_modifier(Modifier::BOLD)),
            Span::styled(format!(" {detail}"), Style::new().fg(ASH)),
        ])],
        Entry::System(text) => text
            .lines()
            .map(|l| Line::from(Span::styled(l.to_string(), Style::new().fg(ASH))))
            .collect(),
    }
}

fn labeled_block(label: &str, color: ratatui::style::Color, text: &str, _width: u16) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    out.push(Line::from(Span::styled(
        format!("{label}"),
        Style::new().fg(color).add_modifier(Modifier::BOLD),
    )));
    for l in text.lines() {
        if l.is_empty() {
            out.push(Line::from(""));
        } else {
            out.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(l.to_string(), style_fg(BONE)),
            ]));
        }
    }
    out.push(Line::from(""));
    out
}

fn draw_transcript(f: &mut Frame, app: &mut App, area: Rect) {
    let width = area.width.saturating_sub(2);

    let mut lines: Vec<Line> = Vec::new();
    if app.entries.is_empty() && app.streaming.is_none() {
        lines.extend(welcome_lines(area));
    } else {
        for e in &app.entries {
            lines.extend(entry_lines(e, width));
        }
        if let Some(partial) = &app.streaming {
            lines.extend(labeled_block("dragon", EMBER, partial, width));
            // blinking cursor line
            if let Some(last) = lines.last_mut() {
                last.spans.push(Span::styled("▌", style_fg(GOLD)));
            }
        }
    }

    // scroll math
    let vis = area.height as usize;
    let offset = if app.scroll_offset == 0 {
        lines.len().saturating_sub(vis)
    } else {
        app.scroll_offset.min(lines.len().saturating_sub(1))
    };

    let frame_block = Block::bordered()
        .border_set(border::ROUNDED)
        .border_style(style_fg(SCALE))
        .style(Style::new().bg(NIGHT))
        .title(Span::styled(
            format!(" {} ", app.status),
            Style::new().fg(if app.busy { FLAME } else { ASH }),
        ));
    f.render_widget(&frame_block, area);
    let chat_area = frame_block.inner(area);

    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((offset as u16, 0)),
        chat_area,
    );
}

fn welcome_lines(area: Rect) -> Vec<Line<'static>> {
    let mut v = Vec::new();
    let pad = (area.height as usize / 6).max(0);
    for _ in 0..pad {
        v.push(Line::from(""));
    }
    for l in brand_lines() {
        v.push(l);
    }
    v.push(Line::from(""));
    v.push(Line::from(Span::styled(
        "a fast AI agent with a long memory".to_string(),
        Style::new().fg(ASH).italic(),
    ))
    .alignment(Alignment::Center));
    v.push(Line::from(Span::styled(
        "by mamad720220 · t.me/mamad720220 · MIT".to_string(),
        Style::new().fg(SCALE),
    ))
    .alignment(Alignment::Center));
    v.push(Line::from(""));
    v.push(Line::from(Span::styled(
        "type a message and press enter — /help for commands".to_string(),
        Style::new().fg(FLAME),
    ))
    .alignment(Alignment::Center));
    v
}

// -------------------------------------------------------------------- input

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    let setup = app.wizard.is_some();
    let border_color = if app.busy {
        SCALE
    } else if setup {
        GOLD
    } else {
        EMBER
    };
    let block = Block::bordered()
        .border_set(border::ROUNDED)
        .border_style(style_fg(border_color))
        .style(Style::new().bg(NIGHT))
        .title(Span::styled(
            if setup { " setup " } else { " prompt " },
            Style::new().fg(if setup { GOLD } else { ASH }),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let prefix = "> ";
    let spans = Line::from(vec![
        Span::styled(prefix, style_fg(EMBER).add_modifier(Modifier::BOLD)),
        Span::styled(app.input.clone(), style_fg(BONE)),
        Span::styled("█", style_fg(BONE)), // cursor approximation
    ]);
    f.render_widget(Paragraph::new(spans), inner);

    // place the real terminal cursor at end of text
    let cx = inner.x + 2 + UnicodeWidthStr::width(app.input.as_str()) as u16;
    if !app.busy && cx < inner.x + inner.width {
        f.set_cursor_position((cx, inner.y));
    }
}

// ------------------------------------------------------------------- footer

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let left = " enter send · pgup/pgdn scroll · ctrl+n new chat · esc quit";
    let right = if app.busy {
        format!("{} {}", SPINNER[app.spinner_frame % SPINNER.len()], app.status)
    } else {
        String::new()
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(left.to_string(), Style::new().fg(SCALE)),
            Span::raw(" "),
        ]))
        .alignment(Alignment::Left),
        area,
    );
    if !right.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(right, style_fg(FLAME))))
                .alignment(Alignment::Right),
            area,
        );
    }
}
