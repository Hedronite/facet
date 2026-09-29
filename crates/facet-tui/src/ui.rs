//! TUI rendering: title, collection tree, request editor, response
//! pane, and footer status line. All colors come from `Theme`/`Palette`
//! so a single switch on `Appearance` restyles every pane.
//!
//! Layout follows the desktop analog: sidebar on the left; work area on
//! the right splits **vertically** (request over response). Chrome is
//! quiet — pane borders are stone. Accent fill is reserved for the title
//! chip, the tree selection pill, and Send.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::app::{
    App, BodyKind, Focus, Mode, RequestFocus, ResponseTab, ResponseView, RunStatus, Section,
};
use crate::theme::{PaletteToken, Styles, resolve};
use crate::tree::Row;

const SIDEBAR_WIDTH: Constraint = Constraint::Length(36);

/// Top-level entry: paint the whole frame for `app`.
pub fn draw(frame: &mut Frame, app: &App) {
    let styles = Styles::for_theme(app.theme());
    let area = frame.area();
    frame.render_widget(Block::default().style(styles.base), area);

    let shell = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    render_title(frame, shell[0], app, styles);
    render_body(frame, shell[1], app, styles);
    render_footer(frame, shell[2], app, styles);

    if app.env_dropdown_open() {
        render_env_dropdown(frame, shell[1], app, styles);
    }
    if matches!(app.status(), RunStatus::Running) {
        render_running_overlay(frame, shell[1], styles);
    }
    if app.help_open() {
        render_help_overlay(frame, shell[1], styles);
    }
    if app.history_grid().is_some() {
        render_history_grid(frame, shell[1], app, styles);
    }
    if app.env_overlay().is_some() {
        render_env_overlay(frame, shell[1], app, styles);
    }
    if let Some((title, lines)) = app.data_overlay() {
        render_data_overlay(frame, shell[1], title, lines, styles);
    }
    if app.open_picker().is_some() {
        render_open_picker(frame, shell[1], app, styles);
    }
    if let Some(path) = app.open_confirm() {
        render_open_confirm(frame, shell[1], path, styles);
    }
}

fn render_title(frame: &mut Frame, area: Rect, app: &App, styles: Styles) {
    let dirty = if app.editor_dirty() { " •" } else { "" };
    let path = app.collection_path_short();
    let full_title = match (app.collection_name(), path.as_deref()) {
        (Some(name), Some(path)) => format!(" facet · {name}{dirty} · {path} "),
        (Some(name), None) => format!(" facet · {name}{dirty} "),
        (None, Some(path)) => format!(" facet · {path}{dirty} "),
        (None, None) => format!(" facet{dirty} "),
    };
    let name_title = match app.collection_name() {
        Some(name) => format!(" facet · {name}{dirty} "),
        None => format!(" facet{dirty} "),
    };
    let appearance = app
        .custom_theme_name()
        .map(str::to_string)
        .unwrap_or_else(|| app.theme().appearance().label().to_string());
    let label_width = appearance.width() + 1;
    let title_width = (area.width as usize).saturating_sub(label_width);
    let title = if full_title.width() <= title_width {
        full_title
    } else {
        name_title
    };
    let title = cell(&title, title_width);
    let line = Line::from(vec![
        Span::styled(title, styles.title),
        Span::styled(format!(" {appearance}"), styles.muted),
    ]);
    frame.render_widget(Paragraph::new(line).style(styles.base), area);
}

fn render_body(frame: &mut Frame, area: Rect, app: &App, styles: Styles) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([SIDEBAR_WIDTH, Constraint::Min(20)])
        .split(area);

    render_sidebar(frame, columns[0], app, styles);
    render_work_area(frame, columns[1], app, styles);
}

fn render_sidebar(frame: &mut Frame, area: Rect, app: &App, styles: Styles) {
    let focused = app.focus() == Focus::Tree;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(if focused {
            styles.title_focused
        } else {
            styles.border
        })
        .style(styles.sidebar);
    let inner = block.inner(area);
    app.viewports.tree.set(inner.height);
    frame.render_widget(block, area);
    render_pane_title(frame, area, " Collection ", focused, styles);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(inner);
    render_search(frame, rows[0], app, styles);
    render_tree(frame, rows[1], app, styles);
}

fn render_search(frame: &mut Frame, area: Rect, app: &App, styles: Styles) {
    if area.width == 0 {
        return;
    }
    let query = app.search();
    let line = if app.searching() {
        Line::from(vec![
            Span::styled(" /", styles.accent_text),
            Span::styled(query.to_string(), styles.sidebar),
            Span::styled("▌", styles.accent_text),
        ])
    } else if query.is_empty() {
        Line::from(Span::styled(" Search requests…", styles.placeholder))
    } else {
        Line::from(vec![
            Span::styled(" /", styles.muted),
            Span::styled(query.to_string(), styles.sidebar),
        ])
    };
    frame.render_widget(Paragraph::new(line).style(styles.sidebar), area);
}

fn render_work_area(frame: &mut Frame, area: Rect, app: &App, styles: Styles) {
    let focused = matches!(app.focus(), Focus::Request | Focus::Response);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(if focused {
            styles.title_focused
        } else {
            styles.border
        })
        .style(styles.editor);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if !app.has_collection() {
        render_pane_title(frame, area, " facet ", focused, styles);
        render_splash(frame, inner, styles);
        return;
    }
    render_pane_title(frame, area, " Request / Response ", focused, styles);

    let request_height = if inner.height >= 22 { 12 } else { 8 };
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(request_height), Constraint::Min(5)])
        .split(inner);

    render_request_pane(frame, split[0], app, styles);
    render_response_pane(frame, split[1], app, styles);
}

/// Release-train codename shown in the footer's first segment.
pub const CODENAME: &str = "G38";
/// Subtitle under the wordmark on the splash.
pub const TAGLINE: &str = "local-first api client";

/// Seven-node lattice glyph from the design alpha: a hexagon of six nodes
/// around a center, spokes to every vertex. Nodes take the bright brand
/// color, lines the accent.
const LATTICE_GLYPH: [&str; 11] = [
    "     ●     ",
    "   ╱ │ ╲   ",
    " ●   │   ● ",
    " │╲  │  ╱│ ",
    " │ ╲ │ ╱ │ ",
    " │   ●   │ ",
    " │ ╱ │ ╲ │ ",
    " │╱  │  ╲│ ",
    " ●   │   ● ",
    "   ╲ │ ╱   ",
    "     ●     ",
];

/// Box-drawing wordmark, three rows, honey.
const WORDMARK: [&str; 3] = [
    "┌─  ┌─┐ ┌─  ┌─┐ ─┬─",
    "├─  ├─┤ │   ├─┘  │ ",
    "│   ┴ ┴ └─  └─┘  ┴ ",
];

/// The lattice glyph inside an outer hexagon outline, 14×23. The hexagon
/// is drawn with ASCII `/`, `\`, `-` so the renderer can style it apart
/// from the glyph: edges become `╱ ╲ ─` in `brand_fill` (the one filled
/// brand frame). Glyph chars keep their node/line styles. Concentric
/// with the glyph: every glyph row clears the outline by at least one
/// cell.
const SPLASH_HEXAGON: [&str; 14] = [
    "         /---\\         ",
    "        /  ●  \\        ",
    "       / ╱ │ ╲ \\       ",
    "      /●   │   ●\\      ",
    "     / │╲  │  ╱│ \\     ",
    "    /  │ ╲ │ ╱ │  \\    ",
    "   /   │   ●   │   \\   ",
    "   \\   │ ╱ │ ╲ │   /   ",
    "    \\  │╱  │  ╲│  /    ",
    "     \\ ●   │   ● /     ",
    "      \\  ╲ │ ╱  /      ",
    "       \\   ●   /       ",
    "        \\     /        ",
    "         \\---/         ",
];

/// Splash for `facet tui` without a collection: lattice glyph, wordmark,
/// tagline, and how to open something. Tall and wide enough panes get the
/// glyph inside the outer hexagon outline. Degrades by height: hexagon
/// first, then the bare glyph, then the wordmark, then a single line.
fn render_splash(frame: &mut Frame, area: Rect, styles: Styles) {
    if area.width < 24 || area.height < 3 {
        let line = Line::from(Span::styled("facet · local-first api client", styles.brand));
        frame.render_widget(
            Paragraph::new(line)
                .alignment(Alignment::Center)
                .style(styles.editor),
            area,
        );
        return;
    }
    let wordmark_height = WORDMARK.len() as u16 + 5;
    let show_hexagon = area.height >= SPLASH_HEXAGON.len() as u16 + wordmark_height
        && area.width >= SPLASH_HEXAGON[0].chars().count() as u16 + 2;
    let show_glyph = area.height >= LATTICE_GLYPH.len() as u16 + wordmark_height;
    let mut lines: Vec<Line> = Vec::new();
    if show_hexagon {
        for row in SPLASH_HEXAGON {
            lines.push(Line::from(splash_art_spans(row, styles)));
        }
        lines.push(Line::raw(""));
    } else if show_glyph {
        for row in LATTICE_GLYPH {
            lines.push(Line::from(splash_art_spans(row, styles)));
        }
        lines.push(Line::raw(""));
    }
    for row in WORDMARK {
        lines.push(Line::from(Span::styled(row, styles.brand)));
    }
    lines.push(Line::from(Span::styled(TAGLINE, styles.muted)));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "open a collection · facet tui <path>",
        styles.placeholder,
    )));
    let height = (lines.len() as u16).min(area.height);
    let target = centered(area, area.width, height);
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .style(styles.editor),
        target,
    );
}

/// Styles one row of splash art. ASCII `/ \ -` are stand-ins for the
/// hexagon outline (rendered as `╱ ╲ ─` in the filled brand band); `●`
/// is a lattice node; any other non-space char is a lattice edge.
fn splash_art_spans(row: &str, styles: Styles) -> Vec<Span<'static>> {
    row.chars()
        .map(|ch| match ch {
            '●' => Span::styled(ch.to_string(), styles.brand_bright),
            '/' => Span::styled("╱", styles.brand_fill),
            '\\' => Span::styled("╲", styles.brand_fill),
            '-' => Span::styled("─", styles.brand_fill),
            ' ' => Span::raw(" "),
            _ => Span::styled(ch.to_string(), styles.brand_line),
        })
        .collect()
}

fn render_pane_title(frame: &mut Frame, area: Rect, label: &str, focused: bool, styles: Styles) {
    let marker = if focused { "▸ " } else { "  " };
    let style = if focused {
        styles.title_focused
    } else {
        styles.muted
    };
    let paragraph = Paragraph::new(Line::from(vec![
        Span::raw(marker),
        Span::styled(label, style),
    ]));
    let title_area = Rect {
        x: area.x + 2,
        y: area.y,
        width: area.width.saturating_sub(2),
        height: 1,
    };
    frame.render_widget(paragraph, title_area);
}

fn render_tree(frame: &mut Frame, area: Rect, app: &App, styles: Styles) {
    if area.height == 0 {
        return;
    }
    let rows = app.rows();
    if rows.is_empty() {
        let empty = if app.has_search() {
            "  no matching requests"
        } else {
            "  Create or open a collection…"
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(empty, styles.muted))).style(styles.sidebar),
            area,
        );
        return;
    }
    let items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| row_item(*row, index, app.selection(), app, styles))
        .collect();
    let list = List::new(items)
        .style(styles.sidebar)
        .highlight_style(styles.selected_row)
        .highlight_symbol("  ");
    let mut state = ratatui::widgets::ListState::default();
    state.select(Some(app.selection()));
    frame.render_stateful_widget(list, area, &mut state);
}

fn row_item(row: Row, index: usize, selection: usize, app: &App, styles: Styles) -> ListItem<'_> {
    let selected = index == selection;
    let (depth, prefix, method_label, name) = match row {
        Row::Folder {
            depth,
            key,
            request_count,
        } => {
            let name = app
                .folder_name(key)
                .unwrap_or_else(|| "<folder>".to_string());
            let collapsed = app.tree_is_collapsed(key);
            let prefix = if collapsed { "▸ " } else { "▾ " };
            (
                depth,
                prefix,
                String::new(),
                format!("{name} ({request_count})"),
            )
        }
        Row::Request { depth, key } => {
            let (name, method) = app
                .request_name_and_method(key)
                .unwrap_or_else(|| ("<request>".to_string(), "GET".to_string()));
            (depth, "  ", method_label(&method), name)
        }
    };
    let indent = "  ".repeat(depth as usize);
    let method_cell = if method_label.is_empty() {
        "    ".to_string()
    } else {
        format!("{:<4}", method_label)
    };
    let line = if selected {
        // Filled honey pill: method cell inverts with the row — desktop
        // suppresses domain color on the selected tree row.
        Line::from(vec![
            Span::styled(indent, styles.selected_row),
            Span::styled(prefix, styles.selected_row),
            Span::styled(method_cell, styles.selected_row),
            Span::styled(" ", styles.selected_row),
            Span::styled(name, styles.selected_row),
        ])
    } else {
        let method_style = if method_label.is_empty() {
            styles.sidebar
        } else {
            styles
                .method
                .fg(resolve(app.theme(), method_token(&method_label)))
        };
        Line::from(vec![
            Span::styled(indent, styles.sidebar),
            Span::styled(prefix, styles.sidebar),
            Span::styled(method_cell, method_style),
            Span::styled(" ", styles.sidebar),
            Span::styled(name, styles.sidebar),
        ])
    };
    ListItem::new(line)
}

fn method_label(method: &str) -> String {
    let upper = method.to_ascii_uppercase();
    match upper.as_str() {
        "DELETE" => "DEL".to_string(),
        "PATCH" => "PAT".to_string(),
        "OPTIONS" => "OPT".to_string(),
        "CONNECT" => "CON".to_string(),
        "HEAD" => "HEAD".to_string(),
        other => {
            if other.len() <= 4 {
                other.to_string()
            } else {
                "HTTP".to_string()
            }
        }
    }
}

fn method_token(label: &str) -> PaletteToken {
    match label {
        "GET" => PaletteToken::MethodGet,
        "POST" => PaletteToken::MethodPost,
        "PUT" => PaletteToken::MethodPut,
        "PAT" => PaletteToken::MethodPatch,
        "DEL" => PaletteToken::MethodDelete,
        _ => PaletteToken::MethodOther,
    }
}

fn render_request_pane(frame: &mut Frame, area: Rect, app: &App, styles: Styles) {
    if area.height == 0 {
        return;
    }
    app.viewports.request.set(area.height);
    let show_kinds = app.section() == Section::Body && area.height >= 6;
    let mut constraints = vec![
        Constraint::Length(1), // breadcrumb
        Constraint::Length(1), // url + send
        Constraint::Length(1), // section tabs + env
    ];
    if show_kinds {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(1));
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    render_breadcrumb(frame, rows[0], app, styles);
    render_url_bar(frame, rows[1], app, styles);
    render_section_tabs(frame, rows[2], app, styles);
    let editor_index = if show_kinds {
        render_body_kinds(frame, rows[3], app, styles);
        4
    } else {
        3
    };
    render_section_editor(frame, rows[editor_index], app, styles);
}

fn render_breadcrumb(frame: &mut Frame, area: Rect, app: &App, styles: Styles) {
    let text = match app.selected_request() {
        Some((key, _)) => {
            let ancestors = app.ancestor_chain(key);
            if ancestors.is_empty() {
                " Collection".to_string()
            } else {
                format!(" Collection › {ancestors}")
            }
        }
        None => " no request selected".to_string(),
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(text, styles.muted))).style(styles.editor),
        area,
    );
}

fn render_url_bar(frame: &mut Frame, area: Rect, app: &App, styles: Styles) {
    let send_width = 8;
    let split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(8),
            Constraint::Length(send_width.min(area.width)),
        ])
        .split(area);

    let method = app.method();
    let method_color = app.theme().palette().method(method);
    let url_focused = app.focus() == Focus::Request && app.request_focus() == RequestFocus::Url;
    let insert = app.mode() == Mode::Insert && url_focused;
    let mut url = app.editor().url.clone();
    if insert {
        url.push('▌');
    }
    let url_style = if url_focused {
        styles
            .url
            .add_modifier(ratatui::style::Modifier::UNDERLINED)
    } else {
        styles.url
    };
    let url_line = Line::from(vec![
        Span::styled(format!(" {:<4} ", method), styles.method.fg(method_color)),
        Span::styled("▾ ", styles.muted),
        Span::styled(url, url_style),
    ]);
    frame.render_widget(Paragraph::new(url_line).style(styles.editor), split[0]);

    let send_label = match app.status() {
        RunStatus::Running => " Cancel ",
        _ => "  Send  ",
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(send_label, styles.title))),
        split[1],
    );
}

fn render_section_tabs(frame: &mut Frame, area: Rect, app: &App, styles: Styles) {
    let env_label = match app.active_environment_name() {
        Some(name) => format!(" {name} ▾ "),
        None => " No env ▾ ".to_string(),
    };
    let env_width = env_label.chars().count() as u16;
    let split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(10),
            Constraint::Length(env_width.min(area.width)),
        ])
        .split(area);

    let editor = app.editor();
    let mut spans = vec![Span::raw(" ")];
    for section in Section::ALL {
        let count = match section {
            Section::Path => editor.path_rows.len(),
            Section::Query => editor.query_rows.len(),
            Section::Headers => editor.header_rows.len(),
            Section::Body => {
                if editor.body_kind == BodyKind::None && editor.body_text.is_empty() {
                    0
                } else {
                    1
                }
            }
            Section::Auth => usize::from(editor.auth_label != "No authentication"),
        };
        let label = if matches!(section, Section::Body | Section::Auth) && count == 0 {
            format!(" {} ", section.label())
        } else {
            format!(" {} {count} ", section.label())
        };
        let selected = app.section() == section;
        let style = if selected {
            styles.stone_pill
        } else {
            styles.muted
        };
        spans.push(Span::styled(label, style));
        spans.push(Span::raw(" "));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(styles.editor),
        split[0],
    );

    let env_style = if app.env_dropdown_open() {
        styles.accent_text
    } else {
        styles.muted
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(env_label, env_style))).style(styles.editor),
        split[1],
    );
}

fn render_body_kinds(frame: &mut Frame, area: Rect, app: &App, styles: Styles) {
    let mut spans = vec![Span::raw(" ")];
    for kind in BodyKind::ALL {
        let label = format!(" {} ", kind.label());
        let selected = app.editor().body_kind == kind;
        let style = if selected {
            styles
                .accent_text
                .add_modifier(ratatui::style::Modifier::UNDERLINED)
        } else {
            styles.muted
        };
        spans.push(Span::styled(label, style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(styles.editor), area);
}

fn render_section_editor(frame: &mut Frame, area: Rect, app: &App, styles: Styles) {
    if area.height == 0 {
        return;
    }
    let editor_focused =
        app.focus() == Focus::Request && app.request_focus() == RequestFocus::Editor;
    let insert = app.mode() == Mode::Insert && editor_focused;
    let lines = match app.section() {
        Section::Path => kv_lines(
            &app.editor().path_rows,
            app.kv_index(),
            app.kv_on_value(),
            editor_focused,
            insert,
            styles,
        ),
        Section::Query => kv_lines(
            &app.editor().query_rows,
            app.kv_index(),
            app.kv_on_value(),
            editor_focused,
            insert,
            styles,
        ),
        Section::Headers => kv_lines(
            &app.editor().header_rows,
            app.kv_index(),
            app.kv_on_value(),
            editor_focused,
            insert,
            styles,
        ),
        Section::Body => body_lines(app, insert, styles),
        Section::Auth => vec![Line::from(Span::styled(
            format!("  {}", app.editor().auth_label),
            styles.muted,
        ))],
    };
    frame.render_widget(
        Paragraph::new(lines)
            .style(styles.editor)
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn kv_lines(
    rows: &[(String, String)],
    index: usize,
    on_value: bool,
    focused: bool,
    insert: bool,
    styles: Styles,
) -> Vec<Line<'static>> {
    if rows.is_empty() {
        return vec![Line::from(Span::styled(
            "  empty · n add row · i edit",
            styles.placeholder,
        ))];
    }
    rows.iter()
        .enumerate()
        .map(|(row_index, (name, value))| {
            let active = focused && row_index == index;
            let mut name = name.clone();
            let mut value = value.clone();
            if active && insert {
                if on_value {
                    value.push('▌');
                } else {
                    name.push('▌');
                }
            }
            let name_style = if active && !on_value {
                styles.stone_pill
            } else {
                styles.muted
            };
            let value_style = if active && on_value {
                styles.stone_pill
            } else {
                styles.editor
            };
            Line::from(vec![
                Span::styled("  ", styles.editor),
                Span::styled(format!("{name:<16}"), name_style),
                Span::styled("  ", styles.editor),
                Span::styled(value, value_style),
            ])
        })
        .collect()
}

fn body_lines(app: &App, insert: bool, styles: Styles) -> Vec<Line<'static>> {
    match app.editor().body_kind {
        BodyKind::Form | BodyKind::Multipart | BodyKind::File => {
            vec![Line::from(Span::styled(
                "  this body kind is not editable in the TUI yet",
                styles.placeholder,
            ))]
        }
        BodyKind::None if app.editor().body_text.is_empty() => {
            vec![Line::from(Span::styled(
                "  no body · b cycle kind · i edit",
                styles.placeholder,
            ))]
        }
        _ => {
            let mut text = app.editor().body_text.clone();
            if insert {
                text.push('▌');
            }
            if text.is_empty() {
                vec![Line::from(Span::styled("  ", styles.editor))]
            } else {
                text.lines()
                    .map(|line| Line::from(Span::styled(format!("  {line}"), styles.editor)))
                    .collect()
            }
        }
    }
}

fn render_response_pane(frame: &mut Frame, area: Rect, app: &App, styles: Styles) {
    if area.height == 0 {
        return;
    }
    app.viewports.response.set(area.height.saturating_sub(2));
    let focused = app.focus() == Focus::Response;
    let header_style = if focused {
        styles.title_focused
    } else {
        styles.muted
    };
    let lines: Vec<Line> = match app.response() {
        Some(response) => {
            response_lines(response, app, styles, header_style, app.response_origin())
        }
        None => match app.status() {
            RunStatus::Running => vec![Line::from(Span::styled(
                "  Sending… / waiting for the server",
                styles.status_running,
            ))],
            RunStatus::Failed(message) => vec![
                Line::from(Span::styled("  error", styles.status_error)),
                Line::from(Span::styled(format!("  {message}"), styles.muted)),
            ],
            _ => vec![Line::from(Span::styled(
                "  Send a request to see its response.",
                styles.muted,
            ))],
        },
    };
    frame.render_widget(
        Paragraph::new(lines)
            .style(styles.editor)
            .scroll((app.response_scroll(), 0))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn response_lines<'a>(
    response: &'a ResponseView,
    app: &'a App,
    styles: Styles,
    header_style: ratatui::style::Style,
    origin: Option<&'a str>,
) -> Vec<Line<'a>> {
    let palette = app.theme().palette();
    let status_color = palette.response_status(response.status);
    let mut lines = Vec::new();
    // A hydrated run names itself (`run 01K… · replayed view`); a live
    // send keeps the plain label.
    let label = origin.unwrap_or("Response");
    lines.push(Line::from(vec![
        Span::styled(format!("  {label}  "), header_style),
        Span::styled(
            format!("{} {}", response.status, response.reason),
            styles.base.fg(status_color),
        ),
        Span::styled(
            format!("  · {} ms", response.duration.as_millis()),
            styles.muted,
        ),
    ]));
    lines.push(Line::from(Span::styled(
        format!("  {}", response.url),
        styles.url,
    )));

    // Stone-pill tab row, mirroring the request section tabs.
    let mut tabs = vec![Span::raw("  ")];
    for tab in ResponseTab::ALL {
        let style = if app.response_tab() == tab {
            styles.stone_pill
        } else {
            styles.muted
        };
        tabs.push(Span::styled(format!(" {} ", tab.label()), style));
        tabs.push(Span::raw(" "));
    }
    lines.push(Line::from(tabs));
    lines.push(Line::from(""));

    match app.response_tab() {
        ResponseTab::Pretty => push_body_lines(&mut lines, &response.pretty_body(), styles),
        ResponseTab::Raw => push_body_lines(&mut lines, &response.body, styles),
        ResponseTab::Headers => {
            for (name, value) in &response.headers {
                lines.push(Line::from(vec![
                    Span::styled(format!("    {name}: "), styles.muted),
                    Span::styled(value.clone(), styles.editor),
                ]));
            }
        }
        ResponseTab::Inspect => {
            let content_type = response.content_type().unwrap_or("—");
            let rows = [
                ("status", format!("{} {}", response.status, response.reason)),
                ("url", response.url.clone()),
                ("duration", format!("{} ms", response.duration.as_millis())),
                ("body", format!("{} bytes", response.body_len)),
                ("content-type", content_type.to_string()),
                ("headers", format!("{}", response.headers.len())),
            ];
            for (name, value) in rows {
                lines.push(Line::from(vec![
                    Span::styled(format!("    {name}: "), styles.muted),
                    Span::styled(value, styles.editor),
                ]));
            }
        }
    }
    lines
}

fn push_body_lines<'a>(lines: &mut Vec<Line<'a>>, body: &str, styles: Styles) {
    for line in body.lines() {
        lines.push(Line::from(Span::styled(
            format!("    {line}"),
            styles.editor,
        )));
    }
}

/// Footer from the design alpha: `[ G38 │ lattice ready │ hints │ graphite honey ]`.
/// Brackets in honey, separators in stone. Segments drop from the middle
/// outward when the terminal is narrow.
fn render_footer(frame: &mut Frame, area: Rect, app: &App, styles: Styles) {
    let status_style = match app.status() {
        RunStatus::Idle => styles.status_idle,
        RunStatus::Running => styles.status_running,
        RunStatus::Done { status, .. } => {
            let color = app.theme().palette().response_status(*status);
            styles.base.fg(color)
        }
        RunStatus::Failed(_) => styles.status_error,
    };
    let status_label = match app.status() {
        RunStatus::Idle if app.lattice_ready() => "lattice ready".to_string(),
        RunStatus::Idle => "lattice idle".to_string(),
        RunStatus::Done { status, duration } => {
            let lattice = match app.last_recording() {
                Some(summary) if summary.run_id.is_some() => " · recorded",
                Some(_) => " · unrecorded",
                None => "",
            };
            format!("{} · {} ms{lattice}", status, duration.as_millis())
        }
        other => other.label().to_string(),
    };
    let mode = app.mode();
    let mode_style = match mode {
        Mode::Normal => styles.muted,
        Mode::Insert | Mode::Command => styles.accent_text,
    };
    let hint = if mode == Mode::Command {
        format!(":{}", app.command_line())
    } else if mode == Mode::Insert {
        "insert · Esc normal · Enter save".to_string()
    } else if app.help_open() {
        "help · Esc close".to_string()
    } else if app.history_grid().is_some() {
        "history · Enter hydrate · y yank · / filter · Esc close".to_string()
    } else if app.open_confirm().is_some() {
        "unsaved changes · w write+open · y discard · n cancel".to_string()
    } else if app.open_picker().is_some() {
        "open · j/k select · Enter mount · / filter · r refresh · Esc close".to_string()
    } else if app.env_overlay().is_some() {
        "env · a add · e set value · d delete · Esc close".to_string()
    } else if app.data_overlay().is_some() {
        "overlay · Esc close".to_string()
    } else if app.searching() {
        "search · Enter apply · Esc clear".to_string()
    } else if app.env_dropdown_open() {
        "env · j/k select · Enter close".to_string()
    } else {
        "j/k · Enter send · i insert · : command · ? help · q quit".to_string()
    };
    let appearance = app
        .custom_theme_name()
        .map(str::to_string)
        .unwrap_or_else(|| app.theme().appearance().label().to_lowercase());

    let mut segments: Vec<(String, Style)> = vec![
        (CODENAME.to_string(), styles.muted),
        (mode.indicator().to_string(), mode_style),
        (status_label, status_style),
        (hint, styles.muted),
        (appearance, styles.muted),
    ];
    let width = |segments: &[(String, Style)]| -> usize {
        // "[ " + segments joined by " │ " + " ]"
        4 + segments.iter().map(|(text, _)| text.width()).sum::<usize>()
            + 3 * segments.len().saturating_sub(1)
    };
    let available = area.width as usize;
    if width(&segments) > available {
        segments.remove(3);
    }
    if width(&segments) > available {
        segments.remove(0);
    }

    let mut spans = vec![Span::styled("[ ", styles.brand)];
    for (index, (text, style)) in segments.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" │ ", styles.border));
        }
        spans.push(Span::styled(text.clone(), *style));
    }
    let used = width(&segments);
    let pad = available.saturating_sub(used);
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(" ]", styles.brand));
    frame.render_widget(Paragraph::new(Line::from(spans)).style(styles.base), area);
}

fn render_env_dropdown(frame: &mut Frame, area: Rect, app: &App, styles: Styles) {
    let names: Vec<String> = std::iter::once("No environment".to_string())
        .chain(app.environments().iter().map(|entry| {
            let inherit = entry
                .inherits_from
                .as_deref()
                .map(|parent| format!(" ← {parent}"))
                .unwrap_or_default();
            format!("{}{inherit}  ({})", entry.name, entry.variable_count)
        }))
        .collect();
    let width = names
        .iter()
        .map(|name| name.chars().count() as u16)
        .max()
        .unwrap_or(16)
        .saturating_add(4)
        .min(area.width.saturating_sub(2))
        .max(18);
    let height = (names.len() as u16 + 2)
        .min(area.height.saturating_sub(2))
        .max(3);
    let overlay = Rect {
        x: area.x + area.width.saturating_sub(width).saturating_sub(2),
        y: area.y.saturating_add(2),
        width,
        height,
    };
    frame.render_widget(Clear, overlay);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(styles.border)
        .style(styles.editor)
        .title(" env ");
    let inner = block.inner(overlay);
    frame.render_widget(block, overlay);

    let selected = app.active_environment().map(|index| index + 1).unwrap_or(0);
    let items: Vec<ListItem> = names
        .into_iter()
        .enumerate()
        .map(|(index, name)| {
            let active = index == selected;
            let style = if active {
                styles.accent_text
            } else {
                styles.editor
            };
            let prefix = if active { " › " } else { "   " };
            ListItem::new(Line::from(vec![
                Span::styled(prefix, style),
                Span::styled(name, style),
            ]))
        })
        .collect();
    frame.render_widget(List::new(items).style(styles.editor), inner);
}

/// Send-in-flight overlay: the seven-node lattice glyph over a caption.
/// Falls back to the caption-only box when the pane is too small for the
/// glyph.
fn render_running_overlay(frame: &mut Frame, area: Rect, styles: Styles) {
    // Box fits the glyph (11 wide) and the caption (16 cells) with margin.
    let overlay_width = 20u16;
    let overlay_height = LATTICE_GLYPH.len() as u16 + 4;
    if area.width < overlay_width || area.height < overlay_height {
        let overlay = centered(area, 30.min(area.width), 3.min(area.height));
        frame.render_widget(Clear, overlay);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(styles.border);
        let text =
            Paragraph::new(Span::styled("  Sending request…", styles.status_running)).block(block);
        frame.render_widget(text, overlay);
        return;
    }
    let overlay = centered(area, overlay_width, overlay_height);
    frame.render_widget(Clear, overlay);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(styles.border)
        .style(styles.editor);
    let mut lines: Vec<Line> = Vec::new();
    for row in LATTICE_GLYPH {
        lines.push(Line::from(splash_art_spans(row, styles)));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "Sending request…",
        styles.status_running,
    )));
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .block(block),
        overlay,
    );
}

/// `?` help overlay: the whole keymap in one stone box. Keys in accent,
/// actions in editor text. This is option A's discoverability device —
/// one overlay, no which-key.
fn render_help_overlay(frame: &mut Frame, area: Rect, styles: Styles) {
    const KEYS: [(&str, &str); 21] = [
        ("j/k · arrows", "move (tree, rows, response scroll)"),
        ("Ctrl-U / Ctrl-D", "half-page up / down (focused pane)"),
        ("gg / G", "first / last row (tree, request, response)"),
        ("Enter", "folder toggle · open · send · :send"),
        ("i / a", "insert (URL or section) · Esc back"),
        ("/", "search · Enter apply · Esc clear"),
        ("e", "environment dropdown · :env <name>"),
        ("[ ]", "section / response tabs"),
        ("h/l · Space", "collapse folder · name/value"),
        ("m / b", "method · body kind"),
        ("n / d", "add / delete a row"),
        ("Ctrl-W h/j/k/l", "focus pane · Ctrl-W w cycles"),
        ("Ctrl-S", "save to disk · :w"),
        (":", "command line (:w :q :send :theme :history :sql)"),
        (
            ":open [path]",
            "mount a collection without quitting · bare opens the picker",
        ),
        (":recent", "recently seen collections · Enter mounts"),
        (
            ":history grid",
            "j/k · Enter hydrate · y yank id · / filter",
        ),
        (
            ":open picker",
            "j/k · Enter mount · / filter · r refresh · Esc close",
        ),
        (
            ":env overlay",
            "metadata only · a add · e set value · d delete",
        ),
        ("?", "this help · :help"),
        ("q", "quit · Esc cancels run/overlay"),
    ];
    let width = 68u16.min(area.width);
    let height = (KEYS.len() as u16 + 4).min(area.height);
    let overlay = centered(area, width, height);
    frame.render_widget(Clear, overlay);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(styles.border)
        .style(styles.editor)
        .title(Span::styled(" facet keys ", styles.brand));
    let mut lines = Vec::new();
    for (key, action) in KEYS {
        lines.push(Line::from(vec![
            Span::styled(format!(" {key:<14}"), styles.accent_text),
            Span::styled(action, styles.editor),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        " normal · i inserts · : commands · arrows always work",
        styles.muted,
    )));
    frame.render_widget(Paragraph::new(lines).block(block), overlay);
}

/// Fixed-width cell: exact fits pass through, longer text truncates from
/// the right with `…`, shorter text space-pads.
fn cell(text: &str, width: usize) -> String {
    let text_width = UnicodeWidthStr::width(text);
    if text_width == width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    if text_width < width {
        out.push_str(text);
        used = text_width;
    } else {
        for ch in text.chars() {
            let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + ch_width > width.saturating_sub(1) {
                break;
            }
            out.push(ch);
            used += ch_width;
        }
        out.push('…');
        used += 1;
    }
    while used < width {
        out.push(' ');
        used += 1;
    }
    out
}

/// `:history` grid (Goal 2): one row per Lattice run, run id visible.
/// Column order is fixed — `STARTED STATUS MS METHOD REQUEST ACTOR ID` —
/// with REQUEST taking the slack. The focused row's full ULID is on the
/// overlay footer so `y` is discoverable.
#[allow(clippy::too_many_lines)]
fn render_history_grid(frame: &mut Frame, area: Rect, app: &App, styles: Styles) {
    use crate::app::format_started;

    let Some(grid) = app.history_grid() else {
        return;
    };
    let palette = app.theme().palette();
    let visible = grid.visible_indices();

    const STARTED_W: usize = 11; // "MM-DD HH:MM"
    const STATUS_W: usize = 6;
    const MS_W: usize = 6;
    const METHOD_W: usize = 7;
    const ACTOR_W: usize = 12;
    const ID_W: usize = 8;
    const REQUEST_MIN: usize = 12;
    // 1 leading pad + 6 single-space separators + 1 trailing pad.
    const CHROME: usize = 1 + 6 + 1;
    let fixed = STARTED_W + STATUS_W + MS_W + METHOD_W + ACTOR_W + ID_W + CHROME + REQUEST_MIN;
    let width = ((fixed + 12) as u16).clamp(64, 110).min(area.width);
    let sparkline_rows = u16::from(!grid.sparkline.is_empty());
    let height = (visible.len().max(1) as u16 + 4 + sparkline_rows)
        .min(area.height.saturating_sub(2))
        .max(5);
    let overlay = centered(area, width, height);
    frame.render_widget(Clear, overlay);

    let mut title = " history ".to_string();
    if grid.session_only {
        title = " history · this session ".to_string();
    }
    if !grid.filter.is_empty() {
        title = format!("{}· /{} ", title.trim_end(), grid.filter);
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(styles.border)
        .style(styles.editor)
        .title(Span::styled(title, styles.brand));
    let inner = block.inner(overlay);
    frame.render_widget(block, overlay);
    if inner.height < 2 {
        return;
    }

    let request_w = (inner.width as usize).saturating_sub(fixed - REQUEST_MIN);
    let header = Line::from(Span::styled(
        format!(
            " {} {} {} {} {} {} {}",
            cell("STARTED", STARTED_W),
            cell("STATUS", STATUS_W),
            cell("MS", MS_W),
            cell("METHOD", METHOD_W),
            cell("REQUEST", request_w),
            cell("ACTOR", ACTOR_W),
            cell("ID", ID_W),
        ),
        styles.muted,
    ));

    // Sparkline (Goal bells): the last ≤24 statuses of the focused row's
    // selector, oldest → newest. Palette buckets, stone for unrecorded.
    // Paint only — no key, no verb, flat fallback unaffected.
    let sparkline = if grid.sparkline.is_empty() {
        None
    } else {
        let mut spans = vec![Span::raw(" ")];
        for status in &grid.sparkline {
            let color = match status {
                Some(code) => palette.response_status(u16::try_from(*code).unwrap_or(0)),
                None => palette.text_placeholder, // stone
            };
            spans.push(Span::styled("●", styles.base.fg(color)));
        }
        let label = match &grid.sparkline_selector {
            Some(selector) => format!("  last {} · {}", grid.sparkline.len(), selector),
            None => format!("  last {}", grid.sparkline.len()),
        };
        let used = 1 + grid.sparkline.len();
        spans.push(Span::styled(
            cell(&label, (inner.width as usize).saturating_sub(used)),
            styles.muted,
        ));
        Some(Line::from(spans))
    };

    let body_height = inner.height.saturating_sub(2 + sparkline_rows) as usize;
    app.viewports.grid.set(body_height as u16);
    let offset = if grid.selected >= body_height {
        grid.selected + 1 - body_height
    } else {
        0
    };
    let mut lines = vec![header];
    if let Some(sparkline) = sparkline {
        lines.push(sparkline);
    }
    if visible.is_empty() {
        let empty = if grid.filter.is_empty() {
            " (no runs)"
        } else {
            " (no runs match the filter)"
        };
        lines.push(Line::from(Span::styled(empty, styles.placeholder)));
    }
    for (row_index, &run_index) in visible.iter().enumerate().skip(offset).take(body_height) {
        let row = &grid.rows[run_index];
        let selected = row_index == grid.selected;
        let status_text = row
            .status
            .map(|code| code.to_string())
            .unwrap_or_else(|| "ERR".to_string());
        let ms = row
            .duration_ms
            .map(|value| format!("{value:>6}"))
            .unwrap_or_else(|| "     -".to_string());
        let id8: String = row.id.chars().take(ID_W).collect();
        if selected {
            lines.push(Line::from(Span::styled(
                format!(
                    " {} {} {} {} {} {} {}",
                    cell(&format_started(row.started_at), STARTED_W),
                    cell(&status_text, STATUS_W),
                    ms,
                    cell(&row.method, METHOD_W),
                    cell(&row.request_path, request_w),
                    cell(&row.actor, ACTOR_W),
                    cell(&id8, ID_W),
                ),
                styles.selected_row,
            )));
        } else {
            let status_style = match row.status {
                Some(code) => styles
                    .base
                    .fg(palette.response_status(u16::try_from(code).unwrap_or(0))),
                None => styles.status_error,
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {}", cell(&format_started(row.started_at), STARTED_W)),
                    styles.editor,
                ),
                Span::styled(format!(" {}", cell(&status_text, STATUS_W)), status_style),
                Span::styled(format!(" {ms} "), styles.editor),
                Span::styled(cell(&row.method, METHOD_W), styles.editor),
                Span::styled(
                    format!(" {}", cell(&row.request_path, request_w)),
                    styles.editor,
                ),
                Span::styled(format!(" {}", cell(&row.actor, ACTOR_W)), styles.editor),
                Span::styled(format!(" {}", cell(&id8, ID_W)), styles.muted),
            ]));
        }
    }

    // Footer: the `/` input while filtering, else the notice, else the
    // focused run's full ULID so `y` is discoverable.
    let footer = if grid.filtering {
        format!(" /{}▌", grid.filter)
    } else if let Some(notice) = &grid.notice {
        format!(" {notice}")
    } else {
        match grid.selected_row() {
            Some(row) => format!(" {} · Enter hydrate · y yank · Esc close", row.id),
            None => " Esc close".to_string(),
        }
    };
    lines.push(Line::from(Span::styled(
        cell(&footer, inner.width as usize),
        styles.muted,
    )));

    let drawn_height = (lines.len() as u16).min(inner.height);
    frame.render_widget(
        Paragraph::new(lines).style(styles.editor),
        Rect {
            height: drawn_height,
            ..inner
        },
    );
}

/// The `:env` overlay (Facet rest 5): environment metadata from the
/// machine store. Values are never rendered — the set form masks secret
/// input, and the list shows environment / name / secret / updated only.
fn render_env_overlay(frame: &mut Frame, area: Rect, app: &App, styles: Styles) {
    use crate::app::{EnvFormStep, format_started};

    let Some(overlay) = app.env_overlay() else {
        return;
    };

    const ENV_W: usize = 16;
    const NAME_W: usize = 28;
    const SECRET_W: usize = 6;
    const UPDATED_W: usize = 11; // "MM-DD HH:MM"
    // 1 leading pad + 3 single-space separators + 1 trailing pad.
    const CHROME: usize = 1 + 3 + 1;
    let fixed = ENV_W + NAME_W + SECRET_W + UPDATED_W + CHROME;
    let width = ((fixed + 8) as u16).clamp(56, 96).min(area.width);
    let height = (overlay.rows.len().max(1) as u16 + 4)
        .min(area.height.saturating_sub(2))
        .max(5);
    let rect = centered(area, width, height);
    frame.render_widget(Clear, rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(styles.border)
        .style(styles.editor)
        .title(Span::styled(" env ", styles.brand));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if inner.height < 2 {
        return;
    }

    let header = Line::from(Span::styled(
        format!(
            " {} {} {} {}",
            cell("ENVIRONMENT", ENV_W),
            cell("NAME", NAME_W),
            cell("SECRET", SECRET_W),
            cell("UPDATED", UPDATED_W),
        ),
        styles.muted,
    ));

    let body_height = inner.height.saturating_sub(2) as usize;
    app.viewports.env.set(body_height as u16);
    let offset = if overlay.selected >= body_height {
        overlay.selected + 1 - body_height
    } else {
        0
    };
    let mut lines = vec![header];
    if overlay.rows.is_empty() {
        lines.push(Line::from(Span::styled(
            " (nothing set — `a` adds an entry)",
            styles.placeholder,
        )));
    }
    for (index, row) in overlay
        .rows
        .iter()
        .enumerate()
        .skip(offset)
        .take(body_height)
    {
        let text = format!(
            " {} {} {} {}",
            cell(&row.name, ENV_W),
            cell(&row.key, NAME_W),
            cell(if row.secret { "yes" } else { "no" }, SECRET_W),
            cell(&format_started(row.updated_at), UPDATED_W),
        );
        if index == overlay.selected {
            lines.push(Line::from(Span::styled(text, styles.selected_row)));
        } else {
            lines.push(Line::from(Span::styled(text, styles.editor)));
        }
    }

    // Footer: the set form's field prompt, else the delete confirm, else
    // the notice, else hints. Secret values render as bullets.
    let footer = if let Some(form) = &overlay.form {
        let (label, masked) = match form.step {
            EnvFormStep::Environment => ("environment", false),
            EnvFormStep::Name => ("name", false),
            EnvFormStep::Secret => ("secret? y/N", false),
            EnvFormStep::Value => ("value", form.secret),
        };
        let context = match form.step {
            EnvFormStep::Environment => " set".to_string(),
            EnvFormStep::Name => format!(" set {}", form.environment),
            EnvFormStep::Secret | EnvFormStep::Value => {
                format!(" set {}/{}", form.environment, form.name)
            }
        };
        let input = if masked {
            "•".repeat(form.input.chars().count())
        } else {
            form.input.clone()
        };
        format!("{context} · {label}: {input}▌")
    } else if overlay.confirm_delete {
        match overlay.rows.get(overlay.selected) {
            Some(row) => format!(" delete {}/{}? y/N", row.name, row.key),
            None => " delete? y/N".to_string(),
        }
    } else if let Some(notice) = &overlay.notice {
        format!(" {notice}")
    } else {
        " j/k move · a add · e set value · d delete · Esc close".to_string()
    };
    lines.push(Line::from(Span::styled(
        cell(&footer, inner.width as usize),
        styles.muted,
    )));

    let drawn_height = (lines.len() as u16).min(inner.height);
    frame.render_widget(
        Paragraph::new(lines).style(styles.editor),
        Rect {
            height: drawn_height,
            ..inner
        },
    );
}

fn render_data_overlay(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    lines: &[String],
    styles: Styles,
) {
    let longest = lines
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(20)
        .max(title.chars().count());
    let width = ((longest + 4) as u16).clamp(40, 80).min(area.width);
    let height = (lines.len() as u16 + 2)
        .min(area.height.saturating_sub(2))
        .max(3);
    let overlay = centered(area, width, height);
    frame.render_widget(Clear, overlay);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(styles.border)
        .style(styles.editor)
        .title(Span::styled(title.to_string(), styles.brand));
    let body: Vec<Line> = lines
        .iter()
        .map(|line| Line::from(Span::styled(format!(" {line}"), styles.editor)))
        .collect();
    frame.render_widget(
        Paragraph::new(body).block(block).wrap(Wrap { trim: false }),
        overlay,
    );
}

/// `:open` / `:recent` collection picker (TUI-01). Recents come from the
/// machine store, nearby rows from a one-level scan of the cwd; the origin
/// column says which, so an operator can tell a remembered workspace from a
/// file that merely sits next to them.
fn render_open_picker(frame: &mut Frame, area: Rect, app: &App, styles: Styles) {
    use crate::app::OpenOrigin;

    let Some(picker) = app.open_picker() else {
        return;
    };

    const ORIGIN_W: usize = 6;
    let visible = picker.visible_indices();
    let width = 72u16.min(area.width);
    let height = (visible.len().max(1) as u16 + 4)
        .min(area.height.saturating_sub(2))
        .max(5);
    let rect = centered(area, width, height);
    frame.render_widget(Clear, rect);

    let title = if picker.recents_only {
        " recent collections "
    } else {
        " open collection "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(styles.border)
        .style(styles.editor)
        .title(Span::styled(title, styles.brand));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if inner.height < 2 {
        return;
    }

    let body_height = inner.height.saturating_sub(1) as usize;
    let offset = if picker.selected >= body_height {
        picker.selected + 1 - body_height
    } else {
        0
    };

    let mut lines = Vec::new();
    if visible.is_empty() {
        lines.push(Line::from(Span::styled(
            " (nothing to show — `:open <path>` takes a path directly)",
            styles.placeholder,
        )));
    }
    let label_w = (inner.width as usize).saturating_sub(ORIGIN_W + 3);
    for (position, &index) in visible.iter().enumerate().skip(offset).take(body_height) {
        let row = &picker.rows[index];
        let origin = match row.origin {
            OpenOrigin::Recent => OpenOrigin::Recent.label(),
            OpenOrigin::Nearby => OpenOrigin::Nearby.label(),
        };
        let text = format!(" {} {}", cell(&row.label, label_w), cell(origin, ORIGIN_W));
        if position == picker.selected {
            lines.push(Line::from(Span::styled(text, styles.selected_row)));
        } else {
            lines.push(Line::from(Span::styled(text, styles.editor)));
        }
    }

    let footer = if picker.filtering {
        format!(" /{}", picker.filter)
    } else if let Some(notice) = &picker.notice {
        format!(" {notice}")
    } else if picker.filter.is_empty() {
        " j/k move · Enter mount · / filter · r refresh · Esc close".to_string()
    } else {
        format!(" filter /{} · Esc clears", picker.filter)
    };
    lines.push(Line::from(Span::styled(footer, styles.muted)));

    frame.render_widget(Paragraph::new(lines), inner);
}

/// Dirty-buffer confirm before a remount (TUI-02). Cancel is the default:
/// no key other than `w` or `y` discards the operator's edit.
fn render_open_confirm(frame: &mut Frame, area: Rect, path: &std::path::Path, styles: Styles) {
    let target = path.display().to_string();
    let width = 68u16.min(area.width);
    let rect = centered(area, width, 7u16.min(area.height));
    frame.render_widget(Clear, rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(styles.border)
        .style(styles.editor)
        .title(Span::styled(" unsaved changes ", styles.brand));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let body_w = inner.width as usize;
    let lines = vec![
        Line::from(Span::styled(
            format!(
                " {}",
                cell("This request has unsaved edits.", body_w.saturating_sub(1))
            ),
            styles.editor,
        )),
        Line::from(Span::styled(
            format!(" open {}", cell(&target, body_w.saturating_sub(6))),
            styles.muted,
        )),
        Line::raw(""),
        Line::from(vec![
            Span::styled(" w ", styles.accent_text),
            Span::styled("write and open   ", styles.editor),
            Span::styled("y ", styles.accent_text),
            Span::styled("discard and open   ", styles.editor),
            Span::styled("n ", styles.accent_text),
            Span::styled("stay", styles.editor),
        ]),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{Appearance, Depth, Theme};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::{fs, path::PathBuf};

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/opencollection/phase1-bundled.yml")
    }

    fn dump(buffer: &ratatui::buffer::Buffer) -> String {
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[tokio::test]
    async fn graphite_render_includes_ship_chrome() {
        let mut app = App::load(Some(&fixture())).await;
        app.apply_theme(Theme::new(Appearance::Dark).with_depth(Depth::Truecolor));
        let backend = TestBackend::new(120, 32);
        let mut terminal = Terminal::new(backend).expect("backend");
        app.render_to(&mut terminal).expect("render");
        let text = dump(terminal.backend().buffer());

        assert!(text.contains("Search requests"), "search row: {text}");
        assert!(text.contains("Path"), "section tabs: {text}");
        assert!(text.contains("Query"), "{text}");
        assert!(text.contains("Headers"), "{text}");
        assert!(text.contains("Body"), "{text}");
        assert!(text.contains("Auth"), "{text}");
        assert!(text.contains("Send"), "{text}");
        assert!(text.contains("No env"), "{text}");
        assert!(text.contains("Graphite Honey"), "{text}");
        assert!(
            !text.contains('╔') && !text.contains('╗'),
            "no double-line boxes"
        );
        assert!(
            text.contains("List pets") || text.contains("Pets"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn open_picker_renders_rows_and_hints() {
        let mut app = App::load(Some(&fixture())).await;
        app.apply_theme(Theme::new(Appearance::Dark).with_depth(Depth::Truecolor));
        app.command_for_test("open");
        app.execute_command_for_test().await;

        let backend = TestBackend::new(120, 32);
        let mut terminal = Terminal::new(backend).expect("backend");
        app.render_to(&mut terminal).expect("render");
        let text = dump(terminal.backend().buffer());

        assert!(text.contains("open collection"), "picker title: {text}");
        assert!(text.contains("Enter mount"), "picker hints: {text}");
        assert!(text.contains("Esc close"), "picker hints: {text}");
    }

    #[tokio::test]
    async fn dirty_confirm_renders_its_three_choices() {
        let mut app = App::load(Some(&fixture())).await;
        app.apply_theme(Theme::new(Appearance::Dark).with_depth(Depth::Truecolor));
        app.mark_dirty_for_test();
        app.command_for_test("open /somewhere/else.yml");
        app.execute_command_for_test().await;

        let backend = TestBackend::new(120, 32);
        let mut terminal = Terminal::new(backend).expect("backend");
        app.render_to(&mut terminal).expect("render");
        let text = dump(terminal.backend().buffer());

        assert!(text.contains("unsaved changes"), "confirm title: {text}");
        assert!(text.contains("write and open"), "{text}");
        assert!(text.contains("discard and open"), "{text}");
        assert!(text.contains("stay"), "{text}");
    }

    #[tokio::test]
    async fn title_bar_shows_the_collection_path() {
        let app = App::load(Some(&fixture())).await;
        let backend = TestBackend::new(160, 32);
        let mut terminal = Terminal::new(backend).expect("backend");
        app.render_to(&mut terminal).expect("render");
        let text = dump(terminal.backend().buffer());

        assert!(text.contains("Pet Store"), "name still shown: {text}");
        assert!(
            text.contains("phase1-bundled.yml"),
            "chrome shows the collection path (TUI-03): {text}"
        );
    }

    #[tokio::test]
    async fn help_lists_open_and_recent() {
        let mut app = App::load(Some(&fixture())).await;
        app.open_help_for_test();
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("backend");
        app.render_to(&mut terminal).expect("render");
        let text = dump(terminal.backend().buffer());

        assert!(text.contains(":open"), "help lists :open: {text}");
        assert!(text.contains(":recent"), "help lists :recent: {text}");
    }

    #[tokio::test]
    async fn response_pane_shows_tab_row_and_pretty_json() {
        use crate::app::{ResponseView, RunResult};
        use std::time::Duration;

        let mut app = App::load(Some(&fixture())).await;
        app.apply_theme(Theme::new(Appearance::Dark).with_depth(Depth::Truecolor));
        app.apply_run_result(RunResult::Ok(ResponseView {
            status: 200,
            reason: "OK".to_string(),
            url: "https://example.com/pets".to_string(),
            duration: Duration::from_millis(42),
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body: "{\"id\":1}".to_string(),
            body_len: 8,
        }));
        let backend = TestBackend::new(120, 32);
        let mut terminal = Terminal::new(backend).expect("backend");
        app.render_to(&mut terminal).expect("render");
        let text = dump(terminal.backend().buffer());

        assert!(text.contains("200 OK"), "status line: {text}");
        assert!(text.contains("Pretty"), "tab row: {text}");
        assert!(text.contains("Raw"), "{text}");
        assert!(text.contains("Headers"), "{text}");
        assert!(text.contains("Inspect"), "{text}");
        // Pretty tab is the default: JSON is indented, not one line.
        assert!(text.contains("\"id\": 1"), "pretty body: {text}");
    }

    #[tokio::test]
    async fn porcelain_render_uses_light_label() {
        let mut app = App::load(Some(&fixture())).await;
        app.apply_theme(Theme::new(Appearance::Light).with_depth(Depth::Truecolor));
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).expect("backend");
        app.render_to(&mut terminal).expect("render");
        let text = dump(terminal.backend().buffer());
        assert!(text.contains("Porcelain Honey"), "{text}");
        assert!(text.contains("Send"), "{text}");
    }

    #[tokio::test]
    async fn splash_renders_when_no_collection_is_loaded() {
        let mut app = App::load(None).await;
        app.apply_theme(Theme::new(Appearance::Dark).with_depth(Depth::Truecolor));
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("backend");
        app.render_to(&mut terminal).expect("render");
        let text = dump(terminal.backend().buffer());

        assert!(text.contains("local-first api client"), "tagline: {text}");
        assert!(text.contains("┌─┐ ┌─  ┌─┐ ─┬─"), "wordmark: {text}");
        assert_eq!(text.matches('●').count(), 7, "seven lattice nodes: {text}");
        assert!(text.contains("facet tui <path>"), "open hint: {text}");
        assert!(!text.contains("Request / Response"), "{text}");
    }

    #[tokio::test]
    async fn splash_wraps_the_glyph_in_the_hexagon_when_tall() {
        let mut app = App::load(None).await;
        app.apply_theme(Theme::new(Appearance::Dark).with_depth(Depth::Truecolor));
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("backend");
        app.render_to(&mut terminal).expect("render");
        let text = dump(terminal.backend().buffer());

        assert!(text.contains("╱───╲"), "hexagon top edge: {text}");
        assert!(text.contains("╲───╱"), "hexagon bottom edge: {text}");
        assert_eq!(
            text.matches('●').count(),
            7,
            "outline adds no nodes: {text}"
        );
        assert!(text.contains("local-first api client"), "{text}");
    }

    #[tokio::test]
    async fn porcelain_splash_renders_the_hexagon() {
        let mut app = App::load(None).await;
        app.apply_theme(Theme::new(Appearance::Light).with_depth(Depth::Truecolor));
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("backend");
        app.render_to(&mut terminal).expect("render");
        let text = dump(terminal.backend().buffer());

        assert!(text.contains("Porcelain Honey"), "{text}");
        assert!(text.contains("╱───╲"), "hexagon in porcelain: {text}");
        assert!(text.contains("local-first api client"), "{text}");
    }

    #[tokio::test]
    async fn running_overlay_shows_the_lattice_glyph() {
        let mut app = App::load(Some(&fixture())).await;
        app.apply_theme(Theme::new(Appearance::Dark).with_depth(Depth::Truecolor));
        app.preview_running();
        let backend = TestBackend::new(120, 32);
        let mut terminal = Terminal::new(backend).expect("backend");
        app.render_to(&mut terminal).expect("render");
        let text = dump(terminal.backend().buffer());

        assert!(text.contains("Sending request…"), "caption: {text}");
        assert_eq!(
            text.matches('●').count(),
            7,
            "overlay glyph has seven nodes: {text}"
        );
    }

    #[tokio::test]
    async fn footer_is_the_bracketed_status_bar() {
        let mut app = App::load(Some(&fixture())).await;
        app.apply_theme(Theme::new(Appearance::Dark).with_depth(Depth::Truecolor));
        let backend = TestBackend::new(120, 32);
        let mut terminal = Terminal::new(backend).expect("backend");
        app.render_to(&mut terminal).expect("render");
        let text = dump(terminal.backend().buffer());
        let footer = text.lines().last().expect("footer row");

        assert!(footer.starts_with("[ G38 │ "), "{footer}");
        assert!(
            footer.contains("lattice idle"),
            "no store beside the fixture: {footer}"
        );
        assert!(footer.contains("│ graphite honey"), "{footer}");
        assert!(footer.trim_end().ends_with(']'), "{footer}");
        assert!(
            footer.contains("q quit"),
            "hints fit at 120 columns: {footer}"
        );
    }

    #[tokio::test]
    async fn footer_shows_the_mode_indicator() {
        let mut app = App::load(Some(&fixture())).await;
        app.apply_theme(Theme::new(Appearance::Dark).with_depth(Depth::Truecolor));
        let backend = TestBackend::new(120, 32);
        let mut terminal = Terminal::new(backend).expect("backend");
        app.render_to(&mut terminal).expect("render");
        let text = dump(terminal.backend().buffer());
        let footer = text.lines().last().expect("footer row");

        assert!(footer.starts_with("[ G38 │ NOR │ "), "{footer}");
        assert!(
            footer.contains("? help"),
            "normal hints mention help: {footer}"
        );
    }

    #[tokio::test]
    async fn help_overlay_lists_the_keymap() {
        let mut app = App::load(Some(&fixture())).await;
        app.apply_theme(Theme::new(Appearance::Dark).with_depth(Depth::Truecolor));
        app.preview_help();
        let backend = TestBackend::new(120, 32);
        let mut terminal = Terminal::new(backend).expect("backend");
        app.render_to(&mut terminal).expect("render");
        let text = dump(terminal.backend().buffer());

        assert!(text.contains("facet keys"), "title: {text}");
        assert!(text.contains("command line"), ": row: {text}");
        assert!(text.contains(":history"), "history verb: {text}");
        assert!(text.contains("gg / G"), "gg/G: {text}");
        assert!(
            text.contains("arrows always work"),
            "flat fallback note: {text}"
        );
    }

    #[tokio::test]
    async fn history_grid_renders_rows_with_run_ids() {
        let mut app = App::load(Some(&fixture())).await;
        app.apply_theme(Theme::new(Appearance::Dark).with_depth(Depth::Truecolor));
        app.preview_history_grid(vec![lattice::RunRow {
            id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
            started_at: 1_757_250_000_123,
            duration_ms: Some(12),
            request_path: "Pets/List pets".to_string(),
            request_hash: "9f86d081".to_string(),
            environment: None,
            method: "GET".to_string(),
            url: "https://example.com/pets".to_string(),
            status: Some(200),
            error: None,
            req_headers: None,
            res_headers: None,
            req_body: lattice::BodyRef::default(),
            res_body: lattice::BodyRef::default(),
            res_content_type: None,
            session_id: None,
            actor: "claude.halo-fullstack".to_string(),
            tags: None,
            replayed_from: None,
            var_names: None,
        }]);
        app.preview_sparkline(vec![Some(200), Some(200), Some(500), None]);
        let backend = TestBackend::new(120, 32);
        let mut terminal = Terminal::new(backend).expect("backend");
        app.render_to(&mut terminal).expect("render");
        let text = dump(terminal.backend().buffer());
        assert!(text.contains("history"), "title: {text}");
        assert!(text.contains("STARTED"), "header: {text}");
        assert!(text.contains("STATUS"), "{text}");
        assert!(text.contains("Pets/List pets"), "{text}");
        // The grid shows the id fragment in-row and the full ULID on the
        // overlay footer so `y` is discoverable.
        assert!(text.contains("01ARZ3ND"), "id fragment: {text}");
        assert!(
            text.contains("01ARZ3NDEKTSV4RRFFQ69G5FAV · Enter hydrate"),
            "footer id: {text}"
        );
        assert!(
            text.contains("history · Enter hydrate · y yank"),
            "footer hints: {text}"
        );
        // Sparkline: four nodes for four runs, labeled with the selector.
        assert!(text.contains("●●●●"), "sparkline nodes: {text}");
        assert!(
            text.contains("last 4 · Pets/List pets"),
            "sparkline label: {text}"
        );
    }

    #[tokio::test]
    async fn footer_drops_hints_before_codename_when_narrow() {
        let mut app = App::load(Some(&fixture())).await;
        app.apply_theme(Theme::new(Appearance::Dark).with_depth(Depth::Truecolor));
        let backend = TestBackend::new(60, 20);
        let mut terminal = Terminal::new(backend).expect("backend");
        app.render_to(&mut terminal).expect("render");
        let text = dump(terminal.backend().buffer());
        let footer = text.lines().last().expect("footer row");

        assert!(footer.starts_with("[ G38 │ "), "{footer}");
        assert!(!footer.contains("q quit"), "hints dropped: {footer}");
        assert!(footer.contains("graphite honey"), "{footer}");
    }

    #[tokio::test]
    async fn footer_marks_a_recorded_send() {
        use crate::app::{RecordSummary, ResponseView, RunResult};
        use std::time::Duration;
        let mut app = App::load(Some(&fixture())).await;
        app.apply_theme(Theme::new(Appearance::Dark).with_depth(Depth::Truecolor));
        app.apply_run_result(RunResult::Ok(ResponseView {
            status: 200,
            reason: "OK".to_string(),
            url: "http://127.0.0.1/".to_string(),
            duration: Duration::from_millis(12),
            headers: Vec::new(),
            body: "{}".to_string(),
            body_len: 2,
        }));
        app.preview_recording(Some(RecordSummary {
            run_id: Some("01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string()),
            note: "recorded".to_string(),
        }));
        let backend = TestBackend::new(120, 32);
        let mut terminal = Terminal::new(backend).expect("backend");
        app.render_to(&mut terminal).expect("render");
        let text = dump(terminal.backend().buffer());
        let footer = text.lines().last().expect("footer row");
        assert!(footer.contains("200 · 12 ms · recorded"), "{footer}");

        app.preview_recording(Some(RecordSummary {
            run_id: None,
            note: "unrecorded: boom".to_string(),
        }));
        app.render_to(&mut terminal).expect("render");
        let text = dump(terminal.backend().buffer());
        assert!(
            text.lines().last().unwrap().contains("· unrecorded"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn env_overlay_lists_metadata_and_masks_secret_input() {
        let mut app = App::load(Some(&fixture())).await;
        app.apply_theme(Theme::new(Appearance::Dark).with_depth(Depth::Truecolor));
        app.preview_env_overlay(vec![
            lattice::EnvironmentRow {
                name: "local".to_string(),
                key: "token".to_string(),
                secret: true,
                updated_at: 0,
            },
            lattice::EnvironmentRow {
                name: "local".to_string(),
                key: "host".to_string(),
                secret: false,
                updated_at: 0,
            },
        ]);
        // Mid-form on a secret value: the footer masks the input.
        app.preview_env_form(true, "hunter2");
        let backend = TestBackend::new(120, 32);
        let mut terminal = Terminal::new(backend).expect("backend");
        app.render_to(&mut terminal).expect("render");
        let text = dump(terminal.backend().buffer());

        assert!(text.contains(" env "), "title: {text}");
        assert!(text.contains("ENVIRONMENT"), "header: {text}");
        assert!(text.contains("token"), "name column: {text}");
        assert!(text.contains("yes"), "secret column: {text}");
        assert!(
            text.contains("set local/token · value: ••••••"),
            "masked value prompt: {text}"
        );
        assert!(
            !text.contains("hunter2"),
            "secret value must never render: {text}"
        );
    }

    #[tokio::test]
    async fn long_collection_path_keeps_full_theme_label() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("a".repeat(180));
        fs::create_dir_all(&nested).unwrap();
        let collection = nested.join("collection.yml");
        fs::copy(fixture(), &collection).unwrap();
        let app = App::load(Some(&collection)).await;
        let mut terminal = Terminal::new(TestBackend::new(120, 32)).unwrap();
        app.render_to(&mut terminal).unwrap();
        assert!(dump(terminal.backend().buffer()).contains("Graphite Honey"));
    }
}
