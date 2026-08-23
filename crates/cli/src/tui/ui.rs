//! Rendering: clean single-line header, transcript, wizard panel, prompt.

use super::{App, Entry};
use crate::theme::*;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

const SPINNER: [&str; 8] = ["|", "/", "-", "\\", "|", "/", "-", "\\"];

fn fg(c: ratatui::style::Color) -> Style {
    Style::new().fg(c)
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let wiz_rows = app.wizard_rows();
    let wiz_h = if app.wizard.is_some() {
        (wiz_rows.len() as u16 + 2).min(10)
    } else {
        0
    };

    let [header, body, wizard, input, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(4),
        Constraint::Length(wiz_h),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .areas(f.area());

    draw_header(f, app, header);
    draw_transcript(f, app, body);
    if !wiz_rows.is_empty() {
        draw_wizard(f, app, &wiz_rows, wizard);
    }
    draw_input(f, app, input);
    draw_footer(f, app, footer);
}

// ------------------------------------------------------------------- header

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let [left, right] =
        Layout::horizontal([Constraint::Min(20), Constraint::Length(34)]).areas(area);

    let brand = Line::from(vec![
        Span::styled("DRAGON", fg(EMBER).add_modifier(Modifier::BOLD)),
        Span::styled(" AGENT", fg(GOLD).add_modifier(Modifier::BOLD)),
        Span::styled(format!("  v{}", dragon_core::VERSION), fg(SCALE)),
    ]);
    f.render_widget(Paragraph::new(brand), left);

    let meta = Line::from(vec![
        Span::styled(app.model_spec.clone(), fg(ASH)),
        Span::styled("  ·  ", fg(SCALE)),
        Span::styled(app.session_label(), fg(SCALE)),
    ])
    .right_aligned();
    f.render_widget(Paragraph::new(meta), right);
}

// ---------------------------------------------------------------- transcript

pub(super) fn entry_lines(entry: &Entry) -> Vec<Line<'static>> {
    match entry {
        Entry::User(text) => labeled_block("you", SKY, text),
        Entry::Assistant(text) => labeled_block("dragon", EMBER, text),
        Entry::Tool { name, detail } => vec![Line::from(vec![
            Span::raw("  "),
            Span::styled("» ".to_string(), fg(VIOLET)),
            Span::styled(name.clone(), fg(VIOLET).add_modifier(Modifier::BOLD)),
            Span::styled(format!(" {detail}"), fg(ASH)),
        ])],
        Entry::Approval { tool, detail } => vec![
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "PERMISSION ".to_string(),
                    fg(GOLD).add_modifier(Modifier::BOLD),
                ),
                Span::styled(tool.clone(), fg(FLAME).add_modifier(Modifier::BOLD)),
                Span::styled(format!("  {detail}"), fg(BONE)),
            ]),
            Line::from(Span::styled(
                "     [y] allow   [a] always allow   [n] deny   [d] what does this do?".to_string(),
                fg(SCALE),
            )),
            Line::from(""),
        ],
        Entry::System(text) => text
            .lines()
            .map(|l| {
                if l.is_empty() {
                    Line::from("")
                } else {
                    Line::from(vec![Span::raw("  "), Span::styled(l.to_string(), fg(ASH))])
                }
            })
            .collect(),
    }
}

fn labeled_block(label: &str, color: ratatui::style::Color, text: &str) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    out.push(Line::from(Span::styled(
        label.to_string(),
        fg(color).add_modifier(Modifier::BOLD),
    )));
    for l in text.lines() {
        if l.is_empty() {
            out.push(Line::from(""));
        } else {
            out.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(l.to_string(), fg(BONE)),
            ]));
        }
    }
    out.push(Line::from(""));
    out
}

fn draw_transcript(f: &mut Frame, app: &mut App, area: Rect) {
    app.areas.body = area;
    let frame_block = Block::bordered()
        .border_set(border::ROUNDED)
        .border_style(fg(SCALE))
        .style(Style::new().bg(NIGHT))
        .title(Span::styled(
            format!(" {} ", app.status),
            fg(if app.busy { FLAME } else { ASH }),
        ));
    f.render_widget(&frame_block, area);
    let chat_area = frame_block.inner(area);

    let mut lines: Vec<Line> = Vec::new();
    if app.entries.is_empty() && app.streaming.is_none() && app.wizard.is_none() {
        lines.extend(welcome_lines(chat_area));
    } else if app.entries.is_empty() && app.streaming.is_none() {
        lines.extend(brand_block());
        lines.push(Line::from(""));
    } else {
        for e in &app.entries {
            lines.extend(entry_lines(e));
        }
        if let Some(partial) = &app.streaming {
            lines.extend(labeled_block("dragon", EMBER, partial));
            if let Some(last) = lines.last_mut() {
                last.spans.push(Span::styled("▌", fg(GOLD)));
            }
        }
    }

    let vis = chat_area.height as usize;
    let offset = if app.scroll_offset == 0 {
        lines.len().saturating_sub(vis)
    } else {
        app.scroll_offset.min(lines.len().saturating_sub(1))
    };

    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((offset as u16, 0)),
        chat_area,
    );
}

/// The display-type wordmark, used only on the welcome screen.
fn brand_lines() -> Vec<Line<'static>> {
    const ART: [&str; 12] = [
        "██████╗ ██████╗  █████╗  ██████╗  ██████╗ ███╗   ██╗",
        "██╔══██╗██╔══██╗██╔══██╗██╔════╝ ██╔═══██╗████╗  ██║",
        "██║  ██║██████╔╝███████║██║  ███╗██║   ██║██╔██╗ ██║",
        "██║  ██║██╔══██╗██╔══██║██║   ██║██║   ██║██║╚██╗██║",
        "██████╔╝██║  ██║██║  ██║╚██████╔╝╚██████╔╝██║ ╚████║",
        "╚═════╝ ╚═╝  ╚═╝╚═╝  ╚═╝ ╚═════╝  ╚═════╝ ╚═╝  ╚═══╝",
        "",
        " █████╗  ██████╗ ███████╗███╗   ██╗████████╗",
        "██╔══██╗██╔════╝ ██╔════╝████╗  ██║╚══██╔══╝",
        "███████║██║  ███╗█████╗  ██╔██╗ ██║   ██║   ",
        "██╔══██║██║   ██║██╔══╝  ██║╚██╗██║   ██║   ",
        "██║  ██║╚██████╔╝███████╗██║ ╚████║   ██║   ",
    ];
    // ember -> flame -> gold sweep down the block
    let palette = [EMBER, FLAME, GOLD];
    ART.iter()
        .enumerate()
        .map(|(i, line)| {
            if line.is_empty() {
                return Line::from("");
            }
            let color = palette[i % palette.len()];
            Line::from(Span::styled(
                line.to_string(),
                fg(color).add_modifier(Modifier::BOLD),
            ))
        })
        .collect()
}

fn brand_block() -> Vec<Line<'static>> {
    let pad = vec![Line::from(""); 2];
    let mut v = pad;
    v.extend(brand_lines());
    v
}

fn welcome_lines(area: Rect) -> Vec<Line<'static>> {
    let mut v: Vec<Line> = Vec::new();
    let content_h = 12usize;
    let pad = area.height as usize / 2 > content_h / 2 + 1;
    if pad {
        v.push(Line::from(""));
    }
    for l in brand_lines() {
        v.push(l.alignment(Alignment::Center));
    }
    v.push(Line::from(""));
    v.push(
        Line::from(Span::styled(
            "a fast AI agent with a long memory".to_string(),
            Style::new().fg(ASH).add_modifier(Modifier::ITALIC),
        ))
        .alignment(Alignment::Center),
    );
    v
}

// ------------------------------------------------------------------ wizard

fn draw_wizard(f: &mut Frame, app: &mut App, rows: &[String], area: Rect) {
    app.areas.wizard = area;
    let block = Block::bordered()
        .border_set(border::ROUNDED)
        .border_style(fg(GOLD))
        .style(Style::new().bg(NIGHT))
        .title(Span::styled(
            match app.wizard.as_ref().map(|w| w.step) {
                Some("provider") => " setup - provider ",
                Some("url") => " setup - base url ",
                Some("key") => " setup - api key ",
                Some("more") => " setup - models (up/down + space) ",
                _ => " setup ",
            },
            fg(GOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(&block, area);

    // window the list so the selected row stays visible
    let vis = inner.height as usize;
    let sel = app.wizard_row.min(rows.len().saturating_sub(1));
    let start = if sel + 1 > vis.saturating_sub(1) {
        sel + 1 - vis.saturating_sub(1)
    } else {
        0
    };
    let end = (start + vis).min(rows.len());
    app.wizard_top = start;

    let mut lines: Vec<Line> = Vec::new();
    for (i, row) in rows[start..end].iter().enumerate() {
        let idx = start + i;
        if idx == sel {
            lines.push(Line::from(vec![
                Span::styled("▸ ".to_string(), fg(GOLD).add_modifier(Modifier::BOLD)),
                Span::styled(
                    row.clone(),
                    Style::new().fg(NIGHT).bg(GOLD).add_modifier(Modifier::BOLD),
                ),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(row.clone(), fg(BONE)),
            ]));
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
}

// -------------------------------------------------------------------- input

fn draw_input(f: &mut Frame, app: &mut App, area: Rect) {
    app.areas.input = area;
    let setup = app.wizard.is_some();
    let border_color = if setup {
        GOLD
    } else if app.busy {
        SCALE
    } else {
        EMBER
    };
    let block = Block::bordered()
        .border_set(border::ROUNDED)
        .border_style(fg(border_color))
        .style(Style::new().bg(NIGHT))
        .title(Span::styled(
            if setup { " your input " } else { " prompt " },
            fg(if setup { GOLD } else { ASH }),
        ));
    let inner = block.inner(area);

    let cursor_char_pos = app.cursor.min(app.input.chars().count());
    let before: String = app.input.chars().take(cursor_char_pos).collect();
    let at: String = app.input.chars().skip(cursor_char_pos).take(1).collect();
    let after: String = app.input.chars().skip(cursor_char_pos + 1).collect();

    let line = Line::from(vec![
        Span::styled("> ", fg(EMBER).add_modifier(Modifier::BOLD)),
        Span::styled(before.clone(), fg(BONE)),
        if at.is_empty() {
            Span::styled(" ", fg(GOLD)) // caret on empty position
        } else {
            Span::styled(at, Style::new().fg(NIGHT).bg(BONE))
        },
        Span::styled(after, fg(BONE)),
    ]);
    f.render_widget(Paragraph::new(line), inner);

    if !app.busy {
        let cx = inner.x + 2 + UnicodeWidthStr::width(before.as_str()) as u16;
        if cx < inner.x + inner.width {
            f.set_cursor_position((cx, inner.y));
        }
    }
}

// ------------------------------------------------------------------- footer

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let left = if app.wizard.is_some() {
        " setup - answer below (up/down + space)".to_string()
    } else if app.busy {
        " esc stop · pgup/pgdn scroll".to_string()
    } else {
        " enter send · ctrl+n new chat · esc quit".to_string()
    };
    let left = format!("[{}] {}", app.mode.as_str(), left);
    let left = match &app.update_note {
        Some(note) => format!("{left}  ·  ⭡ {note}"),
        None => left,
    };
    let right = if app.busy {
        format!(
            "{} {}",
            SPINNER[app.spinner_frame % SPINNER.len()],
            app.status
        )
    } else {
        String::new()
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(left, fg(SCALE))))
            .alignment(Alignment::Left),
        area,
    );
    if !right.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(right, fg(FLAME))))
                .alignment(Alignment::Right),
            area,
        );
    }
}
