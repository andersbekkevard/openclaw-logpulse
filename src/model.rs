#![allow(dead_code)]

use crate::event::{NormalizedEvent, Severity, TimeFilter, ToolEventKind};
use crate::session_identity::shorten_non_discord_session_label;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, VecDeque};

#[derive(Clone, Debug)]
pub struct ProjectionFilter {
    pub session: Option<String>,
    pub agent: Option<String>,
    pub tool: Option<String>,
    pub min_level: Severity,
    pub time: TimeFilter,
}

impl Default for ProjectionFilter {
    fn default() -> Self {
        Self {
            session: None,
            agent: None,
            tool: None,
            min_level: Severity::Trace,
            time: TimeFilter::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct EventRow {
    pub event_ref: String,
    pub timestamp: Option<DateTime<Utc>>,
    pub session_id: Option<String>,
    pub session_label: Option<String>,
    pub session_label_info: Option<SessionLabel>,
    pub agent_id: Option<String>,
    pub tool_name: Option<String>,
    pub kind: ToolEventKind,
    pub status: Option<String>,
    pub severity: Severity,
    pub call_ids: Vec<String>,
    pub preview: Option<String>,
    pub is_system_event: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MatchConfidence {
    ExplicitId,
    TranscriptBundle,
    FallbackSignature,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallStatus {
    Running,
    Succeeded,
    Failed,
    Stale,
    Incomplete,
    Unknown,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceState {
    Live,
    Quiet,
    Missing,
    Unknown,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    Busy,
    Idle,
    Stale,
    Disconnected,
    Unknown,
}

#[derive(Clone, Debug, Serialize)]
pub struct ToolCallSummary {
    pub call_entity_id: String,
    pub session_id: String,
    pub session_label: String,
    pub session_label_info: SessionLabel,
    pub agent_id: Option<String>,
    pub tool_name: Option<String>,
    pub canonical_call_id: Option<String>,
    pub status: CallStatus,
    pub match_confidence: MatchConfidence,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub last_updated_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<u64>,
    pub event_refs_start: Vec<String>,
    pub event_refs_result: Vec<String>,
    pub event_refs_related: Vec<String>,
    pub severity: Severity,
    pub message_preview: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionLabelKind {
    StableSessionId,
    DiscordChannelId,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SessionLabel {
    pub raw: Option<String>,
    pub display: String,
    pub kind: SessionLabelKind,
    pub channel_id: Option<String>,
}

impl SessionLabel {
    fn from_event(event: &NormalizedEvent, session_id: &str) -> Self {
        let raw = event.session_label().cloned();

        if has_explicit_discord_provider(event) {
            if let Some(channel_id) = event.routing.channel_id.clone() {
                return Self {
                    raw,
                    display: format!("discord:{channel_id}"),
                    kind: SessionLabelKind::DiscordChannelId,
                    channel_id: Some(channel_id),
                };
            }
        }

        let stable_source = event
            .durable_session_id()
            .map(str::to_string)
            .or_else(|| raw.clone())
            .unwrap_or_else(|| session_id.to_string());

        Self {
            raw,
            display: shorten_non_discord_session_label(&stable_source),
            kind: SessionLabelKind::StableSessionId,
            channel_id: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SourceHealth {
    pub last_source_seen_at: Option<DateTime<Utc>>,
    pub source_state: SourceState,
    pub health_status: HealthStatus,
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub session_label: String,
    pub session_label_info: SessionLabel,
    pub agent_id: Option<String>,
    pub last_activity_at: Option<DateTime<Utc>>,
    pub last_event_at: Option<DateTime<Utc>>,
    pub last_source_seen_at: Option<DateTime<Utc>>,
    pub open_call_count: usize,
    pub stale_call_count: usize,
    pub derived_severity: Severity,
    pub health_status: HealthStatus,
    pub source_state: SourceState,
    pub source_health: SourceHealth,
}

#[derive(Clone, Debug)]
pub struct HealthConfig {
    pub quiet_after: Duration,
    pub disconnect_after: Duration,
    pub stale_after: Duration,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            quiet_after: Duration::seconds(30),
            disconnect_after: Duration::seconds(90),
            stale_after: Duration::seconds(30),
        }
    }
}

#[derive(Clone, Debug)]
struct EventRecord {
    event_ref: String,
    event: NormalizedEvent,
    observed_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
struct CallAggregate {
    call_entity_id: String,
    session_id: String,
    session_label_info: SessionLabel,
    agent_id: Option<String>,
    tool_name: Option<String>,
    canonical_call_id: Option<String>,
    fallback_signature: Option<String>,
    confidence: MatchConfidence,
    started_at: Option<DateTime<Utc>>,
    ended_at: Option<DateTime<Utc>>,
    last_updated_at: Option<DateTime<Utc>>,
    duration_ms: Option<u64>,
    start_refs: Vec<String>,
    result_refs: Vec<String>,
    related_refs: Vec<String>,
    severity: Severity,
    message_preview: Option<String>,
}

#[derive(Default)]
pub struct AppModel {
    next_event_seq: u64,
    next_fallback_ordinal: HashMap<(String, String), u64>,
    explicit_index: HashMap<(String, String), String>,
    fallback_open_index: HashMap<(String, String), VecDeque<String>>,
    calls: BTreeMap<String, CallAggregate>,
    events: Vec<EventRecord>,
    source_last_seen: HashMap<String, DateTime<Utc>>,
    session_sources: HashMap<String, Vec<String>>,
}

impl AppModel {
    pub fn ingest_event(&mut self, event: NormalizedEvent, observed_at: DateTime<Utc>) -> String {
        self.next_event_seq = self.next_event_seq.saturating_add(1);
        let event_ref = format!("event-{}", self.next_event_seq);
        let session_id = event
            .durable_session_id()
            .map(str::to_string)
            .unwrap_or_else(|| "<unknown>".to_string());

        if let Some(path) = event.source_path.clone() {
            self.source_last_seen.insert(path.clone(), observed_at);
            let sources = self.session_sources.entry(session_id.clone()).or_default();
            if !sources.iter().any(|existing| existing == &path) {
                sources.push(path);
            }
        }

        self.events.push(EventRecord {
            event_ref: event_ref.clone(),
            event: event.clone(),
            observed_at,
        });

        match event.kind {
            ToolEventKind::ToolCallStart => self.record_start(&event, &session_id, &event_ref),
            ToolEventKind::ToolCallResult => self.record_result(&event, &session_id, &event_ref),
            ToolEventKind::ToolCall => self.record_related(&event, &session_id, &event_ref),
            _ => {}
        }

        event_ref
    }

    pub fn project_events(&self, filter: &ProjectionFilter) -> Vec<EventRow> {
        self.event_rows(filter)
    }

    pub fn event_rows(&self, filter: &ProjectionFilter) -> Vec<EventRow> {
        let mut rows = self
            .events
            .iter()
            .filter(|record| event_matches_filter(record, filter))
            .map(|record| {
                let label_info = record
                    .event
                    .durable_session_id()
                    .map(|session_id| SessionLabel::from_event(&record.event, session_id));

                EventRow {
                    event_ref: record.event_ref.clone(),
                    timestamp: record.event.timestamp.or(Some(record.observed_at)),
                    session_id: record.event.session_id.clone(),
                    session_label: label_info.as_ref().map(|label| label.display.clone()),
                    session_label_info: label_info,
                    agent_id: record.event.agent_id.clone(),
                    tool_name: record.event.tool_name.clone(),
                    kind: record.event.kind.clone(),
                    status: record
                        .event
                        .status
                        .clone()
                        .or(record.event.result_summary.clone()),
                    severity: record.event.level,
                    call_ids: record.event.all_call_ids().map(str::to_string).collect(),
                    preview: event_preview(&record.event),
                    is_system_event: false,
                }
            })
            .collect::<Vec<_>>();
        disambiguate_event_rows(&mut rows);
        rows
    }

    pub fn project_tool_calls(
        &self,
        filter: &ProjectionFilter,
        now: DateTime<Utc>,
        health: &HealthConfig,
    ) -> Vec<ToolCallSummary> {
        self.correlated_calls(filter, now, health)
    }

    pub fn correlated_calls(
        &self,
        filter: &ProjectionFilter,
        now: DateTime<Utc>,
        health: &HealthConfig,
    ) -> Vec<ToolCallSummary> {
        let mut calls = self
            .calls
            .values()
            .filter_map(|call| self.project_call(call, filter, now, health))
            .collect::<Vec<_>>();

        calls.sort_by(|a, b| {
            b.started_at
                .or(b.last_updated_at)
                .cmp(&a.started_at.or(a.last_updated_at))
                .then_with(|| a.call_entity_id.cmp(&b.call_entity_id))
        });
        disambiguate_tool_calls(&mut calls);
        calls
    }

    pub fn project_sessions(
        &self,
        filter: &ProjectionFilter,
        now: DateTime<Utc>,
        health: &HealthConfig,
    ) -> Vec<SessionSummary> {
        self.sessions(filter, now, health)
    }

    pub fn sessions(
        &self,
        filter: &ProjectionFilter,
        now: DateTime<Utc>,
        health: &HealthConfig,
    ) -> Vec<SessionSummary> {
        let mut sessions = BTreeMap::<String, SessionAccumulator>::new();

        for record in &self.events {
            if !event_matches_filter(record, filter) {
                continue;
            }

            let session_id = record
                .event
                .durable_session_id()
                .map(str::to_string)
                .unwrap_or_else(|| "<unknown>".to_string());
            let session = sessions
                .entry(session_id.clone())
                .or_insert_with(|| SessionAccumulator::new(&session_id));
            session.session_label_info = SessionLabel::from_event(&record.event, &session_id);
            if session.agent_id.is_none() {
                session.agent_id = record.event.agent_id.clone();
            }
            let ts = record.event.timestamp.or(Some(record.observed_at));
            session.last_event_at = max_option(session.last_event_at, ts);
            session.last_activity_at = max_option(session.last_activity_at, ts);
            session.derived_severity = max_severity(session.derived_severity, record.event.level);
        }

        for call in self.correlated_calls(filter, now, health) {
            let session = sessions
                .entry(call.session_id.clone())
                .or_insert_with(|| SessionAccumulator::new(&call.session_id));
            session.session_label_info = call.session_label_info.clone();
            if session.agent_id.is_none() {
                session.agent_id = call.agent_id.clone();
            }
            session.last_activity_at = max_option(session.last_activity_at, call.last_updated_at);
            session.derived_severity = max_severity(session.derived_severity, call.severity);
            match call.status {
                CallStatus::Running => session.open_call_count += 1,
                CallStatus::Stale => {
                    session.open_call_count += 1;
                    session.stale_call_count += 1;
                }
                _ => {}
            }
        }

        let mut rows = sessions
            .into_iter()
            .map(|(session_id, session)| {
                let last_source_seen_at =
                    self.session_sources.get(&session_id).and_then(|sources| {
                        sources
                            .iter()
                            .filter_map(|source| self.source_last_seen.get(source).copied())
                            .max()
                    });
                let source_state = derive_source_state(last_source_seen_at, now, health);
                let health_status = derive_health_status(
                    session.open_call_count,
                    session.stale_call_count,
                    source_state.clone(),
                );
                let source_health = SourceHealth {
                    last_source_seen_at,
                    source_state: source_state.clone(),
                    health_status: health_status.clone(),
                };

                SessionSummary {
                    session_id: session_id.clone(),
                    session_label: session.session_label_info.display.clone(),
                    session_label_info: session.session_label_info,
                    agent_id: session.agent_id,
                    last_activity_at: session.last_activity_at.or(session.last_event_at),
                    last_event_at: session.last_event_at,
                    last_source_seen_at,
                    open_call_count: session.open_call_count,
                    stale_call_count: session.stale_call_count,
                    derived_severity: session.derived_severity,
                    health_status,
                    source_state,
                    source_health,
                }
            })
            .filter(|summary| {
                summary.last_activity_at.is_some()
                    && filter
                        .time
                        .contains(summary.last_activity_at.or(summary.last_event_at))
            })
            .collect::<Vec<_>>();

        rows.sort_by(|a, b| {
            b.last_activity_at
                .cmp(&a.last_activity_at)
                .then_with(|| b.stale_call_count.cmp(&a.stale_call_count))
                .then_with(|| b.open_call_count.cmp(&a.open_call_count))
                .then_with(|| a.session_id.cmp(&b.session_id))
        });
        disambiguate_sessions(&mut rows);
        rows
    }

    fn project_call(
        &self,
        call: &CallAggregate,
        filter: &ProjectionFilter,
        now: DateTime<Utc>,
        health: &HealthConfig,
    ) -> Option<ToolCallSummary> {
        if let Some(session) = filter.session.as_ref() {
            let needle = session.to_ascii_lowercase();
            if !session_matches(
                &needle,
                Some(&call.session_label_info),
                Some(call.session_id.as_str()),
            ) {
                return None;
            }
        }

        if let Some(agent) = filter.agent.as_ref() {
            let needle = agent.to_ascii_lowercase();
            if call
                .agent_id
                .as_ref()
                .map(|value| !value.to_ascii_lowercase().contains(&needle))
                .unwrap_or(true)
            {
                return None;
            }
        }

        if let Some(tool) = filter.tool.as_ref() {
            let needle = tool.to_ascii_lowercase();
            if call
                .tool_name
                .as_ref()
                .map(|value| !value.to_ascii_lowercase().contains(&needle))
                .unwrap_or(true)
            {
                return None;
            }
        }

        if !call.severity.should_emit(filter.min_level) {
            return None;
        }

        if !filter
            .time
            .intersects(call.started_at, call.ended_at, call.last_updated_at)
        {
            return None;
        }

        let status = derive_call_status(call, now, health.stale_after);
        Some(ToolCallSummary {
            call_entity_id: call.call_entity_id.clone(),
            session_id: call.session_id.clone(),
            session_label: call.session_label_info.display.clone(),
            session_label_info: call.session_label_info.clone(),
            agent_id: call.agent_id.clone(),
            tool_name: call.tool_name.clone(),
            canonical_call_id: call.canonical_call_id.clone(),
            status,
            match_confidence: call.confidence.clone(),
            started_at: call.started_at,
            ended_at: call.ended_at,
            last_updated_at: call.last_updated_at,
            duration_ms: call.duration_ms,
            event_refs_start: call.start_refs.clone(),
            event_refs_result: call.result_refs.clone(),
            event_refs_related: call.related_refs.clone(),
            severity: call.severity,
            message_preview: call.message_preview.clone(),
        })
    }

    fn record_start(&mut self, event: &NormalizedEvent, session_id: &str, event_ref: &str) {
        if let Some(explicit_id) = event.call_id.as_ref() {
            let key = (session_id.to_string(), explicit_id.clone());
            let entity_id = self
                .explicit_index
                .entry(key)
                .or_insert_with(|| format!("{session_id}:{explicit_id}"))
                .clone();
            let confidence = if event.transcript_tool_call_count.unwrap_or(0) > 1 {
                MatchConfidence::TranscriptBundle
            } else {
                MatchConfidence::ExplicitId
            };
            self.upsert_call(
                &entity_id,
                session_id,
                event,
                Some(explicit_id.clone()),
                event.fallback_signature(),
                confidence,
                |call| {
                    call.start_refs.push(event_ref.to_string());
                    call.started_at = min_option(call.started_at, event.timestamp);
                },
            );
            return;
        }

        let Some(signature) = event.fallback_signature() else {
            return;
        };
        let ordinal_key = (session_id.to_string(), signature.clone());
        let ordinal = self
            .next_fallback_ordinal
            .entry(ordinal_key.clone())
            .and_modify(|value| *value += 1)
            .or_insert(1);
        let entity_id = format!("{session_id}:{signature}:{}", *ordinal);
        self.fallback_open_index
            .entry(ordinal_key)
            .or_default()
            .push_back(entity_id.clone());
        self.upsert_call(
            &entity_id,
            session_id,
            event,
            None,
            Some(signature),
            MatchConfidence::FallbackSignature,
            |call| {
                call.start_refs.push(event_ref.to_string());
                call.started_at = min_option(call.started_at, event.timestamp);
            },
        );
    }

    fn record_result(&mut self, event: &NormalizedEvent, session_id: &str, event_ref: &str) {
        let explicit_match = event.call_id.as_ref().and_then(|call_id| {
            self.explicit_index
                .get(&(session_id.to_string(), call_id.clone()))
                .cloned()
        });

        let fallback_match = if explicit_match.is_none() {
            event.fallback_signature().and_then(|signature| {
                self.fallback_open_index
                    .get_mut(&(session_id.to_string(), signature))
                    .and_then(|queue| queue.pop_front())
            })
        } else {
            None
        };

        let entity_id = explicit_match
            .or(fallback_match)
            .unwrap_or_else(|| self.result_only_entity_id(event, session_id));
        let confidence = if let Some(existing) = self.calls.get(&entity_id) {
            existing.confidence.clone()
        } else if event.call_id.is_some() {
            MatchConfidence::ExplicitId
        } else {
            MatchConfidence::FallbackSignature
        };

        self.upsert_call(
            &entity_id,
            session_id,
            event,
            event.call_id.clone(),
            event.fallback_signature(),
            confidence,
            |call| {
                call.result_refs.push(event_ref.to_string());
                call.ended_at = max_option(call.ended_at, event.timestamp);
                call.last_updated_at = max_option(call.last_updated_at, event.timestamp);
                call.duration_ms = call
                    .duration_ms
                    .or(event.duration_ms)
                    .or_else(|| duration_between(call.started_at, event.timestamp));
            },
        );
    }

    fn record_related(&mut self, event: &NormalizedEvent, session_id: &str, event_ref: &str) {
        let entity_id = event
            .call_id
            .as_ref()
            .and_then(|call_id| {
                self.explicit_index
                    .get(&(session_id.to_string(), call_id.clone()))
                    .cloned()
            })
            .or_else(|| {
                event.fallback_signature().and_then(|signature| {
                    self.fallback_open_index
                        .get(&(session_id.to_string(), signature))
                        .and_then(|queue| queue.front().cloned())
                })
            });

        let Some(entity_id) = entity_id else {
            return;
        };

        self.upsert_call(
            &entity_id,
            session_id,
            event,
            event.call_id.clone(),
            event.fallback_signature(),
            self.calls
                .get(&entity_id)
                .map(|call| call.confidence.clone())
                .unwrap_or(MatchConfidence::FallbackSignature),
            |call| {
                call.related_refs.push(event_ref.to_string());
                call.last_updated_at = max_option(call.last_updated_at, event.timestamp);
            },
        );
    }

    fn result_only_entity_id(&mut self, event: &NormalizedEvent, session_id: &str) -> String {
        if let Some(call_id) = event.call_id.as_ref() {
            let entity_id = format!("{session_id}:{call_id}");
            self.explicit_index
                .insert((session_id.to_string(), call_id.clone()), entity_id.clone());
            return entity_id;
        }

        let signature = event
            .fallback_signature()
            .unwrap_or_else(|| "<unknown>".to_string());
        let key = (session_id.to_string(), signature.clone());
        let ordinal = self
            .next_fallback_ordinal
            .entry(key)
            .and_modify(|value| *value += 1)
            .or_insert(1);
        format!("{session_id}:{signature}:{}", *ordinal)
    }

    #[allow(clippy::too_many_arguments)]
    fn upsert_call<F>(
        &mut self,
        entity_id: &str,
        session_id: &str,
        event: &NormalizedEvent,
        canonical_call_id: Option<String>,
        fallback_signature: Option<String>,
        confidence: MatchConfidence,
        mutate: F,
    ) where
        F: FnOnce(&mut CallAggregate),
    {
        let session_label_info = SessionLabel::from_event(event, session_id);
        let call = self
            .calls
            .entry(entity_id.to_string())
            .or_insert_with(|| CallAggregate {
                call_entity_id: entity_id.to_string(),
                session_id: session_id.to_string(),
                session_label_info,
                agent_id: event.agent_id.clone(),
                tool_name: event.tool_name.clone(),
                canonical_call_id,
                fallback_signature,
                confidence,
                started_at: None,
                ended_at: None,
                last_updated_at: event.timestamp,
                duration_ms: event.duration_ms,
                start_refs: Vec::new(),
                result_refs: Vec::new(),
                related_refs: Vec::new(),
                severity: event.level,
                message_preview: event_preview(event),
            });

        if call.agent_id.is_none() {
            call.agent_id = event.agent_id.clone();
        }
        if call.tool_name.is_none() {
            call.tool_name = event.tool_name.clone();
        }
        if call.canonical_call_id.is_none() {
            call.canonical_call_id = event.call_id.clone();
        }
        if call.fallback_signature.is_none() {
            call.fallback_signature = event.fallback_signature();
        }
        call.session_label_info = SessionLabel::from_event(event, session_id);
        call.last_updated_at = max_option(call.last_updated_at, event.timestamp);
        call.severity = max_severity(call.severity, event.level);
        if call.message_preview.is_none() {
            call.message_preview = event_preview(event);
        }

        mutate(call);
    }
}

#[derive(Clone, Debug)]
struct SessionAccumulator {
    session_label_info: SessionLabel,
    agent_id: Option<String>,
    last_activity_at: Option<DateTime<Utc>>,
    last_event_at: Option<DateTime<Utc>>,
    open_call_count: usize,
    stale_call_count: usize,
    derived_severity: Severity,
}

impl SessionAccumulator {
    fn new(session_id: &str) -> Self {
        Self {
            session_label_info: SessionLabel {
                raw: Some(session_id.to_string()),
                display: shorten_non_discord_session_label(session_id),
                kind: SessionLabelKind::StableSessionId,
                channel_id: None,
            },
            agent_id: None,
            last_activity_at: None,
            last_event_at: None,
            open_call_count: 0,
            stale_call_count: 0,
            derived_severity: Severity::Trace,
        }
    }
}

fn derive_call_status(
    call: &CallAggregate,
    now: DateTime<Utc>,
    stale_after: Duration,
) -> CallStatus {
    if let Some(ended_at) = call.ended_at {
        let lower = call
            .message_preview
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if call.severity.rank() >= Severity::Error.rank()
            || lower.contains("error")
            || lower.contains("failed")
        {
            return CallStatus::Failed;
        }
        if call.started_at.is_some() {
            return CallStatus::Succeeded;
        }
        let _ = ended_at;
        return CallStatus::Incomplete;
    }

    if let Some(started_at) = call.started_at {
        if now.signed_duration_since(started_at) > stale_after {
            return CallStatus::Stale;
        }
        return CallStatus::Running;
    }

    CallStatus::Unknown
}

fn derive_source_state(
    last_source_seen_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    health: &HealthConfig,
) -> SourceState {
    let Some(last_source_seen_at) = last_source_seen_at else {
        return SourceState::Unknown;
    };

    let age = now.signed_duration_since(last_source_seen_at);
    if age <= health.quiet_after {
        SourceState::Live
    } else if age <= health.disconnect_after {
        SourceState::Quiet
    } else {
        SourceState::Missing
    }
}

fn derive_health_status(
    open_call_count: usize,
    stale_call_count: usize,
    source_state: SourceState,
) -> HealthStatus {
    if stale_call_count > 0 {
        return HealthStatus::Stale;
    }
    if source_state == SourceState::Missing {
        return HealthStatus::Disconnected;
    }
    if open_call_count > 0 {
        return HealthStatus::Busy;
    }
    match source_state {
        SourceState::Live | SourceState::Quiet => HealthStatus::Idle,
        SourceState::Missing => HealthStatus::Disconnected,
        SourceState::Unknown => HealthStatus::Unknown,
    }
}

fn max_option(
    current: Option<DateTime<Utc>>,
    candidate: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    match (current, candidate) {
        (Some(current), Some(candidate)) => Some(current.max(candidate)),
        (Some(current), None) => Some(current),
        (None, Some(candidate)) => Some(candidate),
        (None, None) => None,
    }
}

fn min_option(
    current: Option<DateTime<Utc>>,
    candidate: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    match (current, candidate) {
        (Some(current), Some(candidate)) => Some(current.min(candidate)),
        (Some(current), None) => Some(current),
        (None, Some(candidate)) => Some(candidate),
        (None, None) => None,
    }
}

fn max_severity(current: Severity, candidate: Severity) -> Severity {
    if candidate.rank() >= current.rank() {
        candidate
    } else {
        current
    }
}

fn duration_between(start: Option<DateTime<Utc>>, end: Option<DateTime<Utc>>) -> Option<u64> {
    let (Some(start), Some(end)) = (start, end) else {
        return None;
    };
    Some(end.signed_duration_since(start).num_milliseconds().max(0) as u64)
}

fn event_preview(event: &NormalizedEvent) -> Option<String> {
    event
        .message
        .clone()
        .or_else(|| event.result_preview.clone())
        .or_else(|| {
            event
                .preferred_params()
                .first()
                .map(|(key, value)| format!("{key}={value}"))
        })
}

fn event_matches_filter(record: &EventRecord, filter: &ProjectionFilter) -> bool {
    if !record.event.level.should_emit(filter.min_level) {
        return false;
    }

    if !filter
        .time
        .contains(record.event.timestamp.or(Some(record.observed_at)))
    {
        return false;
    }

    if let Some(agent) = filter.agent.as_ref() {
        let needle = agent.to_ascii_lowercase();
        if record
            .event
            .agent_id
            .as_ref()
            .map(|value| !value.to_ascii_lowercase().contains(&needle))
            .unwrap_or(true)
        {
            return false;
        }
    }

    if let Some(tool) = filter.tool.as_ref() {
        let needle = tool.to_ascii_lowercase();
        if record
            .event
            .tool_name
            .as_ref()
            .map(|value| !value.to_ascii_lowercase().contains(&needle))
            .unwrap_or(true)
        {
            return false;
        }
    }

    if let Some(session) = filter.session.as_ref() {
        let needle = session.to_ascii_lowercase();
        let label = record
            .event
            .durable_session_id()
            .map(|session_id| SessionLabel::from_event(&record.event, session_id));
        if !session_matches(&needle, label.as_ref(), record.event.durable_session_id()) {
            return false;
        }
    }

    true
}

fn session_matches(needle: &str, label: Option<&SessionLabel>, session_id: Option<&str>) -> bool {
    if session_id
        .map(|value| value.to_ascii_lowercase().contains(needle))
        .unwrap_or(false)
    {
        return true;
    }

    let Some(label) = label else {
        return false;
    };

    label.display.to_ascii_lowercase().contains(needle)
        || label
            .raw
            .as_ref()
            .map(|value| value.to_ascii_lowercase().contains(needle))
            .unwrap_or(false)
}

fn has_explicit_discord_provider(event: &NormalizedEvent) -> bool {
    event.routing.provider.as_deref() == Some("discord")
        && event.routing.provider_source.as_deref() != Some("channel_id")
}

fn disambiguate_event_rows(rows: &mut [EventRow]) {
    let replacements =
        disambiguated_session_labels(rows.iter().filter_map(|row| {
            Some((row.session_id.as_deref()?, row.session_label_info.as_ref()?))
        }));

    for row in rows {
        let Some(session_id) = row.session_id.as_deref() else {
            continue;
        };
        let Some(replacement) = replacements.get(session_id) else {
            continue;
        };
        row.session_label = Some(replacement.clone());
        if let Some(label) = row.session_label_info.as_mut() {
            label.display = replacement.clone();
        }
    }
}

fn disambiguate_tool_calls(rows: &mut [ToolCallSummary]) {
    let replacements = disambiguated_session_labels(
        rows.iter()
            .map(|row| (row.session_id.as_str(), &row.session_label_info)),
    );

    for row in rows {
        let Some(replacement) = replacements.get(&row.session_id) else {
            continue;
        };
        row.session_label = replacement.clone();
        row.session_label_info.display = replacement.clone();
    }
}

fn disambiguate_sessions(rows: &mut [SessionSummary]) {
    let replacements = disambiguated_session_labels(
        rows.iter()
            .map(|row| (row.session_id.as_str(), &row.session_label_info)),
    );

    for row in rows {
        let Some(replacement) = replacements.get(&row.session_id) else {
            continue;
        };
        row.session_label = replacement.clone();
        row.session_label_info.display = replacement.clone();
    }
}

fn disambiguated_session_labels<'a>(
    rows: impl Iterator<Item = (&'a str, &'a SessionLabel)>,
) -> HashMap<String, String> {
    let mut labels_to_sessions: HashMap<String, Vec<String>> = HashMap::new();

    for (session_id, label) in rows {
        if label.kind != SessionLabelKind::StableSessionId {
            continue;
        }

        let sessions = labels_to_sessions.entry(label.display.clone()).or_default();
        if !sessions.iter().any(|existing| existing == session_id) {
            sessions.push(session_id.to_string());
        }
    }

    let mut replacements = HashMap::new();
    for session_ids in labels_to_sessions.into_values() {
        if session_ids.len() < 2 {
            continue;
        }

        let expanded = session_ids
            .iter()
            .map(|session_id| (session_id.clone(), compact_session_label(session_id, 8, 8)))
            .collect::<Vec<_>>();
        let mut expanded_counts = HashMap::<String, usize>::new();
        for (_, display) in &expanded {
            *expanded_counts.entry(display.clone()).or_insert(0) += 1;
        }

        for (session_id, display) in expanded {
            if expanded_counts.get(&display).copied().unwrap_or_default() > 1 {
                replacements.insert(session_id.clone(), session_id);
            } else {
                replacements.insert(session_id, display);
            }
        }
    }

    replacements
}

fn compact_session_label(value: &str, head: usize, tail: usize) -> String {
    let total = value.chars().count();
    if total <= head + tail + 1 {
        return value.to_string();
    }

    let prefix = value.chars().take(head).collect::<String>();
    let suffix = value
        .chars()
        .skip(total.saturating_sub(tail))
        .collect::<String>();
    format!("{prefix}…{suffix}")
}

pub type CorrelatedCall = ToolCallSummary;
pub type ProjectionStore = AppModel;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::ToolEventKind;

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
            params: Vec::new(),
            args_preview: Vec::new(),
            args_raw: None,
            args_truncated: false,
            message: None,
            raw_line: String::new(),
        }
    }

    #[test]
    fn explicit_id_wins_and_sessions_do_not_cross_match() {
        let mut store = ProjectionStore::default();
        let now = DateTime::parse_from_rfc3339("2026-03-07T10:00:06Z")
            .unwrap()
            .with_timezone(&Utc);
        store.ingest_event(
            event(
                "session-1",
                Some("label-a"),
                "shell",
                Some("call-1"),
                ToolEventKind::ToolCallStart,
                "2026-03-07T10:00:00Z",
            ),
            now,
        );
        store.ingest_event(
            event(
                "session-2",
                Some("label-a"),
                "shell",
                Some("call-1"),
                ToolEventKind::ToolCallResult,
                "2026-03-07T10:00:05Z",
            ),
            now,
        );

        let filter = ProjectionFilter {
            min_level: Severity::Trace,
            ..ProjectionFilter::default()
        };
        let calls = store.correlated_calls(&filter, now, &HealthConfig::default());
        assert_eq!(calls.len(), 2);
        assert!(calls
            .iter()
            .any(|call| call.session_id == "session-1" && call.status == CallStatus::Running));
        assert!(calls
            .iter()
            .any(|call| call.session_id == "session-2" && call.status == CallStatus::Incomplete));
    }

    #[test]
    fn fallback_matching_closes_oldest_open_call() {
        let mut store = ProjectionStore::default();
        let now = DateTime::parse_from_rfc3339("2026-03-07T10:00:03Z")
            .unwrap()
            .with_timezone(&Utc);

        let mut first = event(
            "session-1",
            Some("label-a"),
            "shell",
            None,
            ToolEventKind::ToolCallStart,
            "2026-03-07T10:00:00Z",
        );
        first.params.push(("command".to_string(), "ls".to_string()));
        let mut second = first.clone();
        second.timestamp = Some(
            DateTime::parse_from_rfc3339("2026-03-07T10:00:01Z")
                .unwrap()
                .with_timezone(&Utc),
        );
        let mut result = first.clone();
        result.kind = ToolEventKind::ToolCallResult;
        result.timestamp = Some(
            DateTime::parse_from_rfc3339("2026-03-07T10:00:02Z")
                .unwrap()
                .with_timezone(&Utc),
        );

        store.ingest_event(first, now);
        store.ingest_event(second, now);
        store.ingest_event(result, now);

        let filter = ProjectionFilter {
            min_level: Severity::Trace,
            ..ProjectionFilter::default()
        };
        let calls = store.correlated_calls(&filter, now, &HealthConfig::default());
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.status == CallStatus::Running)
                .count(),
            1
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.status == CallStatus::Succeeded)
                .count(),
            1
        );
        assert!(calls
            .iter()
            .all(|call| call.match_confidence == MatchConfidence::FallbackSignature));
    }

    #[test]
    fn shared_time_filter_applies_to_events_calls_and_sessions() {
        let mut store = ProjectionStore::default();
        let now = DateTime::parse_from_rfc3339("2026-03-07T10:05:00Z")
            .unwrap()
            .with_timezone(&Utc);
        store.ingest_event(
            event(
                "session-1",
                Some("alpha"),
                "shell",
                Some("call-1"),
                ToolEventKind::ToolCallStart,
                "2026-03-07T10:00:00Z",
            ),
            now,
        );
        store.ingest_event(
            event(
                "session-1",
                Some("alpha"),
                "shell",
                Some("call-1"),
                ToolEventKind::ToolCallResult,
                "2026-03-07T10:02:00Z",
            ),
            now,
        );
        store.ingest_event(
            event(
                "session-2",
                Some("beta"),
                "read",
                Some("call-2"),
                ToolEventKind::ToolCallStart,
                "2026-03-07T10:04:00Z",
            ),
            now,
        );

        let filter = ProjectionFilter {
            min_level: Severity::Trace,
            time: TimeFilter {
                since: Some(
                    DateTime::parse_from_rfc3339("2026-03-07T10:01:00Z")
                        .unwrap()
                        .with_timezone(&Utc),
                ),
                until: Some(
                    DateTime::parse_from_rfc3339("2026-03-07T10:03:00Z")
                        .unwrap()
                        .with_timezone(&Utc),
                ),
            },
            ..ProjectionFilter::default()
        };

        assert_eq!(store.event_rows(&filter).len(), 1);
        assert_eq!(
            store
                .correlated_calls(&filter, now, &HealthConfig::default())
                .len(),
            1
        );
        assert_eq!(
            store.sessions(&filter, now, &HealthConfig::default()).len(),
            1
        );
        assert_eq!(
            store.sessions(&filter, now, &HealthConfig::default())[0].session_id,
            "session-1"
        );
    }

    #[test]
    fn session_health_uses_source_freshness_not_open_calls_only() {
        let mut store = ProjectionStore::default();
        let observed_at = DateTime::parse_from_rfc3339("2026-03-07T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        store.ingest_event(
            event(
                "session-1",
                Some("alpha"),
                "shell",
                Some("call-1"),
                ToolEventKind::ToolCallStart,
                "2026-03-07T10:00:00Z",
            ),
            observed_at,
        );

        let filter = ProjectionFilter {
            min_level: Severity::Trace,
            ..ProjectionFilter::default()
        };
        let health = HealthConfig {
            quiet_after: Duration::seconds(5),
            disconnect_after: Duration::seconds(10),
            stale_after: Duration::seconds(20),
        };

        let idle_now = DateTime::parse_from_rfc3339("2026-03-07T10:00:06Z")
            .unwrap()
            .with_timezone(&Utc);
        let disconnected_now = DateTime::parse_from_rfc3339("2026-03-07T10:00:12Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            store.sessions(&filter, idle_now, &health)[0].source_state,
            SourceState::Quiet
        );
        assert_eq!(
            store.sessions(&filter, idle_now, &health)[0].health_status,
            HealthStatus::Busy
        );
        assert_eq!(
            store.sessions(&filter, disconnected_now, &health)[0].source_state,
            SourceState::Missing
        );
        assert_eq!(
            store.sessions(&filter, disconnected_now, &health)[0].health_status,
            HealthStatus::Disconnected
        );
        assert_eq!(
            store.correlated_calls(&filter, disconnected_now, &health)[0].status,
            CallStatus::Running
        );
    }

    #[test]
    fn result_only_calls_remain_visible_as_incomplete_rows() {
        let mut store = ProjectionStore::default();
        let now = DateTime::parse_from_rfc3339("2026-03-07T10:00:06Z")
            .unwrap()
            .with_timezone(&Utc);

        store.ingest_event(
            event(
                "session-1",
                Some("friendly-session"),
                "shell",
                Some("call-42"),
                ToolEventKind::ToolCallResult,
                "2026-03-07T10:00:05Z",
            ),
            now,
        );

        let calls =
            store.project_tool_calls(&ProjectionFilter::default(), now, &HealthConfig::default());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].canonical_call_id.as_deref(), Some("call-42"));
        assert_eq!(calls[0].status, CallStatus::Incomplete);
        assert_eq!(calls[0].event_refs_result.len(), 1);
    }

    #[test]
    fn tool_call_projection_orders_newest_activity_first() {
        let mut store = ProjectionStore::default();
        let now = DateTime::parse_from_rfc3339("2026-03-07T10:00:08Z")
            .unwrap()
            .with_timezone(&Utc);

        store.ingest_event(
            event(
                "session-1",
                Some("alpha"),
                "shell",
                Some("call-older"),
                ToolEventKind::ToolCallStart,
                "2026-03-07T10:00:00Z",
            ),
            now,
        );
        store.ingest_event(
            event(
                "session-1",
                Some("alpha"),
                "shell",
                Some("call-newer"),
                ToolEventKind::ToolCallStart,
                "2026-03-07T10:00:04Z",
            ),
            now,
        );

        let calls =
            store.project_tool_calls(&ProjectionFilter::default(), now, &HealthConfig::default());
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].canonical_call_id.as_deref(), Some("call-newer"));
        assert_eq!(calls[1].canonical_call_id.as_deref(), Some("call-older"));
    }

    #[test]
    fn session_projection_uses_durable_identity_for_non_discord_display_labels() {
        let mut store = ProjectionStore::default();
        let now = DateTime::parse_from_rfc3339("2026-03-07T10:00:08Z")
            .unwrap()
            .with_timezone(&Utc);

        let mut first = event(
            "12345678-1111-2222-3333-aaaaaaaaaaaa",
            Some("Friendly Session"),
            "shell",
            Some("call-1"),
            ToolEventKind::ToolCallStart,
            "2026-03-07T10:00:00Z",
        );
        first.session_label = Some("Friendly Session".to_string());
        let mut second = event(
            "12345678-1111-2222-3333-bbbbbbbbbbbb",
            Some("Friendly Session"),
            "shell",
            Some("call-2"),
            ToolEventKind::ToolCallStart,
            "2026-03-07T10:00:01Z",
        );
        second.session_label = Some("Friendly Session".to_string());

        store.ingest_event(first, now);
        store.ingest_event(second, now);

        let sessions =
            store.project_sessions(&ProjectionFilter::default(), now, &HealthConfig::default());
        assert_eq!(sessions.len(), 2);
        assert!(sessions
            .iter()
            .all(|session| session.session_label != "Friendly Session"));
        assert_ne!(sessions[0].session_label, sessions[1].session_label);
        assert!(sessions
            .iter()
            .all(|session| session.session_label_info.raw.as_deref() == Some("Friendly Session")));
    }

    #[test]
    fn discord_sessions_project_channel_id_display_without_changing_identity() {
        let mut store = ProjectionStore::default();
        let now = DateTime::parse_from_rfc3339("2026-03-07T10:00:08Z")
            .unwrap()
            .with_timezone(&Utc);

        let mut discord_event = event(
            "session-discord-stable",
            Some("friendly-discord"),
            "shell",
            Some("call-1"),
            ToolEventKind::ToolCallStart,
            "2026-03-07T10:00:00Z",
        );
        discord_event.routing.provider = Some("discord".to_string());
        discord_event.routing.channel_id = Some("123456789012345678".to_string());

        store.ingest_event(discord_event, now);

        let sessions =
            store.project_sessions(&ProjectionFilter::default(), now, &HealthConfig::default());
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "session-discord-stable");
        assert_eq!(sessions[0].session_label, "discord:123456789012345678");
        assert_eq!(
            sessions[0].session_label_info.kind,
            SessionLabelKind::DiscordChannelId
        );
        assert_eq!(
            sessions[0].session_label_info.channel_id.as_deref(),
            Some("123456789012345678")
        );
    }

    #[test]
    fn numeric_channel_id_without_explicit_provider_is_not_treated_as_discord() {
        let mut store = ProjectionStore::default();
        let now = DateTime::parse_from_rfc3339("2026-03-07T10:00:08Z")
            .unwrap()
            .with_timezone(&Utc);

        let mut event = event(
            "session-stable-12345678",
            Some("friendly"),
            "shell",
            Some("call-1"),
            ToolEventKind::ToolCallStart,
            "2026-03-07T10:00:00Z",
        );
        event.routing.channel_id = Some("123456789012345678".to_string());
        event.routing.provider = Some("discord".to_string());
        event.routing.provider_source = Some("channel_id".to_string());

        store.ingest_event(event, now);

        let sessions =
            store.project_sessions(&ProjectionFilter::default(), now, &HealthConfig::default());
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0].session_label_info.kind,
            SessionLabelKind::StableSessionId
        );
        assert_ne!(sessions[0].session_label, "discord:123456789012345678");
    }
}
