use crate::cli::Args;
use crate::event::{NormalizedEvent, Severity, ToolEventKind};
use crate::normalizer::normalize;
use crate::stale::{HeartbeatSummary, StaleTracker, StaleWarning};
use crate::tailer;
use chrono::{DateTime, Local, Utc};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Wrap};
use ratatui::{Frame, Terminal};
use serde_json::Value;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::io::{self, stdout};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const MAX_ITEMS: usize = 1500;
const DRAIN_PER_TICK: usize = 128;
const MISSING_TTL_SECONDS: u64 = 30;
const WIDE_PREVIEW_LEN: usize = 80;
const NARROW_PREVIEW_LEN: usize = 50;

enum TimelineItem {
    ToolEvent {
        event: NormalizedEvent,
        source_path: Option<PathBuf>,
    },
    StaleWarning {
        warning: StaleWarning,
        seen_at: DateTime<Utc>,
    },
    Heartbeat {
        summary: HeartbeatSummary,
        seen_at: DateTime<Utc>,
    },
    Error {
        message: String,
        seen_at: DateTime<Utc>,
    },
}

impl TimelineItem {
    fn seen_at(&self) -> DateTime<Utc> {
        match self {
            TimelineItem::ToolEvent { event, .. } => event.timestamp.unwrap_or_else(Utc::now),
            TimelineItem::StaleWarning { seen_at, .. }
            | TimelineItem::Heartbeat { seen_at, .. }
            | TimelineItem::Error { seen_at, .. } => *seen_at,
        }
    }

    fn session_label(&self) -> String {
        match self {
            TimelineItem::ToolEvent { event, .. } => compact_identity(
                event
                    .session_key
                    .as_deref()
                    .or(event.session_id.as_deref())
                    .unwrap_or("-"),
            ),
            TimelineItem::StaleWarning { warning, .. } => {
                compact_identity(warning.session_key.as_deref().unwrap_or("-"))
            }
            TimelineItem::Heartbeat { .. } => "-".to_string(),
            TimelineItem::Error { .. } => "system".to_string(),
        }
    }

    fn agent_label(&self) -> String {
        match self {
            TimelineItem::ToolEvent { event, .. } => {
                compact_identity(event.agent_id.as_deref().unwrap_or("-"))
            }
            _ => "-".to_string(),
        }
    }

    fn tool_label(&self) -> String {
        match self {
            TimelineItem::ToolEvent { event, .. } => {
                event.tool_name.clone().unwrap_or_else(|| "-".into())
            }
            TimelineItem::StaleWarning { warning, .. } => {
                warning.tool_name.clone().unwrap_or_else(|| "-".into())
            }
            TimelineItem::Heartbeat { .. } => "heartbeat".to_string(),
            TimelineItem::Error { .. } => "error".to_string(),
        }
    }

    fn kind_label(&self) -> &'static str {
        match self {
            TimelineItem::ToolEvent { event, .. } => match event.kind {
                ToolEventKind::ToolCallStart => "START",
                ToolEventKind::ToolCallResult => "RESULT",
                ToolEventKind::ToolCall => "CALL",
                ToolEventKind::Other => "OTHER",
                ToolEventKind::Malformed => "BAD",
            },
            TimelineItem::StaleWarning { .. } => "STALE",
            TimelineItem::Heartbeat { .. } => "HB",
            TimelineItem::Error { .. } => "ERR",
        }
    }

    fn status_label(&self) -> String {
        match self {
            TimelineItem::ToolEvent { event, .. } => event
                .status
                .clone()
                .or(event.result_summary.clone())
                .unwrap_or_else(|| "-".to_string()),
            TimelineItem::StaleWarning { warning, .. } => format!("{}s", warning.age_seconds),
            TimelineItem::Heartbeat { summary, .. } => format!(
                "a:{} s:{} u:{}",
                summary.active_calls, summary.stale_calls, summary.active_sessions
            ),
            TimelineItem::Error { .. } => "error".to_string(),
        }
    }

    fn call_label(&self) -> String {
        match self {
            TimelineItem::ToolEvent { event, .. } => compact_call_id(event.call_id.as_deref()),
            TimelineItem::StaleWarning { warning, .. } => compact_call_id(Some(&warning.call_id)),
            _ => "-".to_string(),
        }
    }

    fn preview(&self, max_chars: usize) -> String {
        match self {
            TimelineItem::ToolEvent { event, .. } => {
                truncate_display(&event_preview(event), max_chars)
            }
            TimelineItem::StaleWarning { warning, .. } => truncate_display(
                &warning
                    .message
                    .clone()
                    .unwrap_or_else(|| format!("call {} is stale", warning.call_id)),
                max_chars,
            ),
            TimelineItem::Heartbeat { summary, .. } => {
                truncate_display(&summary.to_line(), max_chars)
            }
            TimelineItem::Error { message, .. } => truncate_display(message, max_chars),
        }
    }
}

struct App {
    items: VecDeque<TimelineItem>,
    state: TableState,
    detail_scroll: u16,
    follow_tail: bool,
    latest_summary: Option<String>,
    filter_summary: String,
    source_summary: String,
    status_text: String,
    raw_payload_expanded: bool,
}

impl App {
    fn new(filter_summary: String, source_summary: String) -> Self {
        let mut state = TableState::default();
        state.select(Some(0));
        Self {
            items: VecDeque::new(),
            state,
            detail_scroll: 0,
            follow_tail: true,
            latest_summary: None,
            filter_summary,
            source_summary,
            status_text: "following newest".to_string(),
            raw_payload_expanded: true,
        }
    }

    fn selected_index(&self) -> usize {
        self.state.selected().unwrap_or(0)
    }

    fn selected_item(&self) -> Option<&TimelineItem> {
        self.items.get(self.selected_index())
    }

    fn push_item(&mut self, item: TimelineItem) {
        self.items.push_front(item);
        while self.items.len() > MAX_ITEMS {
            self.items.pop_back();
        }

        if self.follow_tail || self.items.len() == 1 {
            self.state.select(Some(0));
            self.detail_scroll = 0;
        } else if let Some(selected) = self.state.selected() {
            let max_index = self.items.len().saturating_sub(1);
            self.state.select(Some((selected + 1).min(max_index)));
        }
    }

    fn set_status(&mut self, status: impl Into<String>) {
        self.status_text = status.into();
    }

    fn push_heartbeat(&mut self, summary: HeartbeatSummary) {
        self.latest_summary = Some(summary.to_line());
        self.push_item(TimelineItem::Heartbeat {
            summary,
            seen_at: Utc::now(),
        });
    }

    fn push_error(&mut self, message: impl Into<String>) {
        self.push_item(TimelineItem::Error {
            message: message.into(),
            seen_at: Utc::now(),
        });
    }

    fn next(&mut self) {
        if self.items.is_empty() {
            return;
        }
        self.follow_tail = false;
        let next = (self.selected_index() + 1).min(self.items.len().saturating_sub(1));
        self.state.select(Some(next));
        self.detail_scroll = 0;
        self.set_status("paused on older event");
    }

    fn previous(&mut self) {
        if self.items.is_empty() {
            return;
        }
        self.follow_tail = false;
        let prev = self.selected_index().saturating_sub(1);
        self.state.select(Some(prev));
        self.detail_scroll = 0;
        self.set_status("paused on newer event");
    }

    fn select_newest(&mut self) {
        if self.items.is_empty() {
            return;
        }
        self.follow_tail = true;
        self.state.select(Some(0));
        self.detail_scroll = 0;
        self.set_status("following newest");
    }

    fn select_oldest(&mut self) {
        if self.items.is_empty() {
            return;
        }
        self.follow_tail = false;
        self.state.select(Some(self.items.len().saturating_sub(1)));
        self.detail_scroll = 0;
        self.set_status("paused on oldest event");
    }

    fn scroll_detail_down(&mut self) {
        self.detail_scroll = self.detail_scroll.saturating_add(1);
        self.set_status("detail scroll down");
    }

    fn scroll_detail_up(&mut self) {
        self.detail_scroll = self.detail_scroll.saturating_sub(1);
        self.set_status("detail scroll up");
    }

    fn toggle_follow(&mut self) {
        self.follow_tail = !self.follow_tail;
        if self.follow_tail {
            self.state.select(Some(0));
            self.detail_scroll = 0;
            self.set_status("following newest");
        } else {
            self.set_status("paused");
        }
    }

    fn toggle_raw_payload(&mut self) {
        self.raw_payload_expanded = !self.raw_payload_expanded;
        self.set_status(if self.raw_payload_expanded {
            "raw payload expanded"
        } else {
            "raw payload collapsed"
        });
    }
}

pub fn run(args: &Args) -> io::Result<()> {
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let result = run_app(&mut terminal, args);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    args: &Args,
) -> io::Result<()> {
    let mut app = App::new(format_filters(args), source_summary(args));
    let mut tracker = StaleTracker::new(args.stale_seconds);
    let heartbeat_interval = args.heartbeat_duration();
    let ui_tick = Duration::from_millis(50);
    let mut last_heartbeat = Instant::now();

    if args.log_file.is_none() {
        let mut discovered_paths = crate::discover_initial_session_logs();
        let mut tailer = tailer::MultiTailer::new(
            discovered_paths,
            !args.no_follow,
            args.from_start,
            args.poll_duration(),
            Duration::from_secs(MISSING_TTL_SECONDS),
        );
        let mut last_scan = Instant::now();

        loop {
            if handle_input(&mut app, ui_tick)? {
                break;
            }

            let now = Instant::now();
            if now.duration_since(last_heartbeat) >= heartbeat_interval {
                app.push_heartbeat(tracker.heartbeat(Utc::now()));
                last_heartbeat = now;
            }

            if !args.no_follow && now.duration_since(last_scan) >= tailer.poll_interval() {
                discovered_paths = crate::discover_initial_session_logs();
                tailer.sync(discovered_paths);
                last_scan = now;
            }

            drain_multi_tailer(&mut tailer, args, &mut tracker, &mut app);
            terminal.draw(|frame| render(frame, &app))?;
        }

        return Ok(());
    }

    let Some(log_file) = args.log_file.clone() else {
        return Ok(());
    };

    let mut tailer = match tailer::Tailer::new(
        log_file.clone(),
        !args.no_follow,
        args.from_start,
        args.poll_duration(),
    ) {
        Ok(state) => state,
        Err(err) => {
            app.push_error(format!("failed to open {}: {}", log_file.display(), err));
            loop {
                terminal.draw(|frame| render(frame, &app))?;
                if handle_input(&mut app, ui_tick)? {
                    break;
                }
            }
            return Ok(());
        }
    };

    loop {
        if handle_input(&mut app, ui_tick)? {
            break;
        }

        let now = Instant::now();
        if now.duration_since(last_heartbeat) >= heartbeat_interval {
            app.push_heartbeat(tracker.heartbeat(Utc::now()));
            last_heartbeat = now;
        }

        drain_single_tailer(&mut tailer, &log_file, args, &mut tracker, &mut app);
        terminal.draw(|frame| render(frame, &app))?;
    }

    Ok(())
}

fn handle_input(app: &mut App, timeout: Duration) -> io::Result<bool> {
    if !event::poll(timeout)? {
        return Ok(false);
    }

    if let Event::Key(key) = event::read()? {
        if key.kind != KeyEventKind::Press {
            return Ok(false);
        }

        match key.code {
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Down | KeyCode::Char('j') => app.next(),
            KeyCode::Up | KeyCode::Char('k') => app.previous(),
            KeyCode::Char('g') | KeyCode::Home => app.select_newest(),
            KeyCode::Char('G') | KeyCode::End => app.select_oldest(),
            KeyCode::PageDown => app.scroll_detail_down(),
            KeyCode::PageUp => app.scroll_detail_up(),
            KeyCode::Char('f') => app.toggle_follow(),
            KeyCode::Char('r') => app.toggle_raw_payload(),
            _ => {}
        }
    }

    Ok(false)
}

fn drain_multi_tailer(
    tailer: &mut tailer::MultiTailer,
    args: &Args,
    tracker: &mut StaleTracker,
    app: &mut App,
) {
    for _ in 0..DRAIN_PER_TICK {
        match tailer.next_line() {
            Ok(Some((path, raw_line))) => {
                ingest_line(&raw_line, Some(path.as_path()), args, tracker, app);
            }
            Ok(None) => break,
            Err(err) => {
                app.push_error(err.to_string());
                app.set_status("tailer error");
                break;
            }
        }
    }
}

fn drain_single_tailer(
    tailer: &mut tailer::Tailer,
    log_file: &Path,
    args: &Args,
    tracker: &mut StaleTracker,
    app: &mut App,
) {
    for _ in 0..DRAIN_PER_TICK {
        match tailer.next_line() {
            Ok(Some(raw_line)) => ingest_line(&raw_line, Some(log_file), args, tracker, app),
            Ok(None) => break,
            Err(err) => {
                app.push_error(err.to_string());
                app.set_status("tailer error");
                break;
            }
        }
    }
}

fn ingest_line(
    raw_line: &str,
    source_path: Option<&Path>,
    args: &Args,
    tracker: &mut StaleTracker,
    app: &mut App,
) {
    let event = normalize(raw_line);
    let notices = tracker.on_event(&event, Utc::now());

    if crate::event_matches_filters(&event, args) {
        app.push_item(TimelineItem::ToolEvent {
            event,
            source_path: source_path.map(Path::to_path_buf),
        });
    }

    for warning in notices {
        if !crate::stale_warning_matches_filters(&warning, args) {
            continue;
        }

        app.push_item(TimelineItem::StaleWarning {
            warning,
            seen_at: Utc::now(),
        });
    }
}

fn render(frame: &mut Frame, app: &App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(2),
        ])
        .split(frame.area());

    render_header(frame, root[0], app);

    let wide = root[1].width >= 120;
    let body = Layout::default()
        .direction(if wide {
            Direction::Horizontal
        } else {
            Direction::Vertical
        })
        .constraints(if wide {
            [Constraint::Percentage(58), Constraint::Percentage(42)]
        } else {
            [Constraint::Percentage(56), Constraint::Percentage(44)]
        })
        .split(root[1]);

    render_table(frame, body[0], app, wide);
    render_detail(frame, body[1], app);
    render_footer(frame, root[2], app);
}

fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let badge = if app.follow_tail { "LIVE" } else { "PAUSED" };
    let badge_color = if app.follow_tail {
        Color::Green
    } else {
        Color::Yellow
    };
    let summary = app
        .latest_summary
        .as_ref()
        .cloned()
        .unwrap_or_else(|| "waiting for heartbeat".to_string());

    let text = Text::from(vec![
        Line::from(vec![
            Span::styled(
                "OpenClaw Logpulse",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                badge,
                Style::default()
                    .fg(badge_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled("source ", Style::default().fg(Color::DarkGray)),
            Span::raw(&app.source_summary),
        ]),
        Line::from(vec![
            Span::styled("filters ", Style::default().fg(Color::DarkGray)),
            Span::raw(&app.filter_summary),
            Span::raw("  "),
            Span::styled("heartbeat ", Style::default().fg(Color::DarkGray)),
            Span::raw(summary),
        ]),
    ]);

    frame.render_widget(
        Paragraph::new(text).block(Block::default().borders(Borders::ALL).title("Status")),
        area,
    );
}

fn render_table(frame: &mut Frame, area: Rect, app: &App, wide: bool) {
    let header = Row::new(vec![
        Cell::from("Time"),
        Cell::from("Kind"),
        Cell::from("Session"),
        Cell::from("Agent"),
        Cell::from("Tool"),
        Cell::from("Call"),
        Cell::from("State"),
        Cell::from("Preview"),
    ])
    .style(
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );

    let preview_len = if wide {
        WIDE_PREVIEW_LEN
    } else {
        NARROW_PREVIEW_LEN
    };
    let rows = app.items.iter().map(|item| timeline_row(item, preview_len));

    let widths = if wide {
        vec![
            Constraint::Length(8),
            Constraint::Length(7),
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Length(12),
            Constraint::Length(8),
            Constraint::Length(12),
            Constraint::Min(20),
        ]
    } else {
        vec![
            Constraint::Length(8),
            Constraint::Length(7),
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Length(10),
            Constraint::Length(7),
            Constraint::Length(10),
            Constraint::Min(14),
        ]
    };

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title("Timeline"))
        .row_highlight_style(
            Style::default()
                .bg(Color::Rgb(32, 43, 59))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");

    let mut state = app.state.clone();
    frame.render_stateful_widget(table, area, &mut state);
}

fn render_detail(frame: &mut Frame, area: Rect, app: &App) {
    let detail = app
        .selected_item()
        .map(|item| detail_text(item, app.raw_payload_expanded))
        .unwrap_or_else(|| Text::from(vec![Line::from("No events yet.")]));

    frame.render_widget(
        Paragraph::new(detail)
            .block(Block::default().borders(Borders::ALL).title("Detail"))
            .wrap(Wrap { trim: false })
            .scroll((app.detail_scroll, 0)),
        area,
    );
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let line = Line::from(vec![
        key_hint("q", "quit"),
        Span::raw("  "),
        key_hint("j/k", "select"),
        Span::raw("  "),
        key_hint("f", if app.follow_tail { "pause" } else { "resume" }),
        Span::raw("  "),
        key_hint("PgUp/PgDn", "detail"),
        Span::raw("  "),
        key_hint(
            "r",
            if app.raw_payload_expanded {
                "hide raw"
            } else {
                "show raw"
            },
        ),
        Span::raw("  "),
        Span::styled(app.status_text.clone(), Style::default().fg(Color::Gray)),
    ]);

    frame.render_widget(
        Paragraph::new(line).block(Block::default().borders(Borders::ALL).title("Keys")),
        area,
    );
}

fn timeline_row(item: &TimelineItem, preview_len: usize) -> Row<'static> {
    let tool_name = item.tool_label();

    Row::new(vec![
        Cell::from(format_ts(item.seen_at())),
        Cell::from(item.kind_label()).style(kind_style(item)),
        Cell::from(item.session_label()).style(Style::default().fg(Color::White)),
        Cell::from(item.agent_label()).style(Style::default().fg(Color::Gray)),
        Cell::from(truncate_display(&tool_name, 12)).style(
            Style::default()
                .fg(tool_color(&tool_name))
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from(item.call_label()).style(Style::default().fg(Color::DarkGray)),
        Cell::from(truncate_display(&item.status_label(), 12)).style(status_style(item)),
        Cell::from(item.preview(preview_len)),
    ])
}

fn detail_text(item: &TimelineItem, raw_payload_expanded: bool) -> Text<'static> {
    match item {
        TimelineItem::ToolEvent { event, source_path } => {
            detail_tool_event(event, source_path.as_deref(), raw_payload_expanded)
        }
        TimelineItem::StaleWarning { warning, seen_at } => {
            Text::from(vec![
                title_line("STALE WARNING", Color::Yellow),
                kv_line("Timestamp", &seen_at.to_rfc3339()),
                kv_line("Session", warning.session_key.as_deref().unwrap_or("-")),
                kv_line("Tool", warning.tool_name.as_deref().unwrap_or("-")),
                kv_line("Call ID", &warning.call_id),
                kv_line("Status", "stale"),
                kv_line("Age", &format!("{} seconds", warning.age_seconds)),
                section_header("Message"),
                Line::from(warning.message.clone().unwrap_or_else(|| {
                    "Long-running tool call has not completed yet.".to_string()
                })),
            ])
        }
        TimelineItem::Heartbeat { summary, seen_at } => Text::from(vec![
            title_line("HEARTBEAT", Color::Cyan),
            kv_line("Timestamp", &seen_at.to_rfc3339()),
            kv_line("Active calls", &summary.active_calls.to_string()),
            kv_line("Stale calls", &summary.stale_calls.to_string()),
            kv_line("Active sessions", &summary.active_sessions.to_string()),
        ]),
        TimelineItem::Error { message, seen_at } => Text::from(vec![
            title_line("SYSTEM ERROR", Color::LightRed),
            kv_line("Timestamp", &seen_at.to_rfc3339()),
            section_header("Message"),
            Line::from(message.clone()),
        ]),
    }
}

fn detail_tool_event(
    event: &NormalizedEvent,
    source_path: Option<&Path>,
    raw_payload_expanded: bool,
) -> Text<'static> {
    let mut lines = vec![title_line(
        &format!(
            "{} {}",
            event.kind_label(),
            event.tool_name.as_deref().unwrap_or("event")
        ),
        kind_color(&event.kind),
    )];

    lines.push(kv_line(
        "Timestamp",
        &event.timestamp.unwrap_or_else(Utc::now).to_rfc3339(),
    ));
    lines.push(kv_line(
        "Session",
        event
            .session_key
            .as_deref()
            .or(event.session_id.as_deref())
            .unwrap_or("-"),
    ));
    lines.push(kv_line("Agent", event.agent_id.as_deref().unwrap_or("-")));
    lines.push(kv_line("Tool", event.tool_name.as_deref().unwrap_or("-")));
    lines.push(kv_line("Kind", event.kind_label()));
    lines.push(kv_line("Severity", severity_label(event.level)));
    lines.push(kv_line(
        "Status",
        event
            .status
            .as_deref()
            .or(event.result_summary.as_deref())
            .unwrap_or("-"),
    ));
    if let Some(call_id) = &event.call_id {
        lines.push(kv_line("Call ID", call_id));
    }
    if let Some(path) = source_path {
        lines.push(kv_line("Source", &path.display().to_string()));
    }

    if let Some(summary) = &event.result_summary {
        lines.push(section_header("Result summary"));
        lines.extend(multiline_lines(summary, Color::White));
    }

    if let Some(message) = &event.message {
        if Some(message) != event.result_summary.as_ref() {
            lines.push(section_header("Message"));
            lines.extend(multiline_lines(message, Color::White));
        }
    }

    lines.push(section_header("Decoded params"));
    if event.params.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "(none)",
            Style::default().fg(Color::DarkGray),
        )]));
    } else {
        for (key, value) in ordered_params(event) {
            lines.push(kv_line(&key, &value));
        }
    }

    lines.push(section_header("Raw payload"));
    if raw_payload_expanded {
        lines.extend(multiline_lines(
            &pretty_raw_json(&event.raw_line),
            Color::Gray,
        ));
    } else {
        lines.push(Line::from(vec![Span::styled(
            "(hidden, press r to expand)",
            Style::default().fg(Color::DarkGray),
        )]));
    }

    Text::from(lines)
}

trait KindLabel {
    fn kind_label(&self) -> &'static str;
}

impl KindLabel for NormalizedEvent {
    fn kind_label(&self) -> &'static str {
        match self.kind {
            ToolEventKind::ToolCallStart => "START",
            ToolEventKind::ToolCallResult => "RESULT",
            ToolEventKind::ToolCall => "CALL",
            ToolEventKind::Other => "OTHER",
            ToolEventKind::Malformed => "BAD",
        }
    }
}

fn title_line(title: &str, color: Color) -> Line<'static> {
    Line::from(vec![Span::styled(
        title.to_string(),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )])
}

fn section_header(text: &str) -> Line<'static> {
    Line::from(vec![
        Span::raw(""),
        Span::styled(
            text.to_string(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

fn kv_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label}: "),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(value.to_string()),
    ])
}

fn multiline_lines(text: &str, color: Color) -> Vec<Line<'static>> {
    text.lines()
        .map(|line| {
            Line::from(vec![Span::styled(
                line.to_string(),
                Style::default().fg(color),
            )])
        })
        .collect()
}

fn key_hint(key: &str, desc: &str) -> Span<'static> {
    Span::styled(
        format!("{key} {desc}"),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )
}

fn pretty_raw_json(raw: &str) -> String {
    match serde_json::from_str::<Value>(raw) {
        Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_else(|_| raw.to_string()),
        Err(_) => raw.to_string(),
    }
}

fn ordered_params(event: &NormalizedEvent) -> Vec<(String, String)> {
    let mut params = event.params.clone();
    let priority = if matches!(
        event.tool_name.as_deref(),
        Some(tool) if tool.eq_ignore_ascii_case("exec") || tool.eq_ignore_ascii_case("shell")
    ) {
        vec![
            "command",
            "cwd",
            "exit_code",
            "duration",
            "stdout",
            "stderr",
            "result",
        ]
    } else if matches!(
        event.tool_name.as_deref(),
        Some(tool)
            if tool.eq_ignore_ascii_case("memory")
                || tool.eq_ignore_ascii_case("read")
                || tool.eq_ignore_ascii_case("write")
                || tool.eq_ignore_ascii_case("edit")
    ) {
        vec!["path", "file_path", "query", "operation", "result"]
    } else {
        Vec::new()
    };

    params.sort_by_key(|(key, _)| {
        priority
            .iter()
            .position(|candidate| candidate == key)
            .unwrap_or(priority.len())
    });
    params
}

fn event_preview(event: &NormalizedEvent) -> String {
    if let Some(tool) = event.tool_name.as_deref() {
        if tool.eq_ignore_ascii_case("exec") || tool.eq_ignore_ascii_case("shell") {
            if let Some(command) = event
                .params
                .iter()
                .find(|(key, _)| key == "command")
                .map(|(_, value)| value.as_str())
            {
                let mut preview = format!("cmd: {command}");
                if let Some(cwd) = event
                    .params
                    .iter()
                    .find(|(key, _)| key == "cwd")
                    .map(|(_, value)| value.as_str())
                {
                    preview.push_str(" @ ");
                    preview.push_str(cwd);
                }
                return preview;
            }
        }

        if matches!(
            tool.to_ascii_lowercase().as_str(),
            "read" | "write" | "edit"
        ) {
            if let Some(path) = event
                .params
                .iter()
                .find(|(key, _)| key == "path" || key == "file_path")
                .map(|(_, value)| value.as_str())
            {
                return format!("path: {path}");
            }
        }

        if tool.eq_ignore_ascii_case("memory") {
            if let Some(query) = event
                .params
                .iter()
                .find(|(key, _)| key == "query" || key == "key")
                .map(|(_, value)| value.as_str())
            {
                return format!("query: {query}");
            }
        }
    }

    if let Some(status) = event.result_summary.as_deref().or(event.status.as_deref()) {
        if let Some(message) = event.message.as_deref() {
            return format!("{status}: {message}");
        }
        return status.to_string();
    }

    if let Some(message) = event.message.as_deref() {
        return message.to_string();
    }

    if !event.params.is_empty() {
        return event
            .params
            .iter()
            .take(3)
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join("  ");
    }

    event.raw_line.clone()
}

fn format_ts(ts: DateTime<Utc>) -> String {
    ts.with_timezone(&Local).format("%H:%M:%S").to_string()
}

fn kind_style(item: &TimelineItem) -> Style {
    Style::default()
        .fg(match item {
            TimelineItem::ToolEvent { event, .. } => kind_color(&event.kind),
            TimelineItem::StaleWarning { .. } => Color::Yellow,
            TimelineItem::Heartbeat { .. } => Color::Cyan,
            TimelineItem::Error { .. } => Color::LightRed,
        })
        .add_modifier(Modifier::BOLD)
}

fn kind_color(kind: &ToolEventKind) -> Color {
    match kind {
        ToolEventKind::ToolCallStart => Color::Blue,
        ToolEventKind::ToolCallResult => Color::Green,
        ToolEventKind::ToolCall => Color::Magenta,
        ToolEventKind::Other => Color::Gray,
        ToolEventKind::Malformed => Color::Red,
    }
}

fn status_style(item: &TimelineItem) -> Style {
    let status = item.status_label().to_ascii_lowercase();
    if status.contains("error")
        || status.contains("fail")
        || status.contains("timeout")
        || status.contains("forbidden")
    {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else if status.contains("ok")
        || status.contains("success")
        || status.contains("complete")
        || status.contains("done")
    {
        Style::default().fg(Color::Green)
    } else if status.contains("running") || status.contains("started") || status == "-" {
        Style::default().fg(Color::Blue)
    } else if status.contains("wait") || status.contains("pending") || status.ends_with('s') {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Gray)
    }
}

fn severity_label(level: Severity) -> &'static str {
    match level {
        Severity::Trace => "trace",
        Severity::Debug => "debug",
        Severity::Info => "info",
        Severity::Warn => "warn",
        Severity::Error => "error",
        Severity::Fatal => "fatal",
        Severity::Unknown => "unknown",
    }
}

fn tool_color(tool: &str) -> Color {
    let palette = [
        Color::Cyan,
        Color::Green,
        Color::Magenta,
        Color::Yellow,
        Color::LightBlue,
        Color::LightGreen,
        Color::LightMagenta,
        Color::LightCyan,
    ];

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tool.hash(&mut hasher);
    palette[(hasher.finish() as usize) % palette.len()]
}

fn truncate_display(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }

    let mut shortened = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    shortened.push('…');
    shortened
}

fn compact_identity(value: &str) -> String {
    if value.len() > 12 && value.contains('-') {
        value[value.len().saturating_sub(12)..].to_string()
    } else {
        value.to_string()
    }
}

fn compact_call_id(value: Option<&str>) -> String {
    match value {
        Some(call_id) if call_id.len() > 8 => {
            call_id[call_id.len().saturating_sub(8)..].to_string()
        }
        Some(call_id) => call_id.to_string(),
        None => "-".to_string(),
    }
}

fn source_summary(args: &Args) -> String {
    match &args.log_file {
        Some(path) => path.display().to_string(),
        None => "auto-discovery".to_string(),
    }
}

fn format_filters(args: &Args) -> String {
    let mut parts = Vec::new();

    if let Some(session) = &args.session {
        parts.push(format!("session={session}"));
    }
    if let Some(agent) = &args.agent {
        parts.push(format!("agent={agent}"));
    }
    if let Some(tool) = &args.tool {
        parts.push(format!("tool={tool}"));
    }
    if let Some(since) = &args.since {
        parts.push(format!("since={since}"));
    }
    if let Some(until) = &args.until {
        parts.push(format!("until={until}"));
    }
    parts.push(format!("min={}", severity_label(args.min_severity())));
    parts.push(if args.no_follow { "one-shot" } else { "follow" }.to_string());

    parts.join("  ")
}

#[cfg(test)]
mod tests {
    use super::{detail_tool_event, event_preview};
    use crate::event::{NormalizedEvent, Severity, ToolEventKind};

    fn sample_event() -> NormalizedEvent {
        NormalizedEvent {
            kind: ToolEventKind::ToolCallStart,
            timestamp: None,
            timestamp_raw: None,
            source_path: None,
            source_kind: None,
            session_key: Some("session-123".to_string()),
            session_id: None,
            session_source: None,
            agent_id: Some("agent-456".to_string()),
            agent_source: None,
            tool_name: Some("shell".to_string()),
            status: Some("running".to_string()),
            result_summary: None,
            result_preview: None,
            result_raw: None,
            result_metrics: Vec::new(),
            exit_code: None,
            duration_ms: None,
            is_error: None,
            call_id: Some("call-7890".to_string()),
            call_ids: vec!["call-7890".to_string()],
            correlation_ids: Vec::new(),
            message_id: None,
            parent_message_id: None,
            level: Severity::Info,
            level_raw: None,
            params: vec![("command".to_string(), "git status".to_string())],
            args_preview: vec![("command".to_string(), "git status".to_string())],
            args_raw: None,
            args_truncated: false,
            message: None,
            raw_line: r#"{"event":"tool_call_start"}"#.to_string(),
        }
    }

    #[test]
    fn preview_prioritizes_exec_command() {
        assert!(event_preview(&sample_event()).contains("cmd: git status"));
    }

    #[test]
    fn detail_includes_decoded_params_and_raw_payload() {
        let rendered = detail_tool_event(&sample_event(), None, true).to_string();
        assert!(rendered.contains("Decoded params"));
        assert!(rendered.contains("command: git status"));
        assert!(rendered.contains("Raw payload"));
    }

    #[test]
    fn empty_params_show_none() {
        let mut event = sample_event();
        event.params.clear();
        let rendered = detail_tool_event(&event, None, true).to_string();
        assert!(rendered.contains("(none)"));
    }
}
