use crate::cli::Args;
use crate::event::{NormalizedEvent, Severity, TimeFilter, ToolEventKind};
use crate::normalizer::normalize_many_with_source;
use crate::projection::{
    CallStatus, CorrelatedCall, HealthConfig, HealthStatus, MatchConfidence, ProjectionFilter,
    ProjectionStore, SessionSummary,
};
use crate::stale::{HeartbeatSummary, StaleTracker, StaleWarning};
use crate::{discovery, tailer};
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
use std::env;
use std::hash::{Hash, Hasher};
use std::io::{self, stdout};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const DRAIN_PER_TICK: usize = 128;
const MISSING_TTL_SECONDS: u64 = 30;
const PREVIEW_LEN: usize = 72;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Tab {
    Events,
    Calls,
    Sessions,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::Events, Tab::Calls, Tab::Sessions];

    fn title(self) -> &'static str {
        match self {
            Tab::Events => "Events",
            Tab::Calls => "Correlated Tool Calls",
            Tab::Sessions => "Sessions",
        }
    }

    fn short_title(self) -> &'static str {
        match self {
            Tab::Events => "Events",
            Tab::Calls => "Tool Calls",
            Tab::Sessions => "Sessions",
        }
    }

    fn index(self) -> usize {
        match self {
            Tab::Events => 0,
            Tab::Calls => 1,
            Tab::Sessions => 2,
        }
    }

    fn previous(self) -> Self {
        match self {
            Tab::Events => Tab::Sessions,
            Tab::Calls => Tab::Events,
            Tab::Sessions => Tab::Calls,
        }
    }

    fn next(self) -> Self {
        match self {
            Tab::Events => Tab::Calls,
            Tab::Calls => Tab::Sessions,
            Tab::Sessions => Tab::Events,
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
    Live,
    Pinned,
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
    search_match_index: usize,
    scope: DrilldownScope,
}

impl Default for TabStateModel {
    fn default() -> Self {
        Self {
            selected: None,
            scroll_offset: 0,
            follow_mode: FollowMode::Live,
            unseen_count: 0,
            search_match_index: 0,
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
    scroll: u16,
}

#[derive(Clone, Debug)]
struct WorkspaceFilters {
    session: Option<String>,
    agent: Option<String>,
    tool: Option<String>,
    min_level: Severity,
    time: TimeFilter,
    include_system_events: bool,
    stale_only: bool,
    text_search: Option<String>,
    summary: String,
}

impl WorkspaceFilters {
    fn from_args(args: &Args, time: TimeFilter) -> Self {
        Self {
            session: args.session.clone(),
            agent: args.agent.clone(),
            tool: args.tool.clone(),
            min_level: args.min_severity(),
            time,
            include_system_events: false,
            stale_only: false,
            text_search: None,
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
    Error(String),
}

#[derive(Clone, Debug)]
struct NoticeRecord {
    id: String,
    seen_at: DateTime<Utc>,
    kind: NoticeKind,
}

#[derive(Clone, Debug)]
struct VisibleRow {
    key: EntityKey,
    cells: Vec<String>,
    searchable: String,
    sort_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActiveLayer {
    Workspace,
    Detail,
    Help,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Close,
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
const ALL_TABS: &[Tab] = &[Tab::Events, Tab::Calls, Tab::Sessions];
const EVENTS_ONLY: &[Tab] = &[Tab::Events];

const KEY_BINDINGS: &[KeyBinding] = &[
    KeyBinding {
        matcher: KeyMatcher::Char('q'),
        action: Action::Close,
        description: "Close overlay/detail or quit",
        layers: &[
            ActiveLayer::Workspace,
            ActiveLayer::Detail,
            ActiveLayer::Help,
        ],
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Esc,
        action: Action::Close,
        description: "Close overlay/detail or unwind route",
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
        action: Action::FirstRow,
        description: "Jump to newest row",
        layers: WORKSPACE_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Char('G'),
        action: Action::LastRow,
        description: "Jump to oldest row",
        layers: WORKSPACE_ONLY,
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
        action: Action::TabEvents,
        description: "Jump to Events",
        layers: WORKSPACE_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Char('2'),
        action: Action::TabCalls,
        description: "Jump to Correlated Tool Calls",
        layers: WORKSPACE_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Char('3'),
        action: Action::TabSessions,
        description: "Jump to Sessions",
        layers: WORKSPACE_ONLY,
        tabs: ALL_TABS,
    },
    KeyBinding {
        matcher: KeyMatcher::Char('f'),
        action: Action::ResumeLive,
        description: "Resume LIVE",
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
];

struct App {
    store: ProjectionStore,
    events_by_ref: HashMap<String, NormalizedEvent>,
    notices: Vec<NoticeRecord>,
    latest_heartbeat: Option<HeartbeatSummary>,
    current_tab: Tab,
    tabs: [TabStateModel; 3],
    route_stack: Vec<RouteSnapshot>,
    detail: Option<DetailState>,
    help_open: bool,
    help_scroll: u16,
    filters: WorkspaceFilters,
    health: HealthConfig,
    next_notice_id: u64,
    now: DateTime<Utc>,
}

impl App {
    fn new(filters: WorkspaceFilters, stale_after_seconds: u64) -> Self {
        Self {
            store: ProjectionStore::default(),
            events_by_ref: HashMap::new(),
            notices: Vec::new(),
            latest_heartbeat: None,
            current_tab: Tab::Events,
            tabs: [
                TabStateModel::default(),
                TabStateModel::default(),
                TabStateModel::default(),
            ],
            route_stack: Vec::new(),
            detail: None,
            help_open: false,
            help_scroll: 0,
            filters,
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

    fn ingest_event(&mut self, event: NormalizedEvent, observed_at: DateTime<Utc>) {
        let before = self.snapshot_visible_keys();
        let event_ref = self.store.ingest_event(event.clone(), observed_at);
        self.events_by_ref.insert(event_ref, event);
        self.now = observed_at;
        self.reconcile_after_data_change(before);
    }

    fn ingest_warning(&mut self, warning: StaleWarning, observed_at: DateTime<Utc>) {
        let before = self.snapshot_visible_keys();
        self.push_notice(NoticeKind::Stale(warning), observed_at);
        self.now = observed_at;
        self.reconcile_after_data_change(before);
    }

    fn ingest_heartbeat(&mut self, summary: HeartbeatSummary, observed_at: DateTime<Utc>) {
        let before = self.snapshot_visible_keys();
        self.latest_heartbeat = Some(summary.clone());
        self.push_notice(NoticeKind::Heartbeat(summary), observed_at);
        self.now = observed_at;
        self.reconcile_after_data_change(before);
    }

    fn ingest_error(&mut self, message: impl Into<String>, observed_at: DateTime<Utc>) {
        let before = self.snapshot_visible_keys();
        self.push_notice(NoticeKind::Error(message.into()), observed_at);
        self.now = observed_at;
        self.reconcile_after_data_change(before);
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
            .map(|tab| {
                (
                    tab,
                    self.visible_rows(tab)
                        .into_iter()
                        .map(|row| row.key)
                        .collect::<Vec<_>>(),
                )
            })
            .collect()
    }

    fn reconcile_after_data_change(&mut self, before: HashMap<Tab, Vec<EntityKey>>) {
        for tab in Tab::ALL {
            let after_rows = self.visible_rows(tab);
            let after_keys = after_rows
                .iter()
                .map(|row| row.key.clone())
                .collect::<Vec<_>>();
            let state = self.tab_state_mut(tab);
            if after_keys.is_empty() {
                state.selected = None;
                state.scroll_offset = 0;
                state.unseen_count = 0;
                continue;
            }

            if state.follow_mode == FollowMode::Live {
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

            if state
                .selected
                .as_ref()
                .map(|selected| after_keys.iter().any(|candidate| candidate == selected))
                != Some(true)
            {
                state.selected = after_keys.first().cloned();
                state.scroll_offset = 0;
            }
        }
    }

    fn visible_rows(&self, tab: Tab) -> Vec<VisibleRow> {
        match tab {
            Tab::Events => self.visible_event_rows(),
            Tab::Calls => self.visible_call_rows(),
            Tab::Sessions => self.visible_session_rows(),
        }
    }

    fn visible_event_rows(&self) -> Vec<VisibleRow> {
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
                self.matches_text_search(&[
                    row.session_label.as_deref().unwrap_or_default(),
                    row.agent_id.as_deref().unwrap_or_default(),
                    row.tool_name.as_deref().unwrap_or_default(),
                    row.status.as_deref().unwrap_or_default(),
                    row.preview.as_deref().unwrap_or_default(),
                ])
            })
            .map(|row| VisibleRow {
                key: EntityKey::Event(row.event_ref.clone()),
                searchable: [
                    row.session_label.clone().unwrap_or_default(),
                    row.agent_id.clone().unwrap_or_default(),
                    row.tool_name.clone().unwrap_or_default(),
                    row.status.clone().unwrap_or_default(),
                    row.preview.clone().unwrap_or_default(),
                ]
                .join(" "),
                sort_at: row.timestamp.unwrap_or(self.now),
                cells: vec![
                    format_ts(row.timestamp.unwrap_or(self.now)),
                    kind_label(&row.kind).to_string(),
                    truncate_display(row.session_label.as_deref().unwrap_or("-"), 20),
                    truncate_display(row.agent_id.as_deref().unwrap_or("-"), 12),
                    truncate_display(row.tool_name.as_deref().unwrap_or("-"), 14),
                    truncate_display(row.status.as_deref().unwrap_or("-"), 14),
                    truncate_display(row.preview.as_deref().unwrap_or("-"), PREVIEW_LEN),
                ],
            })
            .collect::<Vec<_>>();

        rows.extend(self.visible_notice_rows(scope));

        rows.sort_by(|a, b| {
            b.sort_at
                .cmp(&a.sort_at)
                .then_with(|| a.searchable.cmp(&b.searchable))
        });
        rows
    }

    fn visible_notice_rows(&self, scope: &DrilldownScope) -> Vec<VisibleRow> {
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
                    Some(VisibleRow {
                        key: EntityKey::Notice(notice.id.clone()),
                        cells: vec![
                            format_ts(notice.seen_at),
                            "STALE".to_string(),
                            truncate_display(warning.session_key.as_deref().unwrap_or("-"), 20),
                            "-".to_string(),
                            truncate_display(warning.tool_name.as_deref().unwrap_or("-"), 14),
                            format!("{}s", warning.age_seconds),
                            truncate_display(
                                warning.message.as_deref().unwrap_or("Long-running call"),
                                PREVIEW_LEN,
                            ),
                        ],
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
                    Some(VisibleRow {
                        key: EntityKey::Notice(notice.id.clone()),
                        cells: vec![
                            format_ts(notice.seen_at),
                            "HB".to_string(),
                            "-".to_string(),
                            "-".to_string(),
                            "heartbeat".to_string(),
                            format!("a={} s={}", summary.active_calls, summary.stale_calls),
                            truncate_display(&summary.to_line(), PREVIEW_LEN),
                        ],
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
                    Some(VisibleRow {
                        key: EntityKey::Notice(notice.id.clone()),
                        cells: vec![
                            format_ts(notice.seen_at),
                            "ERR".to_string(),
                            "system".to_string(),
                            "-".to_string(),
                            "error".to_string(),
                            "error".to_string(),
                            truncate_display(message, PREVIEW_LEN),
                        ],
                        searchable: message.clone(),
                        sort_at: notice.seen_at,
                    })
                }
                _ => None,
            })
            .collect()
    }

    fn visible_call_rows(&self) -> Vec<VisibleRow> {
        let state = self.tab_state(Tab::Calls);
        self.store
            .correlated_calls(&self.projection_filter(), self.now, &self.health)
            .into_iter()
            .filter(|call| {
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
                    &call.session_label,
                    call.agent_id.as_deref().unwrap_or_default(),
                    call.tool_name.as_deref().unwrap_or_default(),
                    call.message_preview.as_deref().unwrap_or_default(),
                ])
            })
            .map(|call| VisibleRow {
                key: EntityKey::Call(call.call_entity_id.clone()),
                searchable: [
                    call.session_label.clone(),
                    call.agent_id.clone().unwrap_or_default(),
                    call.tool_name.clone().unwrap_or_default(),
                    call.message_preview.clone().unwrap_or_default(),
                ]
                .join(" "),
                sort_at: call.started_at.or(call.last_updated_at).unwrap_or(self.now),
                cells: vec![
                    format_ts(call.started_at.or(call.last_updated_at).unwrap_or(self.now)),
                    call_status_label(call.status).to_string(),
                    truncate_display(&call.session_label, 20),
                    truncate_display(call.agent_id.as_deref().unwrap_or("-"), 12),
                    truncate_display(call.tool_name.as_deref().unwrap_or("-"), 14),
                    match call.duration_ms {
                        Some(ms) => format!("{ms}ms"),
                        None => "-".to_string(),
                    },
                    truncate_display(call.message_preview.as_deref().unwrap_or("-"), PREVIEW_LEN),
                ],
            })
            .collect()
    }

    fn visible_session_rows(&self) -> Vec<VisibleRow> {
        self.store
            .sessions(&self.projection_filter(), self.now, &self.health)
            .into_iter()
            .filter(|session| {
                if self.filters.stale_only
                    && session.health_status != HealthStatus::Stale
                    && session.health_status != HealthStatus::Disconnected
                {
                    return false;
                }
                self.matches_text_search(&[
                    &session.session_id,
                    &session.session_label,
                    session.agent_id.as_deref().unwrap_or_default(),
                ])
            })
            .map(|session| {
                let health = health_status_label(session.health_status.clone()).to_string();
                VisibleRow {
                    key: EntityKey::Session(session.session_id.clone()),
                    searchable: [
                        session.session_label.clone(),
                        session.agent_id.clone().unwrap_or_default(),
                        health.clone(),
                    ]
                    .join(" "),
                    sort_at: session.last_activity_at.unwrap_or(self.now),
                    cells: vec![
                        format_ts(session.last_activity_at.unwrap_or(self.now)),
                        truncate_display(&session.session_label, 22),
                        truncate_display(session.agent_id.as_deref().unwrap_or("-"), 12),
                        health,
                        session.open_call_count.to_string(),
                        session.stale_call_count.to_string(),
                        severity_label(session.derived_severity).to_string(),
                    ],
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

    fn selected_index(&self, tab: Tab, rows: &[VisibleRow]) -> Option<usize> {
        self.tab_state(tab)
            .selected
            .as_ref()
            .and_then(|selected| rows.iter().position(|row| &row.key == selected))
            .or_else(|| (!rows.is_empty()).then_some(0))
    }

    fn resume_live(&mut self) {
        let rows = self.visible_rows(self.current_tab);
        let state = self.current_tab_state_mut();
        state.follow_mode = FollowMode::Live;
        state.unseen_count = 0;
        state.scroll_offset = 0;
        state.selected = rows.first().map(|row| row.key.clone());
    }

    fn move_selection(&mut self, delta: isize) {
        let rows = self.visible_rows(self.current_tab);
        if rows.is_empty() {
            return;
        }
        let current_index = self.selected_index(self.current_tab, &rows).unwrap_or(0) as isize;
        let next_index = (current_index + delta).clamp(0, rows.len().saturating_sub(1) as isize);
        let state = self.current_tab_state_mut();
        state.follow_mode = FollowMode::Pinned;
        state.selected = rows.get(next_index as usize).map(|row| row.key.clone());
        state.scroll_offset = next_index.saturating_sub(3) as usize;
    }

    fn jump_to(&mut self, index: usize) {
        let rows = self.visible_rows(self.current_tab);
        if rows.is_empty() {
            return;
        }
        let target = index.min(rows.len().saturating_sub(1));
        let state = self.current_tab_state_mut();
        state.follow_mode = FollowMode::Pinned;
        state.selected = rows.get(target).map(|row| row.key.clone());
        state.scroll_offset = target.saturating_sub(3);
    }

    fn switch_tab(&mut self, tab: Tab) {
        self.current_tab = tab;
        if self.current_tab_state().follow_mode == FollowMode::Live {
            let rows = self.visible_rows(tab);
            self.tab_state_mut(tab).selected = rows.first().map(|row| row.key.clone());
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
                    preview[Tab::Calls.index()].follow_mode = FollowMode::Live;
                    let original = std::mem::replace(&mut self.tabs, preview);
                    let rows = self.visible_rows(Tab::Calls);
                    self.tabs = original;
                    rows.first().map(|row| row.key.clone())
                };
                let target = self.tab_state_mut(Tab::Calls);
                target.scope = scope;
                target.follow_mode = FollowMode::Live;
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
                    preview[Tab::Events.index()].follow_mode = FollowMode::Pinned;
                    let original = std::mem::replace(&mut self.tabs, preview);
                    let rows = self.visible_rows(Tab::Events);
                    self.tabs = original;
                    rows.first().map(|row| row.key.clone())
                };
                let target = self.tab_state_mut(Tab::Events);
                target.scope = scope;
                target.follow_mode = FollowMode::Pinned;
                target.unseen_count = 0;
                target.scroll_offset = 0;
                target.selected = selected;
                self.current_tab = Tab::Events;
            }
            Tab::Events => self.open_detail(),
        }
    }

    fn open_detail(&mut self) {
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
        lines.push(kv_line(
            "Session",
            event
                .session_id
                .as_deref()
                .unwrap_or_else(|| event.session_key.as_deref().unwrap_or("-")),
        ));
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
        Text::from(vec![
            title_line(
                &format!("CALL {}", call.tool_name.as_deref().unwrap_or("-")),
                tool_color(call.tool_name.as_deref().unwrap_or("call")),
            ),
            kv_line("Entity ID", &call.call_entity_id),
            kv_line("Session", &call.session_label),
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
        Text::from(vec![
            title_line("SESSION", Color::Cyan),
            kv_line("Session ID", &session.session_id),
            kv_line("Label", &session.session_label),
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
            kv_line("Health", health_status_label(session.health_status)),
            kv_line("Open calls", &session.open_call_count.to_string()),
            kv_line("Stale calls", &session.stale_call_count.to_string()),
            kv_line("Severity", severity_label(session.derived_severity)),
        ])
    }

    fn breadcrumb(&self) -> String {
        let scope = self.current_tab_state().scope.clone();
        let mut parts = vec![self.current_tab.title().to_string()];
        if let Some(session_id) = scope.session_id {
            parts.push(format!("session {session_id}"));
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
                    FollowMode::Live => "LIVE",
                    FollowMode::Pinned => "PINNED",
                },
            ),
            kv_line(
                "Search match",
                &self.current_tab_state().search_match_index.to_string(),
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
    let heartbeat_interval = args.heartbeat_duration();
    let ui_tick = Duration::from_millis(50);
    let mut last_heartbeat = Instant::now();

    if args.log_file.is_none() {
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
                app.ingest_heartbeat(tracker.heartbeat(Utc::now()), Utc::now());
                last_heartbeat = now;
            }

            if !args.no_follow && now.duration_since(last_scan) >= tailer.poll_interval() {
                discovered_paths = discover_initial_session_logs();
                tailer.sync(discovered_paths);
                last_scan = now;
            }

            drain_multi_tailer(&mut tailer, &mut tracker, &mut app);
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
            app.ingest_error(
                format!("failed to open {}: {err}", log_file.display()),
                Utc::now(),
            );
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
            app.ingest_heartbeat(tracker.heartbeat(Utc::now()), Utc::now());
            last_heartbeat = now;
        }

        drain_single_tailer(&mut tailer, &log_file, &mut tracker, &mut app);
        terminal.draw(|frame| render(frame, &app))?;
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

fn handle_input(app: &mut App, timeout: Duration) -> io::Result<bool> {
    if !event::poll(timeout)? {
        return Ok(false);
    }

    if let Event::Key(key) = event::read()? {
        if key.kind != KeyEventKind::Press {
            return Ok(false);
        }

        let Some(action) = resolve_action(app, &key) else {
            return Ok(false);
        };

        return Ok(perform_action(app, action));
    }

    Ok(false)
}

fn perform_action(app: &mut App, action: Action) -> bool {
    match action {
        Action::Close => app.unwind_route(),
        Action::NextRow => {
            app.move_selection(1);
            false
        }
        Action::PreviousRow => {
            app.move_selection(-1);
            false
        }
        Action::FirstRow => {
            app.jump_to(0);
            false
        }
        Action::LastRow => {
            let last = app.visible_rows(app.current_tab).len().saturating_sub(1);
            app.jump_to(last);
            false
        }
        Action::ScrollDown => {
            if app.help_open {
                app.help_scroll = app.help_scroll.saturating_add(1);
            } else if let Some(detail) = app.detail.as_mut() {
                detail.scroll = detail.scroll.saturating_add(1);
            }
            false
        }
        Action::ScrollUp => {
            if app.help_open {
                app.help_scroll = app.help_scroll.saturating_sub(1);
            } else if let Some(detail) = app.detail.as_mut() {
                detail.scroll = detail.scroll.saturating_sub(1);
            }
            false
        }
        Action::ResumeLive => {
            app.resume_live();
            false
        }
        Action::PreviousTab => {
            app.switch_tab(app.current_tab.previous());
            false
        }
        Action::NextTab => {
            app.switch_tab(app.current_tab.next());
            false
        }
        Action::TabEvents => {
            app.switch_tab(Tab::Events);
            false
        }
        Action::TabCalls => {
            app.switch_tab(Tab::Calls);
            false
        }
        Action::TabSessions => {
            app.switch_tab(Tab::Sessions);
            false
        }
        Action::Activate => {
            app.activate_selected();
            false
        }
        Action::OpenDetail => {
            app.open_detail();
            false
        }
        Action::ToggleHelp => {
            app.help_open = !app.help_open;
            if app.help_open {
                app.help_scroll = 0;
            }
            false
        }
    }
}

fn drain_multi_tailer(tailer: &mut tailer::MultiTailer, tracker: &mut StaleTracker, app: &mut App) {
    for _ in 0..DRAIN_PER_TICK {
        match tailer.next_line() {
            Ok(Some((path, raw_line))) => {
                ingest_line(&raw_line, Some(path.as_path()), tracker, app)
            }
            Ok(None) => break,
            Err(err) => {
                app.ingest_error(err.to_string(), Utc::now());
                break;
            }
        }
    }
}

fn drain_single_tailer(
    tailer: &mut tailer::Tailer,
    log_file: &Path,
    tracker: &mut StaleTracker,
    app: &mut App,
) {
    for _ in 0..DRAIN_PER_TICK {
        match tailer.next_line() {
            Ok(Some(raw_line)) => ingest_line(&raw_line, Some(log_file), tracker, app),
            Ok(None) => break,
            Err(err) => {
                app.ingest_error(err.to_string(), Utc::now());
                break;
            }
        }
    }
}

fn ingest_line(
    raw_line: &str,
    source_path: Option<&Path>,
    tracker: &mut StaleTracker,
    app: &mut App,
) {
    let now = Utc::now();
    for event in normalize_many_with_source(raw_line, source_path) {
        let warnings = tracker.on_event(&event, now);
        app.ingest_event(event, now);
        for warning in warnings {
            if app.filters.time.contains(Some(now)) {
                app.ingest_warning(warning, now);
            }
        }
    }
}

fn render(frame: &mut Frame, app: &App) {
    if app.detail.is_some() {
        render_detail(frame, frame.area(), app);
        if app.help_open {
            render_help(frame, app);
        }
        return;
    }

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(10),
            Constraint::Length(2),
        ])
        .split(frame.area());

    render_header(frame, layout[0], app);
    render_workspace(frame, layout[1], app);
    render_footer(frame, layout[2], app);

    if app.help_open {
        render_help(frame, app);
    }
}

fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let (busy, stale, disconnected) = app.health_counts();
    let tab_line = Tab::ALL
        .into_iter()
        .map(|tab| {
            let state = app.tab_state(tab);
            let live = match state.follow_mode {
                FollowMode::Live => "LIVE",
                FollowMode::Pinned => "PINNED",
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
                format!(" {} [{}{}] ", tab.short_title(), live, unseen),
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
        Line::from(vec![
            Span::styled("health ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!(
                "busy {}  stale {}  disconnected {}",
                busy, stale, disconnected
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
    let rows = app.visible_rows(app.current_tab);
    let selected_index = app.selected_index(app.current_tab, &rows);
    let tab_state = app.current_tab_state();
    let widths = match app.current_tab {
        Tab::Events => [
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Length(20),
            Constraint::Length(12),
            Constraint::Length(14),
            Constraint::Length(14),
            Constraint::Min(20),
        ]
        .to_vec(),
        Tab::Calls => [
            Constraint::Length(8),
            Constraint::Length(11),
            Constraint::Length(20),
            Constraint::Length(12),
            Constraint::Length(14),
            Constraint::Length(10),
            Constraint::Min(20),
        ]
        .to_vec(),
        Tab::Sessions => [
            Constraint::Length(8),
            Constraint::Length(22),
            Constraint::Length(12),
            Constraint::Length(14),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(10),
        ]
        .to_vec(),
    };

    let header = match app.current_tab {
        Tab::Events => vec![
            "time", "kind", "session", "agent", "tool", "status", "preview",
        ],
        Tab::Calls => vec![
            "time", "status", "session", "agent", "tool", "duration", "preview",
        ],
        Tab::Sessions => vec![
            "time", "session", "agent", "health", "open", "stale", "level",
        ],
    };

    let table_rows = rows
        .iter()
        .map(|row| Row::new(row.cells.clone()))
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

    let mut state = TableState::default().with_offset(tab_state.scroll_offset);
    state.select(selected_index);
    frame.render_stateful_widget(table, area, &mut state);
}

fn render_inspector(frame: &mut Frame, area: Rect, app: &App) {
    let paragraph = Paragraph::new(app.inspector_text())
        .block(Block::default().borders(Borders::ALL).title("Inspector"))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_detail(frame: &mut Frame, area: Rect, app: &App) {
    let scroll = app.detail.as_ref().map(|detail| detail.scroll).unwrap_or(0);
    let paragraph = Paragraph::new(app.inspector_text())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(app.breadcrumb()),
        )
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(paragraph, area);
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let layer = app.layer();
    let bindings = if layer == ActiveLayer::Help {
        active_bindings(ActiveLayer::Help, app.current_tab)
    } else {
        active_bindings(layer, app.current_tab)
    };
    let spans = bindings
        .iter()
        .take(6)
        .flat_map(|binding| {
            vec![
                Span::styled(
                    binding.matcher.label().to_string(),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!(" {}  ", binding.description)),
            ]
        })
        .collect::<Vec<_>>();
    let paragraph = Paragraph::new(Line::from(spans)).block(Block::default().borders(Borders::ALL));
    frame.render_widget(paragraph, area);
}

fn render_help(frame: &mut Frame, app: &App) {
    let popup = centered_rect(80, 70, frame.area());
    frame.render_widget(Clear, popup);
    let paragraph = Paragraph::new(app.help_lines())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Contextual Help"),
        )
        .wrap(Wrap { trim: false })
        .scroll((app.help_scroll, 0));
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

fn kind_label(kind: &ToolEventKind) -> &'static str {
    match kind {
        ToolEventKind::ToolCallStart => "START",
        ToolEventKind::ToolCallResult => "RESULT",
        ToolEventKind::ToolCall => "CALL",
        ToolEventKind::Other => "OTHER",
        ToolEventKind::Malformed => "BAD",
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

    parts.join("  •  ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use std::io;

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

    fn app() -> App {
        App::new(
            WorkspaceFilters {
                session: None,
                agent: None,
                tool: None,
                min_level: Severity::Trace,
                time: TimeFilter::default(),
                include_system_events: false,
                stale_only: false,
                text_search: None,
                summary: "test".to_string(),
            },
            30,
        )
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

    fn render_string(app: &App) -> io::Result<String> {
        let backend = TestBackend::new(120, 40);
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

    #[test]
    fn tab_state_is_remembered_per_tab() {
        let mut app = app();
        seed(&mut app);

        app.switch_tab(Tab::Calls);
        app.move_selection(1);
        let calls_selected = app.tab_state(Tab::Calls).selected.clone();
        assert_eq!(app.tab_state(Tab::Calls).follow_mode, FollowMode::Pinned);

        app.switch_tab(Tab::Sessions);
        app.move_selection(1);
        let sessions_selected = app.tab_state(Tab::Sessions).selected.clone();

        app.switch_tab(Tab::Calls);
        assert_eq!(app.tab_state(Tab::Calls).selected, calls_selected);

        app.switch_tab(Tab::Sessions);
        assert_eq!(app.tab_state(Tab::Sessions).selected, sessions_selected);
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
    fn detail_and_pinned_selection_stay_on_same_entity_after_prepends() {
        let mut app = app();
        seed(&mut app);

        app.tab_state_mut(Tab::Events).selected = Some(EntityKey::Event("event-3".to_string()));
        app.current_tab_state_mut().follow_mode = FollowMode::Pinned;
        app.open_detail();
        let before = app.selected_detail_entity().cloned();

        let new_event = event(
            "session-z",
            Some("z"),
            "shell",
            Some("call-z"),
            ToolEventKind::ToolCallStart,
            "2026-03-07T10:00:10Z",
        );
        let ts = new_event.timestamp.unwrap();
        app.ingest_event(new_event, ts);

        assert_eq!(app.tab_state(Tab::Events).selected, before);
        assert_eq!(app.selected_detail_entity().cloned(), before);
    }

    #[test]
    fn fullscreen_detail_hides_workspace_chrome() {
        let mut app = app();
        seed(&mut app);
        app.tab_state_mut(Tab::Events).selected = Some(EntityKey::Event("event-3".to_string()));
        app.open_detail();

        let rendered = render_string(&app).expect("rendered");
        assert!(rendered.contains("Event Detail"));
        assert!(!rendered.contains("OpenClaw Logpulse"));
        assert!(!rendered.contains("Toggle contextual help"));
    }

    #[test]
    fn help_overlay_scroll_keys_change_help_view() {
        let mut test_app = app();
        seed(&mut test_app);
        test_app.help_open = true;

        let rendered = render_string(&test_app).expect("rendered");
        assert!(rendered.contains("Contextual Help"));
        assert!(rendered.contains("Events [LIVE]"));
        assert!(rendered.contains("Enter"));
        assert!(rendered.contains("Toggle contextual help"));

        let before_scroll = render_string(&test_app).expect("before scroll");
        assert_eq!(test_app.help_scroll, 0);
        assert!(!perform_action(&mut test_app, Action::ScrollDown));
        assert_eq!(test_app.help_scroll, 1);
        assert!(!perform_action(&mut test_app, Action::ScrollDown));
        assert_eq!(test_app.help_scroll, 2);
        let after_scroll = render_string(&test_app).expect("after scroll");
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
}
