use crate::event::{NormalizedEvent, Severity, ToolEventKind};
use crate::parser::ParsedLine;
use chrono::{DateTime, Utc};
use serde_json::Value;

const MAX_PARAM_VALUE_LENGTH: usize = 120;
const MAX_PARAM_COUNT: usize = 4;

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
    let obj = match value.as_object() {
        Some(map) => map,
        None => {
            return NormalizedEvent {
                kind: ToolEventKind::Other,
                timestamp: None,
                timestamp_raw: Some(raw.to_string()),
                session_key: None,
                session_id: None,
                agent_id: None,
                tool_name: None,
                status: None,
                result_summary: Some("non-object json entry".to_string()),
                call_id: None,
                level: Severity::Unknown,
                level_raw: None,
                params: Vec::new(),
                message: Some(raw.to_string()),
                raw_line: raw.to_string(),
            };
        }
    };

    let (timestamp, timestamp_raw) = parse_timestamp(obj);
    let level_raw = first_string(
        obj,
        &["level", "severity", "log_level", "severityText", "lvl"],
    );
    let level = level_raw
        .as_deref()
        .map(Severity::from_string)
        .unwrap_or(Severity::Unknown);

    let session_key = first_string(
        obj,
        &[
            "session_key",
            "session-key",
            "sessionId",
            "sessionIdentifier",
        ],
    );
    let session_id = first_string(obj, &["session_id", "sid"])
        .or_else(|| first_string_from_nested(obj, &["session", "id"]));

    let agent_id = first_string(obj, &["agent_id", "agent", "source", "agentId"])
        .or_else(|| first_string_from_nested(obj, &["agent", "id"]));

    let tool_name = first_string(
        obj,
        &[
            "tool_name",
            "tool",
            "toolName",
            "name",
            "operation",
            "toolCall",
        ],
    )
    .or_else(|| first_string_from_nested(obj, &["tool_call", "name"]));

    let call_id = first_string(
        obj,
        &[
            "call_id",
            "callId",
            "tool_call_id",
            "request_id",
            "correlation_id",
            "requestId",
        ],
    )
    .or_else(|| first_string_from_nested(obj, &["tool_call", "id"]));

    let status = first_string(obj, &["status", "result_status", "outcome", "result"])
        .or_else(|| {
            value.get("ok").and_then(|value| value.as_bool()).map(|ok| {
                if ok {
                    "ok".to_string()
                } else {
                    "error".to_string()
                }
            })
        })
        .or_else(|| first_string(obj, &["error"]));

    let result_summary = first_string(obj, &["result_summary", "summary"])
        .or_else(|| first_string_from_nested(obj, &["result", "summary"]));

    let kind = infer_kind(obj, raw);
    let params = extract_params(value);

    let message = first_string(obj, &["message", "msg"])
        .or_else(|| first_string_from_nested(obj, &["event", "message"]));

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
        level,
        level_raw,
        params,
        message,
        raw_line: raw.to_string(),
    }
}

fn infer_kind(obj: &serde_json::Map<String, Value>, raw: &str) -> ToolEventKind {
    let event_type = first_string(
        obj,
        &["event", "type", "action", "operation", "kind", "state"],
    )
    .unwrap_or_default()
    .to_ascii_lowercase();

    let joined = format!("{} {}", event_type, raw.to_ascii_lowercase());

    if contains_any(
        &joined,
        &[
            "tool_call",
            "toolcall",
            "call_start",
            "started",
            "invoked",
            "invocation",
        ],
    ) {
        return ToolEventKind::ToolCallStart;
    }

    if contains_any(
        &joined,
        &[
            "result",
            "completed",
            "finished",
            "done",
            "tool_result",
            "toolresult",
        ],
    ) {
        return ToolEventKind::ToolCallResult;
    }

    if has_result_fields(obj) {
        return ToolEventKind::ToolCallResult;
    }

    if has_call_fields(obj) {
        return ToolEventKind::ToolCallStart;
    }

    if has_any_fields(
        obj,
        &[
            "tool",
            "tool_name",
            "toolName",
            "name",
            "params",
            "arguments",
        ],
    ) {
        return ToolEventKind::ToolCall;
    }

    ToolEventKind::Other
}

fn has_result_fields(obj: &serde_json::Map<String, Value>) -> bool {
    obj.contains_key("result")
        || obj.contains_key("output")
        || obj.contains_key("status")
        || obj.contains_key("error")
        || obj.contains_key("exit_code")
}

fn has_call_fields(obj: &serde_json::Map<String, Value>) -> bool {
    obj.contains_key("tool")
        || obj.contains_key("tool_name")
        || obj.contains_key("toolName")
        || obj.contains_key("params")
        || obj.contains_key("arguments")
        || obj.contains_key("args")
}

fn has_any_fields(obj: &serde_json::Map<String, Value>, keys: &[&str]) -> bool {
    keys.iter().any(|key| obj.contains_key(*key))
}

fn extract_params(value: &Value) -> Vec<(String, String)> {
    let values = ["parameters", "params", "arguments", "args", "input"]
        .iter()
        .find_map(|key| value.get(*key))
        .map(|value| match value {
            Value::Object(map) => map
                .iter()
                .map(|(key, value)| (key.clone(), value_summary(value)))
                .collect::<Vec<_>>(),
            Value::Array(values) => values
                .iter()
                .enumerate()
                .map(|(index, value)| (format!("arg_{index}"), value_summary(value)))
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .unwrap_or_default();

    values
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

fn parse_timestamp(
    obj: &serde_json::Map<String, Value>,
) -> (Option<DateTime<Utc>>, Option<String>) {
    for key in [
        "timestamp",
        "time",
        "ts",
        "@timestamp",
        "logged_at",
        "created_at",
    ] {
        if let Some(value) = obj.get(key) {
            if let Some(ts_string) = value.as_str() {
                if let Ok(ts) = DateTime::parse_from_rfc3339(ts_string) {
                    return (Some(ts.with_timezone(&Utc)), Some(ts_string.to_string()));
                }
            }

            if let Some(v) = value.as_i64() {
                return (Some(timestamp_from_epoch(v as f64)), Some(v.to_string()));
            }

            if let Some(v) = value.as_f64() {
                return (Some(timestamp_from_epoch(v)), Some(v.to_string()));
            }
        }
    }

    (None, None)
}

fn timestamp_from_epoch(value: f64) -> DateTime<Utc> {
    let secs = value.floor() as i64;
    let nanos = ((value - secs as f64) * 1_000_000_000.0).clamp(0.0, 999_999_999.0) as u32;
    DateTime::<Utc>::from_timestamp(secs, nanos).unwrap_or_else(Utc::now)
}

fn first_string(obj: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| obj.get(*key).and_then(value_to_string))
}

fn first_string_from_nested(obj: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    let (head, tail) = keys.split_first()?;
    obj.get(*head)
        .and_then(Value::as_object)
        .and_then(|child| first_string(child, tail))
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
        assert!(matches!(
            normalized.kind,
            crate::event::ToolEventKind::ToolCallStart
        ));
    }

    #[test]
    fn truncate_params() {
        let line = r#"{"event":"tool_call_result","tool":"shell","call_id":"abc","status":"ok","params":{"value":"x"}}"#;
        let normalized = normalize(line);
        assert_eq!(normalized.status.as_deref(), Some("ok"));
        assert_eq!(normalized.params.len(), 1);
    }
}
