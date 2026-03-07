use crate::event::{NormalizedEvent, ToolEventKind};
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug)]
pub struct InFlightCall {
    pub internal_id: String,
    pub session_key: Option<String>,
    pub session_id: Option<String>,
    pub tool_name: Option<String>,
    pub started_at: DateTime<Utc>,
    pub message: Option<String>,
    pub args_preview: Vec<(String, String)>,
    pub warned: bool,
}

#[derive(Debug)]
pub struct StaleTracker {
    threshold_seconds: u64,
    in_flight: HashMap<String, InFlightCall>,
    call_id_index: HashMap<String, String>,
    signature_index: HashMap<String, VecDeque<String>>,
    synthetic_id_seq: u64,
}

#[derive(Debug)]
pub struct StaleWarning {
    pub call_id: String,
    pub session_key: Option<String>,
    pub tool_name: Option<String>,
    pub age_seconds: u64,
    pub message: Option<String>,
}

#[derive(Debug)]
pub struct HeartbeatSummary {
    pub active_calls: usize,
    pub stale_calls: usize,
    pub active_sessions: usize,
}

impl StaleTracker {
    pub fn new(threshold_seconds: u64) -> Self {
        Self {
            threshold_seconds,
            in_flight: HashMap::new(),
            call_id_index: HashMap::new(),
            signature_index: HashMap::new(),
            synthetic_id_seq: 0,
        }
    }

    pub fn on_event(&mut self, event: &NormalizedEvent, now: DateTime<Utc>) -> Vec<StaleWarning> {
        match event.kind {
            ToolEventKind::ToolCallStart => self.record_start(event, now),
            ToolEventKind::ToolCallResult => self.record_completion(event),
            _ => {}
        };

        self.collect_stale_warnings(now)
    }

    pub fn heartbeat(&self, now: DateTime<Utc>) -> HeartbeatSummary {
        let stale_calls = self
            .in_flight
            .values()
            .filter(|call| {
                now.signed_duration_since(call.started_at)
                    .num_seconds()
                    .max(0) as u64
                    > self.threshold_seconds
            })
            .count();

        let active_sessions = {
            let mut sessions = HashSet::new();
            for call in self.in_flight.values() {
                let key = match call.session_key.as_ref().or(call.session_id.as_ref()) {
                    Some(key) => key.clone(),
                    None => "<unknown>".to_string(),
                };
                sessions.insert(key);
            }
            sessions.len()
        };

        HeartbeatSummary {
            active_calls: self.in_flight.len(),
            stale_calls,
            active_sessions,
        }
    }

    fn next_synthetic_id(&mut self, event: &NormalizedEvent) -> String {
        self.synthetic_id_seq = self.synthetic_id_seq.saturating_add(1);
        let tool = event.tool_name.as_deref().unwrap_or("tool");
        format!("call-{tool}-{}", self.synthetic_id_seq)
    }

    fn record_start(&mut self, event: &NormalizedEvent, now: DateTime<Utc>) {
        let internal_id = event
            .all_call_ids()
            .next()
            .map_or_else(|| self.next_synthetic_id(event), |id| id.to_string());

        let call = InFlightCall {
            internal_id: internal_id.clone(),
            session_key: event.session_key.clone(),
            session_id: event.session_id.clone(),
            tool_name: event.tool_name.clone(),
            started_at: event.timestamp.unwrap_or(now),
            message: event
                .message
                .clone()
                .or_else(|| event.result_preview.clone())
                .or_else(|| {
                    event
                        .preferred_params()
                        .first()
                        .map(|(key, value)| format!("{key}={value}"))
                }),
            args_preview: event.preferred_params().to_vec(),
            warned: false,
        };

        for call_id in event.all_call_ids() {
            self.call_id_index
                .insert(call_id.to_string(), internal_id.clone());
        }

        if let Some(signature) = event.fallback_signature() {
            self.signature_index
                .entry(signature)
                .or_default()
                .push_back(internal_id.clone());
        }

        self.in_flight.insert(internal_id, call);
    }

    fn record_completion(&mut self, event: &NormalizedEvent) {
        let removed_id = self
            .find_in_flight_call_id(event)
            .or_else(|| self.find_by_signature(event));

        if let Some(removed_id) = removed_id {
            if self.in_flight.remove(&removed_id).is_some() {
                self.call_id_index.retain(|_, value| value != &removed_id);
                self.remove_from_signatures(&removed_id);
            }
        }
    }

    fn find_in_flight_call_id(&self, event: &NormalizedEvent) -> Option<String> {
        event
            .all_call_ids()
            .find_map(|call_id| self.call_id_index.get(call_id).cloned())
            .or_else(|| {
                event
                    .call_id
                    .as_deref()
                    .and_then(|id| self.call_id_index.get(id).cloned())
            })
    }

    fn find_by_signature(&mut self, event: &NormalizedEvent) -> Option<String> {
        let signature = event.fallback_signature()?;
        let candidates = self.signature_index.get_mut(&signature)?;
        while let Some(candidate) = candidates.pop_front() {
            if self.in_flight.contains_key(&candidate) {
                return Some(candidate);
            }
        }

        None
    }

    fn remove_from_signatures(&mut self, removed_id: &str) {
        self.signature_index.retain(|_, candidates| {
            candidates.retain(|candidate| candidate != removed_id);
            !candidates.is_empty()
        });
    }

    fn collect_stale_warnings(&mut self, now: DateTime<Utc>) -> Vec<StaleWarning> {
        let mut warnings = Vec::new();
        for call in self.in_flight.values_mut() {
            let age_seconds = now
                .signed_duration_since(call.started_at)
                .num_seconds()
                .max(0) as u64;
            if age_seconds > self.threshold_seconds && !call.warned {
                call.warned = true;
                warnings.push(StaleWarning {
                    call_id: call.internal_id.clone(),
                    session_key: call.session_key.clone(),
                    tool_name: call.tool_name.clone(),
                    age_seconds,
                    message: call.message.clone().or_else(|| {
                        call.args_preview
                            .first()
                            .map(|(key, value)| format!("{key}={value}"))
                    }),
                });
            }
        }

        warnings
    }
}

impl HeartbeatSummary {
    pub fn to_line(&self) -> String {
        format!(
            "active_calls={}, stale_calls={}, active_sessions={}",
            self.active_calls, self.stale_calls, self.active_sessions
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{NormalizedEvent, Severity, ToolEventKind};
    use chrono::{Duration, Utc};

    fn make_event(
        kind: ToolEventKind,
        call_ids: &[&str],
        timestamp: DateTime<Utc>,
    ) -> NormalizedEvent {
        NormalizedEvent {
            kind,
            timestamp: Some(timestamp),
            timestamp_raw: None,
            session_key: Some("session-A".to_string()),
            session_id: None,
            agent_id: None,
            tool_name: Some("shell".to_string()),
            status: None,
            result_summary: None,
            result_preview: None,
            result_raw: None,
            result_metrics: Vec::new(),
            exit_code: None,
            duration_ms: None,
            is_error: None,
            call_id: call_ids.first().map(|value| (*value).to_string()),
            call_ids: call_ids.iter().map(|value| (*value).to_string()).collect(),
            correlation_ids: call_ids.iter().map(|value| (*value).to_string()).collect(),
            message_id: None,
            parent_message_id: None,
            level: Severity::Info,
            level_raw: Some("info".to_string()),
            params: Vec::new(),
            args_preview: Vec::new(),
            args_raw: None,
            args_truncated: false,
            message: None,
            raw_line: String::new(),
            source_path: None,
            source_kind: None,
            session_source: None,
            agent_source: None,
        }
    }

    #[test]
    fn tracks_and_completes_inflight_calls() {
        let mut tracker = StaleTracker::new(10);
        let now = Utc::now();
        let start = make_event(ToolEventKind::ToolCallStart, &["c1"], now);
        let end = make_event(ToolEventKind::ToolCallResult, &["c1"], now);

        assert!(tracker.on_event(&start, now).is_empty());
        assert!(tracker.on_event(&end, now).is_empty());
        assert_eq!(tracker.heartbeat(now).active_calls, 0);
    }

    #[test]
    fn falls_back_from_result_id_alias() {
        let mut tracker = StaleTracker::new(10);
        let now = Utc::now();
        let start = make_event(
            ToolEventKind::ToolCallStart,
            &["call-start-id", "alias-1"],
            now,
        );
        let end = make_event(ToolEventKind::ToolCallResult, &["alias-1"], now);

        assert!(tracker.on_event(&start, now).is_empty());
        assert!(tracker.on_event(&end, now).is_empty());
        assert_eq!(tracker.heartbeat(now).active_calls, 0);
    }

    #[test]
    fn warns_when_stale() {
        let mut tracker = StaleTracker::new(1);
        let start_time = Utc::now();
        let stale_time = start_time + Duration::seconds(5);
        let start = make_event(ToolEventKind::ToolCallStart, &["c2"], start_time);

        assert!(tracker.on_event(&start, start_time).is_empty());
        let warnings = tracker.on_event(
            &make_event(ToolEventKind::Other, &["c2"], stale_time),
            stale_time,
        );
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].call_id, "c2");
    }
}
