use chrono::{DateTime, Utc};
use serde::Serialize;

#[derive(Clone, Debug, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Severity {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
    Unknown,
}

impl Severity {
    pub fn rank(&self) -> u8 {
        match self {
            Severity::Trace => 0,
            Severity::Debug => 1,
            Severity::Info => 2,
            Severity::Warn => 3,
            Severity::Error => 4,
            Severity::Fatal => 5,
            Severity::Unknown => 2,
        }
    }

    pub fn from_string(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "trace" => Severity::Trace,
            "debug" => Severity::Debug,
            "info" => Severity::Info,
            "warn" | "warning" => Severity::Warn,
            "error" | "err" => Severity::Error,
            "fatal" | "critical" => Severity::Fatal,
            _ => Severity::Unknown,
        }
    }

    pub fn should_emit(self, min_level: Self) -> bool {
        self == Severity::Unknown || self.rank() >= min_level.rank()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event_kind", rename_all = "snake_case")]
pub enum ToolEventKind {
    ToolCallStart,
    ToolCallResult,
    ToolCall,
    Other,
    Malformed,
}

#[derive(Debug, Clone, Serialize)]
pub struct NormalizedEvent {
    pub kind: ToolEventKind,
    pub timestamp: Option<DateTime<Utc>>,
    pub timestamp_raw: Option<String>,
    pub session_key: Option<String>,
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
    pub tool_name: Option<String>,
    pub status: Option<String>,
    pub result_summary: Option<String>,
    pub call_id: Option<String>,
    pub level: Severity,
    pub level_raw: Option<String>,
    pub params: Vec<(String, String)>,
    pub message: Option<String>,
    pub raw_line: String,
}

impl NormalizedEvent {
    pub fn should_filter(
        &self,
        session_substring: Option<&String>,
        tool_name: Option<&String>,
        min_level: Severity,
    ) -> bool {
        if !min_level.should_emit(self.level) {
            return false;
        }

        if let Some(session_filter) = session_substring {
            let needle = session_filter.to_ascii_lowercase();
            let session_matches = self
                .session_key
                .as_ref()
                .or(self.session_id.as_ref())
                .map(|value| value.to_ascii_lowercase().contains(&needle))
                .unwrap_or(false);
            if !session_matches {
                return false;
            }
        }

        if let Some(tool_filter) = tool_name {
            let needle = tool_filter.to_ascii_lowercase();
            if self
                .tool_name
                .as_ref()
                .map(|value| !value.to_ascii_lowercase().contains(&needle))
                .unwrap_or(true)
            {
                return false;
            }
        }

        true
    }
}
