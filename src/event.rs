use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

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
    pub source_path: Option<String>,
    pub source_kind: Option<String>,
    pub session_key: Option<String>,
    pub session_id: Option<String>,
    pub session_source: Option<String>,
    pub agent_id: Option<String>,
    pub agent_source: Option<String>,
    pub tool_name: Option<String>,
    pub status: Option<String>,
    pub result_summary: Option<String>,
    pub result_preview: Option<String>,
    pub result_raw: Option<Value>,
    pub result_metrics: Vec<(String, String)>,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<u64>,
    pub is_error: Option<bool>,
    pub call_id: Option<String>,
    pub call_ids: Vec<String>,
    #[serde(skip)]
    pub correlation_ids: Vec<String>,
    pub message_id: Option<String>,
    pub parent_message_id: Option<String>,
    pub level: Severity,
    pub level_raw: Option<String>,
    pub params: Vec<(String, String)>,
    pub args_preview: Vec<(String, String)>,
    pub args_raw: Option<Value>,
    pub args_truncated: bool,
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
        if !self.level.should_emit(min_level) {
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

    pub fn all_call_ids(&self) -> impl Iterator<Item = &str> {
        self.call_ids
            .iter()
            .map(|value| value.as_str())
            .chain(self.call_id.as_deref().into_iter())
            .chain(self.correlation_ids.iter().map(|value| value.as_str()))
    }

    pub fn fallback_signature(&self) -> Option<String> {
        let session = self.session_key.as_ref().or(self.session_id.as_ref())?;
        let tool = self.tool_name.as_ref()?;
        let detail = self
            .preferred_identity_hint()
            .unwrap_or_else(|| "-".to_string());
        Some(format!(
            "{session}|{tool}|{}|{detail}",
            self.agent_id.as_deref().unwrap_or("-"),
        ))
    }

    pub fn preferred_params(&self) -> &[(String, String)] {
        if self.args_preview.is_empty() {
            &self.params
        } else {
            &self.args_preview
        }
    }

    fn preferred_identity_hint(&self) -> Option<String> {
        let preferred_keys = ["command", "cmd", "path", "file_path", "query", "url"];
        for key in preferred_keys {
            if let Some((_, value)) = self
                .preferred_params()
                .iter()
                .find(|(candidate, _)| candidate == key)
            {
                return Some(value.clone());
            }
        }

        self.preferred_params()
            .first()
            .map(|(key, value)| format!("{key}={value}"))
            .or_else(|| self.message.clone())
            .or_else(|| self.result_preview.clone())
            .or_else(|| self.result_summary.clone())
    }
}
