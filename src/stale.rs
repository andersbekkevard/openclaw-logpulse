use crate::event::{NormalizedEvent, ToolEventKind};
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};

#[derive(Debug)]
pub struct InFlightCall {
    pub call_id: String,
    pub tool_name: Option<String>,
    pub session_key: Option<String>,
    pub session_id: Option<String>,
    pub started_at: DateTime<Utc>,
    pub message: Option<String>,
    pub warned: bool,
}

#[derive(Debug)]
pub struct StaleTracker {
    threshold_seconds: u64,
    in_flight: HashMap<String, InFlightCall>,
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
        }
    }

    pub fn on_event(&mut self, event: &NormalizedEvent, now: DateTime<Utc>) -> Vec<StaleWarning> {
        if let Some(call_id) = event.call_id.as_ref() {
            match event.kind {
                ToolEventKind::ToolCallStart => {
                    self.in_flight.insert(
                        call_id.clone(),
                        InFlightCall {
                            call_id: call_id.clone(),
                            tool_name: event.tool_name.clone(),
                            session_key: event.session_key.clone(),
                            session_id: event.session_id.clone(),
                            started_at: event.timestamp.unwrap_or(now),
                            message: event.message.clone(),
                            warned: false,
                        },
                    );
                }
                ToolEventKind::ToolCallResult => {
                    self.in_flight.remove(call_id);
                }
                _ => {}
            }
        }

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
                    call_id: call.call_id.clone(),
                    session_key: call.session_key.clone(),
                    tool_name: call.tool_name.clone(),
                    age_seconds,
                    message: call.message.clone(),
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

    fn make_event(kind: ToolEventKind, call_id: &str, timestamp: DateTime<Utc>) -> NormalizedEvent {
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
            call_id: Some(call_id.to_string()),
            level: Severity::Info,
            level_raw: Some("info".to_string()),
            params: Vec::new(),
            message: None,
            raw_line: String::new(),
        }
    }

    #[test]
    fn tracks_and_completes_inflight_calls() {
        let mut tracker = StaleTracker::new(10);
        let now = Utc::now();
        let start = make_event(ToolEventKind::ToolCallStart, "c1", now);
        let end = make_event(ToolEventKind::ToolCallResult, "c1", now);

        assert!(tracker.on_event(&start, now).is_empty());
        assert!(tracker.on_event(&end, now).is_empty());
        assert_eq!(tracker.heartbeat(now).active_calls, 0);
    }

    #[test]
    fn warns_when_stale() {
        let mut tracker = StaleTracker::new(1);
        let start_time = Utc::now();
        let stale_time = start_time + Duration::seconds(5);
        let start = make_event(ToolEventKind::ToolCallStart, "c2", start_time);

        assert!(tracker.on_event(&start, start_time).is_empty());
        let warnings = tracker.on_event(
            &make_event(ToolEventKind::Other, "c2", stale_time),
            stale_time,
        );
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].call_id, "c2");
    }
}
