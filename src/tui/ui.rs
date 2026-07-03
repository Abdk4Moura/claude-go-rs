use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use ratatui::Frame;

use super::app::{App, Screen, StatusKind};
use super::style;
use crate::settings::PathKind;

pub fn render(f: &mut Frame, app: &App) {
    let area = f.area();
    f.render_widget(Clear, area);
    let bg = Block::default().style(ratatui::style::Style::default().bg(style::PAPER));
    f.render_widget(bg, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Min(10),  // body
            Constraint::Length(3), // help
        ])
        .split(area);

    render_header(f, app, chunks[0]);
    render_body(f, app, chunks[1]);
    render_help(f, app, chunks[2]);
}

fn render_header(f: &mut Frame, app: &App, area: Rect) {
    let title = match app.screen {
        Screen::Provider => "claude-go  /  provider",
        Screen::Model => "claude-go  /  model",
        Screen::Status => "claude-go  /  status",
    };
    let crumbs = match app.screen {
        Screen::Provider => "1  .  2  .  3",
        Screen::Model => "1  >  2  .  3",
        Screen::Status => "1  .  2  >  3",
    };
    let header_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(30), Constraint::Min(10)])
        .split(area);
    let title_paragraph = Paragraph::new(Line::from(Span::styled(
        title,
        style::title_style(),
    )))
    .block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(style::body_dim_style()),
    );
    f.render_widget(title_paragraph, header_chunks[0]);
    let crumbs_paragraph = Paragraph::new(Line::from(Span::styled(
        format!("  {crumbs}"),
        style::body_dim_style(),
    )))
    .block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(style::body_dim_style()),
    )
    .right_aligned();
    f.render_widget(crumbs_paragraph, header_chunks[1]);
}

fn render_help(f: &mut Frame, app: &App, area: Rect) {
    let hint = if app.input_active {
        format!("{}  (Enter to submit, Esc to cancel)", app.input_prompt)
    } else {
        match app.screen {
            Screen::Provider => {
                "j/k or arrows  move     Enter  select     a  add custom     d  remove custom     Tab  next screen     q  quit".to_string()
            }
            Screen::Model => "j/k or arrows  move     Enter  apply     Esc  back     Tab  next screen     q  quit".to_string(),
            Screen::Status => "o  toggle on/off     v  verify     r  refresh     Esc  back     q  quit".to_string(),
        }
    };
    let widget = Paragraph::new(Line::from(Span::styled(hint, style::help_style())))
        .block(Block::default().borders(Borders::TOP).border_style(style::body_dim_style()));
    f.render_widget(widget, area);
}

fn render_body(f: &mut Frame, app: &App, area: Rect) {
    match app.screen {
        Screen::Provider => render_provider(f, app, area),
        Screen::Model => render_model(f, app, area),
        Screen::Status => render_status(f, app, area),
    }
}

fn render_provider(f: &mut Frame, app: &App, area: Rect) {
    let mut items: Vec<ListItem> = Vec::new();
    for (i, p) in app.providers.iter().enumerate() {
        let selected = i == app.provider_index;
        let marker = if selected { ">>" } else { "  " };
        let format_tag = match p.format {
            crate::provider::ProviderFormat::Anthropic => "[anthropic]",
            crate::provider::ProviderFormat::OpenAI => "[openai  ]",
        };
        let badge = if p.id == crate::provider::CUSTOM_URL_ID {
            " "
        } else if p.implemented {
            " "
        } else {
            "!"
        };
        let is_current = app.settings.enabled
            && (p.base_url == app.settings.base_url
                || (p.format == crate::provider::ProviderFormat::OpenAI
                    && app.settings.path_kind == PathKind::OpenAI));
        let current_marker = if is_current { "*" } else { " " };

        let line = Line::from(vec![
            Span::styled(format!(" {marker} {badge}{current_marker} "), base_line_style(selected)),
            Span::styled(format!("{:<22}", p.display_name), base_line_style(selected).add_modifier(ratatui::style::Modifier::BOLD)),
            Span::styled("  ", base_line_style(selected)),
            Span::styled(format_tag, base_dim_style(selected)),
            Span::styled("  ", base_line_style(selected)),
            Span::styled(
                if p.base_url.is_empty() { "...".to_string() } else { p.base_url.clone() },
                base_dim_style(selected),
            ),
        ]);
        items.push(ListItem::new(line));
    }
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::NONE)
                .style(ratatui::style::Style::default().bg(style::PAPER)),
        )
        .highlight_style(style::selected_style());
    f.render_stateful_widget(list, area, &mut ratatui::widgets::ListState::default().with_selected(Some(app.provider_index)));

    // Overlay status / input.
    if app.input_active {
        render_input_overlay(f, app, area);
    } else if let Some(msg) = &app.status_message {
        render_status_overlay(f, app, area, msg);
    }
}

fn render_model(f: &mut Frame, app: &App, area: Rect) {
    let provider_name = app
        .selected_provider()
        .map(|p| p.display_name.clone())
        .unwrap_or_default();
    let subtitle = if app.models.is_empty() && matches!(
        app.selected_provider().map(|p| &p.model_source),
        Some(crate::provider::ModelSource::Any)
    ) {
        "   (any model id accepted; press Enter to type one)"
    } else if app.models_from_live {
        "   (live, may be partial)"
    } else {
        "   (fallback list)"
    };
    let title = Line::from(vec![
        Span::styled("  Models for ", style::body_dim_style()),
        Span::styled(provider_name, style::title_style()),
        Span::styled(subtitle, style::body_dim_style()),
    ]);
    let mut items: Vec<ListItem> = Vec::new();
    if app.models_loading {
        items.push(ListItem::new(Line::from(Span::styled(
            "  loading model list...",
            style::body_dim_style(),
        ))));
    }
    for (i, m) in app.models.iter().enumerate() {
        let selected = i == app.model_index;
        let marker = if selected { ">>" } else { "  " };
        let is_current = app.settings.enabled && app.settings.model == m.id;
        let current_marker = if is_current { "*" } else { " " };
        items.push(ListItem::new(Line::from(vec![
            Span::styled(format!(" {marker} {current_marker} "), base_line_style(selected)),
            Span::styled(format!("{:<22}", m.id), base_line_style(selected).add_modifier(ratatui::style::Modifier::BOLD)),
            Span::styled(m.description.clone(), base_dim_style(selected)),
        ])));
    }
    if items.is_empty() && !app.models_loading {
        let msg = if matches!(
            app.selected_provider().map(|p| &p.model_source),
            Some(crate::provider::ModelSource::Any)
        ) {
            "  (any model id accepted -- press Enter to type one)"
        } else {
            "  no models available"
        };
        items.push(ListItem::new(Line::from(Span::styled(
            msg,
            style::body_dim_style(),
        ))));
    }
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(style::body_dim_style())
        .title(title);
    let list = List::new(items)
        .block(block)
        .style(ratatui::style::Style::default().bg(style::PAPER))
        .highlight_style(style::selected_style());
    f.render_stateful_widget(list, area, &mut ratatui::widgets::ListState::default().with_selected(Some(app.model_index.min(app.models.len().saturating_sub(1)))));

    if app.input_active {
        render_input_overlay(f, app, area);
    } else if let Some(msg) = &app.status_message {
        render_status_overlay(f, app, area, msg);
    }
}

fn render_status(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(40), Constraint::Length(20)])
        .split(area);

    // Big ON / OFF indicator on the right.
    let indicator = if app.settings.enabled {
        ("ON", style::on_indicator_style())
    } else {
        ("OFF", style::off_indicator_style())
    };
    let indicator_block = Block::default()
        .borders(Borders::ALL)
        .border_style(ratatui::style::Style::default().fg(indicator.1.fg.unwrap_or(style::INK)).bg(style::PAPER))
        .style(ratatui::style::Style::default().bg(style::PAPER));
    let indicator_paragraph = Paragraph::new(Line::from(Span::styled(
        format!(" {} ", indicator.0),
        indicator.1.add_modifier(ratatui::style::Modifier::BOLD),
    )))
    .alignment(ratatui::layout::Alignment::Center)
    .block(indicator_block);
    f.render_widget(indicator_paragraph, chunks[1]);

    // Details on the left.
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        "  Settings",
        style::title_style(),
    )));
    let s = &app.settings;
    lines.push(Line::from(vec![
        Span::styled("    provider       ", style::body_dim_style()),
        Span::styled(
            if s.base_url.is_empty() {
                "(none)"
            } else {
                s.base_url.as_str()
            },
            style::body_style(),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("    base url       ", style::body_dim_style()),
        Span::styled(&s.base_url, style::body_style()),
    ]));
    lines.push(Line::from(vec![
        Span::styled("    model          ", style::body_dim_style()),
        Span::styled(&s.model, style::body_style()),
    ]));
    lines.push(Line::from(vec![
        Span::styled("    path kind      ", style::body_dim_style()),
        Span::styled(
            match s.path_kind {
                PathKind::Anthropic => "anthropic",
                PathKind::OpenAI => "openai (via proxy)",
                PathKind::Other => "(none)",
            },
            style::body_style(),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("    auth source    ", style::body_dim_style()),
        Span::styled(
            if std::env::var("OPENCODE_API_KEY").is_ok() {
                "OPENCODE_API_KEY env"
            } else if s.key_in_settings {
                "ANTHROPIC_AUTH_TOKEN (settings.json)"
            } else {
                "(not set)"
            },
            style::body_style(),
        ),
    ]));

    lines.push(Line::from(Span::styled("  ", style::body_style())));
    lines.push(Line::from(Span::styled(
        "  Proxy",
        style::title_style(),
    )));
    match app.proxy_port {
        Some(port) => {
            lines.push(Line::from(vec![
                Span::styled("    state          ", style::body_dim_style()),
                Span::styled("running (in-process)", style::on_indicator_style()),
            ]));
            lines.push(Line::from(vec![
                Span::styled("    port           ", style::body_dim_style()),
                Span::styled(port.to_string(), style::body_style()),
            ]));
        }
        None => {
            lines.push(Line::from(vec![
                Span::styled("    state          ", style::body_dim_style()),
                Span::styled("not running", style::off_indicator_style()),
            ]));
        }
    }

    lines.push(Line::from(Span::styled("  ", style::body_style())));
    lines.push(Line::from(Span::styled(
        "  Files",
        style::title_style(),
    )));
    lines.push(Line::from(vec![
        Span::styled("    settings       ", style::body_dim_style()),
        Span::styled(app.paths.settings_file.display().to_string(), style::body_style()),
    ]));
    lines.push(Line::from(vec![
        Span::styled("    state dir      ", style::body_dim_style()),
        Span::styled(app.paths.state_dir.display().to_string(), style::body_style()),
    ]));

    if let Some(v) = &app.verify_result {
        lines.push(Line::from(Span::styled("  ", style::body_style())));
        lines.push(Line::from(Span::styled(
            "  Last verify",
            style::title_style(),
        )));
        lines.push(Line::from(vec![
            Span::styled("    outcome        ", style::body_dim_style()),
            Span::styled(v.outcome.message(), if v.outcome.is_ok() { style::on_indicator_style() } else { style::error_style() }),
        ]));
        lines.push(Line::from(vec![
            Span::styled("    http code      ", style::body_dim_style()),
            Span::styled(v.http_code.to_string(), style::body_style()),
        ]));
    }

    let block = Block::default()
        .borders(Borders::NONE)
        .style(ratatui::style::Style::default().bg(style::PAPER));
    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, chunks[0]);

    if app.input_active {
        render_input_overlay(f, app, area);
    } else if let Some(msg) = &app.status_message {
        render_status_overlay(f, app, area, msg);
    }
}

fn render_input_overlay(f: &mut Frame, app: &App, area: Rect) {
    let w = 60.min(area.width.saturating_sub(4));
    let h = 3;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let rect = Rect::new(x, y, w, h);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(ratatui::style::Style::default().fg(style::EMBER).bg(style::PAPER))
        .title(Span::styled(" custom url ", style::title_style()))
        .style(ratatui::style::Style::default().bg(style::PAPER));
    let text = format!("{}{}_", app.input_prompt, app.input_buffer);
    let paragraph = Paragraph::new(Span::styled(text, style::body_style())).block(block);
    f.render_widget(paragraph, rect);
}

fn render_status_overlay(f: &mut Frame, app: &App, area: Rect, msg: &str) {
    let w = msg.len() as u16 + 6;
    let w = w.min(area.width.saturating_sub(4)).max(20);
    let h = 3;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + area.height.saturating_sub(h + 2);
    let rect = Rect::new(x, y, w, h);
    f.render_widget(Clear, rect);
    let style = match app.status_kind {
        StatusKind::Info => style::body_style(),
        StatusKind::Warn => style::error_style(),
        StatusKind::Error => style::error_style(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(style)
        .style(ratatui::style::Style::default().bg(style::PAPER));
    let paragraph = Paragraph::new(Span::styled(format!("  {msg}  "), style)).block(block);
    f.render_widget(paragraph, rect);
}

fn base_line_style(selected: bool) -> ratatui::style::Style {
    if selected {
        style::selected_style()
    } else {
        style::body_style()
    }
}

fn base_dim_style(selected: bool) -> ratatui::style::Style {
    if selected {
        style::selected_dim_style()
    } else {
        style::body_dim_style()
    }
}
