use crate::cli::Args;
use crate::event::{NormalizedEvent, Severity, ToolEventKind};
use crate::normalizer::normalize_many_with_source;
use crate::stale::{HeartbeatSummary, StaleTracker, StaleWarning};
use crate::{discovery, tailer};
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
use std::env;
use std::hash::{Hash, Hasher};
use std::io::{self, stdout};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const MAX_ITEMS: usize = 1500;
const DRAIN_PER_TICK: usize = 128;
const MISSING_TTL_SECONDS: u64 = 30;
const PREVIEW_LEN: usize = 72;

#[derive(Clone)]
enum TimelineItem {
    ToolEvent(Box<NormalizedEvent>),
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
            TimelineItem::ToolEvent(event) => event.timestamp.unwrap_or_else(Utc::now),
            TimelineItem::StaleWarning { seen_at, .. }
            | TimelineItem::Heartbeat { seen_at, .. }
            | TimelineItem::Error { seen_at, .. } => *seen_at,
        }
    }

    fn session_label(&self) -> String {
        match self {
            TimelineItem::ToolEvent(event) => event
                .session_key
                .as_ref()
                .or(event.session_id.as_ref())
                .cloned()
                .unwrap_or_else(|| "-".to_string()),
            TimelineItem::StaleWarning { warning, .. } => warning
                .session_key
                .clone()
                .unwrap_or_else(|| "-".to_string()),
            TimelineItem::Heartbeat { .. } => "-".to_string(),
            TimelineItem::Error { .. } => "system".to_string(),
        }
    }

    fn agent_label(&self) -> String {
        match self {
            TimelineItem::ToolEvent(event) => event.agent_id.clone().unwrap_or_else(|| "-".into()),
            _ => "-".to_string(),
        }
    }

    fn tool_label(&self) -> String {
        match self {
            TimelineItem::ToolEvent(event) => event.tool_name.clone().unwrap_or_else(|| "-".into()),
            TimelineItem::StaleWarning { warning, .. } => {
                warning.tool_name.clone().unwrap_or_else(|| "-".into())
            }
            TimelineItem::Heartbeat { .. } => "heartbeat".to_string(),
            TimelineItem::Error { .. } => "error".to_string(),
        }
    }

    fn kind_label(&self) -> &'static str {
        match self {
            TimelineItem::ToolEvent(event) => match event.kind {
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
            TimelineItem::ToolEvent(event) => event
                .status
                .clone()
                .or(event.result_summary.clone())
                .unwrap_or_else(|| "-".to_string()),
            TimelineItem::StaleWarning { warning, .. } => format!("{}s", warning.age_seconds),
            TimelineItem::Heartbeat { summary, .. } => format!(
                "active={} stale={} sessions={}",
                summary.active_calls, summary.stale_calls, summary.active_sessions
            ),
            TimelineItem::Error { .. } => "error".to_string(),
        }
    }

    fn preview(&self) -> String {
        match self {
            TimelineItem::ToolEvent(event) => event_preview(event),
            TimelineItem::StaleWarning { warning, .. } => warning
                .message
                .clone()
                .unwrap_or_else(|| format!("call {} is stale", warning.call_id)),
            TimelineItem::Heartbeat { summary, .. } => summary.to_line(),
            TimelineItem::Error { message, .. } => message.clone(),
        }
    }
}

struct App {
    items: VecDeque<TimelineItem>,
    state: TableState,
    detail_scroll: u16,
    follow_tail: bool,
    latest_summary: Option<HeartbeatSummary>,
    filter_summary: String,
}

impl App {
    fn new(filter_summary: String) -> Self {
        let mut state = TableState::default();
        state.select(Some(0));
        Self {
            items: VecDeque::new(),
            state,
            detail_scroll: 0,
            follow_tail: true,
            latest_summary: None,
            filter_summary,
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

    fn push_heartbeat(&mut self, summary: HeartbeatSummary) {
        self.latest_summary = Some(summary.clone());
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
    }

    fn previous(&mut self) {
        if self.items.is_empty() {
            return;
        }
        self.follow_tail = false;
        let prev = self.selected_index().saturating_sub(1);
        self.state.select(Some(prev));
        self.detail_scroll = 0;
    }

    fn select_first(&mut self) {
        if self.items.is_empty() {
            return;
        }
        self.follow_tail = true;
        self.state.select(Some(0));
        self.detail_scroll = 0;
    }

    fn select_last(&mut self) {
        if self.items.is_empty() {
            return;
        }
        self.follow_tail = false;
        self.state.select(Some(self.items.len().saturating_sub(1)));
        self.detail_scroll = 0;
    }

    fn scroll_detail_down(&mut self) {
        self.detail_scroll = self.detail_scroll.saturating_add(1);
    }

    fn scroll_detail_up(&mut self) {
        self.detail_scroll = self.detail_scroll.saturating_sub(1);
    }

    fn toggle_follow(&mut self) {
        self.follow_tail = !self.follow_tail;
        if self.follow_tail {
            self.state.select(Some(0));
            self.detail_scroll = 0;
        }
    }
}

pub fn run(args: &Args) -> io::Result<()> {
    let time_filter = args
        .time_filter()
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let result = run_app(&mut terminal, args, &time_filter);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    args: &Args,
    time_filter: &crate::event::TimeFilter,
) -> io::Result<()> {
    let filter_summary = format_filters(args);
    let mut app = App::new(filter_summary);
    let mut tracker = StaleTracker::new(args.stale_seconds);
    let heartbeat_interval = args.heartbeat_duration();
    let ui_tick = Duration::from_millis(50);
    let mut last_heartbeat = Instant::now();

    let auto_discover = args.log_file.is_none();

    if auto_discover {
        let mut discovered_paths = discover_initial_session_logs();
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
                discovered_paths = discover_initial_session_logs();
                tailer.sync(discovered_paths);
                last_scan = now;
            }

            drain_multi_tailer(&mut tailer, args, time_filter, &mut tracker, &mut app);
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

        drain_single_tailer(
            &mut tailer,
            &log_file,
            args,
            time_filter,
            &mut tracker,
            &mut app,
        );
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
            KeyCode::Char('g') => app.select_first(),
            KeyCode::Char('G') | KeyCode::End => app.select_last(),
            KeyCode::PageDown => app.scroll_detail_down(),
            KeyCode::PageUp => app.scroll_detail_up(),
            KeyCode::Home => {
                app.detail_scroll = 0;
            }
            KeyCode::Char('f') => app.toggle_follow(),
            _ => {}
        }
    }

    Ok(false)
}

fn drain_multi_tailer(
    tailer: &mut tailer::MultiTailer,
    args: &Args,
    time_filter: &crate::event::TimeFilter,
    tracker: &mut StaleTracker,
    app: &mut App,
) {
    for _ in 0..DRAIN_PER_TICK {
        match tailer.next_line() {
            Ok(Some((path, raw_line))) => {
                ingest_line(
                    &raw_line,
                    Some(path.as_path()),
                    args,
                    time_filter,
                    tracker,
                    app,
                );
            }
            Ok(None) => break,
            Err(err) => {
                app.push_error(err.to_string());
                break;
            }
        }
    }
}

fn drain_single_tailer(
    tailer: &mut tailer::Tailer,
    log_file: &Path,
    args: &Args,
    time_filter: &crate::event::TimeFilter,
    tracker: &mut StaleTracker,
    app: &mut App,
) {
    for _ in 0..DRAIN_PER_TICK {
        match tailer.next_line() {
            Ok(Some(raw_line)) => {
                ingest_line(&raw_line, Some(log_file), args, time_filter, tracker, app)
            }
            Ok(None) => break,
            Err(err) => {
                app.push_error(err.to_string());
                break;
            }
        }
    }
}

fn ingest_line(
    raw_line: &str,
    source_path: Option<&Path>,
    args: &Args,
    time_filter: &crate::event::TimeFilter,
    tracker: &mut StaleTracker,
    app: &mut App,
) {
    let now = Utc::now();
    for event in normalize_many_with_source(raw_line, source_path) {
        let notices = tracker.on_event(&event, now);

        if event.should_filter(
            args.session.as_ref(),
            args.agent.as_ref(),
            args.tool.as_ref(),
            args.min_severity(),
            Some(time_filter),
        ) {
            app.push_item(TimelineItem::ToolEvent(Box::new(event)));
        }

        for warning in notices {
            if !time_filter.contains(Some(now)) {
                continue;
            }
            let session_matches = args.session.as_ref().map(|needle| {
                warning
                    .session_key
                    .as_ref()
                    .map(|value| {
                        value
                            .to_ascii_lowercase()
                            .contains(&needle.to_ascii_lowercase())
                    })
                    .unwrap_or(false)
            });
            let tool_matches = args.tool.as_ref().map(|needle| {
                warning
                    .tool_name
                    .as_ref()
                    .map(|value| {
                        value
                            .to_ascii_lowercase()
                            .contains(&needle.to_ascii_lowercase())
                    })
                    .unwrap_or(false)
            });

            if session_matches == Some(false) || tool_matches == Some(false) {
                continue;
            }

            app.push_item(TimelineItem::StaleWarning {
                warning,
                seen_at: now,
            });
        }
    }
}

fn render(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(2),
        ])
        .split(frame.area());

    render_header(frame, chunks[0], app);

    let body = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(56), Constraint::Percentage(44)])
        .split(chunks[1]);

    render_table(frame, body[0], app);
    render_detail(frame, body[1], app);
    render_footer(frame, chunks[2], app);
}

fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let summary =
        app.latest_summary
            .as_ref()
            .map_or("waiting for heartbeat".to_string(), |summary| {
                format!(
                    "active {}  stale {}  sessions {}",
                    summary.active_calls, summary.stale_calls, summary.active_sessions
                )
            });

    let status = if app.follow_tail {
        "tailing newest"
    } else {
        "manual scroll"
    };

    let text = Text::from(vec![
        Line::from(vec![
            Span::styled(
                "OpenClaw Logpulse",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  •  "),
            Span::styled(status, Style::default().fg(Color::Yellow)),
            Span::raw("  •  "),
            Span::raw(summary),
        ]),
        Line::from(vec![
            Span::styled("filters ", Style::default().fg(Color::DarkGray)),
            Span::raw(&app.filter_summary),
        ]),
    ]);

    let block =
        Paragraph::new(text).block(Block::default().borders(Borders::ALL).title("Overview"));
    frame.render_widget(block, area);
}

fn render_table(frame: &mut Frame, area: Rect, app: &App) {
    let header = Row::new(vec![
        Cell::from("time"),
        Cell::from("kind"),
        Cell::from("session"),
        Cell::from("agent"),
        Cell::from("tool"),
        Cell::from("status"),
        Cell::from("preview"),
    ])
    .style(
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );

    let rows = app.items.iter().map(|item| timeline_row(item));

    let widths = [
        Constraint::Length(9),
        Constraint::Length(7),
        Constraint::Length(20),
        Constraint::Length(12),
        Constraint::Length(12),
        Constraint::Length(16),
        Constraint::Min(20),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title("Timeline"))
        .row_highlight_style(
            Style::default()
                .bg(Color::Rgb(35, 43, 60))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(table, area, &mut app.state.clone());
}

fn render_detail(frame: &mut Frame, area: Rect, app: &App) {
    let detail = app.selected_item().map(detail_text).unwrap_or_else(|| {
        Text::from(vec![Line::from(
            "No events yet — waiting for log activity.",
        )])
    });

    let paragraph = Paragraph::new(detail)
        .block(Block::default().borders(Borders::ALL).title("Details"))
        .wrap(Wrap { trim: false })
        .scroll((app.detail_scroll, 0));

    frame.render_widget(paragraph, area);
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let footer = Paragraph::new(Line::from(vec![
        Span::styled(
            "q",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" quit  "),
        Span::styled(
            "j/k or ↑/↓",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" select  "),
        Span::styled(
            "f",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(if app.follow_tail {
            " freeze tail  "
        } else {
            " resume tail  "
        }),
        Span::styled(
            "PgUp/PgDn",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" detail scroll  "),
        Span::styled(
            "g/G",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" newest/oldest"),
    ]))
    .block(Block::default().borders(Borders::ALL));

    frame.render_widget(footer, area);
}

fn timeline_row(item: &TimelineItem) -> Row<'static> {
    let time = format_ts(item.seen_at());
    let kind = item.kind_label().to_string();
    let session = truncate_display(&item.session_label(), 20);
    let agent = truncate_display(&item.agent_label(), 12);
    let tool_name = item.tool_label();
    let tool = truncate_display(&tool_name, 12);
    let status = truncate_display(&item.status_label(), 16);
    let preview = truncate_display(&item.preview(), PREVIEW_LEN);

    Row::new(vec![
        Cell::from(time),
        Cell::from(kind).style(kind_style(item)),
        Cell::from(session).style(Style::default().fg(Color::White)),
        Cell::from(agent).style(Style::default().fg(Color::Gray)),
        Cell::from(tool).style(
            Style::default()
                .fg(tool_color(&tool_name))
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from(status).style(status_style(item)),
        Cell::from(preview),
    ])
}

fn detail_text(item: &TimelineItem) -> Text<'static> {
    match item {
        TimelineItem::ToolEvent(event) => detail_tool_event(event),
        TimelineItem::StaleWarning { warning, seen_at } => Text::from(vec![
            title_line("STALE WARNING", Color::Yellow),
            kv_line("Seen", &seen_at.to_rfc3339()),
            kv_line("Session", warning.session_key.as_deref().unwrap_or("-")),
            kv_line("Tool", warning.tool_name.as_deref().unwrap_or("-")),
            kv_line("Call ID", &warning.call_id),
            kv_line("Age", &format!("{} seconds", warning.age_seconds)),
            kv_line(
                "Message",
                warning
                    .message
                    .as_deref()
                    .unwrap_or("Long-running tool call has not completed yet."),
            ),
        ]),
        TimelineItem::Heartbeat { summary, seen_at } => Text::from(vec![
            title_line("HEARTBEAT", Color::Cyan),
            kv_line("Seen", &seen_at.to_rfc3339()),
            kv_line("Active calls", &summary.active_calls.to_string()),
            kv_line("Stale calls", &summary.stale_calls.to_string()),
            kv_line("Active sessions", &summary.active_sessions.to_string()),
        ]),
        TimelineItem::Error { message, seen_at } => Text::from(vec![
            title_line("SYSTEM ERROR", Color::Red),
            kv_line("Seen", &seen_at.to_rfc3339()),
            kv_line("Message", message),
        ]),
    }
}

fn detail_tool_event(event: &NormalizedEvent) -> Text<'static> {
    let mut lines = vec![title_line(
        &format!(
            "{} {}",
            event.kind_label(),
            event.tool_name.as_deref().unwrap_or("tool event")
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
    lines.push(kv_line("Status", event.status.as_deref().unwrap_or("-")));
    lines.push(kv_line("Level", severity_label(event.level)));
    if let Some(call_id) = &event.call_id {
        lines.push(kv_line("Call ID", call_id));
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
        for (key, value) in &event.params {
            lines.push(kv_line(key, value));
        }
    }

    lines.push(section_header("Raw payload"));
    lines.extend(multiline_lines(
        &pretty_raw_json(&event.raw_line),
        Color::Gray,
    ));

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
            ToolEventKind::Malformed => "MALFORMED",
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

fn pretty_raw_json(raw: &str) -> String {
    match serde_json::from_str::<Value>(raw) {
        Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_else(|_| raw.to_string()),
        Err(_) => raw.to_string(),
    }
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
                return format!("command {command}");
            }
        }
    }

    if !event.params.is_empty() {
        let rendered = event
            .params
            .iter()
            .take(3)
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join("  ");
        return rendered;
    }

    event
        .result_summary
        .as_ref()
        .or(event.message.as_ref())
        .cloned()
        .unwrap_or_else(|| event.raw_line.clone())
}

fn format_ts(ts: DateTime<Utc>) -> String {
    ts.with_timezone(&Local).format("%H:%M:%S").to_string()
}

fn kind_style(item: &TimelineItem) -> Style {
    Style::default()
        .fg(match item {
            TimelineItem::ToolEvent(event) => kind_color(&event.kind),
            TimelineItem::StaleWarning { .. } => Color::Yellow,
            TimelineItem::Heartbeat { .. } => Color::Cyan,
            TimelineItem::Error { .. } => Color::Red,
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
    match item {
        TimelineItem::ToolEvent(event) => {
            let status = event
                .status
                .as_deref()
                .or(event.result_summary.as_deref())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if status.contains("error") || status.contains("fail") {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            } else if status.contains("ok")
                || status.contains("success")
                || status.contains("complete")
                || status.contains("done")
            {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::Yellow)
            }
        }
        TimelineItem::StaleWarning { .. } => Style::default().fg(Color::Yellow),
        TimelineItem::Heartbeat { .. } => Style::default().fg(Color::Cyan),
        TimelineItem::Error { .. } => Style::default().fg(Color::Red),
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

fn discover_initial_session_logs() -> Vec<PathBuf> {
    let home_dir = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from);

    match home_dir {
        Some(home) => {
            let root = home.join(".openclaw");
            discovery::discover_session_logs(&root).unwrap_or_else(|_| Vec::new())
        }
        None => Vec::new(),
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
    parts.push(format!("min-level={}", severity_label(args.min_severity())));
    parts.push(if args.no_follow {
        "mode=one-shot".to_string()
    } else {
        "mode=follow".to_string()
    });

    if parts.is_empty() {
        "none".to_string()
    } else {
        parts.join("  •  ")
    }
}
