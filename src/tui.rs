use crate::cli::Args;
use crate::daemon;
use crate::event::{NormalizedEvent, Severity, TimeFilter, ToolEventKind};
use crate::history::{PersistedEvent, PersistedHistory, HISTORY_LIMIT};
use crate::projection::{
    call_activity_at, CallStatus, CorrelatedCall, EventRow, HealthConfig, HealthStatus,
    MatchConfidence, ProjectionFilter, ProjectionStore, SessionLabelKind, SessionSummary,
    SourceState,
};
use crate::session_label::{SessionLabelInput, SessionLabelResolver};
use crate::stale::{HeartbeatSummary, StaleTracker, StaleWarning};
use chrono::{DateTime, Local, Utc};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Row, Table, TableState, Wrap};
use ratatui::{Frame, Terminal};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::{self, stdout};
use std::time::{Duration, Instant};

const DRAIN_PER_TICK: usize = 128;
const PREVIEW_LEN: usize = 72;
const SESSION_LABEL_CACHE_TTL_MINUTES: i64 = 15;
const SCROLL_OFF: usize = 5;
const UI_TICK: Duration = Duration::from_millis(50);
const MAX_INPUT_DRAIN: usize = 512;
const SESSION_LABEL_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Tab {
    Sessions,
    Calls,
    Events,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::Sessions, Tab::Calls, Tab::Events];

    fn title(self) -> &'static str {
        match self {
            Tab::Calls => "Tool Calls",
            Tab::Sessions => "Sessions",
            Tab::Events => "Events",
        }
    }

    fn short_title(self) -> &'static str {
        match self {
            Tab::Calls => "Tool Calls",
            Tab::Sessions => "Sessions",
            Tab::Events => "Events",
        }
    }

    fn index(self) -> usize {
        match self {
            Tab::Sessions => 0,
            Tab::Calls => 1,
            Tab::Events => 2,
        }
    }

    fn previous(self) -> Self {
        match self {
            Tab::Sessions => Tab::Events,
            Tab::Calls => Tab::Sessions,
            Tab::Events => Tab::Calls,
        }
    }

    fn next(self) -> Self {
        match self {
            Tab::Sessions => Tab::Calls,
            Tab::Calls => Tab::Events,
            Tab::Events => Tab::Sessions,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum EntityKey {
    Event(String),
    Notice(String),
    Call(String),
    Session(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FollowMode {
    Follow,
    Browse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputOutcome {
    Idle,
    Redraw,
    Quit,
}

#[derive(Default)]
struct PendingInput {
    row_delta: isize,
    scroll_delta: isize,
    saw_resize: bool,
    changed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpsPreset {
    Live,
    StaleOnly,
    Errors,
    System,
    Recent15m,
}

impl OpsPreset {
    const SELECTABLE: [OpsPreset; 5] = [
        OpsPreset::Live,
        OpsPreset::StaleOnly,
        OpsPreset::Errors,
        OpsPreset::System,
        OpsPreset::Recent15m,
    ];

    fn label(self) -> &'static str {
        match self {
            OpsPreset::Live => "Live",
            OpsPreset::StaleOnly => "Stale",
            OpsPreset::Errors => "Errors",
            OpsPreset::System => "System",
            OpsPreset::Recent15m => "Recent 15m",
        }
    }

    fn key(self) -> Option<char> {
        match self {
            OpsPreset::Live => Some('1'),
            OpsPreset::StaleOnly => Some('2'),
            OpsPreset::Errors => Some('3'),
            OpsPreset::System => Some('4'),
            OpsPreset::Recent15m => Some('5'),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct DrilldownScope {
    session_id: Option<String>,
    call_entity_id: Option<String>,
}

#[derive(Clone, Debug)]
struct TabStateModel {
    selected: Option<EntityKey>,
    scroll_offset: usize,
    follow_mode: FollowMode,
    unseen_count: usize,
    scope: DrilldownScope,
}

impl Default for TabStateModel {
    fn default() -> Self {
        Self {
            selected: None,
            scroll_offset: 0,
            follow_mode: FollowMode::Follow,
            unseen_count: 0,
            scope: DrilldownScope::default(),
        }
    }
}

#[derive(Clone, Debug)]
struct RouteSnapshot {
    current_tab: Tab,
    tabs: [TabStateModel; 3],
}

#[derive(Clone, Debug)]
struct DetailState {
    entity: EntityKey,
    scroll: usize,
}

#[derive(Clone, Debug)]
struct WorkspaceFilters {
    session: Option<String>,
    agent: Option<String>,
    tool: Option<String>,
    min_level: Severity,
    base_min_level: Severity,
    time: TimeFilter,
    base_time: TimeFilter,
    include_system_events: bool,
    stale_only: bool,
    text_search: Option<String>,
    active_preset: OpsPreset,
    summary: String,
}

impl WorkspaceFilters {
    fn from_args(args: &Args, time: TimeFilter) -> Self {
        Self {
            session: args.session.clone(),
            agent: args.agent.clone(),
            tool: args.tool.clone(),
            min_level: args.min_severity(),
            base_min_level: args.min_severity(),
            time: time.clone(),
            base_time: time,
            include_system_events: false,
            stale_only: false,
            text_search: None,
            active_preset: OpsPreset::Live,
            summary: format_filters(args),
        }
    }

    fn projection_filter(&self) -> ProjectionFilter {
        ProjectionFilter {
            session: self.session.clone(),
            agent: self.agent.clone(),
            tool: self.tool.clone(),
            min_level: self.min_level,
            time: self.time.clone(),
        }
    }
}

#[derive(Clone, Debug)]
enum NoticeKind {
    Stale(StaleWarning),
    Heartbeat(HeartbeatSummary),
    #[allow(dead_code)]
    Error(String),
}

#[derive(Clone, Debug)]
struct NoticeRecord {
    id: String,
    seen_at: DateTime<Utc>,
    kind: NoticeKind,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
struct VisibleRow {
    key: EntityKey,
    cells: Vec<String>,
    searchable: String,
}

#[derive(Clone, Debug)]
struct VisibleRowItem {
    key: EntityKey,
    searchable: String,
    sort_at: DateTime<Utc>,
    cells: VisibleRowCells,
}

#[derive(Clone, Debug)]
enum VisibleRowCells {
    Event {
        row: EventRow,
        display_label: String,
    },
    Notice(Vec<String>),
    Call {
        call: CorrelatedCall,
        display_label: String,
    },
    Session {
        session: SessionSummary,
        display_label: String,
        call_health: String,
        source_state: String,
    },
}

impl VisibleRowItem {
    fn to_visible_row(&self) -> VisibleRow {
        VisibleRow {
            key: self.key.clone(),
            cells: self.cells(),
            searchable: self.searchable.clone(),
        }
    }

    fn cells(&self) -> Vec<String> {
        match &self.cells {
            VisibleRowCells::Event { row, display_label } => vec![
                format_ts(row.timestamp.unwrap_or(self.sort_at)),
                kind_label(&row.kind).to_string(),
                truncate_display(row.agent_id.as_deref().unwrap_or("-"), 10),
                truncate_display(display_label, 20),
                truncate_display(row.tool_name.as_deref().unwrap_or("-"), 14),
                truncate_display(&event_call_label(row), 12),
                truncate_display(row.status.as_deref().unwrap_or("-"), 14),
                truncate_display(row.preview.as_deref().unwrap_or("-"), PREVIEW_LEN),
            ],
            VisibleRowCells::Notice(cells) => cells.clone(),
            VisibleRowCells::Call {
                call,
                display_label,
            } => vec![
                format_ts(call_activity_at(call).unwrap_or(self.sort_at)),
                call_status_label(call.status.clone()).to_string(),
                confidence_label(&call.match_confidence).to_string(),
                truncate_display(call.agent_id.as_deref().unwrap_or("-"), 12),
                truncate_display(display_label, 20),
                truncate_display(call.tool_name.as_deref().unwrap_or("-"), 14),
                truncate_display(&call_label(call), 12),
                match call.duration_ms {
                    Some(ms) => format!("{ms}ms"),
                    None => "-".to_string(),
                },
                truncate_display(call.message_preview.as_deref().unwrap_or("-"), PREVIEW_LEN),
            ],
            VisibleRowCells::Session {
                session,
                display_label,
                call_health,
                source_state,
            } => vec![
                format_ts(session.last_activity_at.unwrap_or(self.sort_at)),
                truncate_display(session.agent_id.as_deref().unwrap_or("-"), 12),
                truncate_display(display_label, 22),
                call_health.clone(),
                source_state.clone(),
                session.open_call_count.to_string(),
                session.stale_call_count.to_string(),
                severity_label(session.derived_severity).to_string(),
            ],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActiveLayer {
    Workspace,
    Detail,
    Help,
    Presets,
    Search,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Close,
    Quit,
    GotoTopPrefix,
    NextRow,
    PreviousRow,
    FirstRow,
    LastRow,
    ScrollDown,
    ScrollUp,
    ResumeLive,
    PreviousTab,
    NextTab,
    TabEvents,
    TabCalls,
    TabSessions,
    Activate,
    OpenDetail,
    ToggleHelp,
    TogglePresets,
    OpenSearch,
    ToggleStaleOnly,
    ApplyPreset(OpsPreset),
}

#[derive(Clone, Copy)]
enum KeyMatcher {
    Char(char),
    Up,
    Down,
    PageUp,
    PageDown,
    End,
    Esc,
    Enter,
    Backspace,
}

impl KeyMatcher {
    fn matches(self, key: &KeyEvent) -> bool {
        match self {
            KeyMatcher::Char(expected) => {
                matches!(key.code, KeyCode::Char(actual) if actual == expected)
            }
            KeyMatcher::Up => key.code == KeyCode::Up,
            KeyMatcher::Down => key.code == KeyCode::Down,
            KeyMatcher::PageUp => key.code == KeyCode::PageUp,
            KeyMatcher::PageDown => key.code == KeyCode::PageDown,
            KeyMatcher::End => key.code == KeyCode::End,
            KeyMatcher::Esc => key.code == KeyCode::Esc,
            KeyMatcher::Enter => key.code == KeyCode::Enter,
            KeyMatcher::Backspace => key.code == KeyCode::Backspace,
        }
    }

    fn label(self) -> &'static str {
        match self {
            KeyMatcher::Char(value) => match value {
                '?' => "?",
                ' ' => "Space",
                _ => Box::leak(value.to_string().into_boxed_str()),
            },
            KeyMatcher::Up => "Up",
            KeyMatcher::Down => "Down",
            KeyMatcher::PageUp => "PgUp",
            KeyMatcher::PageDown => "PgDn",
            KeyMatcher::End => "End",
            KeyMatcher::Esc => "Esc",
            KeyMatcher::Enter => "Enter",
            KeyMatcher::Backspace => "Backspace",
        }
    }
}

#[derive(Clone, Copy)]
struct KeyBinding {
    matcher: KeyMatcher,
    action: Action,
    description: &'static str,
    layers: &'static [ActiveLayer],
    tabs: &'static [Tab],
}

const WORKSPACE_ONLY: &[ActiveLayer] = &[ActiveLayer::Workspace];
const HELP_ONLY: &[ActiveLayer] = &[ActiveLayer::Help];
const WORKSPACE_AND_DETAIL: &[ActiveLayer] = &[ActiveLayer::Workspace, ActiveLayer::Detail];
const PRESETS_ONLY: &[ActiveLayer] = &[ActiveLayer::Presets];
const SEARCH_ONLY: &[ActiveLayer] = &[ActiveLayer::Search];
const ALL_TABS: &[Tab] = &[Tab::Sessions, Tab::Calls, Tab::Events];
const EVENTS_ONLY: &[Tab] = &[Tab::Events];

const KEY_BINDINGS: &[KeyBinding] = &[
    KeyBinding {
        matcher: KeyMatcher::Char('q'),
        action: Action::Quit,
        description: "Quit",
        layers: &[
            ActiveLayer::Workspace,
            ActiveLayer::Detail,
            ActiveLayer::Help,
            ActiveLayer::Presets,
            ActiveLayer::Search,
        ],
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Esc,
        action: Action::Close,
        description: "Back",
        layers: &[
            ActiveLayer::Workspace,
            ActiveLayer::Detail,
            ActiveLayer::Help,
        ],
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Char('?'),
        action: Action::ToggleHelp,
        description: "Toggle contextual help",
        layers: WORKSPACE_AND_DETAIL,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Char('p'),
        action: Action::TogglePresets,
        description: "Open presets",
        layers: WORKSPACE_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Char('/'),
        action: Action::OpenSearch,
        description: "Search visible rows",
        layers: WORKSPACE_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Char('s'),
        action: Action::ToggleStaleOnly,
        description: "Toggle stale-only",
        layers: WORKSPACE_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Char('j'),
        action: Action::NextRow,
        description: "Move selection down",
        layers: WORKSPACE_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Down,
        action: Action::NextRow,
        description: "Move selection down",
        layers: WORKSPACE_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Char('k'),
        action: Action::PreviousRow,
        description: "Move selection up",
        layers: WORKSPACE_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Up,
        action: Action::PreviousRow,
        description: "Move selection up",
        layers: WORKSPACE_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Char('g'),
        action: Action::GotoTopPrefix,
        description: "Jump to top (press again)",
        layers: WORKSPACE_AND_DETAIL,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Char('G'),
        action: Action::LastRow,
        description: "Jump to oldest row",
        layers: WORKSPACE_AND_DETAIL,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::End,
        action: Action::LastRow,
        description: "Jump to oldest row",
        layers: WORKSPACE_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Char('h'),
        action: Action::PreviousTab,
        description: "Previous tab",
        layers: WORKSPACE_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Char('l'),
        action: Action::NextTab,
        description: "Next tab",
        layers: WORKSPACE_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Char('1'),
        action: Action::TabSessions,
        description: "Jump to Sessions",
        layers: WORKSPACE_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Char('2'),
        action: Action::TabCalls,
        description: "Jump to Tool Calls",
        layers: WORKSPACE_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Char('3'),
        action: Action::TabEvents,
        description: "Jump to Events",
        layers: WORKSPACE_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Char('f'),
        action: Action::ResumeLive,
        description: "Resume FOLLOW",
        layers: WORKSPACE_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Enter,
        action: Action::Activate,
        description: "Drill in or open detail",
        layers: WORKSPACE_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Char('o'),
        action: Action::OpenDetail,
        description: "Open fullscreen detail",
        layers: WORKSPACE_ONLY,
        tabs: EVENTS_ONLY,
    },
    KeyBinding {
        matcher: KeyMatcher::Char('j'),
        action: Action::ScrollDown,
        description: "Scroll detail/help down",
        layers: &[ActiveLayer::Detail, ActiveLayer::Help],
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Down,
        action: Action::ScrollDown,
        description: "Scroll detail/help down",
        layers: &[ActiveLayer::Detail, ActiveLayer::Help],
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::PageDown,
        action: Action::ScrollDown,
        description: "Scroll detail/help down",
        layers: &[ActiveLayer::Detail, ActiveLayer::Help],
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Char('k'),
        action: Action::ScrollUp,
        description: "Scroll detail/help up",
        layers: &[ActiveLayer::Detail, ActiveLayer::Help],
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Up,
        action: Action::ScrollUp,
        description: "Scroll detail/help up",
        layers: &[ActiveLayer::Detail, ActiveLayer::Help],
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::PageUp,
        action: Action::ScrollUp,
        description: "Scroll detail/help up",
        layers: &[ActiveLayer::Detail, ActiveLayer::Help],
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Char('?'),
        action: Action::Close,
        description: "Close help",
        layers: HELP_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Esc,
        action: Action::Close,
        description: "Close presets",
        layers: PRESETS_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Char('p'),
        action: Action::Close,
        description: "Close presets",
        layers: PRESETS_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Char('1'),
        action: Action::ApplyPreset(OpsPreset::Live),
        description: "Preset: Live",
        layers: PRESETS_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Char('2'),
        action: Action::ApplyPreset(OpsPreset::StaleOnly),
        description: "Preset: Stale only",
        layers: PRESETS_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Char('3'),
        action: Action::ApplyPreset(OpsPreset::Errors),
        description: "Preset: Errors",
        layers: PRESETS_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Char('4'),
        action: Action::ApplyPreset(OpsPreset::System),
        description: "Preset: System",
        layers: PRESETS_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Char('5'),
        action: Action::ApplyPreset(OpsPreset::Recent15m),
        description: "Preset: Recent 15m",
        layers: PRESETS_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Esc,
        action: Action::Close,
        description: "Cancel search",
        layers: SEARCH_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Enter,
        action: Action::Close,
        description: "Apply search",
        layers: SEARCH_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Backspace,
        action: Action::Close,
        description: "Erase search character",
        layers: SEARCH_ONLY,
        tabs: ALL_TABS,
    },
];

struct App {
    store: ProjectionStore,
    events_by_ref: HashMap<String, NormalizedEvent>,
    session_labels: SessionLabelResolver,
    notices: Vec<NoticeRecord>,
    latest_heartbeat: Option<HeartbeatSummary>,
    current_tab: Tab,
    tabs: [TabStateModel; 3],
    route_stack: Vec<RouteSnapshot>,
    detail: Option<DetailState>,
    help_open: bool,
    presets_open: bool,
    search_open: bool,
    search_draft: String,
    help_scroll: usize,
    awaiting_gg: bool,
    filters: WorkspaceFilters,
    detail_view_width: u16,
    detail_view_height: u16,
    help_view_width: u16,
    help_view_height: u16,
    health: HealthConfig,
    next_notice_id: u64,
    now: DateTime<Utc>,
}

impl App {
    fn new(filters: WorkspaceFilters, stale_after_seconds: u64) -> Self {
        Self::with_session_labels(
            filters,
            stale_after_seconds,
            SessionLabelResolver::from_env(chrono::Duration::minutes(
                SESSION_LABEL_CACHE_TTL_MINUTES,
            )),
        )
    }

    fn with_session_labels(
        filters: WorkspaceFilters,
        stale_after_seconds: u64,
        session_labels: SessionLabelResolver,
    ) -> Self {
        Self {
            store: ProjectionStore::default(),
            events_by_ref: HashMap::new(),
            session_labels,
            notices: Vec::new(),
            latest_heartbeat: None,
            current_tab: Tab::Sessions,
            tabs: [
                TabStateModel::default(),
                TabStateModel::default(),
                TabStateModel::default(),
            ],
            route_stack: Vec::new(),
            detail: None,
            help_open: false,
            presets_open: false,
            search_open: false,
            search_draft: String::new(),
            help_scroll: 0,
            awaiting_gg: false,
            filters,
            detail_view_width: 0,
            detail_view_height: 0,
            help_view_width: 0,
            help_view_height: 0,
            health: HealthConfig {
                stale_after: chrono::Duration::seconds(stale_after_seconds as i64),
                ..HealthConfig::default()
            },
            next_notice_id: 0,
            now: Utc::now(),
        }
    }

    fn layer(&self) -> ActiveLayer {
        if self.help_open {
            ActiveLayer::Help
        } else if self.presets_open {
            ActiveLayer::Presets
        } else if self.search_open {
            ActiveLayer::Search
        } else if self.detail.is_some() {
            ActiveLayer::Detail
        } else {
            ActiveLayer::Workspace
        }
    }

    fn current_tab_state(&self) -> &TabStateModel {
        &self.tabs[self.current_tab.index()]
    }

    fn current_tab_state_mut(&mut self) -> &mut TabStateModel {
        &mut self.tabs[self.current_tab.index()]
    }

    fn tab_state(&self, tab: Tab) -> &TabStateModel {
        &self.tabs[tab.index()]
    }

    fn tab_state_mut(&mut self, tab: Tab) -> &mut TabStateModel {
        &mut self.tabs[tab.index()]
    }

    fn projection_filter(&self) -> ProjectionFilter {
        self.filters.projection_filter()
    }

    fn apply_filter_change(&mut self, update: impl FnOnce(&mut WorkspaceFilters, DateTime<Utc>)) {
        let before = self.snapshot_visible_keys();
        update(&mut self.filters, self.now);
        self.reconcile_after_data_change(before);
    }

    fn apply_preset(&mut self, preset: OpsPreset) {
        self.apply_filter_change(|filters, now| {
            filters.min_level = filters.base_min_level;
            filters.time = filters.base_time.clone();
            filters.include_system_events = false;
            filters.stale_only = false;
            filters.active_preset = preset;

            match preset {
                OpsPreset::Live => {}
                OpsPreset::StaleOnly => {
                    filters.stale_only = true;
                }
                OpsPreset::Errors => {
                    filters.min_level = Severity::Error;
                    filters.include_system_events = true;
                }
                OpsPreset::System => {
                    filters.include_system_events = true;
                }
                OpsPreset::Recent15m => {
                    filters.time.since = Some(now - chrono::Duration::minutes(15));
                    filters.time.until = None;
                }
            }
        });
    }

    fn toggle_stale_only(&mut self) {
        self.apply_filter_change(|filters, _now| {
            filters.stale_only = !filters.stale_only;
            filters.active_preset = if filters.stale_only {
                OpsPreset::StaleOnly
            } else {
                OpsPreset::Live
            };
        });
    }

    fn open_search(&mut self) {
        self.search_draft = self.filters.text_search.clone().unwrap_or_default();
        self.search_open = true;
        self.presets_open = false;
        self.help_open = false;
    }

    fn apply_search(&mut self) {
        let query = self.search_draft.trim().to_string();
        self.search_open = false;
        self.apply_filter_change(|filters, _now| {
            filters.text_search = (!query.is_empty()).then_some(query);
        });
    }

    fn cancel_search(&mut self) {
        self.search_open = false;
        self.search_draft.clear();
    }

    #[allow(dead_code)]
    fn ingest_event(&mut self, event: NormalizedEvent, observed_at: DateTime<Utc>) {
        let before = self.snapshot_visible_keys();
        self.ingest_event_record(event, observed_at);
        self.reconcile_after_data_change(before);
    }

    fn ingest_event_record(&mut self, event: NormalizedEvent, observed_at: DateTime<Utc>) {
        if let Some(input) = SessionLabelInput::from_event(&event) {
            self.session_labels.observe_session(input, observed_at);
        }
        let event_ref = self.store.ingest_event(event.clone(), observed_at);
        self.events_by_ref.insert(event_ref, event);
        self.now = observed_at;
    }

    fn event_count(&self) -> usize {
        self.store.event_count()
    }

    fn refresh_session_labels(&mut self, now: DateTime<Utc>) -> bool {
        self.session_labels.refresh(now)
    }

    #[allow(dead_code)]
    fn ingest_warning(&mut self, warning: StaleWarning, observed_at: DateTime<Utc>) {
        let before = self.snapshot_visible_keys();
        self.ingest_notice_record(NoticeKind::Stale(warning), observed_at);
        self.reconcile_after_data_change(before);
    }

    fn ingest_heartbeat(&mut self, summary: HeartbeatSummary, observed_at: DateTime<Utc>) {
        let before = self.snapshot_visible_keys();
        self.latest_heartbeat = Some(summary.clone());
        self.ingest_notice_record(NoticeKind::Heartbeat(summary), observed_at);
        self.reconcile_after_data_change(before);
    }

    #[allow(dead_code)]
    fn ingest_error(&mut self, message: impl Into<String>, observed_at: DateTime<Utc>) {
        let before = self.snapshot_visible_keys();
        self.ingest_notice_record(NoticeKind::Error(message.into()), observed_at);
        self.reconcile_after_data_change(before);
    }

    fn ingest_notice_record(&mut self, kind: NoticeKind, seen_at: DateTime<Utc>) {
        self.push_notice(kind, seen_at);
        self.now = seen_at;
    }

    fn push_notice(&mut self, kind: NoticeKind, seen_at: DateTime<Utc>) {
        self.next_notice_id = self.next_notice_id.saturating_add(1);
        self.notices.push(NoticeRecord {
            id: format!("notice-{}", self.next_notice_id),
            seen_at,
            kind,
        });
    }

    fn snapshot_visible_keys(&self) -> HashMap<Tab, Vec<EntityKey>> {
        Tab::ALL
            .into_iter()
            .map(|tab| (tab, self.visible_keys(tab)))
            .collect()
    }

    fn reconcile_after_data_change(&mut self, before: HashMap<Tab, Vec<EntityKey>>) {
        for tab in Tab::ALL {
            let after_keys = self.visible_keys(tab);
            let state = self.tab_state_mut(tab);
            if after_keys.is_empty() {
                state.selected = None;
                state.scroll_offset = 0;
                state.unseen_count = 0;
                continue;
            }

            if state.follow_mode == FollowMode::Follow {
                state.selected = after_keys.first().cloned();
                state.scroll_offset = 0;
                state.unseen_count = 0;
                continue;
            }

            if let Some(previous) = before.get(&tab) {
                let previous_set = previous.iter().cloned().collect::<HashSet<_>>();
                state.unseen_count = state.unseen_count.saturating_add(
                    after_keys
                        .iter()
                        .filter(|key| !previous_set.contains(*key))
                        .count(),
                );
            }

            let Some(selected) = state.selected.clone() else {
                state.selected = after_keys.first().cloned();
                state.scroll_offset = 0;
                continue;
            };

            let Some(after_index) = after_keys
                .iter()
                .position(|candidate| candidate == &selected)
            else {
                state.selected = after_keys.first().cloned();
                state.scroll_offset = 0;
                continue;
            };

            if let Some(previous_index) = before
                .get(&tab)
                .and_then(|previous| previous.iter().position(|candidate| candidate == &selected))
            {
                if after_index >= previous_index {
                    state.scroll_offset = state
                        .scroll_offset
                        .saturating_add(after_index - previous_index);
                } else {
                    state.scroll_offset = state
                        .scroll_offset
                        .saturating_sub(previous_index - after_index);
                }
            }
        }
    }

    fn visible_rows(&self, tab: Tab) -> Vec<VisibleRow> {
        self.visible_row_items(tab)
            .iter()
            .map(VisibleRowItem::to_visible_row)
            .collect()
    }

    fn visible_keys(&self, tab: Tab) -> Vec<EntityKey> {
        self.visible_row_items(tab)
            .into_iter()
            .map(|row| row.key)
            .collect()
    }

    fn visible_row_items(&self, tab: Tab) -> Vec<VisibleRowItem> {
        match tab {
            Tab::Events => self.visible_event_row_items(),
            Tab::Calls => self.visible_call_row_items(),
            Tab::Sessions => self.visible_session_row_items(),
        }
    }

    fn visible_event_row_items(&self) -> Vec<VisibleRowItem> {
        let state = self.tab_state(Tab::Events);
        let scope = &state.scope;
        let call_scope = scope.call_entity_id.as_ref().and_then(|call_id| {
            self.store
                .correlated_calls(&self.projection_filter(), self.now, &self.health)
                .into_iter()
                .find(|call| &call.call_entity_id == call_id)
        });
        let allowed_refs = call_scope.as_ref().map(|call| {
            call.event_refs_start
                .iter()
                .chain(call.event_refs_result.iter())
                .chain(call.event_refs_related.iter())
                .cloned()
                .collect::<HashSet<_>>()
        });

        let mut rows = self
            .store
            .event_rows(&self.projection_filter())
            .into_iter()
            .filter(|row| {
                let display_label = self
                    .display_session_label(row.session_id.as_deref(), row.session_label.as_deref());
                if self.filters.stale_only {
                    return false;
                }
                if let Some(session_id) = scope.session_id.as_ref() {
                    if row.session_id.as_deref() != Some(session_id.as_str()) {
                        return false;
                    }
                }
                if let Some(allowed_refs) = allowed_refs.as_ref() {
                    if !allowed_refs.contains(&row.event_ref) {
                        return false;
                    }
                }
                let call_label = event_call_label(row);
                self.matches_text_search(&[
                    display_label.as_str(),
                    row.agent_id.as_deref().unwrap_or_default(),
                    row.tool_name.as_deref().unwrap_or_default(),
                    call_label.as_str(),
                    row.status.as_deref().unwrap_or_default(),
                    row.preview.as_deref().unwrap_or_default(),
                ])
            })
            .map(|row| {
                let display_label = self.display_event_label(&row);
                let sort_at = row.timestamp.unwrap_or(self.now);
                VisibleRowItem {
                    key: EntityKey::Event(row.event_ref.clone()),
                    searchable: [
                        display_label.clone(),
                        row.agent_id.clone().unwrap_or_default(),
                        row.tool_name.clone().unwrap_or_default(),
                        event_call_label(&row),
                        row.status.clone().unwrap_or_default(),
                        row.preview.clone().unwrap_or_default(),
                    ]
                    .join(" "),
                    sort_at,
                    cells: VisibleRowCells::Event { row, display_label },
                }
            })
            .collect::<Vec<_>>();

        rows.extend(self.visible_notice_row_items(scope));

        rows.sort_by(|a, b| {
            b.sort_at
                .cmp(&a.sort_at)
                .then_with(|| a.searchable.cmp(&b.searchable))
        });
        rows
    }

    fn visible_notice_row_items(&self, scope: &DrilldownScope) -> Vec<VisibleRowItem> {
        let scoped_call = scope
            .call_entity_id
            .as_ref()
            .and_then(|call_id| self.call_by_id(call_id));
        self.notices
            .iter()
            .filter_map(|notice| match &notice.kind {
                NoticeKind::Stale(warning) => {
                    if let Some(session_id) = scope.session_id.as_ref() {
                        let in_session = warning.session_id.as_deref() == Some(session_id.as_str())
                            || warning.session_key.as_deref() == Some(session_id.as_str());
                        if !in_session {
                            return None;
                        }
                    }
                    if let Some(call) = scoped_call.as_ref() {
                        let matches_call_id = call.canonical_call_id.as_deref()
                            == Some(warning.call_id.as_str())
                            || call.call_entity_id == warning.call_id;
                        if !matches_call_id {
                            return None;
                        }
                    }
                    let searchable = format!(
                        "{} {} {}",
                        warning.session_key.as_deref().unwrap_or_default(),
                        warning.tool_name.as_deref().unwrap_or_default(),
                        warning.message.as_deref().unwrap_or_default()
                    );
                    if !self.matches_text_search(&[&searchable]) {
                        return None;
                    }
                    Some(VisibleRowItem {
                        key: EntityKey::Notice(notice.id.clone()),
                        cells: VisibleRowCells::Notice(vec![
                            format_ts(notice.seen_at),
                            "STALE".to_string(),
                            "-".to_string(),
                            truncate_display(warning.session_key.as_deref().unwrap_or("-"), 20),
                            truncate_display(warning.tool_name.as_deref().unwrap_or("-"), 14),
                            truncate_display(&warning.call_id, 12),
                            format!("{}s", warning.age_seconds),
                            truncate_display(
                                warning.message.as_deref().unwrap_or("Long-running call"),
                                PREVIEW_LEN,
                            ),
                        ]),
                        searchable,
                        sort_at: notice.seen_at,
                    })
                }
                NoticeKind::Heartbeat(summary)
                    if self.filters.include_system_events && !self.filters.stale_only =>
                {
                    let searchable = summary.to_line();
                    if !self.matches_text_search(&[&searchable]) {
                        return None;
                    }
                    Some(VisibleRowItem {
                        key: EntityKey::Notice(notice.id.clone()),
                        cells: VisibleRowCells::Notice(vec![
                            format_ts(notice.seen_at),
                            "HB".to_string(),
                            "-".to_string(),
                            "-".to_string(),
                            "heartbeat".to_string(),
                            "-".to_string(),
                            format!("a={} s={}", summary.active_calls, summary.stale_calls),
                            truncate_display(&summary.to_line(), PREVIEW_LEN),
                        ]),
                        searchable,
                        sort_at: notice.seen_at,
                    })
                }
                NoticeKind::Error(message)
                    if self.filters.include_system_events && !self.filters.stale_only =>
                {
                    if !self.matches_text_search(&[message]) {
                        return None;
                    }
                    Some(VisibleRowItem {
                        key: EntityKey::Notice(notice.id.clone()),
                        cells: VisibleRowCells::Notice(vec![
                            format_ts(notice.seen_at),
                            "ERR".to_string(),
                            "system".to_string(),
                            "-".to_string(),
                            "error".to_string(),
                            "-".to_string(),
                            "error".to_string(),
                            truncate_display(message, PREVIEW_LEN),
                        ]),
                        searchable: message.clone(),
                        sort_at: notice.seen_at,
                    })
                }
                _ => None,
            })
            .collect()
    }

    fn visible_call_row_items(&self) -> Vec<VisibleRowItem> {
        let state = self.tab_state(Tab::Calls);
        self.store
            .correlated_calls(&self.projection_filter(), self.now, &self.health)
            .into_iter()
            .filter(|call| {
                let display_label = self.display_session_label(
                    Some(call.session_id.as_str()),
                    Some(call.session_label.as_str()),
                );
                if let Some(session_id) = state.scope.session_id.as_ref() {
                    if &call.session_id != session_id {
                        return false;
                    }
                }
                if self.filters.stale_only && call.status != CallStatus::Stale {
                    return false;
                }
                self.matches_text_search(&[
                    &call.session_id,
                    &display_label,
                    call.agent_id.as_deref().unwrap_or_default(),
                    call.tool_name.as_deref().unwrap_or_default(),
                    call.canonical_call_id.as_deref().unwrap_or_default(),
                    confidence_label(&call.match_confidence),
                    call.message_preview.as_deref().unwrap_or_default(),
                ])
            })
            .map(|call| {
                let display_label = self.display_session_label(
                    Some(call.session_id.as_str()),
                    Some(call.session_label.as_str()),
                );
                let sort_at = call_activity_at(&call).unwrap_or(self.now);
                VisibleRowItem {
                    key: EntityKey::Call(call.call_entity_id.clone()),
                    searchable: [
                        display_label.clone(),
                        call.agent_id.clone().unwrap_or_default(),
                        call.tool_name.clone().unwrap_or_default(),
                        call_label(&call),
                        confidence_label(&call.match_confidence).to_string(),
                        call.message_preview.clone().unwrap_or_default(),
                    ]
                    .join(" "),
                    sort_at,
                    cells: VisibleRowCells::Call {
                        call,
                        display_label,
                    },
                }
            })
            .collect()
    }

    fn visible_session_row_items(&self) -> Vec<VisibleRowItem> {
        self.store
            .sessions(&self.projection_filter(), self.now, &self.health)
            .into_iter()
            .filter(|session| {
                let display_label = self.display_session_label(
                    Some(session.session_id.as_str()),
                    Some(session.session_label.as_str()),
                );
                if self.filters.stale_only
                    && session.health_status != HealthStatus::Stale
                    && session.health_status != HealthStatus::Disconnected
                {
                    return false;
                }
                self.matches_text_search(&[
                    &session.session_id,
                    &display_label,
                    session.agent_id.as_deref().unwrap_or_default(),
                ])
            })
            .map(|session| {
                let call_health = call_health_label(&session).to_string();
                let source_state = source_state_label(&session.source_state).to_string();
                let display_label = self.display_session_label(
                    Some(session.session_id.as_str()),
                    Some(session.session_label.as_str()),
                );
                let sort_at = session.last_activity_at.unwrap_or(self.now);
                VisibleRowItem {
                    key: EntityKey::Session(session.session_id.clone()),
                    searchable: [
                        display_label.clone(),
                        session.agent_id.clone().unwrap_or_default(),
                        call_health.clone(),
                        source_state.clone(),
                    ]
                    .join(" "),
                    sort_at,
                    cells: VisibleRowCells::Session {
                        session,
                        display_label,
                        call_health,
                        source_state,
                    },
                }
            })
            .collect()
    }

    fn matches_text_search(&self, haystacks: &[&str]) -> bool {
        let Some(query) = self.filters.text_search.as_ref() else {
            return true;
        };
        let needle = query.to_ascii_lowercase();
        haystacks
            .iter()
            .any(|value| value.to_ascii_lowercase().contains(&needle))
    }

    fn selected_item_index(&self, tab: Tab, rows: &[VisibleRowItem]) -> Option<usize> {
        self.tab_state(tab)
            .selected
            .as_ref()
            .and_then(|selected| rows.iter().position(|row| &row.key == selected))
            .or_else(|| (!rows.is_empty()).then_some(0))
    }

    fn resume_live(&mut self) {
        let rows = self.visible_row_items(self.current_tab);
        let state = self.current_tab_state_mut();
        state.follow_mode = FollowMode::Follow;
        state.unseen_count = 0;
        state.scroll_offset = 0;
        state.selected = rows.first().map(|row| row.key.clone());
    }

    fn move_selection(&mut self, delta: isize) {
        let rows = self.visible_row_items(self.current_tab);
        if rows.is_empty() {
            return;
        }
        let current_index = self
            .selected_item_index(self.current_tab, &rows)
            .unwrap_or(0) as isize;
        let next_index = (current_index + delta).clamp(0, rows.len().saturating_sub(1) as isize);
        let state = self.current_tab_state_mut();
        state.follow_mode = FollowMode::Browse;
        state.selected = rows.get(next_index as usize).map(|row| row.key.clone());
        state.scroll_offset = list_scroll_offset(next_index as usize, SCROLL_OFF);
    }

    fn scroll_active_layer(&mut self, delta: isize) {
        if self.help_open {
            let max = self.help_scroll_limit();
            self.help_scroll = clamp_signed_delta(self.help_scroll, delta, max);
        } else if let Some(max) = self.detail.as_ref().map(|_| self.detail_scroll_limit()) {
            if let Some(detail) = self.detail.as_mut() {
                detail.scroll = clamp_signed_delta(detail.scroll, delta, max);
            }
        }
    }

    fn jump_to(&mut self, index: usize) {
        let rows = self.visible_row_items(self.current_tab);
        if rows.is_empty() {
            return;
        }
        let target = index.min(rows.len().saturating_sub(1));
        let state = self.current_tab_state_mut();
        state.follow_mode = FollowMode::Browse;
        state.selected = rows.get(target).map(|row| row.key.clone());
        state.scroll_offset = list_scroll_offset(target, SCROLL_OFF);
    }

    fn switch_tab(&mut self, tab: Tab) {
        self.current_tab = tab;
        if self.current_tab_state().follow_mode == FollowMode::Follow {
            let rows = self.visible_row_items(tab);
            let target = self.tab_state_mut(tab);
            target.unseen_count = 0;
            target.scroll_offset = 0;
            target.selected = rows.first().map(|row| row.key.clone());
        }
    }

    fn push_route_snapshot(&mut self) {
        self.route_stack.push(RouteSnapshot {
            current_tab: self.current_tab,
            tabs: self.tabs.clone(),
        });
    }

    fn unwind_route(&mut self) -> bool {
        if self.help_open {
            self.help_open = false;
            self.help_scroll = 0;
            return false;
        }
        if self.detail.is_some() {
            self.detail = None;
            return false;
        }
        if let Some(snapshot) = self.route_stack.pop() {
            self.current_tab = snapshot.current_tab;
            self.tabs = snapshot.tabs;
            return false;
        }
        true
    }

    fn activate_selected(&mut self) {
        match self.current_tab {
            Tab::Sessions => {
                let Some(EntityKey::Session(session_id)) =
                    self.current_tab_state().selected.clone()
                else {
                    return;
                };
                self.push_route_snapshot();
                let scope = DrilldownScope {
                    session_id: Some(session_id),
                    call_entity_id: None,
                };
                let selected = {
                    let mut preview = self.tabs.clone();
                    preview[Tab::Calls.index()].scope = scope.clone();
                    preview[Tab::Calls.index()].follow_mode = FollowMode::Browse;
                    let original = std::mem::replace(&mut self.tabs, preview);
                    let rows = self.visible_rows(Tab::Calls);
                    self.tabs = original;
                    rows.first().map(|row| row.key.clone())
                };
                let target = self.tab_state_mut(Tab::Calls);
                target.scope = scope;
                target.follow_mode = FollowMode::Browse;
                target.unseen_count = 0;
                target.scroll_offset = 0;
                target.selected = selected;
                self.current_tab = Tab::Calls;
            }
            Tab::Calls => {
                let Some(EntityKey::Call(call_id)) = self.current_tab_state().selected.clone()
                else {
                    return;
                };
                let Some(call) = self.call_by_id(&call_id) else {
                    return;
                };
                self.push_route_snapshot();
                let scope = DrilldownScope {
                    session_id: Some(call.session_id.clone()),
                    call_entity_id: Some(call.call_entity_id.clone()),
                };
                let selected = {
                    let mut preview = self.tabs.clone();
                    preview[Tab::Events.index()].scope = scope.clone();
                    preview[Tab::Events.index()].follow_mode = FollowMode::Browse;
                    let original = std::mem::replace(&mut self.tabs, preview);
                    let rows = self.visible_rows(Tab::Events);
                    self.tabs = original;
                    rows.first().map(|row| row.key.clone())
                };
                let target = self.tab_state_mut(Tab::Events);
                target.scope = scope;
                target.follow_mode = FollowMode::Browse;
                target.unseen_count = 0;
                target.scroll_offset = 0;
                target.selected = selected;
                self.current_tab = Tab::Events;
            }
            Tab::Events => self.open_detail(),
        }
    }

    fn open_detail(&mut self) {
        self.awaiting_gg = false;
        if let Some(selected) = self.current_tab_state().selected.clone() {
            self.detail = Some(DetailState {
                entity: selected,
                scroll: 0,
            });
        }
    }

    fn selected_detail_entity(&self) -> Option<&EntityKey> {
        self.detail.as_ref().map(|detail| &detail.entity)
    }

    fn call_by_id(&self, call_id: &str) -> Option<CorrelatedCall> {
        self.store
            .correlated_calls(&self.projection_filter(), self.now, &self.health)
            .into_iter()
            .find(|call| call.call_entity_id == call_id)
    }

    fn session_by_id(&self, session_id: &str) -> Option<SessionSummary> {
        self.store
            .sessions(&self.projection_filter(), self.now, &self.health)
            .into_iter()
            .find(|session| session.session_id == session_id)
    }

    fn notice_by_id(&self, notice_id: &str) -> Option<&NoticeRecord> {
        self.notices.iter().find(|notice| notice.id == notice_id)
    }

    fn inspector_text(&self) -> Text<'static> {
        let entity = self
            .selected_detail_entity()
            .cloned()
            .or_else(|| self.current_tab_state().selected.clone());
        self.entity_text(entity.as_ref())
    }

    fn entity_text(&self, entity: Option<&EntityKey>) -> Text<'static> {
        let Some(entity) = entity else {
            return Text::from(vec![Line::from("No data yet.")]);
        };
        match entity {
            EntityKey::Event(event_ref) => self.event_text(event_ref),
            EntityKey::Notice(notice_id) => self.notice_text(notice_id),
            EntityKey::Call(call_id) => self.call_text(call_id),
            EntityKey::Session(session_id) => self.session_text(session_id),
        }
    }

    fn event_text(&self, event_ref: &str) -> Text<'static> {
        let Some(event) = self.events_by_ref.get(event_ref) else {
            return Text::from("Missing event");
        };
        let display_label = self
            .session_labels
            .state_for_event(event)
            .display()
            .to_string();
        let mut lines = vec![title_line(
            &format!(
                "{} {}",
                kind_label(&event.kind),
                event.tool_name.as_deref().unwrap_or("event")
            ),
            kind_color(&event.kind),
        )];
        lines.push(kv_line(
            "Timestamp",
            &event.timestamp.unwrap_or(self.now).to_rfc3339(),
        ));
        lines.push(kv_line("Surface", &display_label));
        if let Some(session_id) = event.session_id.as_deref() {
            lines.push(kv_line("Session ID", session_id));
        }
        if let Some(channel_id) = event.routing.channel_id.as_deref() {
            lines.push(kv_line("Discord Channel ID", channel_id));
        }
        lines.push(kv_line("Agent", event.agent_id.as_deref().unwrap_or("-")));
        lines.push(kv_line("Tool", event.tool_name.as_deref().unwrap_or("-")));
        lines.push(kv_line("Status", event.status.as_deref().unwrap_or("-")));
        lines.push(kv_line("Severity", severity_label(event.level)));
        if let Some(call_id) = event.call_id.as_deref() {
            lines.push(kv_line("Call ID", call_id));
        }
        if let Some(message) = event.message.as_deref() {
            lines.push(section_header("Message"));
            lines.extend(multiline_lines(message, Color::White));
        }
        if let Some(summary) = event.result_summary.as_deref() {
            lines.push(section_header("Result"));
            lines.extend(multiline_lines(summary, Color::White));
        }
        lines.push(section_header("Params"));
        if event.preferred_params().is_empty() {
            lines.push(Line::from(Span::styled(
                "(none)",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for (key, value) in event.preferred_params() {
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

    fn notice_text(&self, notice_id: &str) -> Text<'static> {
        let Some(notice) = self.notice_by_id(notice_id) else {
            return Text::from("Missing system notice");
        };
        match &notice.kind {
            NoticeKind::Stale(warning) => Text::from(vec![
                title_line("STALE WARNING", Color::Yellow),
                kv_line("Seen", &notice.seen_at.to_rfc3339()),
                kv_line("Session", warning.session_key.as_deref().unwrap_or("-")),
                kv_line("Tool", warning.tool_name.as_deref().unwrap_or("-")),
                kv_line("Call ID", &warning.call_id),
                kv_line("Age", &format!("{} seconds", warning.age_seconds)),
                kv_line(
                    "Message",
                    warning
                        .message
                        .as_deref()
                        .unwrap_or("Long-running tool call"),
                ),
            ]),
            NoticeKind::Heartbeat(summary) => Text::from(vec![
                title_line("HEARTBEAT", Color::Cyan),
                kv_line("Seen", &notice.seen_at.to_rfc3339()),
                kv_line("Active calls", &summary.active_calls.to_string()),
                kv_line("Stale calls", &summary.stale_calls.to_string()),
                kv_line("Active sessions", &summary.active_sessions.to_string()),
            ]),
            NoticeKind::Error(message) => Text::from(vec![
                title_line("SYSTEM ERROR", Color::Red),
                kv_line("Seen", &notice.seen_at.to_rfc3339()),
                kv_line("Message", message),
            ]),
        }
    }

    fn call_text(&self, call_id: &str) -> Text<'static> {
        let Some(call) = self.call_by_id(call_id) else {
            return Text::from("Missing call");
        };
        let display_label =
            self.display_session_label(Some(&call.session_id), Some(&call.session_label));
        Text::from(vec![
            title_line(
                &format!("CALL {}", call.tool_name.as_deref().unwrap_or("-")),
                tool_color(call.tool_name.as_deref().unwrap_or("call")),
            ),
            kv_line("Entity ID", &call.call_entity_id),
            kv_line("Surface", &display_label),
            kv_line("Session ID", &call.session_id),
            match call.session_label_info.kind {
                SessionLabelKind::DiscordChannelId => kv_line(
                    "Discord Channel ID",
                    call.session_label_info.channel_id.as_deref().unwrap_or("-"),
                ),
                SessionLabelKind::StableSessionId => kv_line("Session Label Source", "non-discord"),
            },
            kv_line("Status", call_status_label(call.status)),
            kv_line(
                "Confidence",
                match call.match_confidence {
                    MatchConfidence::ExplicitId => "explicit_id",
                    MatchConfidence::TranscriptBundle => "transcript_bundle",
                    MatchConfidence::FallbackSignature => "fallback_signature",
                },
            ),
            kv_line("Call ID", call.canonical_call_id.as_deref().unwrap_or("-")),
            kv_line(
                "Started",
                &call
                    .started_at
                    .map(|value| value.to_rfc3339())
                    .unwrap_or_else(|| "-".to_string()),
            ),
            kv_line(
                "Ended",
                &call
                    .ended_at
                    .map(|value| value.to_rfc3339())
                    .unwrap_or_else(|| "-".to_string()),
            ),
            kv_line(
                "Duration",
                &call
                    .duration_ms
                    .map(|value| format!("{value}ms"))
                    .unwrap_or_else(|| "-".to_string()),
            ),
            kv_line("Preview", call.message_preview.as_deref().unwrap_or("-")),
            kv_line(
                "Event refs",
                &format!(
                    "start={} result={} related={}",
                    call.event_refs_start.len(),
                    call.event_refs_result.len(),
                    call.event_refs_related.len()
                ),
            ),
        ])
    }

    fn session_text(&self, session_id: &str) -> Text<'static> {
        let Some(session) = self.session_by_id(session_id) else {
            return Text::from("Missing session");
        };
        let display_label =
            self.display_session_label(Some(&session.session_id), Some(&session.session_label));
        let mut lines = vec![
            title_line("SESSION", Color::Cyan),
            kv_line("Session ID", &session.session_id),
            kv_line("Label", &display_label),
            kv_line("Agent", session.agent_id.as_deref().unwrap_or("-")),
            kv_line(
                "Last activity",
                &session
                    .last_activity_at
                    .map(|value| value.to_rfc3339())
                    .unwrap_or_else(|| "-".to_string()),
            ),
            kv_line(
                "Last event",
                &session
                    .last_event_at
                    .map(|value| value.to_rfc3339())
                    .unwrap_or_else(|| "-".to_string()),
            ),
            kv_line(
                "Last source seen",
                &session
                    .last_source_seen_at
                    .map(|value| value.to_rfc3339())
                    .unwrap_or_else(|| "-".to_string()),
            ),
            kv_line("Call health", call_health_label(&session)),
            kv_line("Source state", source_state_label(&session.source_state)),
            kv_line("Overall health", health_status_label(session.health_status)),
            kv_line("Open calls", &session.open_call_count.to_string()),
            kv_line("Stale calls", &session.stale_call_count.to_string()),
            kv_line("Severity", severity_label(session.derived_severity)),
        ];

        if session.session_label_info.kind == SessionLabelKind::DiscordChannelId {
            lines.push(kv_line(
                "Discord Channel ID",
                session
                    .session_label_info
                    .channel_id
                    .as_deref()
                    .unwrap_or("-"),
            ));
        }

        Text::from(lines)
    }

    fn breadcrumb(&self) -> String {
        let scope = self.current_tab_state().scope.clone();
        let mut parts = vec![self.current_tab.title().to_string()];
        if let Some(session_id) = scope.session_id {
            let label = self.display_session_label(Some(&session_id), Some(&session_id));
            parts.push(format!("session {label}"));
        }
        if let Some(call_id) = scope.call_entity_id {
            let label = self
                .call_by_id(&call_id)
                .map(|call| {
                    format!(
                        "call {}:{}",
                        call.tool_name.unwrap_or_else(|| "tool".to_string()),
                        call.canonical_call_id.unwrap_or(call.call_entity_id)
                    )
                })
                .unwrap_or_else(|| format!("call {call_id}"));
            parts.push(label);
        }
        if self.detail.is_some() {
            parts[0] = "Event Detail".to_string();
        }
        parts.join(" / ")
    }

    fn display_session_label(&self, session_id: Option<&str>, raw_label: Option<&str>) -> String {
        match session_id {
            Some(session_id) => self
                .session_labels
                .state_for_session(session_id, raw_label)
                .display()
                .to_string(),
            None => {
                crate::session_identity::shorten_non_discord_session_label(raw_label.unwrap_or("-"))
            }
        }
    }

    fn display_event_label(&self, row: &EventRow) -> String {
        self.events_by_ref
            .get(&row.event_ref)
            .map(|event| {
                self.session_labels
                    .state_for_event(event)
                    .display()
                    .to_string()
            })
            .unwrap_or_else(|| {
                self.display_session_label(row.session_id.as_deref(), row.session_label.as_deref())
            })
    }

    fn health_counts(&self) -> (usize, usize, usize) {
        let mut busy = 0;
        let mut stale = 0;
        let mut disconnected = 0;
        for session in self
            .store
            .sessions(&self.projection_filter(), self.now, &self.health)
        {
            match session.health_status {
                HealthStatus::Busy => busy += 1,
                HealthStatus::Stale => stale += 1,
                HealthStatus::Disconnected => disconnected += 1,
                _ => {}
            }
        }
        (busy, stale, disconnected)
    }

    fn active_filter_summary(&self) -> String {
        let mut parts = vec![format!("preset={}", self.filters.active_preset.label())];
        if self.filters.stale_only {
            parts.push("stale-only".to_string());
        }
        if self.filters.include_system_events {
            parts.push("system-events".to_string());
        }
        if let Some(query) = self.filters.text_search.as_ref() {
            parts.push(format!("search={query}"));
        }
        parts.push(format!(
            "min-level={}",
            severity_label(self.filters.min_level)
        ));
        if self.filters.time.since != self.filters.base_time.since {
            if let Some(since) = self.filters.time.since {
                parts.push(format!(
                    "since={}",
                    since.with_timezone(&Local).format("%H:%M:%S")
                ));
            }
        }
        if self.filters.time.until != self.filters.base_time.until {
            if let Some(until) = self.filters.time.until {
                parts.push(format!(
                    "until={}",
                    until.with_timezone(&Local).format("%H:%M:%S")
                ));
            }
        }
        parts.join("  ")
    }

    fn help_lines(&self) -> Text<'static> {
        let underlying = if self.detail.is_some() {
            ActiveLayer::Detail
        } else {
            ActiveLayer::Workspace
        };
        let bindings = active_bindings(underlying, self.current_tab);
        let mut lines = vec![
            title_line("Help", Color::Cyan),
            kv_line("Route", &self.breadcrumb()),
            kv_line(
                "State",
                match self.current_tab_state().follow_mode {
                    FollowMode::Follow => "FOLLOW",
                    FollowMode::Browse => "BROWSE",
                },
            ),
            kv_line("Preset", self.filters.active_preset.label()),
            kv_line(
                "Search",
                self.filters.text_search.as_deref().unwrap_or("(none)"),
            ),
        ];
        lines.push(section_header("Bindings"));
        for binding in bindings {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<6}", binding.matcher.label()),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(binding.description),
            ]));
        }
        lines.push(section_header("Overlay"));
        for binding in active_bindings(ActiveLayer::Help, self.current_tab) {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<6}", binding.matcher.label()),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(binding.description),
            ]));
        }
        Text::from(lines)
    }

    fn detail_text_lines(&self) -> usize {
        self.inspector_text().to_string().lines().count()
    }

    fn set_detail_viewport(&mut self, area: Rect) {
        self.detail_view_width = area.width;
        self.detail_view_height = area.height;
    }

    fn set_help_viewport(&mut self, area: Rect) {
        self.help_view_width = area.width;
        self.help_view_height = area.height;
    }

    fn detail_visible_size(&self) -> Option<(usize, usize)> {
        let width = self.detail_view_width.saturating_sub(2);
        let height = self.detail_view_height.saturating_sub(2);
        (width > 0 && height > 0).then_some((width as usize, height as usize))
    }

    fn help_visible_size(&self) -> Option<(usize, usize)> {
        let width = self.help_view_width.saturating_sub(2);
        let height = self.help_view_height.saturating_sub(2);
        (width > 0 && height > 0).then_some((width as usize, height as usize))
    }

    fn help_text_lines(&self) -> usize {
        self.help_lines().to_string().lines().count()
    }

    fn detail_scroll_limit(&self) -> usize {
        let Some((width, height)) = self.detail_visible_size() else {
            return self.detail_text_lines().saturating_sub(1);
        };
        rendered_text_scroll_limit(&self.inspector_text(), width, height)
    }

    fn help_scroll_limit(&self) -> usize {
        let Some((width, height)) = self.help_visible_size() else {
            return self.help_text_lines().saturating_sub(1);
        };
        rendered_text_scroll_limit(&self.help_lines(), width, height)
    }
}

pub fn run(args: &Args, auto_discover: bool) -> io::Result<()> {
    let time_filter = args
        .time_filter()
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
    daemon::ensure_background_daemon(args, auto_discover)?;
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let result = run_app(&mut terminal, args, time_filter);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    args: &Args,
    time_filter: TimeFilter,
) -> io::Result<()> {
    let filters = WorkspaceFilters::from_args(args, time_filter.clone());
    let mut app = App::new(filters, args.stale_seconds);
    let mut tracker = StaleTracker::new(args.stale_seconds);
    let history = PersistedHistory::open_default()?;
    restore_history(&mut app, &mut tracker, &history, args.fresh)?;
    let mut last_history_id = history.max_id()?;
    let heartbeat_interval = args.heartbeat_duration();
    let mut last_heartbeat = Instant::now();
    let mut last_label_refresh = Instant::now() - SESSION_LABEL_REFRESH_INTERVAL;
    terminal.draw(|frame| render(frame, &mut app))?;

    loop {
        let mut needs_draw = match handle_input(&mut app, UI_TICK)? {
            InputOutcome::Idle => false,
            InputOutcome::Redraw => true,
            InputOutcome::Quit => break,
        };

        let now = Instant::now();
        if now.duration_since(last_heartbeat) >= heartbeat_interval {
            app.ingest_heartbeat(tracker.heartbeat(Utc::now()), Utc::now());
            last_heartbeat = now;
            needs_draw = true;
        }

        if refresh_history(
            &mut app,
            &mut tracker,
            &history,
            &mut last_history_id,
            args.stale_seconds,
        )? > 0
        {
            needs_draw = true;
        }
        if now.duration_since(last_label_refresh) >= SESSION_LABEL_REFRESH_INTERVAL {
            last_label_refresh = now;
            if app.refresh_session_labels(Utc::now()) {
                needs_draw = true;
            }
        }
        if needs_draw {
            terminal.draw(|frame| render(frame, &mut app))?;
        }
    }

    Ok(())
}

fn resolve_action(app: &App, key: &KeyEvent) -> Option<Action> {
    KEY_BINDINGS
        .iter()
        .find(|binding| {
            binding.matcher.matches(key)
                && binding.layers.contains(&app.layer())
                && binding.tabs.contains(&app.current_tab)
        })
        .map(|binding| binding.action)
}

fn active_bindings(layer: ActiveLayer, tab: Tab) -> Vec<&'static KeyBinding> {
    KEY_BINDINGS
        .iter()
        .filter(|binding| binding.layers.contains(&layer) && binding.tabs.contains(&tab))
        .collect()
}

fn handle_input(app: &mut App, timeout: Duration) -> io::Result<InputOutcome> {
    if !event::poll(timeout)? {
        return Ok(InputOutcome::Idle);
    }

    let mut pending = PendingInput::default();
    for _ in 0..MAX_INPUT_DRAIN {
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if process_key_input(app, &key, &mut pending) {
                    return Ok(InputOutcome::Quit);
                }
            }
            Event::Resize(_, _) => {
                pending.saw_resize = true;
            }
            _ => {}
        }

        if !event::poll(Duration::ZERO)? {
            break;
        }
    }

    flush_pending_input(app, &mut pending);
    if pending.changed || pending.saw_resize {
        Ok(InputOutcome::Redraw)
    } else {
        Ok(InputOutcome::Idle)
    }
}

fn process_key_input(app: &mut App, key: &KeyEvent, pending: &mut PendingInput) -> bool {
    if app.search_open {
        flush_pending_input(app, pending);
        pending.changed = true;
        return handle_search_input(app, key);
    }

    if app.awaiting_gg {
        flush_pending_input(app, pending);
        app.awaiting_gg = false;
        if key.code == KeyCode::Char('g') {
            pending.changed = true;
            return perform_action(app, Action::FirstRow);
        }

        if let Some(action) = resolve_action(app, key) {
            pending.changed = true;
            return perform_action(app, action);
        }

        pending.changed = true;
        return false;
    }

    let Some(action) = resolve_action(app, key) else {
        return false;
    };

    match action {
        Action::NextRow => {
            flush_pending_scroll(app, pending);
            pending.row_delta = pending.row_delta.saturating_add(1);
            pending.changed = true;
            false
        }
        Action::PreviousRow => {
            flush_pending_scroll(app, pending);
            pending.row_delta = pending.row_delta.saturating_sub(1);
            pending.changed = true;
            false
        }
        Action::ScrollDown => {
            flush_pending_rows(app, pending);
            pending.scroll_delta = pending.scroll_delta.saturating_add(1);
            pending.changed = true;
            false
        }
        Action::ScrollUp => {
            flush_pending_rows(app, pending);
            pending.scroll_delta = pending.scroll_delta.saturating_sub(1);
            pending.changed = true;
            false
        }
        _ => {
            flush_pending_input(app, pending);
            pending.changed = true;
            perform_action(app, action)
        }
    }
}

fn flush_pending_input(app: &mut App, pending: &mut PendingInput) {
    flush_pending_rows(app, pending);
    flush_pending_scroll(app, pending);
}

fn flush_pending_rows(app: &mut App, pending: &mut PendingInput) {
    if pending.row_delta != 0 {
        let delta = pending.row_delta;
        pending.row_delta = 0;
        app.move_selection(delta);
    }
}

fn flush_pending_scroll(app: &mut App, pending: &mut PendingInput) {
    if pending.scroll_delta != 0 {
        let delta = pending.scroll_delta;
        pending.scroll_delta = 0;
        app.scroll_active_layer(delta);
    }
}

fn handle_search_input(app: &mut App, key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc => {
            app.cancel_search();
            false
        }
        KeyCode::Enter => {
            app.apply_search();
            false
        }
        KeyCode::Backspace => {
            app.search_draft.pop();
            false
        }
        KeyCode::Char(value) => {
            app.search_draft.push(value);
            false
        }
        _ => false,
    }
}

fn perform_action(app: &mut App, action: Action) -> bool {
    match action {
        Action::Close => {
            app.awaiting_gg = false;
            app.unwind_route();
            false
        }
        Action::Quit => {
            app.awaiting_gg = false;
            true
        }
        Action::GotoTopPrefix => {
            app.awaiting_gg = true;
            false
        }
        Action::NextRow => {
            app.awaiting_gg = false;
            app.move_selection(1);
            false
        }
        Action::PreviousRow => {
            app.awaiting_gg = false;
            app.move_selection(-1);
            false
        }
        Action::FirstRow => {
            app.awaiting_gg = false;
            if app.help_open {
                app.help_scroll = 0;
            } else if app.detail.is_some() {
                app.detail.as_mut().expect("detail").scroll = 0;
            } else {
                app.jump_to(0);
            }
            false
        }
        Action::LastRow => {
            app.awaiting_gg = false;
            if app.help_open {
                app.help_scroll = app.help_scroll_limit();
            } else if let Some(max) = app.detail.as_ref().map(|_| app.detail_scroll_limit()) {
                if let Some(detail) = app.detail.as_mut() {
                    detail.scroll = max;
                }
            } else {
                let last = app.visible_rows(app.current_tab).len().saturating_sub(1);
                app.jump_to(last);
            }
            false
        }
        Action::ScrollDown => {
            app.awaiting_gg = false;
            app.scroll_active_layer(1);
            false
        }
        Action::ScrollUp => {
            app.awaiting_gg = false;
            app.scroll_active_layer(-1);
            false
        }
        Action::ResumeLive => {
            app.awaiting_gg = false;
            app.resume_live();
            false
        }
        Action::PreviousTab => {
            app.awaiting_gg = false;
            app.switch_tab(app.current_tab.previous());
            false
        }
        Action::NextTab => {
            app.awaiting_gg = false;
            app.switch_tab(app.current_tab.next());
            false
        }
        Action::TabEvents => {
            app.awaiting_gg = false;
            app.switch_tab(Tab::Events);
            false
        }
        Action::TabCalls => {
            app.awaiting_gg = false;
            app.switch_tab(Tab::Calls);
            false
        }
        Action::TabSessions => {
            app.awaiting_gg = false;
            app.switch_tab(Tab::Sessions);
            false
        }
        Action::Activate => {
            app.activate_selected();
            app.awaiting_gg = false;
            false
        }
        Action::OpenDetail => {
            app.open_detail();
            app.awaiting_gg = false;
            false
        }
        Action::ToggleHelp => {
            app.awaiting_gg = false;
            app.help_open = !app.help_open;
            if app.help_open {
                app.help_scroll = 0;
                app.presets_open = false;
                app.search_open = false;
            }
            false
        }
        Action::TogglePresets => {
            app.awaiting_gg = false;
            app.presets_open = !app.presets_open;
            if app.presets_open {
                app.help_open = false;
                app.search_open = false;
            }
            false
        }
        Action::OpenSearch => {
            app.awaiting_gg = false;
            app.open_search();
            false
        }
        Action::ToggleStaleOnly => {
            app.awaiting_gg = false;
            app.toggle_stale_only();
            false
        }
        Action::ApplyPreset(preset) => {
            app.awaiting_gg = false;
            app.apply_preset(preset);
            app.presets_open = false;
            false
        }
    }
}

fn refresh_history(
    app: &mut App,
    tracker: &mut StaleTracker,
    history: &PersistedHistory,
    last_history_id: &mut i64,
    stale_after_seconds: u64,
) -> io::Result<usize> {
    let restored = history.load_after_id(*last_history_id, DRAIN_PER_TICK)?;
    if restored.is_empty() {
        return Ok(0);
    }

    let restored_count = restored.len();
    ingest_persisted_events(app, tracker, restored, last_history_id);
    if app.event_count() > HISTORY_LIMIT {
        rebuild_history_view(app, tracker, history, last_history_id, stale_after_seconds)?;
    }
    Ok(restored_count)
}

fn ingest_persisted_events(
    app: &mut App,
    tracker: &mut StaleTracker,
    persisted_events: Vec<PersistedEvent>,
    last_history_id: &mut i64,
) {
    let before = app.snapshot_visible_keys();
    for persisted in persisted_events {
        *last_history_id = (*last_history_id).max(persisted.id);
        let event_time = persisted.event.timestamp.unwrap_or(persisted.observed_at);
        let warnings = tracker.on_event(&persisted.event, event_time);
        app.ingest_event_record(persisted.event, persisted.observed_at);
        for warning in warnings {
            if app.filters.time.contains(Some(event_time)) {
                app.ingest_notice_record(NoticeKind::Stale(warning), event_time);
            }
        }
    }
    app.reconcile_after_data_change(before);
}

fn rebuild_history_view(
    app: &mut App,
    tracker: &mut StaleTracker,
    history: &PersistedHistory,
    last_history_id: &mut i64,
    stale_after_seconds: u64,
) -> io::Result<()> {
    let before = app.snapshot_visible_keys();
    let current_tab = app.current_tab;
    let tabs = app.tabs.clone();
    let route_stack = app.route_stack.clone();
    let detail = app.detail.clone();
    let help_open = app.help_open;
    let presets_open = app.presets_open;
    let search_open = app.search_open;
    let search_draft = app.search_draft.clone();
    let help_scroll = app.help_scroll;
    let awaiting_gg = app.awaiting_gg;
    let detail_view_width = app.detail_view_width;
    let detail_view_height = app.detail_view_height;
    let help_view_width = app.help_view_width;
    let help_view_height = app.help_view_height;

    let filters = app.filters.clone();
    let mut rebuilt = App::new(filters, stale_after_seconds);
    let mut rebuilt_tracker = StaleTracker::new(stale_after_seconds);
    restore_history(&mut rebuilt, &mut rebuilt_tracker, history, false)?;
    *last_history_id = history.max_id()?;

    rebuilt.current_tab = current_tab;
    rebuilt.tabs = tabs;
    rebuilt.route_stack = route_stack;
    rebuilt.detail = detail;
    rebuilt.help_open = help_open;
    rebuilt.presets_open = presets_open;
    rebuilt.search_open = search_open;
    rebuilt.search_draft = search_draft;
    rebuilt.help_scroll = help_scroll;
    rebuilt.awaiting_gg = awaiting_gg;
    rebuilt.detail_view_width = detail_view_width;
    rebuilt.detail_view_height = detail_view_height;
    rebuilt.help_view_width = help_view_width;
    rebuilt.help_view_height = help_view_height;
    rebuilt.reconcile_after_data_change(before);

    *app = rebuilt;
    *tracker = rebuilt_tracker;
    Ok(())
}

fn restore_history(
    app: &mut App,
    tracker: &mut StaleTracker,
    history: &PersistedHistory,
    fresh: bool,
) -> io::Result<usize> {
    if fresh {
        return Ok(0);
    }

    let restored = history.load_recent()?;
    let restored_count = restored.len();
    let before = app.snapshot_visible_keys();
    for persisted in restored {
        let event_time = persisted.event.timestamp.unwrap_or(persisted.observed_at);
        tracker.on_event(&persisted.event, event_time);
        app.ingest_event_record(persisted.event, persisted.observed_at);
    }
    app.reconcile_after_data_change(before);
    Ok(restored_count)
}

fn render(frame: &mut Frame, app: &mut App) {
    if app.detail.is_some() {
        render_detail(frame, frame.area(), app);
        if app.help_open {
            render_help(frame, app);
        } else if app.presets_open {
            render_presets(frame, app);
        } else if app.search_open {
            render_search(frame, app);
        }
        return;
    }

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Min(10),
            Constraint::Length(2),
        ])
        .split(frame.area());

    render_header(frame, layout[0], app);
    render_workspace(frame, layout[1], app);
    render_footer(frame, layout[2], app);

    if app.help_open {
        render_help(frame, app);
    } else if app.presets_open {
        render_presets(frame, app);
    } else if app.search_open {
        render_search(frame, app);
    }
}

fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let (busy, stale, disconnected) = app.health_counts();
    let tab_line = Tab::ALL
        .into_iter()
        .map(|tab| {
            let state = app.tab_state(tab);
            let mode = match state.follow_mode {
                FollowMode::Follow => "FOLLOW",
                FollowMode::Browse => "BROWSE",
            };
            let unseen = if state.unseen_count == 0 {
                String::new()
            } else {
                format!(" +{}", state.unseen_count)
            };
            let style = if tab == app.current_tab {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            Span::styled(
                format!(" {} [{}{}] ", tab.short_title(), mode, unseen),
                style,
            )
        })
        .collect::<Vec<_>>();

    let preset_line = OpsPreset::SELECTABLE
        .into_iter()
        .map(|preset| {
            let style = if preset == app.filters.active_preset {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            Span::styled(
                format!(" {} {} ", preset.key().unwrap_or('?'), preset.label()),
                style,
            )
        })
        .collect::<Vec<_>>();

    let text = Text::from(vec![
        Line::from(vec![
            Span::styled(
                "OpenClaw Logpulse",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(app.breadcrumb(), Style::default().fg(Color::White)),
        ]),
        Line::from(tab_line),
        Line::from(preset_line),
        Line::from(vec![
            Span::styled("status ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!(
                "busy {}  stale {}  disconnected {}  {}",
                busy,
                stale,
                disconnected,
                app.active_filter_summary()
            )),
        ]),
        Line::from(vec![
            Span::styled("filters ", Style::default().fg(Color::DarkGray)),
            Span::raw(app.filters.summary.clone()),
        ]),
    ]);

    let paragraph = Paragraph::new(text).block(Block::default().borders(Borders::ALL));
    frame.render_widget(paragraph, area);
}

fn render_workspace(frame: &mut Frame, area: Rect, app: &App) {
    let constraints = if area.width < 120 {
        vec![Constraint::Percentage(58), Constraint::Percentage(42)]
    } else {
        vec![Constraint::Percentage(62), Constraint::Percentage(38)]
    };
    let direction = if area.width < 120 {
        Direction::Vertical
    } else {
        Direction::Horizontal
    };
    let chunks = Layout::default()
        .direction(direction)
        .constraints(constraints)
        .split(area);

    render_list(frame, chunks[0], app);
    render_inspector(frame, chunks[1], app);
}

fn render_list(frame: &mut Frame, area: Rect, app: &App) {
    let rows = app.visible_row_items(app.current_tab);
    let selected_index = app.selected_item_index(app.current_tab, &rows);
    let tab_state = app.current_tab_state();
    let widths = match app.current_tab {
        Tab::Events => [
            Constraint::Length(8),
            Constraint::Length(7),
            Constraint::Length(8),
            Constraint::Length(16),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Min(12),
        ]
        .to_vec(),
        Tab::Calls => [
            Constraint::Length(8),
            Constraint::Length(9),
            Constraint::Length(5),
            Constraint::Length(8),
            Constraint::Length(12),
            Constraint::Length(9),
            Constraint::Length(9),
            Constraint::Length(7),
            Constraint::Min(10),
        ]
        .to_vec(),
        Tab::Sessions => [
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Length(16),
            Constraint::Length(7),
            Constraint::Length(8),
            Constraint::Length(6),
            Constraint::Length(6),
            Constraint::Length(7),
        ]
        .to_vec(),
    };

    let header = match app.current_tab {
        Tab::Events => vec![
            "time", "kind", "agent", "surface", "tool", "call", "status", "preview",
        ],
        Tab::Calls => vec![
            "time", "status", "conf", "agent", "surface", "tool", "call", "dur", "preview",
        ],
        Tab::Sessions => vec![
            "time", "agent", "surface", "calls", "source", "open", "stale", "level",
        ],
    };

    let visible_height = area.height.saturating_sub(3).max(1) as usize;
    let row_start = tab_state.scroll_offset.min(rows.len());
    let row_end = row_start.saturating_add(visible_height).min(rows.len());
    let visible_selection = selected_index
        .and_then(|index| (index >= row_start && index < row_end).then_some(index - row_start));
    let table_rows = rows[row_start..row_end]
        .iter()
        .map(|row| Row::new(row.cells()))
        .collect::<Vec<_>>();
    let table = Table::new(table_rows, widths)
        .header(Row::new(header).style(Style::default().add_modifier(Modifier::BOLD)))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(app.current_tab.title()),
        )
        .highlight_symbol("▶ ")
        .row_highlight_style(
            Style::default()
                .bg(Color::Rgb(35, 43, 60))
                .add_modifier(Modifier::BOLD),
        );

    let mut state = TableState::default();
    state.select(visible_selection);
    frame.render_stateful_widget(table, area, &mut state);
}

fn render_inspector(frame: &mut Frame, area: Rect, app: &App) {
    let paragraph = Paragraph::new(app.inspector_text())
        .block(Block::default().borders(Borders::ALL).title("Inspector"))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_detail(frame: &mut Frame, area: Rect, app: &mut App) {
    app.set_detail_viewport(area);
    let detail_scroll_limit = app
        .detail
        .as_ref()
        .map(|_| app.detail_scroll_limit())
        .unwrap_or(0);
    if let Some(detail) = app.detail.as_mut() {
        detail.scroll = detail.scroll.min(detail_scroll_limit);
    }
    let scroll = app
        .detail
        .as_ref()
        .map(|detail| detail.scroll.min(u16::MAX as usize))
        .unwrap_or(0);
    let paragraph = Paragraph::new(app.inspector_text())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(app.breadcrumb()),
        )
        .wrap(Wrap { trim: false })
        .scroll((scroll.min(u16::MAX as usize) as u16, 0));
    frame.render_widget(paragraph, area);
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let spans = footer_items(app)
        .into_iter()
        .flat_map(|(key, label)| {
            vec![
                Span::styled(
                    key,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!(" {label}  ")),
            ]
        })
        .collect::<Vec<_>>();
    let paragraph = Paragraph::new(Line::from(spans)).block(Block::default().borders(Borders::ALL));
    frame.render_widget(paragraph, area);
}

fn footer_items(app: &App) -> Vec<(String, String)> {
    match app.layer() {
        ActiveLayer::Help => vec![
            ("?".to_string(), "close help".to_string()),
            ("Esc".to_string(), "back".to_string()),
            ("j/k".to_string(), "scroll".to_string()),
            ("q".to_string(), "quit".to_string()),
        ],
        ActiveLayer::Presets => OpsPreset::SELECTABLE
            .into_iter()
            .filter_map(|preset| {
                preset
                    .key()
                    .map(|key| (key.to_string(), preset.label().to_string()))
            })
            .chain([
                ("Esc".to_string(), "back".to_string()),
                ("q".to_string(), "quit".to_string()),
            ])
            .collect(),
        ActiveLayer::Search => vec![
            ("/".to_string(), format!("search {}", app.search_draft)),
            ("Enter".to_string(), "apply".to_string()),
            ("Esc".to_string(), "cancel".to_string()),
            ("Backspace".to_string(), "erase".to_string()),
        ],
        ActiveLayer::Detail => vec![
            ("Esc".to_string(), "back".to_string()),
            ("j/k".to_string(), "scroll".to_string()),
            ("gg".to_string(), "top".to_string()),
            ("G".to_string(), "bottom".to_string()),
            ("?".to_string(), "help".to_string()),
            ("q".to_string(), "quit".to_string()),
        ],
        ActiveLayer::Workspace => {
            let enter = match app.current_tab {
                Tab::Sessions => "calls",
                Tab::Calls => "events",
                Tab::Events => "detail",
            };
            vec![
                ("Enter".to_string(), enter.to_string()),
                ("Esc".to_string(), "back".to_string()),
                ("p".to_string(), "presets".to_string()),
                ("/".to_string(), "search".to_string()),
                ("s".to_string(), "stale".to_string()),
                ("f".to_string(), "follow".to_string()),
                ("?".to_string(), "help".to_string()),
                ("q".to_string(), "quit".to_string()),
            ]
        }
    }
}

fn render_help(frame: &mut Frame, app: &mut App) {
    let popup = centered_rect(80, 70, frame.area());
    frame.render_widget(Clear, popup);
    app.set_help_viewport(popup);
    app.help_scroll = app.help_scroll.min(app.help_scroll_limit());
    let help_scroll = app.help_scroll.min(u16::MAX as usize) as u16;
    let paragraph = Paragraph::new(app.help_lines())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Contextual Help"),
        )
        .wrap(Wrap { trim: false })
        .scroll((help_scroll, 0));
    frame.render_widget(paragraph, popup);
}

fn render_presets(frame: &mut Frame, app: &App) {
    let popup = centered_rect(54, 36, frame.area());
    frame.render_widget(Clear, popup);
    let mut lines = vec![title_line("Presets", Color::Cyan), Line::from("")];
    for preset in OpsPreset::SELECTABLE {
        let selected = preset == app.filters.active_preset;
        let marker = if selected { ">" } else { " " };
        let style = if selected {
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        lines.push(Line::from(vec![
            Span::raw(format!("{marker} ")),
            Span::styled(
                format!("{} ", preset.key().unwrap_or('?')),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(preset.label(), style),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("Esc ", Style::default().fg(Color::Yellow)),
        Span::raw("back"),
    ]));

    let paragraph = Paragraph::new(Text::from(lines)).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Preset Operations"),
    );
    frame.render_widget(paragraph, popup);
}

fn render_search(frame: &mut Frame, app: &App) {
    let popup = centered_rect(60, 18, frame.area());
    frame.render_widget(Clear, popup);
    let query = if app.search_draft.is_empty() {
        "(empty clears search)".to_string()
    } else {
        app.search_draft.clone()
    };
    let text = Text::from(vec![
        title_line("Search", Color::Cyan),
        Line::from(vec![
            Span::styled("/ ", Style::default().fg(Color::Yellow)),
            Span::raw(query),
        ]),
        Line::from(""),
        Line::from("Enter applies  Esc cancels  Backspace erases"),
    ]);
    let paragraph =
        Paragraph::new(text).block(Block::default().borders(Borders::ALL).title("Search Rows"));
    frame.render_widget(paragraph, popup);
}

fn centered_rect(horizontal_percent: u16, vertical_percent: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - vertical_percent) / 2),
        Constraint::Percentage(vertical_percent),
        Constraint::Percentage((100 - vertical_percent) / 2),
    ])
    .flex(Flex::Center)
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - horizontal_percent) / 2),
        Constraint::Percentage(horizontal_percent),
        Constraint::Percentage((100 - horizontal_percent) / 2),
    ])
    .flex(Flex::Center)
    .split(vertical[1])[1]
}

fn rendered_text_scroll_limit(text: &Text, width: usize, height: usize) -> usize {
    let visible_lines = rendered_text_lines(text, width);
    if visible_lines == 0 || height == 0 {
        return 0;
    }
    visible_lines.saturating_sub(height)
}

fn rendered_text_lines(text: &Text, width: usize) -> usize {
    if width == 0 {
        return text.to_string().lines().count();
    }

    let rendered = text.to_string();
    rendered
        .lines()
        .map(|line| {
            let len = line.to_string().chars().count().max(1);
            len.div_ceil(width)
        })
        .sum()
}

fn list_scroll_offset(index: usize, scrolloff: usize) -> usize {
    if index == 0 || scrolloff == 0 {
        return 0;
    }
    index.saturating_sub(scrolloff)
}

fn clamp_signed_delta(current: usize, delta: isize, max: usize) -> usize {
    if delta >= 0 {
        current.saturating_add(delta as usize).min(max)
    } else {
        current.saturating_sub(delta.unsigned_abs()).min(max)
    }
}

fn kind_label(kind: &ToolEventKind) -> &'static str {
    match kind {
        ToolEventKind::ToolCallStart => "START",
        ToolEventKind::ToolCallResult => "RESULT",
        ToolEventKind::ToolCall => "CALL",
        ToolEventKind::Other => "OTHER",
        ToolEventKind::Malformed => "BAD",
    }
}

fn event_call_label(row: &EventRow) -> String {
    row.call_ids
        .first()
        .map(|call_id| compact_id(call_id, 12))
        .unwrap_or_else(|| "-".to_string())
}

fn call_label(call: &CorrelatedCall) -> String {
    call.canonical_call_id
        .as_deref()
        .map(|call_id| compact_id(call_id, 12))
        .unwrap_or_else(|| compact_id(&call.call_entity_id, 12))
}

fn compact_id(value: &str, max_chars: usize) -> String {
    let short = value
        .rsplit([':', '|'])
        .next()
        .filter(|part| !part.is_empty())
        .unwrap_or(value);
    truncate_display(short, max_chars)
}

fn confidence_label(confidence: &MatchConfidence) -> &'static str {
    match confidence {
        MatchConfidence::ExplicitId => "id",
        MatchConfidence::TranscriptBundle => "bundle",
        MatchConfidence::FallbackSignature => "guess",
    }
}

fn call_health_label(session: &SessionSummary) -> &'static str {
    if session.stale_call_count > 0 {
        "stale"
    } else if session.open_call_count > 0 {
        "busy"
    } else {
        "idle"
    }
}

fn source_state_label(state: &SourceState) -> &'static str {
    match state {
        SourceState::Live => "live",
        SourceState::Quiet => "quiet",
        SourceState::Missing => "missing",
        SourceState::Unknown => "unknown",
    }
}

fn title_line(title: &str, color: Color) -> Line<'static> {
    Line::from(vec![Span::styled(
        title.to_string(),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )])
}

fn section_header(text: &str) -> Line<'static> {
    Line::from(vec![Span::styled(
        text.to_string(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )])
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
        .map(|line| Line::from(Span::styled(line.to_string(), Style::default().fg(color))))
        .collect()
}

fn pretty_raw_json(raw: &str) -> String {
    match serde_json::from_str::<Value>(raw) {
        Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_else(|_| raw.to_string()),
        Err(_) => raw.to_string(),
    }
}

fn format_ts(ts: DateTime<Utc>) -> String {
    ts.with_timezone(&Local).format("%H:%M:%S").to_string()
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

fn call_status_label(status: CallStatus) -> &'static str {
    match status {
        CallStatus::Running => "running",
        CallStatus::Succeeded => "succeeded",
        CallStatus::Failed => "failed",
        CallStatus::Stale => "stale",
        CallStatus::Incomplete => "incomplete",
        CallStatus::Unknown => "unknown",
    }
}

fn health_status_label(status: HealthStatus) -> &'static str {
    match status {
        HealthStatus::Busy => "busy",
        HealthStatus::Idle => "idle",
        HealthStatus::Stale => "stale",
        HealthStatus::Disconnected => "disconnected",
        HealthStatus::Unknown => "unknown",
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
    if args.fresh {
        parts.push("history=fresh".to_string());
    }

    parts.join("  •  ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::{DiscordLookup, DiscordLookupError};
    use crate::history::PersistedHistory;
    use crate::normalizer::normalize_many_with_source;
    use crate::session_identity::SessionRoutingMetadata;
    use crate::session_label::SessionLabelResolver;
    use ratatui::backend::TestBackend;
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration as StdDuration, SystemTime, UNIX_EPOCH};

    struct BlockingDiscordLookup {
        request_tx: mpsc::Sender<String>,
        release_rx: mpsc::Receiver<Result<String, DiscordLookupError>>,
    }

    struct ImmediateDiscordLookup {
        result: Result<String, DiscordLookupError>,
    }

    impl BlockingDiscordLookup {
        fn new() -> (
            Self,
            mpsc::Receiver<String>,
            mpsc::Sender<Result<String, DiscordLookupError>>,
        ) {
            let (request_tx, request_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            (
                Self {
                    request_tx,
                    release_rx,
                },
                request_rx,
                release_tx,
            )
        }
    }

    impl DiscordLookup for BlockingDiscordLookup {
        fn lookup_channel_name(&self, channel_id: &str) -> Result<String, DiscordLookupError> {
            self.request_tx
                .send(channel_id.to_string())
                .expect("send lookup request");
            self.release_rx.recv().expect("receive lookup result")
        }
    }

    impl DiscordLookup for ImmediateDiscordLookup {
        fn lookup_channel_name(&self, _channel_id: &str) -> Result<String, DiscordLookupError> {
            self.result.clone()
        }
    }

    fn event(
        session_id: &str,
        session_key: Option<&str>,
        tool: &str,
        call_id: Option<&str>,
        kind: ToolEventKind,
        timestamp: &str,
    ) -> NormalizedEvent {
        NormalizedEvent {
            kind: kind.clone(),
            timestamp: Some(
                DateTime::parse_from_rfc3339(timestamp)
                    .expect("timestamp")
                    .with_timezone(&Utc),
            ),
            timestamp_raw: Some(timestamp.to_string()),
            source_path: Some(format!("/tmp/{session_id}.jsonl")),
            source_kind: Some("session_log".to_string()),
            session_key: session_key.map(str::to_string),
            session_label: session_key.map(str::to_string),
            session_id: Some(session_id.to_string()),
            session_source: Some("path".to_string()),
            session_label_source: Some("payload".to_string()),
            session_identity_conflicts: Vec::new(),
            routing: Default::default(),
            agent_id: Some("agent-a".to_string()),
            agent_source: Some("path".to_string()),
            tool_name: Some(tool.to_string()),
            status: Some(match kind {
                ToolEventKind::ToolCallResult => "ok".to_string(),
                _ => "started".to_string(),
            }),
            result_summary: None,
            result_preview: None,
            result_raw: None,
            result_metrics: Vec::new(),
            exit_code: None,
            duration_ms: None,
            is_error: None,
            call_id: call_id.map(str::to_string),
            call_ids: call_id.into_iter().map(str::to_string).collect(),
            correlation_ids: Vec::new(),
            message_id: None,
            parent_message_id: None,
            transcript_tool_call_index: None,
            transcript_tool_call_count: None,
            level: Severity::Info,
            level_raw: Some("info".to_string()),
            params: vec![("command".to_string(), format!("echo {tool}"))],
            args_preview: vec![("command".to_string(), format!("echo {tool}"))],
            args_raw: None,
            args_truncated: false,
            message: Some(format!("{tool} {kind:?}")),
            raw_line: "{\"event\":\"test\"}".to_string(),
        }
    }

    fn discord_event(
        session_id: &str,
        channel_id: &str,
        tool: &str,
        call_id: Option<&str>,
        kind: ToolEventKind,
        timestamp: &str,
    ) -> NormalizedEvent {
        let mut event = event(
            session_id,
            Some(&format!("agent:main:discord:channel:{channel_id}")),
            tool,
            call_id,
            kind,
            timestamp,
        );
        event.routing = SessionRoutingMetadata {
            provider: Some("discord".to_string()),
            provider_source: Some("session_key".to_string()),
            channel_id: Some(channel_id.to_string()),
            channel_id_source: Some("session_key".to_string()),
            issues: Vec::new(),
        };
        event
    }

    fn app() -> App {
        App::new(filters(), 30)
    }

    fn filters() -> WorkspaceFilters {
        WorkspaceFilters {
            session: None,
            agent: None,
            tool: None,
            min_level: Severity::Trace,
            base_min_level: Severity::Trace,
            time: TimeFilter::default(),
            base_time: TimeFilter::default(),
            include_system_events: false,
            stale_only: false,
            text_search: None,
            active_preset: OpsPreset::Live,
            summary: "test".to_string(),
        }
    }

    fn wait_for<F>(mut condition: F)
    where
        F: FnMut() -> bool,
    {
        for _ in 0..50 {
            if condition() {
                return;
            }
            thread::sleep(StdDuration::from_millis(10));
        }
        panic!("condition was not met in time");
    }

    fn seed(app: &mut App) {
        for item in [
            event(
                "session-a",
                Some("label-shared"),
                "shell",
                Some("call-1"),
                ToolEventKind::ToolCallStart,
                "2026-03-07T10:00:00Z",
            ),
            event(
                "session-b",
                Some("label-shared"),
                "shell",
                Some("call-1"),
                ToolEventKind::ToolCallStart,
                "2026-03-07T10:00:01Z",
            ),
            event(
                "session-a",
                Some("label-shared"),
                "shell",
                Some("call-1"),
                ToolEventKind::ToolCallResult,
                "2026-03-07T10:00:02Z",
            ),
            event(
                "session-b",
                Some("label-shared"),
                "shell",
                Some("call-1"),
                ToolEventKind::ToolCallResult,
                "2026-03-07T10:00:03Z",
            ),
        ] {
            let ts = item.timestamp.unwrap();
            app.ingest_event(item, ts);
        }
        app.ingest_warning(
            StaleWarning {
                call_id: "call-1".to_string(),
                session_key: Some("label-shared".to_string()),
                session_id: Some("session-a".to_string()),
                tool_name: Some("shell".to_string()),
                age_seconds: 45,
                message: Some("stuck shell".to_string()),
            },
            DateTime::parse_from_rfc3339("2026-03-07T10:00:04Z")
                .unwrap()
                .with_timezone(&Utc),
        );
    }

    fn seed_many_events(app: &mut App, count: usize) {
        for i in 0..count {
            let event = event(
                &format!("session-{i}"),
                Some(&format!("label-{i}")),
                "shell",
                Some(&format!("call-{i}")),
                if i % 2 == 0 {
                    ToolEventKind::ToolCallStart
                } else {
                    ToolEventKind::ToolCallResult
                },
                &format!("2026-03-07T10:00:{i:02}Z"),
            );
            let ts = event.timestamp.unwrap();
            app.ingest_event(event, ts);
        }
    }

    #[test]
    fn repeated_navigation_input_is_coalesced_before_redraw() {
        let mut app = app();
        seed_many_events(&mut app, 20);
        app.current_tab = Tab::Sessions;
        app.resume_live();

        let rows = app.visible_rows(Tab::Sessions);
        let expected = rows[7].key.clone();
        let mut pending = PendingInput::default();
        for _ in 0..7 {
            assert!(!process_key_input(
                &mut app,
                &KeyEvent::from(KeyCode::Char('j')),
                &mut pending,
            ));
        }

        assert_eq!(
            app.tab_state(Tab::Sessions).selected,
            Some(rows[0].key.clone())
        );
        assert_eq!(pending.row_delta, 7);
        flush_pending_input(&mut app, &mut pending);
        assert_eq!(app.tab_state(Tab::Sessions).selected, Some(expected));
        assert_eq!(pending.row_delta, 0);
    }

    #[test]
    fn non_navigation_input_flushes_coalesced_navigation_first() {
        let mut app = app();
        seed_many_events(&mut app, 20);
        app.current_tab = Tab::Sessions;
        app.resume_live();

        let expected = app.visible_rows(Tab::Sessions)[3].key.clone();
        let mut pending = PendingInput::default();
        for _ in 0..3 {
            assert!(!process_key_input(
                &mut app,
                &KeyEvent::from(KeyCode::Char('j')),
                &mut pending,
            ));
        }
        assert!(!process_key_input(
            &mut app,
            &KeyEvent::from(KeyCode::Char('2')),
            &mut pending,
        ));

        assert_eq!(app.tab_state(Tab::Sessions).selected, Some(expected));
        assert_eq!(app.current_tab, Tab::Calls);
        assert_eq!(pending.row_delta, 0);
    }

    #[test]
    fn calls_tab_follow_tracks_latest_call_activity() {
        let mut app = app();
        app.current_tab = Tab::Calls;

        for item in [
            event(
                "session-a",
                Some("label-a"),
                "shell",
                Some("call-a"),
                ToolEventKind::ToolCallStart,
                "2026-03-07T10:00:00Z",
            ),
            event(
                "session-a",
                Some("label-a"),
                "read",
                Some("call-b"),
                ToolEventKind::ToolCallStart,
                "2026-03-07T10:01:00Z",
            ),
        ] {
            let ts = item.timestamp.unwrap();
            app.ingest_event(item, ts);
        }

        assert_eq!(
            app.visible_rows(Tab::Calls)
                .first()
                .map(|row| row.key.clone()),
            Some(EntityKey::Call("session-a:call-b".to_string()))
        );

        let result = event(
            "session-a",
            Some("label-a"),
            "shell",
            Some("call-a"),
            ToolEventKind::ToolCallResult,
            "2026-03-07T10:02:00Z",
        );
        let ts = result.timestamp.unwrap();
        app.ingest_event(result, ts);

        assert_eq!(
            app.visible_rows(Tab::Calls)
                .first()
                .map(|row| row.key.clone()),
            Some(EntityKey::Call("session-a:call-a".to_string()))
        );
        assert_eq!(
            app.tab_state(Tab::Calls).selected,
            Some(EntityKey::Call("session-a:call-a".to_string()))
        );
    }

    fn render_string_with_size(app: &mut App, width: u16, height: u16) -> io::Result<String> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend)?;
        terminal.draw(|frame| render(frame, app))?;
        let buffer = terminal.backend().buffer().clone();
        let mut lines = Vec::new();
        for y in 0..buffer.area.height {
            let mut line = String::new();
            for x in 0..buffer.area.width {
                line.push_str(buffer[(x, y)].symbol());
            }
            lines.push(line.trim_end().to_string());
        }
        Ok(lines.join("\n"))
    }

    fn selected_row_y(app: &mut App, width: u16, height: u16) -> io::Result<Option<u16>> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend)?;
        terminal.draw(|frame| render(frame, app))?;
        let buffer = terminal.backend().buffer();

        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                if buffer[(x, y)].symbol() == "▶" {
                    return Ok(Some(y));
                }
            }
        }

        Ok(None)
    }

    fn render_string(app: &mut App) -> io::Result<String> {
        render_string_with_size(app, 120, 40)
    }

    fn write_session_fixture(session_id: &str, manifest: &str) -> (PathBuf, Box<dyn FnOnce()>) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("logpulse-tui-{unique}"));
        let sessions_dir = root.join("agents").join("main").join("sessions");
        fs::create_dir_all(&sessions_dir).expect("create sessions dir");
        let session_path = sessions_dir.join(format!("{session_id}.jsonl"));
        fs::write(&session_path, "").expect("write session file");
        fs::write(sessions_dir.join("sessions.json"), manifest).expect("write sessions manifest");

        let cleanup_root = root.clone();
        (
            session_path,
            Box::new(move || {
                let _ = fs::remove_dir_all(cleanup_root);
            }),
        )
    }

    fn history_path() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir()
            .join(format!("logpulse-tui-history-{unique}"))
            .join("history.sqlite3")
    }

    fn inspector_string(app: &App) -> String {
        app.inspector_text()
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn restore_history_rehydrates_persisted_events_before_live_ingest() {
        let path = history_path();
        let mut history = PersistedHistory::open(&path).expect("open history");
        let first = event(
            "session-a",
            Some("label-a"),
            "shell",
            Some("call-1"),
            ToolEventKind::ToolCallStart,
            "2026-03-07T10:00:00Z",
        );
        let second = event(
            "session-a",
            Some("label-a"),
            "shell",
            Some("call-1"),
            ToolEventKind::ToolCallResult,
            "2026-03-07T10:00:01Z",
        );
        history
            .append(first.timestamp.expect("timestamp"), &first)
            .expect("append first");
        history
            .append(second.timestamp.expect("timestamp"), &second)
            .expect("append second");

        let mut app = app();
        let mut tracker = StaleTracker::new(30);
        let restored = restore_history(&mut app, &mut tracker, &history, false).expect("restore");
        let event_refs = app
            .visible_rows(Tab::Events)
            .into_iter()
            .filter_map(|row| match row.key {
                EntityKey::Event(event_ref) => Some(event_ref),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(restored, 2);
        assert_eq!(
            event_refs,
            vec!["event-2".to_string(), "event-1".to_string()]
        );

        fs::remove_dir_all(path.parent().expect("parent")).expect("cleanup");
    }

    #[test]
    fn fresh_launch_skips_restore_without_clearing_persisted_history() {
        let path = history_path();
        let mut history = PersistedHistory::open(&path).expect("open history");
        let saved = event(
            "session-a",
            Some("label-a"),
            "shell",
            Some("call-1"),
            ToolEventKind::ToolCallStart,
            "2026-03-07T10:00:00Z",
        );
        history
            .append(saved.timestamp.expect("timestamp"), &saved)
            .expect("append");

        let mut app = app();
        let mut tracker = StaleTracker::new(30);
        let restored = restore_history(&mut app, &mut tracker, &history, true).expect("restore");

        assert_eq!(restored, 0);
        assert!(app.visible_rows(Tab::Events).is_empty());
        assert_eq!(history.load_recent().expect("load recent").len(), 1);

        fs::remove_dir_all(path.parent().expect("parent")).expect("cleanup");
    }

    #[test]
    fn tab_state_is_remembered_per_tab() {
        let mut app = app();
        seed(&mut app);

        app.switch_tab(Tab::Calls);
        app.move_selection(1);
        let calls_selected = app.tab_state(Tab::Calls).selected.clone();
        assert_eq!(app.tab_state(Tab::Calls).follow_mode, FollowMode::Browse);

        app.switch_tab(Tab::Sessions);
        app.move_selection(1);
        let sessions_selected = app.tab_state(Tab::Sessions).selected.clone();

        app.switch_tab(Tab::Calls);
        assert_eq!(app.tab_state(Tab::Calls).selected, calls_selected);

        app.switch_tab(Tab::Sessions);
        assert_eq!(app.tab_state(Tab::Sessions).selected, sessions_selected);
    }

    #[test]
    fn manual_tab_navigation_preserves_follow_mode() {
        let mut app = app();
        seed(&mut app);

        let calls_top = app
            .visible_rows(Tab::Calls)
            .first()
            .map(|row| row.key.clone());
        assert_eq!(app.tab_state(Tab::Calls).follow_mode, FollowMode::Follow);

        app.switch_tab(Tab::Calls);

        assert_eq!(app.current_tab, Tab::Calls);
        assert_eq!(app.tab_state(Tab::Calls).follow_mode, FollowMode::Follow);
        assert_eq!(app.tab_state(Tab::Calls).selected, calls_top);
        assert_eq!(app.tab_state(Tab::Calls).scroll_offset, 0);
        assert_eq!(app.tab_state(Tab::Calls).unseen_count, 0);
    }

    #[test]
    fn stale_toggle_and_presets_drive_visible_filters() {
        let mut app = app();
        seed(&mut app);
        app.current_tab = Tab::Events;

        assert!(app
            .visible_rows(Tab::Events)
            .iter()
            .any(|row| matches!(row.key, EntityKey::Event(_))));

        assert!(!perform_action(&mut app, Action::ToggleStaleOnly));
        assert!(app.filters.stale_only);
        assert_eq!(app.filters.active_preset, OpsPreset::StaleOnly);
        assert!(app
            .visible_rows(Tab::Events)
            .iter()
            .all(|row| matches!(row.key, EntityKey::Notice(_))));

        assert!(!perform_action(
            &mut app,
            Action::ApplyPreset(OpsPreset::Errors)
        ));
        assert_eq!(app.filters.active_preset, OpsPreset::Errors);
        assert_eq!(app.filters.min_level, Severity::Error);
        assert!(app.filters.include_system_events);
        assert!(!app.filters.stale_only);
    }

    #[test]
    fn search_input_filters_rows_and_can_be_cleared() {
        let mut app = app();
        let read = event(
            "session-c",
            Some("label-c"),
            "read",
            Some("call-read"),
            ToolEventKind::ToolCallStart,
            "2026-03-07T10:00:05Z",
        );
        let ts = read.timestamp.unwrap();
        app.ingest_event(read, ts);
        seed(&mut app);
        app.current_tab = Tab::Events;

        app.open_search();
        for ch in "read".chars() {
            assert!(!handle_search_input(
                &mut app,
                &KeyEvent::from(KeyCode::Char(ch))
            ));
        }
        assert!(!handle_search_input(
            &mut app,
            &KeyEvent::from(KeyCode::Enter)
        ));

        assert_eq!(app.filters.text_search.as_deref(), Some("read"));
        let rows = app.visible_rows(Tab::Events);
        assert!(!rows.is_empty());
        assert!(rows.iter().all(|row| row.searchable.contains("read")));

        app.open_search();
        for _ in 0..4 {
            assert!(!handle_search_input(
                &mut app,
                &KeyEvent::from(KeyCode::Backspace)
            ));
        }
        assert!(!handle_search_input(
            &mut app,
            &KeyEvent::from(KeyCode::Enter)
        ));
        assert!(app.filters.text_search.is_none());
    }

    #[test]
    fn esc_backs_out_without_quitting_at_root() {
        let mut app = app();
        seed(&mut app);

        assert!(!perform_action(&mut app, Action::Close));
        assert_eq!(app.current_tab, Tab::Sessions);
        assert!(perform_action(&mut app, Action::Quit));
    }

    #[test]
    fn ops_columns_expose_call_identity_confidence_and_split_session_health() {
        let mut app = app();
        seed(&mut app);

        let event_row = app
            .visible_rows(Tab::Events)
            .into_iter()
            .find(|row| matches!(row.key, EntityKey::Event(_)))
            .expect("event row");
        assert_eq!(event_row.cells[5], "call-1");

        let call_row = app
            .visible_rows(Tab::Calls)
            .into_iter()
            .next()
            .expect("call row");
        assert_eq!(call_row.cells[2], "id");
        assert_eq!(call_row.cells[6], "call-1");

        let session_row = app
            .visible_rows(Tab::Sessions)
            .into_iter()
            .next()
            .expect("session row");
        assert!(["idle", "busy", "stale"].contains(&session_row.cells[3].as_str()));
        assert!(["live", "quiet", "missing", "unknown"].contains(&session_row.cells[4].as_str()));
    }

    #[test]
    fn codex_rollout_rows_populate_tool_call_status_and_preview_columns() {
        let mut app = app();
        let source_path = Path::new(
            "/home/anders/.openclaw/agents/main/agent/codex-home/sessions/2026/05/20/rollout-2026-05-20T18-01-58-019e468d-2f56-75e2-85d4-2d0a771f796e.jsonl",
        );
        let lines = [
            r#"{"timestamp":"2026-05-20T18:14:13.273Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"pnpm --version\",\"workdir\":\"/home/anders/project\"}","call_id":"call_pnpm"}}"#,
            r#"{"timestamp":"2026-05-20T18:14:16.171Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_pnpm","output":"Chunk ID: 5fbe73\nWall time: 0.2987 seconds\nProcess exited with code 0\nOutput:\n11.1.0\n"}}"#,
        ];

        for line in lines {
            for event in normalize_many_with_source(line, Some(source_path)) {
                let ts = event.timestamp.expect("timestamp");
                app.ingest_event(event, ts);
            }
        }

        let event_rows = app.visible_rows(Tab::Events);
        let start_row = event_rows
            .iter()
            .find(|row| row.cells[1] == "START")
            .expect("start row");
        let result_row = event_rows
            .iter()
            .find(|row| row.cells[1] == "RESULT")
            .expect("result row");

        assert_eq!(start_row.cells[4], "exec_command");
        assert_eq!(start_row.cells[5], "call_pnpm");
        assert_eq!(start_row.cells[6], "started");
        assert_eq!(start_row.cells[7], "cmd=pnpm --version");
        assert_eq!(result_row.cells[4], "exec_command");
        assert_eq!(result_row.cells[5], "call_pnpm");
        assert_eq!(result_row.cells[6], "completed");
        assert!(result_row.cells[7].contains("Process exited with code 0"));

        let call_row = app
            .visible_rows(Tab::Calls)
            .into_iter()
            .next()
            .expect("call row");
        assert_eq!(call_row.cells[1], "succeeded");
        assert_eq!(call_row.cells[2], "id");
        assert_eq!(call_row.cells[5], "exec_command");
        assert_eq!(call_row.cells[6], "call_pnpm");
        assert_eq!(call_row.cells[8], "cmd=pnpm --version");
    }

    #[test]
    fn drilldown_scopes_by_exact_session_identity() {
        let mut app = app();
        seed(&mut app);

        app.switch_tab(Tab::Sessions);
        app.tab_state_mut(Tab::Sessions).selected =
            Some(EntityKey::Session("session-a".to_string()));
        app.activate_selected();
        assert_eq!(app.current_tab, Tab::Calls);

        let call_ids = app
            .visible_rows(Tab::Calls)
            .into_iter()
            .map(|row| row.key)
            .collect::<Vec<_>>();
        assert_eq!(
            call_ids,
            vec![EntityKey::Call("session-a:call-1".to_string())]
        );

        app.tab_state_mut(Tab::Calls).selected =
            Some(EntityKey::Call("session-a:call-1".to_string()));
        app.activate_selected();
        let event_ids = app
            .visible_rows(Tab::Events)
            .into_iter()
            .map(|row| row.key)
            .collect::<Vec<_>>();
        assert_eq!(
            event_ids,
            vec![
                EntityKey::Notice("notice-1".to_string()),
                EntityKey::Event("event-3".to_string()),
                EntityKey::Event("event-1".to_string())
            ]
        );

        assert!(!app.unwind_route());
        assert_eq!(app.current_tab, Tab::Calls);
        assert!(!app.unwind_route());
        assert_eq!(app.current_tab, Tab::Sessions);
    }

    #[test]
    fn session_drilldown_to_calls_enters_browse_mode() {
        let mut app = app();
        seed(&mut app);

        app.switch_tab(Tab::Sessions);
        app.tab_state_mut(Tab::Sessions).selected =
            Some(EntityKey::Session("session-a".to_string()));

        let expected_selected = {
            let scope = DrilldownScope {
                session_id: Some("session-a".to_string()),
                call_entity_id: None,
            };
            let mut preview = app.tabs.clone();
            preview[Tab::Calls.index()].scope = scope;
            let original = std::mem::replace(&mut app.tabs, preview);
            let selected = app
                .visible_rows(Tab::Calls)
                .first()
                .map(|row| row.key.clone());
            app.tabs = original;
            selected
        };

        app.activate_selected();

        assert_eq!(app.current_tab, Tab::Calls);
        assert_eq!(app.tab_state(Tab::Calls).follow_mode, FollowMode::Browse);
        assert_eq!(app.tab_state(Tab::Calls).selected, expected_selected);
        assert_eq!(app.tab_state(Tab::Calls).scroll_offset, 0);
        assert_eq!(app.tab_state(Tab::Calls).unseen_count, 0);
    }

    #[test]
    fn history_rebuild_preserves_interaction_state() {
        let path = history_path();
        let mut history = PersistedHistory::open(&path).expect("open history");
        let events = vec![
            event(
                "session-a",
                Some("label-shared"),
                "shell",
                Some("call-1"),
                ToolEventKind::ToolCallStart,
                "2026-03-07T10:00:00Z",
            ),
            event(
                "session-b",
                Some("label-shared"),
                "shell",
                Some("call-1"),
                ToolEventKind::ToolCallStart,
                "2026-03-07T10:00:01Z",
            ),
            event(
                "session-a",
                Some("label-shared"),
                "shell",
                Some("call-1"),
                ToolEventKind::ToolCallResult,
                "2026-03-07T10:00:02Z",
            ),
            event(
                "session-b",
                Some("label-shared"),
                "shell",
                Some("call-1"),
                ToolEventKind::ToolCallResult,
                "2026-03-07T10:00:03Z",
            ),
        ];
        let mut app = app();
        for item in events {
            let ts = item.timestamp.unwrap();
            history.append(ts, &item).expect("append history");
            app.ingest_event(item, ts);
        }

        app.switch_tab(Tab::Sessions);
        app.tab_state_mut(Tab::Sessions).selected =
            Some(EntityKey::Session("session-a".to_string()));
        app.activate_selected();
        assert_eq!(app.current_tab, Tab::Calls);
        app.activate_selected();
        assert_eq!(app.current_tab, Tab::Events);
        app.open_detail();
        let detail_entity = app.selected_detail_entity().cloned();
        let route_depth = app.route_stack.len();

        let mut tracker = StaleTracker::new(30);
        let mut last_history_id = 0;
        rebuild_history_view(&mut app, &mut tracker, &history, &mut last_history_id, 30)
            .expect("rebuild history view");

        assert_eq!(app.current_tab, Tab::Events);
        assert!(app.detail.is_some());
        assert_eq!(app.selected_detail_entity().cloned(), detail_entity);
        assert_eq!(app.route_stack.len(), route_depth);
        assert_eq!(
            app.tab_state(Tab::Events).scope.session_id.as_deref(),
            Some("session-a")
        );
        assert_eq!(
            app.tab_state(Tab::Events).scope.call_entity_id.as_deref(),
            Some("session-a:call-1")
        );

        fs::remove_dir_all(path.parent().expect("parent")).expect("cleanup");
    }

    #[test]
    fn detail_and_pinned_selection_stay_on_same_entity_after_prepends() {
        let mut app = app();
        seed_many_events(&mut app, 12);

        app.current_tab = Tab::Events;
        app.jump_to(8);
        let before_y = selected_row_y(&mut app, 120, 20)
            .expect("render selected row")
            .expect("selected marker");
        app.open_detail();
        let before = app.selected_detail_entity().cloned();
        let before_scroll = app.tab_state(Tab::Events).scroll_offset;

        let new_event = event(
            "session-z",
            Some("z"),
            "shell",
            Some("call-z"),
            ToolEventKind::ToolCallStart,
            "2026-03-07T10:00:59Z",
        );
        let ts = new_event.timestamp.unwrap();
        app.ingest_event(new_event, ts);
        assert_eq!(app.selected_detail_entity().cloned(), before);
        app.unwind_route();
        let after_y = selected_row_y(&mut app, 120, 20)
            .expect("render selected row after prepend")
            .expect("selected marker");

        assert_eq!(app.tab_state(Tab::Events).selected, before);
        assert_eq!(app.tab_state(Tab::Events).scroll_offset, before_scroll + 1);
        assert_eq!(after_y, before_y);
    }

    #[test]
    fn gg_prefix_is_cleared_when_layers_change() {
        let mut app = app();
        seed(&mut app);
        app.current_tab = Tab::Sessions;
        app.move_selection(1);
        let selection = app.tab_state(Tab::Sessions).selected.clone();

        assert!(!perform_action(&mut app, Action::GotoTopPrefix));
        assert!(app.awaiting_gg);
        assert!(!perform_action(&mut app, Action::OpenDetail));
        assert!(!app.awaiting_gg);
        assert_eq!(app.selected_detail_entity().cloned(), selection);
        assert!(!perform_action(&mut app, Action::Close));
        assert!(app.detail.is_none());

        assert!(!perform_action(&mut app, Action::GotoTopPrefix));
        assert!(app.awaiting_gg);
        assert!(!perform_action(&mut app, Action::ToggleHelp));
        assert!(!app.awaiting_gg);
        assert!(app.help_open);
    }

    #[test]
    fn fullscreen_detail_hides_workspace_chrome() {
        let mut app = app();
        seed(&mut app);
        app.tab_state_mut(Tab::Events).selected = Some(EntityKey::Event("event-3".to_string()));
        app.open_detail();

        let rendered = render_string(&mut app).expect("rendered");
        assert!(rendered.contains("Event Detail"));
        assert!(!rendered.contains("OpenClaw Logpulse"));
        assert!(!rendered.contains("Toggle contextual help"));
    }

    #[test]
    fn help_overlay_scroll_keys_change_help_view() {
        let mut test_app = app();
        seed(&mut test_app);
        test_app.help_open = true;

        let rendered = render_string(&mut test_app).expect("rendered");
        assert!(rendered.contains("Contextual Help"));
        assert!(rendered.contains("Sessions [FOLLOW]"));
        assert!(rendered.contains("Enter"));
        assert!(rendered.contains("Toggle contextual help"));

        let before_scroll = render_string(&mut test_app).expect("before scroll");
        assert_eq!(test_app.help_scroll, 0);
        assert!(!perform_action(&mut test_app, Action::ScrollDown));
        assert_eq!(test_app.help_scroll, 1);
        assert!(!perform_action(&mut test_app, Action::ScrollDown));
        assert_eq!(test_app.help_scroll, 2);
        let after_scroll = render_string(&mut test_app).expect("after scroll");
        assert_ne!(before_scroll, after_scroll);

        let binding = resolve_action(
            &App {
                help_open: false,
                ..app()
            },
            &KeyEvent::from(KeyCode::Char('?')),
        );
        assert_eq!(binding, Some(Action::ToggleHelp));
    }

    #[test]
    fn tabs_are_reordered_with_numeric_and_navigation_bindings() {
        let mut app = app();

        assert_eq!(app.current_tab, Tab::Sessions);

        assert_eq!(
            resolve_action(&app, &KeyEvent::from(KeyCode::Char('1'))),
            Some(Action::TabSessions)
        );
        assert_eq!(
            resolve_action(&app, &KeyEvent::from(KeyCode::Char('2'))),
            Some(Action::TabCalls)
        );
        assert_eq!(
            resolve_action(&app, &KeyEvent::from(KeyCode::Char('3'))),
            Some(Action::TabEvents)
        );

        perform_action(&mut app, Action::NextTab);
        assert_eq!(app.current_tab, Tab::Calls);
        perform_action(&mut app, Action::NextTab);
        assert_eq!(app.current_tab, Tab::Events);
        perform_action(&mut app, Action::PreviousTab);
        assert_eq!(app.current_tab, Tab::Calls);
    }

    #[test]
    fn gg_and_g_jump_in_list_and_detail() {
        let mut app = app();
        seed(&mut app);

        app.current_tab = Tab::Sessions;
        assert_eq!(app.current_tab, Tab::Sessions);

        perform_action(&mut app, Action::NextRow);
        let rows_after_move = app.visible_rows(Tab::Sessions);
        assert!(rows_after_move.len() > 1);

        perform_action(&mut app, Action::LastRow);
        assert_eq!(
            app.tab_state(Tab::Sessions).selected,
            rows_after_move.last().map(|row| row.key.clone())
        );

        assert_eq!(
            resolve_action(&app, &KeyEvent::from(KeyCode::Char('g'))),
            Some(Action::GotoTopPrefix)
        );
        assert!(!perform_action(&mut app, Action::FirstRow));
        assert_eq!(
            app.tab_state(Tab::Sessions).selected,
            rows_after_move.first().map(|row| row.key.clone())
        );

        let mut long_event = event(
            "session-detail",
            Some("session-detail-label"),
            "shell",
            Some("detail-call"),
            ToolEventKind::ToolCallStart,
            "2026-03-07T10:00:59Z",
        );
        long_event.message = Some("x".repeat(1200));
        long_event.raw_line = format!("{{\"detail\":\"{}\"}}", "x".repeat(1200));
        let ts = long_event.timestamp.unwrap();
        app.ingest_event(long_event, ts);

        app.current_tab = Tab::Events;
        app.tab_state_mut(Tab::Events).selected = app
            .visible_rows(Tab::Events)
            .first()
            .map(|row| row.key.clone());
        app.open_detail();

        render_string_with_size(&mut app, 40, 10).expect("detail render");
        let detail_limit = app.detail_scroll_limit();
        assert!(detail_limit > 0);

        perform_action(&mut app, Action::LastRow);
        assert_eq!(app.detail.as_ref().unwrap().scroll, detail_limit);

        if let Some(detail) = app.detail.as_mut() {
            detail.scroll = detail.scroll.saturating_add(1000);
        }

        render_string_with_size(&mut app, 20, 6).expect("detail render narrow");
        let at_bottom_after_resize = app.detail_scroll_limit();
        assert_eq!(app.detail.as_ref().unwrap().scroll, at_bottom_after_resize);

        let at_bottom_after_resize = app.detail.as_ref().unwrap().scroll;
        perform_action(&mut app, Action::ScrollDown);
        assert_eq!(app.detail.as_ref().unwrap().scroll, at_bottom_after_resize);
    }

    #[test]
    fn help_scrolls_to_wrap_aware_bottom_and_stays_clamped() {
        let mut app = app();
        seed(&mut app);
        app.help_open = true;

        render_string_with_size(&mut app, 70, 12).expect("help render");
        let limit = app.help_scroll_limit();
        assert!(limit > 0);

        assert!(!perform_action(&mut app, Action::LastRow));
        assert_eq!(app.help_scroll, limit);

        perform_action(&mut app, Action::ScrollDown);
        assert_eq!(app.help_scroll, limit);
    }

    #[test]
    fn drilldown_events_keep_in_scope_stale_notices() {
        let mut app = app();
        seed(&mut app);

        app.switch_tab(Tab::Calls);
        app.tab_state_mut(Tab::Calls).scope = DrilldownScope {
            session_id: Some("session-a".to_string()),
            call_entity_id: None,
        };
        app.tab_state_mut(Tab::Calls).selected =
            Some(EntityKey::Call("session-a:call-1".to_string()));
        app.activate_selected();

        let event_rows = app.visible_rows(Tab::Events);
        assert!(event_rows
            .iter()
            .any(|row| row.key == EntityKey::Notice("notice-1".to_string())));

        app.tab_state_mut(Tab::Events).scope.call_entity_id = Some("session-a:call-1".to_string());
        let call_scoped_rows = app.visible_rows(Tab::Events);
        assert!(call_scoped_rows
            .iter()
            .any(|row| row.key == EntityKey::Notice("notice-1".to_string())));
    }

    #[test]
    fn discord_resolution_updates_workspace_rows_and_breadcrumbs() {
        let (lookup, request_rx, release_tx) = BlockingDiscordLookup::new();
        let mut app = App::with_session_labels(
            filters(),
            30,
            SessionLabelResolver::with_lookup(chrono::Duration::minutes(5), lookup),
        );
        let item = discord_event(
            "session-discord",
            "1234567890",
            "read",
            Some("call-1"),
            ToolEventKind::ToolCallStart,
            "2026-03-07T10:00:00Z",
        );
        let ts = item.timestamp.expect("timestamp");
        app.ingest_event(item, ts);
        app.switch_tab(Tab::Sessions);

        let pending = render_string(&mut app).expect("pending render");
        assert!(pending.contains("#1234567890 (resolving)"));
        assert_eq!(
            request_rx
                .recv_timeout(StdDuration::from_secs(1))
                .expect("request"),
            "1234567890"
        );

        release_tx
            .send(Ok("ops-war-room".to_string()))
            .expect("release lookup");
        wait_for(|| app.refresh_session_labels(Utc::now()));

        app.tab_state_mut(Tab::Sessions).selected =
            Some(EntityKey::Session("session-discord".to_string()));
        app.activate_selected();

        let resolved = render_string(&mut app).expect("resolved render");
        assert!(resolved.contains("#ops-war-room"));
        assert!(resolved.contains("session #ops-war-room"));
    }

    #[test]
    fn discord_manifest_routing_updates_event_surface_and_inspector() {
        let session_id = "b18666a8-b5d5-4e92-a7a3-a2d1e72ac6f8";
        let channel_id = "1477636729950179490";
        let (session_path, cleanup) = write_session_fixture(
            session_id,
            &format!(
                r#"{{
  "agent:main:main": {{
    "sessionId": "{session_id}",
    "deliveryContext": {{
      "channel": "discord",
      "to": "channel:{channel_id}"
    }},
    "origin": {{
      "provider": "discord",
      "to": "channel:{channel_id}"
    }}
  }}
}}"#
            ),
        );
        let line = r#"{"type":"message","id":"60167cca","parentId":"1f5ac5f2","timestamp":"2026-03-07T09:31:19.656Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"call_a","name":"read","arguments":{"file_path":"/tmp/a"}}],"stopReason":"toolUse","timestamp":1772875879655}}"#;
        let events = normalize_many_with_source(line, Some(&session_path));

        let (lookup, request_rx, release_tx) = BlockingDiscordLookup::new();
        let mut app = App::with_session_labels(
            filters(),
            30,
            SessionLabelResolver::with_lookup(chrono::Duration::minutes(5), lookup),
        );
        for event in events {
            let ts = event.timestamp.expect("timestamp");
            app.ingest_event(event, ts);
        }

        assert_eq!(
            request_rx
                .recv_timeout(StdDuration::from_secs(1))
                .expect("request"),
            channel_id
        );
        release_tx
            .send(Ok("ops-war-room".to_string()))
            .expect("release lookup");
        wait_for(|| app.refresh_session_labels(Utc::now()));

        app.current_tab = Tab::Events;
        app.tab_state_mut(Tab::Events).selected = app
            .visible_rows(Tab::Events)
            .first()
            .map(|row| row.key.clone());

        let rendered = render_string(&mut app).expect("rendered");
        let header = rendered
            .lines()
            .find(|line| {
                line.contains("time") && line.contains("surface") && line.contains("agent")
            })
            .expect("events header");
        let agent_index = header.find("agent").expect("agent column");
        let surface_index = header.find("surface").expect("surface column");
        let event_row = app
            .visible_rows(Tab::Events)
            .into_iter()
            .next()
            .expect("event row");
        let inspector = inspector_string(&app);
        assert!(
            agent_index < surface_index,
            "expected agent before surface: {header}"
        );
        assert_eq!(event_row.cells[2], "main");
        assert_eq!(event_row.cells[3], "#ops-war-room");
        assert!(!rendered.contains(&format!("#{channel_id}")));
        assert!(inspector.contains("Surface: #ops-war-room"));
        assert!(inspector.contains(&format!("Session ID: {session_id}")));
        assert!(inspector.contains(&format!("Discord Channel ID: {channel_id}")));

        cleanup();
    }

    #[test]
    fn discord_manifest_routing_updates_visible_surface_consistently_across_tabs() {
        let session_id = "b18666a8-b5d5-4e92-a7a3-a2d1e72ac6f8";
        let channel_id = "1477636729950179490";
        let (session_path, cleanup) = write_session_fixture(
            session_id,
            &format!(
                r#"{{
  "agent:main:main": {{
    "sessionId": "{session_id}",
    "deliveryContext": {{
      "channel": "discord",
      "to": "channel:{channel_id}"
    }},
    "origin": {{
      "provider": "discord",
      "to": "channel:{channel_id}"
    }}
  }}
}}"#
            ),
        );
        let line = r#"{"type":"message","id":"60167cca","parentId":"1f5ac5f2","timestamp":"2026-03-07T09:31:19.656Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"call_a","name":"read","arguments":{"file_path":"/tmp/a"}}],"stopReason":"toolUse","timestamp":1772875879655}}"#;
        let events = normalize_many_with_source(line, Some(&session_path));

        let mut app = App::with_session_labels(
            filters(),
            30,
            SessionLabelResolver::with_lookup(
                chrono::Duration::minutes(5),
                ImmediateDiscordLookup {
                    result: Ok("main".to_string()),
                },
            ),
        );
        for event in events {
            let ts = event.timestamp.expect("timestamp");
            app.ingest_event(event, ts);
        }
        wait_for(|| app.refresh_session_labels(Utc::now()));

        app.current_tab = Tab::Sessions;
        let sessions_header = render_string(&mut app).expect("sessions render");
        let sessions_header = sessions_header
            .lines()
            .find(|line| {
                line.contains("time") && line.contains("agent") && line.contains("surface")
            })
            .expect("sessions header");
        assert!(
            sessions_header.find("agent").expect("agent")
                < sessions_header.find("surface").expect("surface"),
            "expected agent before surface: {sessions_header}"
        );
        let session_row = app
            .visible_rows(Tab::Sessions)
            .into_iter()
            .next()
            .expect("session row");
        assert_eq!(session_row.cells[1], "main");
        assert_eq!(session_row.cells[2], "#main");

        app.current_tab = Tab::Calls;
        let call_row = app
            .visible_rows(Tab::Calls)
            .into_iter()
            .next()
            .expect("call row");
        assert_eq!(call_row.cells[3], "main");
        assert_eq!(call_row.cells[4], "#main");

        app.current_tab = Tab::Events;
        let event_row = app
            .visible_rows(Tab::Events)
            .into_iter()
            .next()
            .expect("event row");
        assert_eq!(event_row.cells[2], "main");
        assert_eq!(event_row.cells[3], "#main");

        cleanup();
    }

    #[test]
    fn discord_transcript_routing_survives_later_non_discord_events() {
        let source_path = std::path::Path::new("/tmp/b18666a8-b5d5-4e92-a7a3-a2d1e72ac6f8.jsonl");
        let discord_line = r#"{"type":"message","id":"03db303a","parentId":"fa2536d8","timestamp":"2026-03-08T14:18:38.017Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"call_message|fc_test","name":"message","arguments":{"action":"send","channel":"discord","target":"1477636729950179490","message":"done"}}]}}"#;
        let follow_up_line = r#"{"type":"message","id":"a5c4a17f","parentId":"44293b2d","timestamp":"2026-03-08T14:28:37.914Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"call_read|fc_test","name":"read","arguments":{"file_path":"/tmp/a"}}]}}"#;

        let (lookup, request_rx, release_tx) = BlockingDiscordLookup::new();
        let mut app = App::with_session_labels(
            filters(),
            30,
            SessionLabelResolver::with_lookup(chrono::Duration::minutes(5), lookup),
        );
        for raw in [discord_line, follow_up_line] {
            for event in normalize_many_with_source(raw, Some(source_path)) {
                let ts = event.timestamp.expect("timestamp");
                app.ingest_event(event, ts);
            }
        }

        assert_eq!(
            request_rx
                .recv_timeout(StdDuration::from_secs(1))
                .expect("request"),
            "1477636729950179490"
        );
        release_tx
            .send(Ok("ops-war-room".to_string()))
            .expect("release lookup");
        wait_for(|| app.refresh_session_labels(Utc::now()));

        app.current_tab = Tab::Events;
        app.tab_state_mut(Tab::Events).selected = app
            .visible_rows(Tab::Events)
            .into_iter()
            .find(|row| row.searchable.contains("read"))
            .map(|row| row.key)
            .or_else(|| {
                app.visible_rows(Tab::Events)
                    .first()
                    .map(|row| row.key.clone())
            });

        let rendered = render_string(&mut app).expect("rendered");
        assert!(rendered.contains("surface"));
        assert!(rendered.contains("#ops-war-room"));
        assert!(rendered.contains("Surface: #ops-war-room"));
        assert!(!rendered.contains("Surface: b18666a8"));

        let inspector = inspector_string(&app);
        assert!(inspector.contains("Surface: #ops-war-room"));
        assert!(inspector.contains("Session ID: b18666a8-b5d5-4e92-a7a3-a2d1e72ac6f8"));
    }
}
