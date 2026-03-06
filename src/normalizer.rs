use crate::event::{NormalizedEvent, Severity, ToolEventKind};
use crate::parser::ParsedLine;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::HashSet;

const MAX_PARAM_VALUE_LENGTH: usize = 120;
const MAX_PARAM_COUNT: usize = 4;

const SESSION_PATHS: &[&[&str]] = &[
    &["session_key"],
    &["session-key"],
    &["sessionId"],
    &["session_id"],
    &["sid"],
    &["session", "id"],
    &["session", "session_id"],
    &["session", "sessionId"],
    &["meta", "session"],
    &["metadata", "session_id"],
    &["metadata", "session"],
    &["context", "session_id"],
    &["context", "session"],
    &["payload", "session_id"],
    &["payload", "session"],
];

const AGENT_PATHS: &[&[&str]] = &[
    &["agent_id"],
    &["agentId"],
    &["agent", "id"],
    &["agent", "agent_id"],
    &["agent", "identifier"],
    &["meta", "agent_id"],
    &["metadata", "agent_id"],
    &["metadata", "agentId"],
    &["context", "agent_id"],
    &["source", "agent_id"],
];

const TOOL_PATHS: &[&[&str]] = &[
    &["tool_name"],
    &["toolName"],
    &["tool"],
    &["name"],
    &["operation"],
    &["tool_call", "tool"],
    &["tool_call", "name"],
    &["metadata", "tool"],
    &["metadata", "tool_name"],
];

const CORRELATION_ID_PATHS: &[&[&str]] = &[
    &["call_id"],
    &["callId"],
    &["tool_call_id"],
    &["toolCallId"],
    &["request_id"],
    &["requestId"],
    &["correlation_id"],
    &["correlationId"],
    &["event", "id"],
    &["tool_call", "id"],
    &["tool_call", "call_id"],
    &["tool_call", "callId"],
    &["tool_call", "request_id"],
    &["tool_call", "requestId"],
    &["request", "id"],
    &["request", "request_id"],
    &["request", "requestId"],
    &["meta", "request_id"],
    &["metadata", "request_id"],
    &["metadata", "call_id"],
    &["context", "request_id"],
    &["payload", "request_id"],
];

const TIMESTAMP_PATHS: &[&[&str]] = &[
    &["timestamp"],
    &["time"],
    &["ts"],
    &["@timestamp"],
    &["logged_at"],
    &["loggedAt"],
    &["created_at"],
    &["createdAt"],
    &["event", "timestamp"],
    &["meta", "timestamp"],
    &["metadata", "timestamp"],
    &["context", "timestamp"],
    &["payload", "timestamp"],
    &["log", "timestamp"],
];

const KIND_HINT_PATHS: &[&[&str]] = &[
    &["event"],
    &["type"],
    &["kind"],
    &["state"],
    &["status"],
    &["action"],
    &["operation"],
    &["event", "type"],
    &["event", "name"],
    &["metadata", "event"],
    &["metadata", "type"],
    &["metadata", "kind"],
    &["tool_call", "state"],
    &["tool_call", "type"],
    &["tool_call", "status"],
];

const PARAM_PATHS: &[&[&str]] = &[
    &["parameters"],
    &["params"],
    &["arguments"],
    &["args"],
    &["input"],
    &["tool_call", "parameters"],
    &["tool_call", "params"],
    &["tool_call", "arguments"],
    &["tool_call", "args"],
    &["meta", "params"],
    &["metadata", "params"],
    &["payload", "params"],
];

const RESULT_SUMMARY_PATHS: &[&[&str]] = &[
    &["result_summary"],
    &["summary"],
    &["result", "summary"],
    &["result", "message"],
    &["output", "summary"],
    &["error", "message"],
    &["tool_call", "result", "summary"],
    &["tool_call", "error", "message"],
];

const RESULT_PATHS: &[&[&str]] = &[
    &["result"],
    &["output"],
    &["response"],
    &["response", "result"],
    &["tool_call", "result"],
    &["tool_call", "output"],
    &["tool_result"],
];

const ERROR_PATHS: &[&[&str]] = &[
    &["error"],
    &["tool_call", "error"],
    &["payload", "error"],
    &["metadata", "error"],
];

const MESSAGE_PATHS: &[&[&str]] = &[
    &["message"],
    &["msg"],
    &["event", "message"],
    &["metadata", "message"],
    &["meta", "message"],
    &["tool_call", "message"],
    &["payload", "message"],
];

const LEVEL_PATHS: &[&[&str]] = &[
    &["level"],
    &["severity"],
    &["log_level"],
    &["severityText"],
    &["severity_text"],
    &["lvl"],
    &["metadata", "level"],
    &["meta", "level"],
];

const STATUS_PATHS: &[&[&str]] = &[
    &["status"],
    &["result_status"],
    &["result", "status"],
    &["tool_call", "status"],
    &["outcome"],
    &["result", "outcome"],
    &["metadata", "status"],
];

pub fn normalize(line: &str) -> NormalizedEvent {
    match crate::parser::parse_line(line) {
        ParsedLine::Malformed { raw_line, reason } => NormalizedEvent {
            kind: ToolEventKind::Malformed,
            timestamp: None,
            timestamp_raw: None,
            session_key: None,
            session_id: None,
            agent_id: None,
            tool_name: None,
            status: None,
            result_summary: Some(reason),
            call_id: None,
            correlation_ids: Vec::new(),
            level: Severity::Unknown,
            level_raw: None,
            params: Vec::new(),
            message: None,
            raw_line,
        },
        ParsedLine::Json(value) => normalize_json(line, &value),
    }
}

fn normalize_json(raw: &str, value: &Value) -> NormalizedEvent {
    if value.as_object().is_none() {
        return NormalizedEvent {
            kind: ToolEventKind::Malformed,
            timestamp: None,
            timestamp_raw: Some(raw.to_string()),
            session_key: None,
            session_id: None,
            agent_id: None,
            tool_name: None,
            status: None,
            result_summary: Some("non-object json entry".to_string()),
            call_id: None,
            correlation_ids: Vec::new(),
            level: Severity::Unknown,
            level_raw: None,
            params: Vec::new(),
            message: Some(raw.to_string()),
            raw_line: raw.to_string(),
        };
    }

    let (timestamp, timestamp_raw) = parse_timestamp(value);
    let level_raw = first_string_from_paths(value, LEVEL_PATHS);
    let level = level_raw
        .as_deref()
        .map(Severity::from_string)
        .unwrap_or(Severity::Unknown);

    let session_key = first_string_from_paths(value, SESSION_PATHS);
    let session_id = session_key.clone();

    let agent_id = first_string_from_paths(value, AGENT_PATHS);
    let tool_name = first_string_from_paths(value, TOOL_PATHS);

    let correlation_ids = collect_call_ids(value);
    let call_id = correlation_ids.first().cloned();

    let status = extract_status(value);
    let result_summary = extract_result_summary(value);
    let message = first_string_from_paths(value, MESSAGE_PATHS);

    let kind = infer_kind(value, raw, status.as_deref(), &correlation_ids);
    let params = extract_params(value);

    NormalizedEvent {
        kind,
        timestamp,
        timestamp_raw,
        session_key,
        session_id,
        agent_id,
        tool_name,
        status,
        result_summary,
        call_id,
        correlation_ids,
        level,
        level_raw,
        params,
        message,
        raw_line: raw.to_string(),
    }
}

fn infer_kind(
    value: &Value,
    raw: &str,
    status: Option<&str>,
    correlation_ids: &[String],
) -> ToolEventKind {
    let mut event_type = first_string_from_paths(value, KIND_HINT_PATHS)
        .unwrap_or_default()
        .to_ascii_lowercase();
    event_type.push(' ');
    event_type.push_str(&raw.to_ascii_lowercase());

    if has_any_fields(value, ERROR_PATHS) && has_call_marker(&event_type) {
        return ToolEventKind::ToolCallResult;
    }

    if contains_any(
        &event_type,
        &[
            "tool_call_error",
            "tool_error",
            "toolcall_error",
            "error",
            "error_event",
            "failed",
            "exception",
            "tool_call_exception",
        ],
    ) {
        return ToolEventKind::ToolCallResult;
    }

    if contains_any(
        &event_type,
        &[
            "tool_call_start",
            "tool_call.started",
            "tool_call_starting",
            "tool_call.create",
            "tool_start",
            "call_start",
            "started",
            "invoking",
            "invoked",
        ],
    ) {
        return ToolEventKind::ToolCallStart;
    }

    if contains_any(
        &event_type,
        &[
            "tool_call_result",
            "tool_call.done",
            "tool_call.complete",
            "toolresult",
            "call_result",
            "result",
            "completed",
            "finished",
            "done",
            "success",
            "ok",
        ],
    ) {
        return ToolEventKind::ToolCallResult;
    }

    if has_result_fields(value) {
        return ToolEventKind::ToolCallResult;
    }

    if has_call_fields(value) {
        return ToolEventKind::ToolCallStart;
    }

    if correlation_ids.len() > 1 {
        return ToolEventKind::ToolCallStart;
    }

    if let Some(value) = status {
        if matches!(
            value.to_ascii_lowercase().as_str(),
            "ok" | "done" | "success" | "error" | "fail" | "failed"
        ) {
            return ToolEventKind::ToolCallResult;
        }
    }

    if has_any_fields(
        value,
        &[&["tool_name"], &["tool"], &["toolName"], &["operation"]],
    ) {
        return ToolEventKind::ToolCall;
    }

    ToolEventKind::Other
}

fn has_call_marker(value: &str) -> bool {
    contains_any(
        value,
        &[
            "tool_call",
            "call_start",
            "call",
            "toolcall",
            "tool call",
            "tool invocation",
        ],
    )
}

fn has_call_fields(value: &Value) -> bool {
    has_any_fields(
        value,
        &[
            &["tool"],
            &["tool_name"],
            &["toolName"],
            &["name"],
            &["params"],
            &["arguments"],
            &["args"],
            &["tool_call", "tool"],
            &["tool_call", "name"],
            &["tool_call", "params"],
            &["tool_call", "arguments"],
        ],
    )
}

fn has_result_fields(value: &Value) -> bool {
    has_any_fields(
        value,
        &[
            &["result"],
            &["output"],
            &["response"],
            &["tool_result"],
            &["exit_code"],
            &["tool_call", "result"],
            &["tool_call", "output"],
            &["error"],
            &["tool_call", "error"],
        ],
    )
}

fn has_any_fields(value: &Value, paths: &[&[&str]]) -> bool {
    paths
        .iter()
        .any(|path| first_value_from_paths(value, &[*path]).is_some())
}

fn collect_call_ids(value: &Value) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for key in CORRELATION_ID_PATHS {
        if let Some(id) = first_string_from_paths(value, &[*key]) {
            if seen.insert(id.clone()) {
                ids.push(id);
            }
        }
    }

    ids
}

fn extract_status(value: &Value) -> Option<String> {
    if let Some(status) = first_string_from_paths(value, STATUS_PATHS) {
        return Some(status);
    }

    value.get("ok").and_then(|value| value.as_bool()).map(|ok| {
        if ok {
            "ok".to_string()
        } else {
            "error".to_string()
        }
    })
}

fn extract_result_summary(value: &Value) -> Option<String> {
    if let Some(summary) = first_string_from_paths(value, RESULT_SUMMARY_PATHS) {
        return Some(summary);
    }

    if let Some(error) = first_value_from_paths(value, ERROR_PATHS) {
        return Some(value_summary(error));
    }

    if let Some(result) = first_value_from_paths(value, RESULT_PATHS) {
        return Some(value_summary(result));
    }

    first_string_from_paths(value, &[&["result_summary"], &["message"], &["msg"]])
}

fn extract_params(value: &Value) -> Vec<(String, String)> {
    let values = first_value_from_paths(value, PARAM_PATHS).map(|value| match value {
        Value::Object(map) => {
            let mut pairs = map
                .iter()
                .map(|(key, value)| (key.to_string(), value_summary(value)))
                .collect::<Vec<_>>();
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            pairs
        }
        Value::Array(values) => values
            .iter()
            .enumerate()
            .map(|(index, value)| (format!("arg_{index}"), value_summary(value)))
            .collect::<Vec<_>>(),
        value => vec![("value".to_string(), value_summary(value))],
    });

    values
        .unwrap_or_default()
        .into_iter()
        .take(MAX_PARAM_COUNT)
        .map(|(key, value)| {
            (
                truncate(&key, MAX_PARAM_VALUE_LENGTH),
                truncate(&value, MAX_PARAM_VALUE_LENGTH),
            )
        })
        .collect()
}

fn parse_timestamp(value: &Value) -> (Option<DateTime<Utc>>, Option<String>) {
    for path in TIMESTAMP_PATHS {
        if let Some(value) = first_value_from_paths(value, &[*path]) {
            if let Some(timestamp) = parse_timestamp_value(value) {
                let raw = match value {
                    Value::String(value) => Some(value.to_string()),
                    _ => Some(value.to_string()),
                };
                return (Some(timestamp), raw);
            }
        }
    }

    (None, None)
}

fn parse_timestamp_value(value: &Value) -> Option<DateTime<Utc>> {
    match value {
        Value::String(value) => parse_timestamp_str(value)
            .or_else(|| value.parse::<f64>().ok().and_then(parse_epoch_timestamp)),
        Value::Number(value) => value.as_f64().and_then(parse_epoch_timestamp),
        _ => None,
    }
}

fn parse_timestamp_str(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .or_else(|| {
            DateTime::parse_from_rfc2822(value)
                .ok()
                .map(|timestamp| timestamp.with_timezone(&Utc))
        })
}

fn parse_epoch_timestamp(value: f64) -> Option<DateTime<Utc>> {
    if !value.is_finite() {
        return None;
    }

    let abs = value.abs();
    let scaled = if abs >= 1_000_000_000_000_000_000.0 {
        value / 1_000_000_000.0
    } else if abs >= 1_000_000_000_000_000.0 {
        value / 1_000_000.0
    } else if abs >= 1_000_000_000.0 {
        value / 1_000.0
    } else {
        value
    };

    let secs = scaled.floor() as i64;
    let nanos = ((scaled - secs as f64).abs() * 1_000_000_000.0).round() as u32;

    DateTime::from_timestamp(secs, nanos)
}

fn first_string_from_paths(value: &Value, paths: &[&[&str]]) -> Option<String> {
    paths
        .iter()
        .find_map(|path| first_value_from_paths(value, &[*path]).and_then(value_to_string))
}

fn first_value_from_paths<'a>(value: &'a Value, paths: &[&[&str]]) -> Option<&'a Value> {
    paths.iter().find_map(|path| get_value_by_path(value, path))
}

fn get_value_by_path<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

fn value_summary(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => truncate(value, MAX_PARAM_VALUE_LENGTH),
        Value::Array(values) => format!("array(len={})", values.len()),
        Value::Object(map) => format!("object(len={})", map.len()),
    }
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    let value = value.to_ascii_lowercase();
    needles.iter().any(|needle| value.contains(needle))
}

fn truncate(text: &str, max_len: usize) -> String {
    if text.chars().count() <= max_len {
        text.to_string()
    } else {
        let mut out = text
            .chars()
            .take(max_len.saturating_sub(1))
            .collect::<String>();
        out.push('\u{2026}');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn parse_tool_call_from_json() {
        let line = r#"{"event":"tool_call","timestamp":"2026-03-06T20:00:00Z","session_key":"session-123","tool":"search","call_id":"abc","params":{"query":"status check","depth":4},"level":"info"}"#;
        let normalized = normalize(line);
        assert_eq!(normalized.session_key.as_deref(), Some("session-123"));
        assert_eq!(normalized.tool_name.as_deref(), Some("search"));
        assert_eq!(normalized.call_id.as_deref(), Some("abc"));
        assert_eq!(normalized.correlation_ids, vec!["abc".to_string()]);
        assert!(matches!(
            normalized.kind,
            crate::event::ToolEventKind::ToolCallStart
        ));
    }

    #[test]
    fn parse_tool_call_result_with_request_id_only() {
        let line = r#"{"event":"tool_call_result","tool":"shell","request_id":"req-1","status":"ok","params":{"value":"x"}}"#;
        let normalized = normalize(line);
        assert_eq!(normalized.call_id.as_deref(), Some("req-1"));
        assert_eq!(normalized.status.as_deref(), Some("ok"));
        assert_eq!(normalized.params.len(), 1);
        assert!(matches!(
            normalized.kind,
            crate::event::ToolEventKind::ToolCallResult
        ));
    }

    #[test]
    fn parse_nested_openclaw_error_event() {
        let line = r#"{"type":"tool","tool_call":{"tool":"shell","event":"error","id":"call-3","error":{"message":"quota exceeded","code":429}},"session":{"id":"session-3"}}"#;
        let normalized = normalize(line);
        assert_eq!(normalized.tool_name.as_deref(), Some("shell"));
        assert_eq!(normalized.call_id.as_deref(), Some("call-3"));
        assert!(matches!(
            normalized.kind,
            crate::event::ToolEventKind::ToolCallResult
        ));
        assert!(normalized.result_summary.is_some());
    }

    #[test]
    fn truncate_params() {
        let line = r#"{"event":"tool_call_result","tool":"shell","call_id":"abc","status":"ok","params":{"value":"x"}}"#;
        let normalized = normalize(line);
        assert_eq!(normalized.status.as_deref(), Some("ok"));
        assert_eq!(normalized.params.len(), 1);
    }

    #[test]
    fn parse_non_object_json_as_malformed() {
        let normalized = normalize("\"plain string\"");
        assert!(matches!(
            normalized.kind,
            crate::event::ToolEventKind::Malformed
        ));
        assert!(normalized.result_summary.is_some());
    }
}
