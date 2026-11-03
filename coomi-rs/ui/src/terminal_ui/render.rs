use super::COMMANDS;
use super::CatalogTab;
use super::NoticeKind;
use super::OverlayKind;
use super::PlanModeChoice;
use super::SettingsTab;
use super::TimelineEntry;
use super::ToolState;
use super::TuiState;
use super::catalog_matches;
use super::model_matches;
use super::session_matches;
use super::theme;
use pulldown_cmark::Alignment as MarkdownAlignment;
use pulldown_cmark::CodeBlockKind;
use pulldown_cmark::Event;
use pulldown_cmark::Options;
use pulldown_cmark::Parser;
use pulldown_cmark::Tag;
use pulldown_cmark::TagEnd;
use ratatui::Frame;
use ratatui::layout::Alignment;
use ratatui::layout::Constraint;
use ratatui::layout::Direction;
use ratatui::layout::Layout;
use ratatui::layout::Margin;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::text::Text;
use ratatui::widgets::Block;
use ratatui::widgets::BorderType;
use ratatui::widgets::Borders;
use ratatui::widgets::Clear;
use ratatui::widgets::List;
use ratatui::widgets::ListItem;
use ratatui::widgets::ListState;
use ratatui::widgets::Padding;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Scrollbar;
use ratatui::widgets::ScrollbarOrientation;
use ratatui::widgets::ScrollbarState;
use ratatui::widgets::Wrap;
use serde_json::Value;
use std::path::Path;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

pub fn draw(frame: &mut Frame<'_>, app: &TuiState) {
    let area = frame.area();
    frame.render_widget(Block::default().style(theme::base()), area);

    let composer_width = area.width.saturating_sub(4).max(1);
    let composer_height = app
        .editor
        .line_count(composer_width)
        .saturating_add(2)
        .clamp(3, 7);
    let queue_height = u16::from(!app.queue.is_empty());
    let sheet_height = bottom_sheet_height(app, area.height, composer_height, queue_height);
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(5),
            Constraint::Length(queue_height),
            Constraint::Length(sheet_height),
            Constraint::Length(composer_height),
            Constraint::Length(1),
        ])
        .split(area);

    render_header(frame, layout[0], app);
    if app.timeline.is_empty() {
        render_welcome(frame, layout[1], app);
    } else {
        render_timeline(frame, layout[1], app);
    }
    if queue_height > 0 {
        render_queue(frame, layout[2], app);
    }
    let sheet = layout[3];
    render_composer(frame, layout[4], app);
    render_footer(frame, layout[5], app);

    if let Some(overlay) = &app.overlay {
        match overlay.kind {
            OverlayKind::Commands => render_commands(frame, sheet, app),
            OverlayKind::Models => render_models(frame, sheet, app),
            OverlayKind::History => render_history(frame, sheet, app),
            OverlayKind::Catalog => render_catalog(frame, sheet, app),
            OverlayKind::Help => render_help(frame, sheet),
            OverlayKind::McpConfig => render_mcp_form(frame, sheet, app),
            OverlayKind::Settings => render_settings(frame, area, app),
        }
    }
    if let Some(target) = &app.confirm_delete {
        render_delete_confirmation(frame, sheet, app, target);
    }
    if app.pending_approval.is_some() {
        render_approval(frame, sheet, app);
    }
    if app.pending_user_input.is_some() {
        render_user_input(frame, sheet, app);
    }
    if app.plan_mode_confirm.is_some() {
        render_plan_mode_confirm(frame, sheet, app);
    }
    if app.pending_update.is_some() {
        render_update_prompt(frame, sheet, app);
    }
}

fn bottom_sheet_height(
    app: &TuiState,
    total_height: u16,
    composer_height: u16,
    queue_height: u16,
) -> u16 {
    let requested = if app.pending_update.is_some() {
        10
    } else if app.plan_mode_confirm.is_some() {
        9
    } else if app.pending_user_input.is_some() {
        14
    } else if app.pending_approval.is_some() {
        15
    } else if app.confirm_delete.is_some() {
        8
    } else {
        match app.overlay.as_ref().map(|overlay| overlay.kind) {
            Some(OverlayKind::Commands) => 18,
            Some(OverlayKind::Models | OverlayKind::History) => 16,
            Some(OverlayKind::Catalog) => 18,
            Some(OverlayKind::Help) => 15,
            Some(OverlayKind::McpConfig) => app
                .mcp_form
                .as_ref()
                .map_or(10, |form| (form.fields.len() as u16 * 3 + 6).min(18)),
            Some(OverlayKind::Settings) => 0,
            None => 0,
        }
    };
    let available = total_height
        .saturating_sub(2)
        .saturating_sub(3)
        .saturating_sub(queue_height)
        .saturating_sub(composer_height)
        .saturating_sub(1);
    requested.min(available)
}

fn render_header(frame: &mut Frame<'_>, area: Rect, app: &TuiState) {
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(theme::BORDER))
        .style(Style::default().bg(theme::CANVAS));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(12),
            Constraint::Min(12),
            Constraint::Percentage(42),
        ])
        .split(inner);
    frame.render_widget(
        Paragraph::new(Span::styled("  COOMI", theme::title())),
        columns[0],
    );

    let model = if area.width < 70 {
        app.session.model.clone()
    } else {
        format!("{} / {}", app.session.provider_id, app.session.model)
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("model  ", Style::default().fg(theme::MUTED)),
            Span::styled(
                truncate_cells(&model, columns[1].width as usize),
                theme::base(),
            ),
        ])),
        columns[1],
    );

    let cwd = compact_path(&display_path(&app.cwd), columns[2].width as usize);
    frame.render_widget(
        Paragraph::new(Span::styled(cwd, Style::default().fg(theme::MUTED)))
            .alignment(Alignment::Right),
        columns[2],
    );
}

fn render_welcome(frame: &mut Frame<'_>, area: Rect, app: &TuiState) {
    if area.width >= 82 && area.height >= 15 {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(area.inner(Margin::new(2, 1)));
        render_brand(frame, columns[0], app);
        render_recent_sessions(frame, columns[1], app, true);
    } else {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(10), Constraint::Min(4)])
            .split(area.inner(Margin::new(1, 0)));
        render_brand(frame, rows[0], app);
        render_recent_sessions(frame, rows[1], app, false);
    }
}

fn render_brand(frame: &mut Frame<'_>, area: Rect, app: &TuiState) {
    let brand_width = 31_u16.min(area.width);
    let content_width = brand_width.saturating_add(15).min(area.width);
    let left = area.x + area.width.saturating_sub(content_width) / 2;
    let centered = Rect::new(left, area.y, content_width, area.height);
    let columns = if area.width >= 46 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(14), Constraint::Min(28)])
            .split(centered)
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(0), Constraint::Min(28)])
            .split(centered)
    };
    if columns[0].width > 0 {
        frame.render_widget(Paragraph::new(mascot_text()), columns[0]);
    }

    let mut lines = big_logo_lines();
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("Build clearly. ", Style::default().fg(theme::MUTED)),
        Span::styled("Move deliberately.", Style::default().fg(theme::CORAL)),
    ]));
    let mode = format!("{} tools  ·  {}", 7, app.policy.label());
    lines.push(Line::from(Span::styled(
        mode,
        Style::default().fg(theme::MUTED),
    )));
    frame.render_widget(Paragraph::new(lines), columns[1]);
}

fn render_recent_sessions(frame: &mut Frame<'_>, area: Rect, app: &TuiState, divider: bool) {
    let block = if divider {
        Block::default()
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(theme::BORDER))
            .padding(Padding::left(2))
    } else {
        Block::default().padding(Padding::horizontal(1))
    };
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = vec![Line::from(vec![
        Span::styled("RECENT", Style::default().fg(theme::SKY).bold()),
        Span::styled(
            format!("  {} sessions", app.sessions.len()),
            Style::default().fg(theme::MUTED),
        ),
    ])];
    lines.push(Line::from(""));
    let available = inner.height.saturating_sub(5) as usize;
    if app.sessions.is_empty() {
        lines.push(Line::from(Span::styled(
            "Your first conversation will appear here.",
            Style::default().fg(theme::MUTED),
        )));
    } else {
        for session in app.sessions.iter().take(available.max(1)) {
            let date = session.updated_at.format("%m-%d %H:%M");
            let preview_width = inner.width.saturating_sub(18) as usize;
            lines.push(Line::from(vec![
                Span::styled("● ", Style::default().fg(theme::MINT)),
                Span::styled(format!("{date}  "), Style::default().fg(theme::MUTED)),
                Span::styled(
                    truncate_cells(session_label(session), preview_width),
                    Style::default().fg(theme::TEXT),
                ),
            ]));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("Ctrl+R", theme::key()),
        Span::styled(" history   ", Style::default().fg(theme::MUTED)),
        Span::styled("Ctrl+K", theme::key()),
        Span::styled(" commands", Style::default().fg(theme::MUTED)),
    ]));
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_timeline(frame: &mut Frame<'_>, area: Rect, app: &TuiState) {
    let block = Block::default().padding(Padding::horizontal(2));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let text = timeline_text(app, inner.width);
    let line_count = wrapped_line_count(&text, inner.width.max(1));
    let paragraph = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .style(theme::base());
    let max_scroll = line_count.saturating_sub(inner.height as usize);
    let max_scroll_u16 = u16::try_from(max_scroll).unwrap_or(u16::MAX);
    app.timeline_max_scroll.set(max_scroll_u16);
    let offset = if app.follow_tail {
        max_scroll
    } else {
        usize::from(app.scroll.min(max_scroll_u16))
    };
    frame.render_widget(paragraph.scroll((offset as u16, 0)), inner);
    if line_count > inner.height as usize {
        // The scrollbar occupies the rightmost column of `area`.
        let scrollbar_rect = Rect {
            x: area.x + area.width.saturating_sub(1),
            y: area.y,
            width: 1,
            height: area.height,
        };
        app.scrollbar_area.set(Some(scrollbar_rect));
        let mut scrollbar_state = ScrollbarState::new(line_count)
            .viewport_content_length(inner.height as usize)
            .position(offset);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .thumb_style(Style::default().fg(theme::BORDER))
                .track_style(Style::default().fg(theme::SURFACE)),
            area,
            &mut scrollbar_state,
        );
    } else {
        app.scrollbar_area.set(None);
    }
}

fn timeline_text(app: &TuiState, width: u16) -> Text<'static> {
    let mut lines = Vec::new();
    for entry in &app.timeline {
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        match entry {
            TimelineEntry::User(content) => {
                lines.push(Line::from(vec![
                    Span::styled("YOU  ", Style::default().fg(theme::MINT).bold()),
                    Span::styled("user", Style::default().fg(theme::MUTED)),
                ]));
                lines.extend(plain_content_lines(content, theme::TEXT));
            }
            TimelineEntry::Assistant(content) => {
                lines.push(Line::from(vec![
                    Span::styled("COOMI  ", Style::default().fg(theme::CORAL).bold()),
                    Span::styled("assistant", Style::default().fg(theme::MUTED)),
                ]));
                lines.extend(markdown_lines_with_width(content, width));
            }
            TimelineEntry::Reasoning(content) => {
                lines.push(Line::from(vec![
                    Span::styled("THINKING  ", Style::default().fg(theme::MUTED).bold()),
                    Span::styled(
                        truncate_cells(&single_line(content), 108),
                        Style::default().fg(theme::MUTED),
                    ),
                ]));
            }
            TimelineEntry::SideUser(content) => {
                lines.push(Line::from(vec![
                    Span::styled("SIDE YOU  ", Style::default().fg(theme::SKY).bold()),
                    Span::styled("temporary", Style::default().fg(theme::MUTED)),
                ]));
                lines.extend(plain_content_lines(content, theme::TEXT));
            }
            TimelineEntry::SideAssistant(content) => {
                lines.push(Line::from(vec![
                    Span::styled("SIDE COOMI  ", Style::default().fg(theme::ORANGE).bold()),
                    Span::styled("read-only", Style::default().fg(theme::MUTED)),
                ]));
                lines.extend(markdown_lines_with_width(content, width));
            }
            TimelineEntry::Tool {
                name,
                arguments,
                state,
                ..
            } => {
                let (mark, mark_style, status) = match state {
                    ToolState::Running => (
                        spinner(app.spinner_tick).to_string(),
                        Style::default().fg(theme::AMBER),
                        "running",
                    ),
                    ToolState::Complete { success: true, .. } => {
                        ("✓".into(), Style::default().fg(theme::SUCCESS), "complete")
                    }
                    ToolState::Complete { success: false, .. } => {
                        ("×".into(), Style::default().fg(theme::ERROR), "failed")
                    }
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("{mark} "), mark_style),
                    Span::styled(name.clone(), Style::default().fg(theme::AMBER).bold()),
                    Span::styled(format!("  {status}  "), Style::default().fg(theme::MUTED)),
                    Span::styled(
                        truncate_cells(&compact_json(arguments), 72),
                        Style::default().fg(theme::MUTED),
                    ),
                ]));
                if let ToolState::Complete { output, .. } = state
                    && !output.trim().is_empty()
                {
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(
                            truncate_cells(&single_line(output), 110),
                            Style::default().fg(theme::MUTED),
                        ),
                    ]));
                }
            }
            TimelineEntry::Notice { kind, text } => {
                let (mark, color) = match kind {
                    NoticeKind::Success => ("✓", theme::SUCCESS),
                    NoticeKind::Warning => ("!", theme::AMBER),
                    NoticeKind::Error => ("×", theme::ERROR),
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("{mark}  "), Style::default().fg(color).bold()),
                    Span::styled(text.clone(), Style::default().fg(color)),
                ]));
            }
        }
    }
    Text::from(lines)
}

fn render_queue(frame: &mut Frame<'_>, area: Rect, app: &TuiState) {
    let preview = app.queue.front().map_or("", String::as_str);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!("  QUEUED {}  ", app.queue.len()),
                Style::default().fg(theme::CANVAS).bg(theme::AMBER).bold(),
            ),
            Span::styled(
                format!(" {}", truncate_cells(&single_line(preview), 72)),
                Style::default().fg(theme::MUTED),
            ),
        ])),
        area,
    );
}

fn render_composer(frame: &mut Frame<'_>, area: Rect, app: &TuiState) {
    let active_color = if app.busy { theme::AMBER } else { theme::MINT };
    let title = if app.busy {
        format!(
            " {} Working · Enter queues · Alt+I Side ",
            spinner(app.spinner_tick)
        )
    } else {
        " Message ".into()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(active_color))
        .title(Span::styled(
            title,
            Style::default().fg(active_color).bold(),
        ))
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let text = app.editor.text();
    let placeholder = if app.busy {
        "Add a follow-up and press Enter to queue"
    } else {
        "Ask about this project..."
    };
    let content = if text.is_empty() {
        Text::from(Span::styled(placeholder, Style::default().fg(theme::MUTED)))
    } else {
        Text::from(Span::styled(text, Style::default().fg(theme::TEXT)))
    };
    let (cursor_x, cursor_y) = app.editor.cursor_position(inner.width.max(1));
    let vertical_scroll = cursor_y.saturating_sub(inner.height.saturating_sub(1));
    frame.render_widget(
        Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .scroll((vertical_scroll, 0)),
        inner,
    );

    if app.overlay.is_none()
        && app.pending_approval.is_none()
        && app.pending_user_input.is_none()
        && app.confirm_delete.is_none()
        && inner.height > 0
    {
        frame.set_cursor_position((
            inner
                .x
                .saturating_add(cursor_x.min(inner.width.saturating_sub(1))),
            inner
                .y
                .saturating_add(cursor_y.saturating_sub(vertical_scroll)),
        ));
    }
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &TuiState) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(area);
    let hint = if area.width >= 86 {
        Line::from(vec![
            Span::styled(" Ctrl+K", theme::key()),
            Span::styled(" commands  ", Style::default().fg(theme::MUTED)),
            Span::styled("Ctrl+R", theme::key()),
            Span::styled(" history  ", Style::default().fg(theme::MUTED)),
            Span::styled("Ctrl+Y", theme::key()),
            Span::styled(" copy  ", Style::default().fg(theme::MUTED)),
            Span::styled("Alt+M", theme::key()),
            Span::styled(" models  ", Style::default().fg(theme::MUTED)),
            Span::styled("Alt+H", theme::key()),
            Span::styled(" help", Style::default().fg(theme::MUTED)),
        ])
    } else {
        Line::from(vec![
            Span::styled(" Ctrl+K", theme::key()),
            Span::styled(" menu  ", Style::default().fg(theme::MUTED)),
            Span::styled("Ctrl+Y", theme::key()),
            Span::styled(" copy  ", Style::default().fg(theme::MUTED)),
            Span::styled("Alt+H", theme::key()),
            Span::styled(" help", Style::default().fg(theme::MUTED)),
        ])
    };
    frame.render_widget(Paragraph::new(hint), columns[0]);

    let used_percent = app.context_status.used_percent;
    let context = if app.context_status.effective_context_window == 0 {
        "ctx --".to_owned()
    } else if area.width < 86 {
        format!("ctx {used_percent}%")
    } else {
        format!(
            "ctx {}% used {}/{}",
            used_percent,
            compact_tokens(app.context_status.used_tokens),
            compact_tokens(app.context_status.effective_context_window)
        )
    };
    let policy_color = if app.plan_mode {
        theme::AMBER
    } else {
        match app.policy {
            coomi_security::AccessMode::ReadOnly => theme::SUCCESS,
            coomi_security::AccessMode::WorkspaceWrite => theme::BLUE,
            coomi_security::AccessMode::FullAccess => theme::ORANGE,
        }
    };
    let plan_label = if app.plan_mode {
        "plan mode"
    } else {
        app.policy.label()
    };
    let context_color = match used_percent {
        0..=49 => theme::SUCCESS,
        50..=74 => theme::BLUE,
        75..=89 => theme::ORANGE,
        _ => theme::ERROR,
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(plan_label, Style::default().fg(policy_color).bold()),
            Span::styled("  ·  ", Style::default().fg(theme::BORDER)),
            Span::styled(context, Style::default().fg(context_color).bold()),
            Span::raw(" "),
        ]))
        .alignment(Alignment::Right),
        columns[1],
    );
}

fn render_commands(frame: &mut Frame<'_>, area: Rect, app: &TuiState) {
    let popup = popup_rect(area, 68, 19);
    let overlay = app.overlay.as_ref().expect("command overlay");
    let query = overlay.query.text();
    let items = COMMANDS
        .iter()
        .filter(|item| {
            item.label
                .to_ascii_lowercase()
                .contains(&query.to_ascii_lowercase())
        })
        .map(|item| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:<13}", command_slash(item.action)),
                    Style::default().fg(theme::ORANGE).bold(),
                ),
                Span::styled(format!("{:<18}", item.label), Style::default().bold()),
                Span::styled(item.detail, Style::default().fg(theme::MUTED)),
            ]))
        })
        .collect::<Vec<_>>();
    render_picker(
        frame,
        popup,
        Picker {
            title: " Command palette ",
            subtitle: "Type to filter",
            query: &query,
            items,
            selected: overlay.selected,
            footer: "Enter select  ·  Esc close",
        },
    );
}

fn render_models(frame: &mut Frame<'_>, area: Rect, app: &TuiState) {
    let popup = popup_rect(area, 76, 21);
    let overlay = app.overlay.as_ref().expect("model overlay");
    let query = overlay.query.text();
    let current = format!("{}:{}", app.session.provider_id, app.session.model);
    let items = app
        .models
        .iter()
        .filter(|choice| model_matches(choice, &query.to_ascii_lowercase()))
        .map(|choice| {
            let selected =
                choice.provider_id == app.session.provider_id && choice.model == app.session.model;
            ListItem::new(Line::from(vec![
                Span::styled(
                    if selected { "● " } else { "○ " },
                    Style::default().fg(if selected { theme::MINT } else { theme::BORDER }),
                ),
                Span::styled(
                    format!("{:<22}", choice.selector),
                    Style::default().fg(theme::TEXT).bold(),
                ),
                Span::styled(
                    truncate_cells(&choice.provider_display, 18),
                    Style::default().fg(theme::SKY),
                ),
                Span::styled(
                    format!("  {}", choice.model),
                    Style::default().fg(theme::MUTED),
                ),
                Span::styled(
                    if choice.is_fast { "  FAST" } else { "" },
                    Style::default().fg(theme::AMBER),
                ),
            ]))
        })
        .collect::<Vec<_>>();
    render_picker(
        frame,
        popup,
        Picker {
            title: " Models ",
            subtitle: &format!("Current  {current}"),
            query: &query,
            items,
            selected: overlay.selected,
            footer: "Enter switch  ·  Type to filter  ·  Esc close",
        },
    );
}

fn render_history(frame: &mut Frame<'_>, area: Rect, app: &TuiState) {
    let popup = popup_rect(area, 82, 23);
    let overlay = app.overlay.as_ref().expect("history overlay");
    let query = overlay.query.text();
    let items = app
        .sessions
        .iter()
        .filter(|session| session_matches(session, &query.to_ascii_lowercase()))
        .map(|session| {
            let active = session.id == app.session.id;
            ListItem::new(Line::from(vec![
                Span::styled(
                    if active { "● " } else { "○ " },
                    Style::default().fg(if active { theme::CORAL } else { theme::BORDER }),
                ),
                Span::styled(
                    format!("{}  ", session.updated_at.format("%m-%d %H:%M")),
                    Style::default().fg(theme::MUTED),
                ),
                Span::styled(
                    format!("{:<18}", truncate_cells(&session.model, 17)),
                    Style::default().fg(theme::SKY),
                ),
                Span::styled(
                    truncate_cells(session_label(session), 38),
                    Style::default().fg(theme::TEXT),
                ),
            ]))
        })
        .collect::<Vec<_>>();
    render_picker(
        frame,
        popup,
        Picker {
            title: " Session history ",
            subtitle: &format!("{} saved in this workspace", app.sessions.len()),
            query: &query,
            items,
            selected: overlay.selected,
            footer: "Enter resume  ·  D delete  ·  Type to filter  ·  Esc close",
        },
    );
}

struct Picker<'a> {
    title: &'a str,
    subtitle: &'a str,
    query: &'a str,
    items: Vec<ListItem<'static>>,
    selected: usize,
    footer: &'a str,
}

fn render_picker(frame: &mut Frame<'_>, area: Rect, picker: Picker<'_>) {
    frame.render_widget(Clear, area);
    let block = modal_block(picker.title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(inner);
    frame.render_widget(
        Paragraph::new(Span::styled(
            picker.subtitle,
            Style::default().fg(theme::MUTED),
        )),
        rows[0],
    );
    let search = if picker.query.is_empty() {
        Line::from(Span::styled(
            "  Search...",
            Style::default().fg(theme::MUTED),
        ))
    } else {
        Line::from(vec![
            Span::styled("  / ", Style::default().fg(theme::MINT)),
            Span::styled(picker.query.to_string(), Style::default().fg(theme::TEXT)),
        ])
    };
    frame.render_widget(
        Paragraph::new(search).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(theme::BORDER)),
        ),
        rows[1],
    );
    let selected = picker.selected.min(picker.items.len().saturating_sub(1));
    let (start, end) = visible_range(picker.items.len(), selected, rows[2].height as usize);
    let visible = picker
        .items
        .into_iter()
        .skip(start)
        .take(end - start)
        .collect::<Vec<_>>();
    let relative = selected.saturating_sub(start);
    let mut state = ListState::default().with_selected((end > start).then_some(relative));
    frame.render_stateful_widget(
        List::new(visible)
            .highlight_style(theme::selected())
            .highlight_symbol(" ")
            .repeat_highlight_symbol(true),
        rows[2],
        &mut state,
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            picker.footer,
            Style::default().fg(theme::MUTED),
        ))
        .alignment(Alignment::Center),
        rows[3],
    );
}

fn render_catalog(frame: &mut Frame<'_>, area: Rect, app: &TuiState) {
    let popup = popup_rect(area, 92, 25);
    frame.render_widget(Clear, popup);
    let block = modal_block(" Catalogs ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Min(5),
            Constraint::Length(1),
        ])
        .split(inner);
    let overlay = app.overlay.as_ref().expect("catalog overlay");
    let tabs = Line::from(vec![
        catalog_tab(" MCP ", overlay.catalog_tab == CatalogTab::Mcp),
        Span::raw("  "),
        catalog_tab(" Skills ", overlay.catalog_tab == CatalogTab::Skills),
    ]);
    frame.render_widget(Paragraph::new(tabs), rows[0]);
    let query = overlay.query.text();
    frame.render_widget(
        Paragraph::new(if query.is_empty() {
            Line::from(Span::styled(
                "  Search catalog...",
                Style::default().fg(theme::MUTED),
            ))
        } else {
            Line::from(vec![
                Span::styled("  / ", Style::default().fg(theme::MINT)),
                Span::styled(query.clone(), Style::default().fg(theme::TEXT)),
            ])
        })
        .block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(theme::BORDER)),
        ),
        rows[1],
    );
    let content = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(rows[2]);
    let query_lower = query.to_ascii_lowercase();
    match overlay.catalog_tab {
        CatalogTab::Mcp => {
            let entries = app
                .mcp_entries
                .iter()
                .filter(|entry| catalog_matches(&entry.id, &entry.name, &query_lower))
                .collect::<Vec<_>>();
            render_catalog_list(
                frame,
                content[0],
                entries.iter().map(|entry| entry.name.as_str()),
                overlay.selected,
            );
            if let Some(entry) = entries.get(overlay.selected) {
                let requirements = if entry.required_parameters.is_empty() {
                    "Ready to configure".to_string()
                } else {
                    format!(
                        "Needs {}",
                        entry
                            .required_parameters
                            .iter()
                            .map(|item| item.label.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                render_catalog_detail(
                    frame,
                    content[1],
                    &entry.name,
                    &entry.description,
                    &format!("{}  ·  {}", entry.transport, requirements),
                );
            }
        }
        CatalogTab::Skills => {
            let entries = app
                .skill_entries
                .iter()
                .filter(|entry| catalog_matches(&entry.id, &entry.name, &query_lower))
                .collect::<Vec<_>>();
            render_catalog_list(
                frame,
                content[0],
                entries.iter().map(|entry| entry.name.as_str()),
                overlay.selected,
            );
            if let Some(entry) = entries.get(overlay.selected) {
                render_catalog_detail(
                    frame,
                    content[1],
                    &entry.name,
                    &entry.description,
                    &entry.repository,
                );
            }
        }
    }
    frame.render_widget(
        Paragraph::new(Span::styled(
            "←/→ tabs  ·  Enter install  ·  Type to filter  ·  Esc close",
            Style::default().fg(theme::MUTED),
        ))
        .alignment(Alignment::Center),
        rows[3],
    );
}

fn render_catalog_list<'a>(
    frame: &mut Frame<'_>,
    area: Rect,
    names: impl Iterator<Item = &'a str>,
    selected: usize,
) {
    let names = names.collect::<Vec<_>>();
    let selected = selected.min(names.len().saturating_sub(1));
    let (start, end) = visible_range(names.len(), selected, area.height as usize);
    let lines = names
        .iter()
        .enumerate()
        .skip(start)
        .take(end - start)
        .map(|(index, name)| {
            let style = if index == selected {
                theme::selected()
            } else {
                Style::default().fg(theme::TEXT)
            };
            Line::from(Span::styled(format!("  {name}"), style))
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::RIGHT)
                .border_style(Style::default().fg(theme::BORDER)),
        ),
        area,
    );
}

fn render_catalog_detail(
    frame: &mut Frame<'_>,
    area: Rect,
    name: &str,
    description: &str,
    metadata: &str,
) {
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(name.to_string(), theme::title())),
            Line::from(""),
            Line::from(description.to_string()),
            Line::from(""),
            Line::from(Span::styled(
                metadata.to_string(),
                Style::default().fg(theme::SKY),
            )),
        ])
        .wrap(Wrap { trim: true })
        .block(Block::default().padding(Padding::horizontal(2))),
        area,
    );
}

fn render_help(frame: &mut Frame<'_>, area: Rect) {
    let popup = popup_rect(area, 72, 23);
    frame.render_widget(Clear, popup);
    let block = modal_block(" Keyboard ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let lines = vec![
        help_line("Ctrl+K", "Open command palette"),
        help_line("Ctrl+R", "Open session history"),
        help_line("Ctrl+Y", "Copy last assistant response"),
        help_line("Ctrl+L", "Clear the timeline"),
        help_line("Alt+M", "Switch configured model"),
        help_line("Alt+S", "Open Settings"),
        help_line("Alt+H", "Open keyboard help"),
        help_line("Alt+L", "Create or control a Loop"),
        help_line("Alt+I", "Start a read-only Side Session"),
        help_line("Shift+Tab", "Cycle access policy"),
        help_line("Enter", "Send, or queue while a turn runs"),
        help_line("Shift+Enter", "Insert a new line"),
        help_line("Up/Down", "Recall input (editor empty) or move cursor"),
        help_line("Alt+Up/Down", "Recall previous input"),
        help_line("PageUp/PageDown", "Scroll the conversation"),
        help_line("Home/End", "Jump to top/bottom (editor empty)"),
        help_line("Mouse wheel", "Scroll the conversation"),
        help_line("Esc", "Close, cancel, or press twice to exit"),
        help_line("Ctrl+C", "Cancel a turn, clear input, or exit"),
        Line::from(""),
        Line::from(Span::styled(
            "Slash commands: /status /compact /model /history /loop /plan /exit_plan /memory /mcp /skills /settings /new /clear /quit",
            Style::default().fg(theme::MUTED),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().padding(Padding::new(2, 2, 1, 1)))
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn render_mcp_form(frame: &mut Frame<'_>, area: Rect, app: &TuiState) {
    let Some(form) = &app.mcp_form else {
        return;
    };
    let height = (form.fields.len() as u16 * 3 + 8).min(26);
    let popup = popup_rect(area, 72, height);
    frame.render_widget(Clear, popup);
    let block = modal_block(" Configure MCP ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let mut constraints = vec![Constraint::Length(2)];
    constraints.extend((0..form.fields.len()).map(|_| Constraint::Length(3)));
    constraints.push(Constraint::Min(1));
    constraints.push(Constraint::Length(1));
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(form.entry.name.clone(), theme::title()),
            Span::styled("  local configuration", Style::default().fg(theme::MUTED)),
        ])),
        rows[0],
    );
    for (index, (parameter, editor)) in form.fields.iter().enumerate() {
        let value = if parameter.secret && !editor.is_empty() {
            "•".repeat(editor.text().chars().count())
        } else {
            editor.text()
        };
        let border = if index == form.selected {
            theme::MINT
        } else {
            theme::BORDER
        };
        frame.render_widget(
            Paragraph::new(if value.is_empty() {
                Span::styled("required", Style::default().fg(theme::MUTED))
            } else {
                Span::styled(value, Style::default().fg(theme::TEXT))
            })
            .block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(border))
                    .title(Span::styled(
                        format!(" {} ", parameter.label),
                        Style::default().fg(border),
                    )),
            ),
            rows[index + 1],
        );
    }
    frame.render_widget(
        Paragraph::new(Span::styled(
            "Tab next  ·  Enter continue/install  ·  Esc cancel",
            Style::default().fg(theme::MUTED),
        ))
        .alignment(Alignment::Center),
        *rows.last().expect("MCP form footer"),
    );
}

fn render_settings(frame: &mut Frame<'_>, area: Rect, app: &TuiState) {
    let Some(settings) = &app.settings else {
        return;
    };
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BLUE))
        .title(Line::from(vec![
            Span::styled(" COOMI SETTINGS ", theme::title()),
            Span::styled(
                " Providers · MCP · Skills · Runtime ",
                Style::default().fg(theme::MUTED),
            ),
        ]))
        .style(Style::default().bg(theme::CANVAS).fg(theme::TEXT))
        .padding(Padding::new(2, 2, 1, 1));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if let Some(form) = &settings.form {
        render_provider_form(frame, inner, form, settings.error.as_deref());
        return;
    }

    let settings_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(8)])
        .split(inner);
    let tabs = [
        (SettingsTab::Providers, "1 Providers"),
        (SettingsTab::Mcp, "2 MCP"),
        (SettingsTab::Skills, "3 Skills"),
        (SettingsTab::Runtime, "4 Runtime"),
    ];
    frame.render_widget(
        Paragraph::new(Line::from(
            tabs.into_iter()
                .flat_map(|(tab, label)| {
                    let style = if settings.tab == tab {
                        theme::selected()
                    } else {
                        Style::default().fg(theme::MUTED)
                    };
                    [Span::styled(format!(" {label} "), style), Span::raw("  ")]
                })
                .collect::<Vec<_>>(),
        )),
        settings_rows[0],
    );
    let inner = settings_rows[1];
    if settings.tab != SettingsTab::Providers {
        render_service_settings(frame, inner, app);
        return;
    }

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(34), Constraint::Percentage(66)])
        .split(inner);
    let items = settings
        .provider_ids
        .iter()
        .map(|id| {
            let active = id == &settings.document.active;
            ListItem::new(Line::from(vec![
                Span::styled(
                    if active { "● " } else { "○ " },
                    Style::default().fg(if active { theme::ORANGE } else { theme::BORDER }),
                ),
                Span::styled(id.clone(), Style::default().fg(theme::TEXT)),
            ]))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default()
        .with_selected(Some(settings.selected.min(items.len().saturating_sub(1))));
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .borders(Borders::RIGHT)
                    .border_style(Style::default().fg(theme::BORDER))
                    .title(Span::styled(" Providers ", theme::title())),
            )
            .highlight_style(theme::selected())
            .highlight_symbol(" "),
        columns[0],
        &mut state,
    );
    if let Some(id) = settings.provider_ids.get(settings.selected)
        && let Some(provider) = settings.document.providers.get(id)
    {
        let api_key = if settings.show_secret {
            provider.api_key.clone()
        } else {
            mask_secret(&provider.api_key)
        };
        let details = vec![
            setting_line("Display", &provider.display),
            setting_line("Protocol", &provider.provider_type),
            setting_line(
                "Tool protocol",
                provider.tool_protocol.as_deref().unwrap_or("default"),
            ),
            setting_line("Base URL", &provider.base_url),
            setting_line("Model", &provider.model),
            setting_line("Fast model", provider.fast_model.as_deref().unwrap_or("-")),
            setting_line("API Key", &api_key),
            setting_line(
                "Context",
                &provider
                    .context_window
                    .map(compact_tokens)
                    .unwrap_or_else(|| "256.0K default".into()),
            ),
            setting_line(
                "Effective",
                &provider
                    .effective_context_window_percent
                    .map_or_else(|| "95% default".into(), |value| format!("{value}%")),
            ),
            setting_line(
                "Compact at",
                &provider
                    .auto_compact_token_limit
                    .map(compact_tokens)
                    .unwrap_or_else(|| "90% adaptive".into()),
            ),
            setting_line(
                "Compact scope",
                match provider.auto_compact_scope {
                    coomi_engine::AutoCompactScope::Total => "total",
                    coomi_engine::AutoCompactScope::BodyAfterPrefix => "body_after_prefix",
                },
            ),
            setting_line(
                "Remote compact",
                &format!(
                    "{} / {:?}",
                    provider.supports_remote_compaction.unwrap_or(false),
                    provider.remote_compaction_mode
                )
                .to_ascii_lowercase(),
            ),
            setting_line("Vision", &provider.supports_vision.to_string()),
            setting_line("Native search", &provider.supports_web_search.to_string()),
            setting_line(
                "Native tools",
                &(provider.supports_native_tools
                    && provider
                        .tool_protocol
                        .as_deref()
                        .is_none_or(|protocol| !protocol.eq_ignore_ascii_case("disabled")))
                .to_string(),
            ),
            Line::from(""),
            Line::from(Span::styled(
                if id == &settings.document.active {
                    "Active for new sessions"
                } else {
                    "Press Space to make active"
                },
                Style::default().fg(theme::ORANGE),
            )),
        ];
        frame.render_widget(
            Paragraph::new(details)
                .wrap(Wrap { trim: false })
                .block(Block::default().padding(Padding::horizontal(3))),
            columns[1],
        );
    }
    let footer = Rect::new(inner.x, inner.bottom().saturating_sub(2), inner.width, 2);
    let mut lines = vec![Line::from(vec![
        Span::styled("N", theme::key()),
        Span::raw(" new   "),
        Span::styled("E / Enter", theme::key()),
        Span::raw(" edit   "),
        Span::styled("D", theme::key()),
        Span::raw(" delete   "),
        Span::styled("V", theme::key()),
        Span::raw(" show/hide key   "),
        Span::styled("M", theme::key()),
        Span::raw(" catalogs   "),
        Span::styled("Esc", theme::key()),
        Span::raw(" close"),
    ])];
    if let Some(error) = &settings.error {
        lines.push(Line::from(Span::styled(
            truncate_cells(error, inner.width as usize),
            Style::default().fg(theme::ERROR),
        )));
    }
    frame.render_widget(Paragraph::new(lines), footer);
}

fn render_service_settings(frame: &mut Frame<'_>, area: Rect, app: &TuiState) {
    let Some(settings) = &app.settings else {
        return;
    };
    if settings.tab == SettingsTab::Runtime {
        let provider = format!("{} / {}", app.session.provider_id, app.session.model);
        let lines = vec![
            setting_line("Version", env!("CARGO_PKG_VERSION")),
            setting_line("Update", &app.update_status),
            setting_line("Provider", &provider),
            setting_line("Access", app.policy.label()),
            setting_line("Coomi home", &display_path(&app.home)),
            setting_line("Workspace", &display_path(&app.cwd)),
            Line::from(""),
            Line::from(vec![
                Span::styled("R", theme::key()),
                Span::raw(" check updates   "),
                Span::styled("Tab / Left / Right", theme::key()),
                Span::raw(" switch page   "),
                Span::styled("Esc", theme::key()),
                Span::raw(" close"),
            ]),
        ];
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(Block::default().padding(Padding::new(3, 3, 1, 1))),
            area,
        );
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(6), Constraint::Length(2)])
        .split(area);
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
        .split(rows[0]);
    match settings.tab {
        SettingsTab::Mcp => {
            let mcp_items = super::settings_mcp_items(app);
            let items = mcp_items
                .iter()
                .map(|item| {
                    let status = item.configured.as_ref().and_then(|configured| {
                        settings
                            .mcp_statuses
                            .iter()
                            .find(|status| status.name == configured.name)
                    });
                    let (marker, color) = if let Some(configured) = &item.configured {
                        if !configured.enabled {
                            ("o", theme::MUTED)
                        } else if status.and_then(|status| status.error.as_ref()).is_some() {
                            ("x", theme::ERROR)
                        } else if status.is_some() {
                            ("*", theme::SUCCESS)
                        } else {
                            ("-", theme::ORANGE)
                        }
                    } else {
                        ("+", theme::BLUE)
                    };
                    let label = item
                        .entry
                        .as_ref()
                        .map(|entry| entry.name.as_str())
                        .unwrap_or_else(|| item.id());
                    ListItem::new(Line::from(vec![
                        Span::styled(format!("{marker} "), Style::default().fg(color)),
                        Span::raw(label.to_owned()),
                    ]))
                })
                .collect::<Vec<_>>();
            render_settings_list(frame, columns[0], " Curated MCP ", items, settings.selected);
            if let Some(item) = mcp_items.get(settings.selected) {
                let status = item.configured.as_ref().and_then(|configured| {
                    settings
                        .mcp_statuses
                        .iter()
                        .find(|status| status.name == configured.name)
                });
                let state = if let Some(configured) = &item.configured {
                    status.map_or_else(
                        || {
                            if configured.enabled {
                                "not checked"
                            } else {
                                "disabled"
                            }
                            .to_owned()
                        },
                        |status| {
                            status.error.clone().unwrap_or_else(|| {
                                format!("connected, {} tool(s)", status.tools_count)
                            })
                        },
                    )
                } else {
                    "available to install".to_owned()
                };
                let name = item
                    .entry
                    .as_ref()
                    .map(|entry| entry.name.as_str())
                    .unwrap_or_else(|| item.id());
                let transport = item
                    .configured
                    .as_ref()
                    .map(|configured| configured.transport.as_str())
                    .or_else(|| item.entry.as_ref().map(|entry| entry.transport.as_str()))
                    .unwrap_or("-");
                let target = item
                    .configured
                    .as_ref()
                    .map(|configured| configured.target.as_str())
                    .or_else(|| item.entry.as_ref().map(|entry| entry.command.as_str()))
                    .unwrap_or("-");
                let installed = item.configured.as_ref().map_or("no", |_| "yes");
                let description = item
                    .entry
                    .as_ref()
                    .map(|entry| entry.description.as_str())
                    .unwrap_or("Custom local MCP configuration");
                frame.render_widget(
                    Paragraph::new(vec![
                        setting_line("Name", name),
                        setting_line("Installed", installed),
                        setting_line("Transport", transport),
                        setting_line("Target", target),
                        setting_line("Status", &state),
                        Line::from(""),
                        Line::from(Span::styled(
                            description.to_owned(),
                            Style::default().fg(theme::MUTED),
                        )),
                    ])
                    .wrap(Wrap { trim: false })
                    .block(Block::default().padding(Padding::horizontal(3))),
                    columns[1],
                );
            }
            render_settings_footer(
                frame,
                rows[1],
                "Enter install/configure   Space enable/disable   R test   D remove",
                app.settings_busy,
            );
        }
        SettingsTab::Skills => {
            let skill_items = super::settings_skill_items(app);
            let items = skill_items
                .iter()
                .map(|item| {
                    let (marker, color) = match &item.installed {
                        Some(installed) if installed.enabled => ("* ", theme::SUCCESS),
                        Some(_) => ("o ", theme::MUTED),
                        None => ("+ ", theme::BLUE),
                    };
                    let label = item
                        .entry
                        .as_ref()
                        .map(|entry| entry.name.as_str())
                        .unwrap_or_else(|| item.id());
                    ListItem::new(Line::from(vec![
                        Span::styled(marker, Style::default().fg(color)),
                        Span::raw(label.to_owned()),
                    ]))
                })
                .collect::<Vec<_>>();
            render_settings_list(
                frame,
                columns[0],
                " Curated Skills ",
                items,
                settings.selected,
            );
            if let Some(item) = skill_items.get(settings.selected) {
                let commit = item.installed.as_ref().map_or_else(
                    || "-".to_owned(),
                    |installed| {
                        if installed.commit.is_empty() {
                            "-".into()
                        } else {
                            installed.commit.chars().take(12).collect()
                        }
                    },
                );
                let name = item
                    .entry
                    .as_ref()
                    .map(|entry| entry.name.as_str())
                    .unwrap_or_else(|| item.id());
                let installed = item.installed.as_ref().map_or("no", |_| "yes");
                let source = item
                    .installed
                    .as_ref()
                    .map(|installed| installed.source.as_str())
                    .or_else(|| item.entry.as_ref().map(|entry| entry.repository.as_str()))
                    .unwrap_or("-");
                let path = item
                    .installed
                    .as_ref()
                    .map(|installed| display_path(&installed.path))
                    .unwrap_or_else(|| "-".into());
                let description = item
                    .entry
                    .as_ref()
                    .map(|entry| entry.description.as_str())
                    .unwrap_or("Custom local Skill");
                frame.render_widget(
                    Paragraph::new(vec![
                        setting_line("Name", name),
                        setting_line("Installed", installed),
                        setting_line("Source", source),
                        setting_line("Commit", &commit),
                        setting_line("Path", &path),
                        Line::from(""),
                        Line::from(Span::styled(
                            description.to_owned(),
                            Style::default().fg(theme::MUTED),
                        )),
                    ])
                    .wrap(Wrap { trim: false })
                    .block(Block::default().padding(Padding::horizontal(3))),
                    columns[1],
                );
            }
            render_settings_footer(
                frame,
                rows[1],
                "Enter install   Space enable/disable   U update   D uninstall",
                app.settings_busy,
            );
        }
        SettingsTab::Providers | SettingsTab::Runtime => {}
    }
}

fn render_settings_list(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    items: Vec<ListItem<'static>>,
    selected: usize,
) {
    let mut state = ListState::default()
        .with_selected((!items.is_empty()).then_some(selected.min(items.len().saturating_sub(1))));
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .borders(Borders::RIGHT)
                    .border_style(Style::default().fg(theme::BORDER))
                    .title(Span::styled(title.to_owned(), theme::title())),
            )
            .highlight_style(theme::selected())
            .highlight_symbol(" "),
        area,
        &mut state,
    );
}

fn render_settings_footer(frame: &mut Frame<'_>, area: Rect, controls: &str, busy: bool) {
    let prefix = if busy { "Working...   " } else { "" };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(prefix, Style::default().fg(theme::ORANGE)),
            Span::styled(controls.to_owned(), Style::default().fg(theme::MUTED)),
            Span::raw("   "),
            Span::styled("Tab / Left / Right", theme::key()),
            Span::raw(" switch page"),
        ])),
        area,
    );
}

fn render_provider_form(
    frame: &mut Frame<'_>,
    area: Rect,
    form: &super::ProviderForm,
    error: Option<&str>,
) {
    let title = if form.original_id.is_some() {
        "Edit Provider"
    } else {
        "New Provider"
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(8),
            Constraint::Length(2),
        ])
        .split(area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(title, theme::title()),
            Span::styled(
                "  API keys are stored as plain JSON and masked only in this view",
                Style::default().fg(theme::MUTED),
            ),
        ])),
        rows[0],
    );
    let column_count = if rows[1].width >= 90 { 3usize } else { 2usize };
    let field_columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(
            (0..column_count)
                .map(|_| Constraint::Ratio(1, u32::try_from(column_count).unwrap_or(1)))
                .collect::<Vec<_>>(),
        )
        .split(rows[1]);
    let field_height = 3u16;
    let rows_per_column = usize::from((rows[1].height / field_height).max(1));
    let page_capacity = rows_per_column.saturating_mul(column_count).max(1);
    let page_start = form.selected / page_capacity * page_capacity;
    let page_end = (page_start + page_capacity).min(form.fields.len());
    let mut active_cursor = None;
    for (index, (field, editor)) in form
        .fields
        .iter()
        .enumerate()
        .skip(page_start)
        .take(page_end.saturating_sub(page_start))
    {
        let local_index = index - page_start;
        let column = local_index / rows_per_column;
        let row_index = local_index % rows_per_column;
        let field_area = Rect::new(
            field_columns[column].x,
            field_columns[column].y + u16::try_from(row_index).unwrap_or(0) * field_height,
            field_columns[column].width.saturating_sub(2),
            field_height,
        );
        let color = if index == form.selected {
            theme::BLUE
        } else {
            theme::BORDER
        };
        let (value, cursor_column) = if field.is_choice() && !field.is_custom_input(&editor.text())
        {
            (format!("◀ {} ▶", field.option_label(&editor.text())), None)
        } else {
            let mask = (field.secret && !form.show_secret && !editor.is_empty()).then_some('*');
            let (visible, cursor) = editor.single_line_viewport(field_area.width.max(1), mask);
            (visible, Some(cursor))
        };
        frame.render_widget(
            Paragraph::new(if value.is_empty() {
                Span::styled("-", Style::default().fg(theme::MUTED))
            } else {
                Span::styled(value, Style::default().fg(theme::TEXT))
            })
            .block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(color))
                    .title(Span::styled(
                        format!(" {} ", field.label),
                        Style::default().fg(color),
                    )),
            ),
            field_area,
        );
        if index == form.selected
            && let Some(cursor_column) = cursor_column
            && field_area.width > 0
        {
            active_cursor = Some((
                field_area
                    .x
                    .saturating_add(cursor_column.min(field_area.width.saturating_sub(1))),
                field_area.y.saturating_add(1),
            ));
        }
    }
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Tab / ↑↓", theme::key()),
            Span::raw(" field   "),
            Span::styled("← / →", theme::key()),
            Span::raw(" option   "),
            Span::styled("Enter", theme::key()),
            Span::raw(" next/select   "),
            Span::styled("Ctrl+S", theme::key()),
            Span::raw(" save   "),
            Span::styled("Alt+V", theme::key()),
            Span::raw(" show key   "),
            Span::styled("Esc", theme::key()),
            Span::raw(" cancel   "),
            Span::styled(
                format!("field {}/{}   ", form.selected + 1, form.fields.len()),
                Style::default().fg(theme::MUTED),
            ),
            Span::styled(
                error.unwrap_or_default().to_owned(),
                Style::default().fg(theme::ERROR),
            ),
        ])),
        rows[2],
    );
    if form.choice_open {
        render_provider_choice_menu(frame, area, form);
    } else if let Some(position) = active_cursor {
        frame.set_cursor_position(position);
    }
}

fn render_provider_choice_menu(frame: &mut Frame<'_>, area: Rect, form: &super::ProviderForm) {
    let (field, editor) = &form.fields[form.selected];
    if field.options.is_empty() {
        return;
    }
    let height = u16::try_from(field.options.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let popup = popup_rect(area, 48, height);
    frame.render_widget(Clear, popup);
    let items = field
        .options
        .iter()
        .map(|option| ListItem::new(option.label))
        .collect::<Vec<_>>();
    let selected = field
        .options
        .iter()
        .position(|option| option.value == editor.text())
        .unwrap_or(field.options.len() - 1);
    let mut state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(theme::BLUE))
                    .title(Span::styled(format!(" {} ", field.label), theme::title()))
                    .style(Style::default().bg(theme::SURFACE)),
            )
            .highlight_style(theme::selected())
            .highlight_symbol("> "),
        popup,
        &mut state,
    );
}

fn setting_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<16}"), Style::default().fg(theme::MUTED)),
        Span::styled(value.to_owned(), Style::default().fg(theme::TEXT)),
    ])
}

fn mask_secret(secret: &str) -> String {
    if secret.is_empty() {
        "not set".into()
    } else if secret.chars().count() <= 8 {
        "•".repeat(secret.chars().count())
    } else {
        let start = secret.chars().take(3).collect::<String>();
        let end = secret
            .chars()
            .rev()
            .take(3)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>();
        format!("{start}••••••{end}")
    }
}

fn render_approval(frame: &mut Frame<'_>, area: Rect, app: &TuiState) {
    let Some(approval) = &app.pending_approval else {
        return;
    };
    let popup = popup_rect(area, 76, 15);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(theme::AMBER))
        .title(Span::styled(
            " Approval required ",
            Style::default().fg(theme::AMBER).bold(),
        ))
        .style(Style::default().bg(theme::SURFACE).fg(theme::TEXT))
        .padding(Padding::new(2, 2, 1, 1));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(approval.reason.clone()),
            Line::from(""),
            Line::from(vec![
                Span::styled(format!("{}  ", approval.call.name), theme::title()),
                Span::styled(
                    truncate_cells(&compact_json(&approval.call.arguments), 64),
                    Style::default().fg(theme::MUTED),
                ),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    " Approve once ",
                    if approval.selected == super::ApprovalChoice::Approve {
                        theme::selected()
                    } else {
                        Style::default().fg(theme::MUTED)
                    },
                ),
                Span::raw("   "),
                Span::styled(
                    " Deny ",
                    if approval.selected == super::ApprovalChoice::Deny {
                        theme::selected()
                    } else {
                        Style::default().fg(theme::MUTED)
                    },
                ),
            ]),
            Line::from(vec![
                Span::styled("Left / Right", theme::key()),
                Span::styled(" choose   ", Style::default().fg(theme::MUTED)),
                Span::styled("Enter", theme::key()),
                Span::styled(" confirm   ", Style::default().fg(theme::MUTED)),
                Span::styled("Esc", theme::key()),
                Span::styled(" deny", Style::default().fg(theme::MUTED)),
            ]),
        ])
        .wrap(Wrap { trim: true }),
        inner,
    );
}

fn render_update_prompt(frame: &mut Frame<'_>, area: Rect, app: &TuiState) {
    let Some(update) = &app.pending_update else {
        return;
    };
    let popup = popup_rect(area, 72, 10);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(theme::SUCCESS))
        .title(Span::styled(
            " Update available ",
            Style::default().fg(theme::SUCCESS).bold(),
        ))
        .style(Style::default().bg(theme::SURFACE).fg(theme::TEXT))
        .padding(Padding::new(2, 2, 1, 1));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(format!(
                "A new version of Coomi is available: {} -> {}",
                update.current_version, update.latest_version
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    " Yes, open release page ",
                    if update.selected == super::UpdateChoice::Yes {
                        theme::selected()
                    } else {
                        Style::default().fg(theme::MUTED)
                    },
                ),
                Span::raw("   "),
                Span::styled(
                    " Not now ",
                    if update.selected == super::UpdateChoice::No {
                        theme::selected()
                    } else {
                        Style::default().fg(theme::MUTED)
                    },
                ),
            ]),
            Line::from(vec![
                Span::styled("Up / Down", theme::key()),
                Span::styled(" choose   ", Style::default().fg(theme::MUTED)),
                Span::styled("Enter", theme::key()),
                Span::styled(" confirm   ", Style::default().fg(theme::MUTED)),
                Span::styled("Esc", theme::key()),
                Span::styled(" dismiss", Style::default().fg(theme::MUTED)),
            ]),
        ])
        .wrap(Wrap { trim: true }),
        inner,
    );
}

fn render_user_input(frame: &mut Frame<'_>, area: Rect, app: &TuiState) {
    let Some(pending) = &app.pending_user_input else {
        return;
    };
    let Some(question) = pending.request.questions.get(pending.question_index) else {
        return;
    };
    let popup = popup_rect(area, 82, 14);
    frame.render_widget(Clear, popup);
    let title = format!(
        " Question {}/{} · {} ",
        pending.question_index + 1,
        pending.request.questions.len(),
        question.header
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::BLUE))
        .title(Span::styled(title, theme::title()))
        .style(Style::default().bg(theme::SURFACE).fg(theme::TEXT))
        .padding(Padding::horizontal(1));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(4),
            Constraint::Length(2),
            Constraint::Length(1),
        ])
        .split(inner);
    frame.render_widget(
        Paragraph::new(question.question.clone()).wrap(Wrap { trim: true }),
        rows[0],
    );
    let mut items = question
        .options
        .iter()
        .map(|option| {
            ListItem::new(Line::from(vec![
                Span::styled(format!("{:<20}", option.label), Style::default().bold()),
                Span::styled(
                    option.description.clone(),
                    Style::default().fg(theme::MUTED),
                ),
            ]))
        })
        .collect::<Vec<_>>();
    items.push(ListItem::new(Line::from(vec![
        Span::styled("Other", Style::default().bold()),
        Span::styled("  Enter a custom answer", Style::default().fg(theme::MUTED)),
    ])));
    let mut state = ListState::default().with_selected(Some(
        pending.option_index.min(items.len().saturating_sub(1)),
    ));
    frame.render_stateful_widget(
        List::new(items)
            .highlight_style(theme::selected())
            .highlight_symbol(" "),
        rows[1],
        &mut state,
    );
    let custom = pending.other_editor.as_ref().map_or_else(
        || Line::from(""),
        |editor| {
            Line::from(vec![
                Span::styled("Custom  ", Style::default().fg(theme::ORANGE).bold()),
                Span::styled(editor.text(), Style::default().fg(theme::TEXT)),
            ])
        },
    );
    frame.render_widget(
        Paragraph::new(custom).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(theme::BORDER)),
        ),
        rows[2],
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            "←/↑ previous · →/↓ next · Enter answer · Esc cancel",
            Style::default().fg(theme::MUTED),
        ))
        .alignment(Alignment::Center),
        rows[3],
    );
}

fn render_delete_confirmation(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &TuiState,
    target: &super::DeleteTarget,
) {
    let popup = popup_rect(area, 60, 9);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(theme::ERROR))
        .title(Span::styled(
            " Confirm deletion ",
            Style::default().fg(theme::ERROR).bold(),
        ))
        .style(Style::default().bg(theme::SURFACE).fg(theme::TEXT))
        .padding(Padding::new(2, 2, 1, 1));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let (message, blocked) = match target {
        super::DeleteTarget::Session(id) if *id == app.session.id => {
            ("The active session cannot be deleted.".to_owned(), true)
        }
        super::DeleteTarget::Session(_) => (
            "Delete this saved session? This cannot be undone.".to_owned(),
            false,
        ),
        super::DeleteTarget::Provider(id) => (
            format!("Delete Provider `{id}`? This cannot be undone."),
            false,
        ),
        super::DeleteTarget::Mcp(name) => (
            format!("Remove MCP `{name}` from the configuration?"),
            false,
        ),
        super::DeleteTarget::Skill(name) => (
            format!("Uninstall Skill `{name}` and remove its local files?"),
            false,
        ),
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(message),
            Line::from(""),
            Line::from(if blocked {
                vec![Span::styled("Esc", theme::key()), Span::raw(" close")]
            } else {
                vec![
                    Span::styled("Y / Enter", theme::key()),
                    Span::raw(" delete    "),
                    Span::styled("N / Esc", theme::key()),
                    Span::raw(" keep"),
                ]
            }),
        ]),
        inner,
    );
}

fn render_plan_mode_confirm(frame: &mut Frame<'_>, area: Rect, app: &TuiState) {
    let confirm = app.plan_mode_confirm.as_ref().expect("plan mode confirm");
    let popup = popup_rect(area, 62, 9);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(theme::AMBER))
        .title(Span::styled(
            " Plan mode ",
            Style::default().fg(theme::AMBER).bold(),
        ))
        .style(Style::default().bg(theme::SURFACE).fg(theme::TEXT))
        .padding(Padding::new(2, 2, 1, 1));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let choices = [
        PlanModeChoice::ExitAndSend,
        PlanModeChoice::SendReadOnly,
        PlanModeChoice::Cancel,
    ];
    let mut lines = vec![
        Line::from("You are in plan mode. Exit plan mode to make changes?"),
        Line::from(""),
    ];
    for choice in &choices {
        let selected = *choice == confirm.selected;
        let prefix = if selected { "▸ " } else { "  " };
        let style = if selected {
            Style::default().fg(theme::TEXT).bold()
        } else {
            Style::default().fg(theme::MUTED)
        };
        lines.push(Line::from(Span::styled(
            format!("{prefix}{}", choice.label()),
            style,
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("↑/↓", theme::key()),
        Span::raw(" navigate    "),
        Span::styled("Enter", theme::key()),
        Span::raw(" confirm    "),
        Span::styled("Esc", theme::key()),
        Span::raw(" cancel"),
    ]));
    frame.render_widget(Paragraph::new(lines), inner);
}

fn modal_block(title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::MINT))
        .title(Span::styled(title.to_owned(), theme::title()))
        .style(Style::default().bg(theme::SURFACE).fg(theme::TEXT))
        .padding(Padding::horizontal(1))
}

fn popup_rect(area: Rect, target_width: u16, target_height: u16) -> Rect {
    let width = target_width.min(area.width).max(1);
    let height = target_height.min(area.height).max(1);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height),
        width,
        height,
    )
}

fn compact_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

fn visible_range(length: usize, selected: usize, capacity: usize) -> (usize, usize) {
    if length == 0 || capacity == 0 {
        return (0, 0);
    }
    let capacity = capacity.min(length);
    let start = selected
        .saturating_add(1)
        .saturating_sub(capacity)
        .min(length - capacity);
    (start, start + capacity)
}

fn wrapped_line_count(text: &Text<'_>, width: u16) -> usize {
    let width = usize::from(width.max(1));
    text.lines
        .iter()
        .map(|line| line.width().max(1).div_ceil(width))
        .sum()
}

fn plain_content_lines(content: &str, color: Color) -> Vec<Line<'static>> {
    content
        .lines()
        .map(|line| Line::from(Span::styled(line.to_string(), Style::default().fg(color))))
        .collect()
}

#[cfg(test)]
fn markdown_lines(content: &str) -> Vec<Line<'static>> {
    markdown_lines_with_width(content, 100)
}

fn markdown_lines_with_width(content: &str, width: u16) -> Vec<Line<'static>> {
    let mut renderer = MarkdownRenderer::new(usize::from(width.max(8)));
    for event in Parser::new_ext(content, Options::all()) {
        renderer.consume(event);
    }
    renderer.finish()
}

struct MarkdownRenderer {
    lines: Vec<Line<'static>>,
    spans: Vec<Span<'static>>,
    styles: Vec<Style>,
    lists: Vec<Option<u64>>,
    links: Vec<String>,
    code_block: bool,
    table: Option<MarkdownTable>,
    table_width: usize,
}

#[derive(Default)]
struct MarkdownTable {
    alignments: Vec<MarkdownAlignment>,
    rows: Vec<Vec<Vec<Span<'static>>>>,
    current_row: Vec<Vec<Span<'static>>>,
    current_cell: Option<Vec<Span<'static>>>,
    header_rows: usize,
    in_header: bool,
}

impl Default for MarkdownRenderer {
    fn default() -> Self {
        Self::new(100)
    }
}

impl MarkdownRenderer {
    fn new(table_width: usize) -> Self {
        Self {
            lines: Vec::new(),
            spans: Vec::new(),
            styles: Vec::new(),
            lists: Vec::new(),
            links: Vec::new(),
            code_block: false,
            table: None,
            table_width,
        }
    }

    fn consume(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.push_text(&text),
            Event::Code(code) => self.push_span(Span::styled(
                code.into_string(),
                self.style().fg(theme::ORANGE).bg(theme::SURFACE),
            )),
            Event::SoftBreak => self.push_span(Span::raw(" ")),
            Event::HardBreak if self.in_table_cell() => self.push_span(Span::raw(" ")),
            Event::HardBreak => self.flush_line(),
            Event::Rule => {
                self.flush_line();
                self.lines.push(Line::from(Span::styled(
                    "─".repeat(32),
                    Style::default().fg(theme::BORDER),
                )));
            }
            Event::TaskListMarker(checked) => self.push_span(Span::styled(
                if checked { "[x] " } else { "[ ] " },
                Style::default().fg(theme::ORANGE),
            )),
            Event::Html(html) | Event::InlineHtml(html) => self.push_text(&html),
            Event::FootnoteReference(reference) => self.push_span(Span::styled(
                format!("[{}]", reference),
                Style::default().fg(theme::SKY),
            )),
            Event::InlineMath(math) | Event::DisplayMath(math) => self.push_span(Span::styled(
                math.into_string(),
                Style::default().fg(theme::ORANGE),
            )),
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { .. } => self.styles.push(
                Style::default()
                    .fg(theme::BLUE)
                    .add_modifier(Modifier::BOLD),
            ),
            Tag::Emphasis => self
                .styles
                .push(Style::default().add_modifier(Modifier::ITALIC)),
            Tag::Strong => self
                .styles
                .push(Style::default().add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => self
                .styles
                .push(Style::default().add_modifier(Modifier::CROSSED_OUT)),
            Tag::BlockQuote(_) => {
                self.push_span(Span::styled("│ ", Style::default().fg(theme::ORANGE)))
            }
            Tag::CodeBlock(kind) => {
                self.flush_line();
                self.code_block = true;
                if let CodeBlockKind::Fenced(language) = kind
                    && !language.is_empty()
                {
                    self.lines.push(Line::from(Span::styled(
                        format!("  {}", language),
                        Style::default().fg(theme::SKY).bg(theme::SURFACE),
                    )));
                }
            }
            Tag::List(start) => self.lists.push(start),
            Tag::Item => {
                let prefix = match self.lists.last_mut() {
                    Some(Some(number)) => {
                        let prefix = format!("{number}. ");
                        *number = number.saturating_add(1);
                        prefix
                    }
                    _ => "• ".into(),
                };
                self.push_span(Span::styled(prefix, Style::default().fg(theme::ORANGE)));
            }
            Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                self.links.push(dest_url.into_string());
                self.styles.push(
                    Style::default()
                        .fg(theme::SKY)
                        .add_modifier(Modifier::UNDERLINED),
                );
            }
            Tag::Table(alignments) => {
                self.flush_line();
                self.table = Some(MarkdownTable {
                    alignments,
                    ..MarkdownTable::default()
                });
            }
            Tag::TableHead => {
                if let Some(table) = &mut self.table {
                    table.in_header = true;
                    table.current_row.clear();
                }
            }
            Tag::TableRow => {
                if let Some(table) = &mut self.table {
                    table.current_row.clear();
                }
            }
            Tag::TableCell => {
                if let Some(table) = &mut self.table {
                    table.current_cell = Some(Vec::new());
                }
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_) => {
                self.flush_line();
                self.styles.pop();
            }
            TagEnd::Paragraph | TagEnd::Item | TagEnd::BlockQuote(_) => self.flush_line(),
            TagEnd::CodeBlock => {
                self.flush_line();
                self.code_block = false;
            }
            TagEnd::List(_) => {
                self.lists.pop();
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.styles.pop();
            }
            TagEnd::Link | TagEnd::Image => {
                self.styles.pop();
                if let Some(url) = self.links.pop()
                    && !url.is_empty()
                {
                    self.push_span(Span::styled(
                        format!(" ({url})"),
                        Style::default().fg(theme::MUTED),
                    ));
                }
            }
            TagEnd::TableCell => {
                if let Some(table) = &mut self.table
                    && let Some(cell) = table.current_cell.take()
                {
                    table.current_row.push(cell);
                }
            }
            TagEnd::TableRow => {
                if let Some(table) = &mut self.table
                    && !table.current_row.is_empty()
                {
                    table.rows.push(std::mem::take(&mut table.current_row));
                    if table.in_header {
                        table.header_rows = table.rows.len();
                    }
                }
            }
            TagEnd::TableHead => {
                if let Some(table) = &mut self.table {
                    if !table.current_row.is_empty() {
                        table.rows.push(std::mem::take(&mut table.current_row));
                    }
                    table.in_header = false;
                    table.header_rows = table.rows.len();
                }
            }
            TagEnd::Table => self.flush_table(),
            _ => {}
        }
    }

    fn push_text(&mut self, text: &str) {
        if self.code_block {
            for (index, line) in text.split('\n').enumerate() {
                if index > 0 {
                    self.flush_line();
                }
                if !line.is_empty() {
                    self.push_span(Span::styled(
                        format!("  {line}"),
                        Style::default().fg(theme::TEXT).bg(theme::SURFACE),
                    ));
                }
            }
        } else {
            self.push_span(Span::styled(text.to_owned(), self.style()));
        }
    }

    fn in_table_cell(&self) -> bool {
        self.table
            .as_ref()
            .is_some_and(|table| table.current_cell.is_some())
    }

    fn push_span(&mut self, span: Span<'static>) {
        if let Some(cell) = self
            .table
            .as_mut()
            .and_then(|table| table.current_cell.as_mut())
        {
            cell.push(span);
        } else {
            self.spans.push(span);
        }
    }

    fn style(&self) -> Style {
        self.styles
            .iter()
            .fold(Style::default().fg(theme::TEXT), |style, next| {
                style.patch(*next)
            })
    }

    fn flush_line(&mut self) {
        if self.in_table_cell() {
            return;
        }
        if !self.spans.is_empty() {
            self.lines.push(Line::from(std::mem::take(&mut self.spans)));
        }
    }

    fn flush_table(&mut self) {
        let Some(table) = self.table.take() else {
            return;
        };
        if table.rows.is_empty() {
            return;
        }
        let column_count = table.rows.iter().map(Vec::len).max().unwrap_or(0);
        if column_count == 0 {
            return;
        }
        let mut widths = (0..column_count)
            .map(|column| {
                table
                    .rows
                    .iter()
                    .filter_map(|row| row.get(column))
                    .map(|cell| spans_width(cell))
                    .max()
                    .unwrap_or(1)
                    .max(1)
            })
            .collect::<Vec<_>>();
        let chrome_width = column_count.saturating_mul(3).saturating_add(1);
        let available = self
            .table_width
            .saturating_sub(chrome_width)
            .max(column_count);
        let minimum = if available >= column_count.saturating_mul(3) {
            3
        } else {
            1
        };
        while widths.iter().sum::<usize>() > available {
            let Some((index, _)) = widths
                .iter()
                .enumerate()
                .filter(|(_, width)| **width > minimum)
                .max_by_key(|(_, width)| **width)
            else {
                break;
            };
            widths[index] -= 1;
        }

        self.lines.push(table_border(&widths, '┌', '┬', '┐'));
        for (row_index, row) in table.rows.iter().enumerate() {
            self.lines.push(table_row(
                row,
                &widths,
                &table.alignments,
                row_index < table.header_rows,
            ));
            if row_index + 1 == table.header_rows && row_index + 1 < table.rows.len() {
                self.lines.push(table_border(&widths, '├', '┼', '┤'));
            }
        }
        self.lines.push(table_border(&widths, '└', '┴', '┘'));
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush_line();
        if self.lines.is_empty() {
            self.lines.push(Line::from(""));
        }
        self.lines
    }
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|span| span.content.width()).sum()
}

fn table_border(widths: &[usize], left: char, middle: char, right: char) -> Line<'static> {
    let mut border = String::new();
    border.push(left);
    for (index, width) in widths.iter().enumerate() {
        border.push_str(&"─".repeat(width.saturating_add(2)));
        border.push(if index + 1 == widths.len() {
            right
        } else {
            middle
        });
    }
    Line::from(Span::styled(border, Style::default().fg(theme::BORDER)))
}

fn table_row(
    row: &[Vec<Span<'static>>],
    widths: &[usize],
    alignments: &[MarkdownAlignment],
    header: bool,
) -> Line<'static> {
    let mut output = vec![Span::styled("│", Style::default().fg(theme::BORDER))];
    for (column, width) in widths.iter().copied().enumerate() {
        let mut cell =
            truncate_table_spans(row.get(column).map(Vec::as_slice).unwrap_or(&[]), width);
        if header {
            for span in &mut cell {
                span.style = span.style.add_modifier(Modifier::BOLD).fg(theme::BLUE);
            }
        }
        let content_width = spans_width(&cell);
        let remaining = width.saturating_sub(content_width);
        let alignment = alignments
            .get(column)
            .copied()
            .unwrap_or(MarkdownAlignment::None);
        let (left_padding, right_padding) = match alignment {
            MarkdownAlignment::Right => (remaining, 0),
            MarkdownAlignment::Center => (remaining / 2, remaining - remaining / 2),
            MarkdownAlignment::None | MarkdownAlignment::Left => (0, remaining),
        };
        output.push(Span::raw(format!(" {}", " ".repeat(left_padding))));
        output.extend(cell);
        output.push(Span::raw(format!("{} ", " ".repeat(right_padding))));
        output.push(Span::styled("│", Style::default().fg(theme::BORDER)));
    }
    Line::from(output)
}

fn truncate_table_spans(spans: &[Span<'static>], max_width: usize) -> Vec<Span<'static>> {
    if spans_width(spans) <= max_width {
        return spans.to_vec();
    }
    let content_limit = max_width.saturating_sub(1);
    let mut width = 0usize;
    let mut output = Vec::new();
    for span in spans {
        let mut content = String::new();
        for character in span.content.chars() {
            let character_width = character.width().unwrap_or(0);
            if width.saturating_add(character_width) > content_limit {
                break;
            }
            content.push(character);
            width = width.saturating_add(character_width);
        }
        if !content.is_empty() {
            output.push(Span::styled(content, span.style));
        }
        if width >= content_limit {
            break;
        }
    }
    output.push(Span::styled(
        "…",
        spans.last().map(|span| span.style).unwrap_or_default(),
    ));
    output
}

fn command_slash(action: super::CommandAction) -> &'static str {
    match action {
        super::CommandAction::NewSession => "/new",
        super::CommandAction::History => "/history",
        super::CommandAction::Models => "/model",
        super::CommandAction::Status => "/status",
        super::CommandAction::Compact => "/compact",
        super::CommandAction::Catalog => "/catalog",
        super::CommandAction::Mcp => "/mcp",
        super::CommandAction::Skills => "/skills",
        super::CommandAction::Memory => "/memory",
        super::CommandAction::Plan => "/plan",
        super::CommandAction::ExitPlan => "/exit_plan",
        super::CommandAction::Loop => "/loop",
        super::CommandAction::Settings => "/settings",
        super::CommandAction::ClearTimeline => "/clear",
        super::CommandAction::Help => "/help",
        super::CommandAction::Quit => "/quit",
    }
}

fn big_logo_lines() -> Vec<Line<'static>> {
    [
        " ███   ███   ███  █   █  ███ ",
        "█     █   █ █   █ ██ ██   █  ",
        "█     █   █ █   █ █ █ █   █  ",
        "█     █   █ █   █ █   █   █  ",
        " ███   ███   ███  █   █  ███ ",
    ]
    .into_iter()
    .map(|line| {
        Line::from(Span::styled(
            line,
            Style::default()
                .fg(theme::BLUE)
                .add_modifier(Modifier::BOLD),
        ))
    })
    .collect()
}

fn mascot_text() -> Text<'static> {
    const ROWS: &[&str] = &[
        "....aaaa....",
        "...dddgdd...",
        ".adddgggdd..",
        ".addddgddda.",
        "adddddddddda",
        "addxddddxdda",
        "adzzddddzzda",
        ".adddddddda.",
        "..d.d..d.d..",
        "..d.d..d.d..",
        ".d..d..d..d.",
        "...d....d...",
        "...d....d...",
    ];
    let color = |key: char| match key {
        'a' => Some(Color::Rgb(45, 80, 171)),
        'd' => Some(Color::Rgb(47, 92, 188)),
        'g' => Some(Color::Rgb(121, 202, 210)),
        'x' => Some(Color::Rgb(15, 20, 22)),
        'z' => Some(theme::CORAL),
        _ => None,
    };
    let mut lines = Vec::new();
    for row in (0..ROWS.len()).step_by(2) {
        let top = ROWS[row].chars().collect::<Vec<_>>();
        let bottom = ROWS
            .get(row + 1)
            .map(|line| line.chars().collect::<Vec<_>>())
            .unwrap_or_default();
        let spans = (0..12)
            .map(|column| {
                let top_color = top.get(column).and_then(|key| color(*key));
                let bottom_color = bottom.get(column).and_then(|key| color(*key));
                match (top_color, bottom_color) {
                    (Some(fg), Some(bg)) => Span::styled("▀", Style::default().fg(fg).bg(bg)),
                    (Some(fg), None) => Span::styled("▀", Style::default().fg(fg)),
                    (None, Some(fg)) => Span::styled("▄", Style::default().fg(fg)),
                    (None, None) => Span::raw(" "),
                }
            })
            .collect::<Vec<_>>();
        lines.push(Line::from(spans));
    }
    Text::from(lines)
}

fn catalog_tab(label: &'static str, active: bool) -> Span<'static> {
    if active {
        Span::styled(label, theme::selected())
    } else {
        Span::styled(label, Style::default().fg(theme::MUTED))
    }
}

fn help_line(key: &'static str, description: &'static str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{key:<16}"), theme::key()),
        Span::styled(description, Style::default().fg(theme::TEXT)),
    ])
}

fn spinner(tick: usize) -> char {
    SPINNER[(tick / 2) % SPINNER.len()]
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".into())
}

fn single_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_cells(value: &str, max_width: usize) -> String {
    if value.width() <= max_width {
        return value.to_string();
    }
    if max_width <= 1 {
        return "…".chars().take(max_width).collect();
    }
    let mut output = String::new();
    for character in value.chars() {
        if output.width() + character.to_string().width() > max_width - 1 {
            break;
        }
        output.push(character);
    }
    output.push('…');
    output
}

fn display_path(path: &Path) -> String {
    let value = path.to_string_lossy();
    if let Some(rest) = value.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{rest}")
    } else {
        value.strip_prefix("\\\\?\\").unwrap_or(&value).to_owned()
    }
}

fn compact_path(path: &str, max_width: usize) -> String {
    if path.width() <= max_width {
        return path.to_string();
    }
    let tail_width = max_width.saturating_sub(2);
    let mut reversed = String::new();
    for character in path.chars().rev() {
        let next = format!("{character}{reversed}");
        if next.width() > tail_width {
            break;
        }
        reversed = next;
    }
    format!("…{reversed}")
}

fn session_label(session: &coomi_engine::SessionSummary) -> &str {
    if session.title.trim().is_empty() {
        &session.preview
    } else {
        &session.title
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cli;
    use crate::RuntimePaths;
    use coomi_engine::Session;
    use coomi_security::AccessMode;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::fs;

    fn rendered_rows(state: &TuiState, width: u16, height: u16) -> Vec<String> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal.draw(|frame| draw(frame, state)).expect("draw");
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    fn test_state() -> (tempfile::TempDir, TuiState) {
        let home = tempfile::tempdir().expect("temporary home");
        let config = home.path().join("config");
        fs::create_dir(&config).expect("create config");
        fs::write(
            config.join("providers.json"),
            r#"{"active":"demo","providers":{"demo":{"type":"generic","display":"Demo","api_key":"","base_url":"http://localhost/v1","model":"demo-model"}}}"#,
        )
        .expect("write providers");
        let cwd = home.path().canonicalize().expect("canonical home");
        let paths = RuntimePaths {
            home: cwd.clone(),
            cwd: cwd.clone(),
        };
        let cli = Cli {
            home: None,
            cwd: cwd.clone(),
            model: None,
            policy: AccessMode::WorkspaceWrite,
            yes: false,
            command: None,
        };
        let session = Session::new("demo", "demo-model", cwd);
        let state = TuiState::new(&cli, &paths, session).expect("TUI state");
        (home, state)
    }

    #[test]
    fn welcome_renders_at_desktop_and_narrow_sizes() {
        let (_home, state) = test_state();
        for (width, height) in [(110, 34), (52, 20)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).expect("test terminal");
            terminal.draw(|frame| draw(frame, &state)).expect("draw");
            let buffer = terminal.backend().buffer();
            let rendered = buffer
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(rendered.contains("COOMI"));
            assert!(rendered.contains("Message"));
        }
    }

    #[test]
    fn timeline_and_history_overlay_render_without_overlap_panics() {
        let (_home, mut state) = test_state();
        state
            .timeline
            .push(TimelineEntry::User("请检查这个工作区并给出清晰结论".into()));
        state.timeline.push(TimelineEntry::Assistant(
            "## Result\n- The interface remains readable.\n```rust\nfn main() {}\n```".into(),
        ));
        state.open_overlay(OverlayKind::History);
        let backend = TestBackend::new(48, 18);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal.draw(|frame| draw(frame, &state)).expect("draw");
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Session history"));
    }

    #[test]
    fn approval_and_questions_render_above_the_composer() {
        let (_home, mut state) = test_state();
        let (approval_tx, _approval_rx) = tokio::sync::oneshot::channel();
        state.pending_approval = Some(super::super::PendingApproval {
            call: coomi_engine::ToolCall {
                id: "call-1".into(),
                name: "local_shell".into(),
                arguments: serde_json::json!({"command": "cargo test"}),
            },
            reason: "Run the test suite?".into(),
            selected: super::super::ApprovalChoice::Approve,
            responder: Some(approval_tx),
        });
        let rows = rendered_rows(&state, 100, 32);
        let approval_y = rows
            .iter()
            .position(|row| row.contains("Approval required"))
            .expect("approval row");
        let composer_y = rows
            .iter()
            .position(|row| row.contains("Message"))
            .expect("composer row");
        assert!(approval_y < composer_y);

        state.pending_approval = None;
        let (question_tx, _question_rx) = tokio::sync::oneshot::channel();
        state.pending_user_input = Some(super::super::PendingUserInput {
            request: coomi_engine::UserInputRequest {
                questions: vec![coomi_engine::UserInputQuestion {
                    id: "scope".into(),
                    header: "Scope".into(),
                    question: "Which scope should be used?".into(),
                    options: vec![
                        coomi_engine::UserInputOption {
                            label: "Workspace".into(),
                            description: "Current project".into(),
                        },
                        coomi_engine::UserInputOption {
                            label: "Global".into(),
                            description: "Every project".into(),
                        },
                    ],
                }],
                auto_resolution_ms: None,
            },
            question_index: 0,
            option_index: 0,
            other_editor: None,
            answers: Default::default(),
            responder: Some(question_tx),
        });
        let rows = rendered_rows(&state, 100, 32);
        let question_y = rows
            .iter()
            .position(|row| row.contains("Question 1/1"))
            .expect("question row");
        let composer_y = rows
            .iter()
            .position(|row| row.contains("Message"))
            .expect("composer row");
        assert!(question_y < composer_y);
    }

    #[test]
    fn markdown_renderer_styles_headings_and_inline_code() {
        let lines = markdown_lines("## Result\nUse `cargo test`.");
        assert_eq!(lines[0].spans[0].style.fg, Some(theme::BLUE));
        assert!(
            lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert!(lines.iter().flat_map(|line| &line.spans).any(|span| {
            span.content.contains("cargo test") && span.style.fg == Some(theme::ORANGE)
        }));
    }

    #[test]
    fn markdown_tables_align_unicode_cells_within_terminal_width() {
        let lines = markdown_lines_with_width(
            "| 名称 | State | Count |\n|:---|:---:|---:|\n| MCP 工具 | Ready | 12 |\n| Skill | 运行中 | 3 |",
            52,
        );
        let widths = lines.iter().map(Line::width).collect::<Vec<_>>();
        assert!(widths.len() >= 6);
        assert!(widths.iter().all(|width| *width == widths[0]));
        assert!(widths[0] <= 52);
        let rendered = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(rendered.contains("MCP 工具"));
        assert!(rendered.contains("运行中"));
        assert!(rendered.contains('┼'));
    }

    #[test]
    fn header_and_footer_use_semantic_colors_without_ready_status() {
        let (_home, mut state) = test_state();
        state.context_status.used_percent = 82;
        state.context_status.used_tokens = 82_000;
        state.context_status.effective_context_window = 100_000;
        let backend = TestBackend::new(110, 34);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal.draw(|frame| draw(frame, &state)).expect("draw");
        let buffer = terminal.backend().buffer();

        let logo_x = (0..105)
            .find(|x| {
                ["C", "O", "O", "M", "I"]
                    .iter()
                    .enumerate()
                    .all(|(offset, symbol)| buffer[(*x + offset as u16, 0)].symbol() == *symbol)
            })
            .expect("COOMI logo");
        for offset in 0..5 {
            assert_eq!(buffer[(logo_x + offset, 0)].fg, theme::BLUE);
        }

        let footer = (0..110)
            .map(|x| buffer[(x, 33)].symbol())
            .collect::<String>();
        assert!(footer.contains("workspace-write"));
        assert!(footer.contains("ctx 82% used"));
        assert!(!footer.contains("Ready"));
        let policy_x = footer.find("workspace-write").expect("policy") as u16;
        let context_x = footer.find("ctx 82% used").expect("context") as u16;
        assert_eq!(buffer[(policy_x, 33)].fg, theme::BLUE);
        assert_eq!(buffer[(context_x, 33)].fg, theme::ORANGE);
    }

    #[test]
    fn provider_form_pages_advanced_fields_on_narrow_terminals() {
        let (_home, mut state) = test_state();
        state.open_overlay(OverlayKind::Settings);
        super::super::begin_provider_form(&mut state, Some("demo".into()));
        {
            let form = state
                .settings
                .as_mut()
                .and_then(|settings| settings.form.as_mut())
                .expect("provider form");
            form.selected = form.fields.len() - 1;
        }
        let rendered = rendered_rows(&state, 52, 20).join("\n");
        assert!(rendered.contains("Edit Provider"));
        assert!(rendered.contains("Parallel tools"));
    }

    #[test]
    fn provider_form_cursor_is_on_the_selected_value_row() {
        let (_home, mut state) = test_state();
        state.open_overlay(OverlayKind::Settings);
        super::super::begin_provider_form(&mut state, Some("demo".into()));
        state
            .settings
            .as_mut()
            .and_then(|settings| settings.form.as_mut())
            .expect("provider form")
            .selected = 5;

        let backend = TestBackend::new(110, 34);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal.draw(|frame| draw(frame, &state)).expect("draw");
        let cursor = terminal.get_cursor_position().expect("cursor position");
        let buffer = terminal.backend().buffer();
        let value_row = (0..34)
            .find(|y| {
                (0..110)
                    .map(|x| buffer[(x, *y)].symbol())
                    .collect::<String>()
                    .contains("demo-model")
            })
            .expect("model value row");
        assert_eq!(cursor.y, value_row);
    }

    #[test]
    fn provider_form_renders_editable_values_and_curated_settings() {
        let (_home, mut state) = test_state();
        state.open_overlay(OverlayKind::Settings);
        super::super::begin_provider_form(&mut state, Some("demo".into()));
        let rendered = rendered_rows(&state, 110, 34).join("\n");
        assert!(rendered.contains("http://localhost/v1"));
        assert!(rendered.contains("demo-model"));
        assert!(rendered.contains("◀ OpenAI Compatible ▶"));

        state.settings.as_mut().expect("settings").form = None;
        state.settings.as_mut().expect("settings").tab = SettingsTab::Mcp;
        let rendered = rendered_rows(&state, 110, 34).join("\n");
        for name in [
            "Filesystem",
            "Git",
            "Fetch",
            "Memory",
            "Playwright",
            "GitHub",
        ] {
            assert!(rendered.contains(name), "missing MCP catalog item {name}");
        }

        state.settings.as_mut().expect("settings").tab = SettingsTab::Skills;
        let rendered = rendered_rows(&state, 110, 34).join("\n");
        for name in [
            "Frontend Design",
            "Web App Testing",
            "Code Review Excellence",
            "Security Review",
            "React Best Practices",
            "API Design Principles",
            "Git Advanced Workflows",
            "Documentation Writer",
        ] {
            assert!(rendered.contains(name), "missing Skill catalog item {name}");
        }
    }
}
