use crate::event::{NormalizedEvent, Severity, ToolEventKind};
use crate::parser::ParsedLine;
use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use std::collections::HashSet;

const MAX_PARAM_VALUE_LENGTH: usize = 120;
const MAX_PARAM_COUNT: usize = 6;

const SESSION_PATHS: &[&[&str]] = &[
    &["session_key"],
    &["session-key"],
    &["sessionId"],
    &["session_id"],
    &["session"],
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

#[derive(Debug, Clone)]
struct ParamExtraction {
    preview: Vec<(String, String)>,
    raw: Option<Value>,
    truncated: bool,
}

#[derive(Debug, Clone)]
struct ResultExtraction {
    preview: Option<String>,
    summary: Option<String>,
    raw: Option<Value>,
    metrics: Vec<(String, String)>,
    status: Option<String>,
    is_error: Option<bool>,
    exit_code: Option<i64>,
    duration_ms: Option<u64>,
    message: Option<String>,
}

pub fn normalize(line: &str) -> NormalizedEvent {
    match crate::parser::parse_line(line) {
        ParsedLine::Malformed { raw_line, reason } => malformed_event(raw_line, reason),
        ParsedLine::Json(value) => normalize_json(line, &value),
    }
}

fn malformed_event(raw_line: String, reason: String) -> NormalizedEvent {
    NormalizedEvent {
        kind: ToolEventKind::Malformed,
        timestamp: None,
        timestamp_raw: None,
        source_path: None,
        source_kind: None,
        session_key: None,
        session_id: None,
        session_source: None,
        agent_id: None,
        agent_source: None,
        tool_name: None,
        status: None,
        result_summary: Some(reason.clone()),
        result_preview: Some(reason),
        result_raw: None,
        result_metrics: Vec::new(),
        exit_code: None,
        duration_ms: None,
        is_error: None,
        call_id: None,
        call_ids: Vec::new(),
        correlation_ids: Vec::new(),
        message_id: None,
        parent_message_id: None,
        level: Severity::Unknown,
        level_raw: None,
        params: Vec::new(),
        args_preview: Vec::new(),
        args_raw: None,
        args_truncated: false,
        message: None,
        raw_line,
    }
}

fn normalize_json(raw: &str, value: &Value) -> NormalizedEvent {
    let object = match value.as_object() {
        Some(object) => object,
        None => {
            return NormalizedEvent {
                kind: ToolEventKind::Malformed,
                timestamp: None,
                timestamp_raw: Some(raw.to_string()),
                source_path: None,
                source_kind: None,
                session_key: None,
                session_id: None,
                session_source: None,
                agent_id: None,
                agent_source: None,
                tool_name: None,
                status: None,
                result_summary: Some("non-object json entry".to_string()),
                result_preview: Some("non-object json entry".to_string()),
                result_raw: Some(value.clone()),
                result_metrics: Vec::new(),
                exit_code: None,
                duration_ms: None,
                is_error: None,
                call_id: None,
                call_ids: Vec::new(),
                correlation_ids: Vec::new(),
                message_id: None,
                parent_message_id: None,
                level: Severity::Unknown,
                level_raw: None,
                params: Vec::new(),
                args_preview: Vec::new(),
                args_raw: None,
                args_truncated: false,
                message: Some(raw.to_string()),
                raw_line: raw.to_string(),
            };
        }
    };

    if is_transcript_message(object) {
        return normalize_transcript_message(raw, object);
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

    let call_ids = collect_call_ids(value);
    let call_id = call_ids.first().cloned();

    let param_extraction = extract_params(value);
    let result_extraction = extract_result(value);
    let status = result_extraction
        .status
        .clone()
        .or_else(|| extract_status(value));
    let message = result_extraction
        .message
        .clone()
        .or_else(|| first_string_from_paths(value, MESSAGE_PATHS));

    let kind = infer_kind(value, raw, status.as_deref(), &call_ids);

    NormalizedEvent {
        kind,
        timestamp,
        timestamp_raw,
        source_path: None,
        source_kind: None,
        session_key,
        session_id,
        session_source: None,
        agent_id,
        agent_source: None,
        tool_name,
        status,
        result_summary: result_extraction.summary,
        result_preview: result_extraction.preview,
        result_raw: result_extraction.raw,
        result_metrics: result_extraction.metrics,
        exit_code: result_extraction.exit_code,
        duration_ms: result_extraction.duration_ms,
        is_error: result_extraction.is_error,
        call_id,
        call_ids: call_ids.clone(),
        correlation_ids: call_ids,
        message_id: first_string_from_paths(value, &[&["id"], &["message_id"], &["message", "id"]]),
        parent_message_id: first_string_from_paths(
            value,
            &[
                &["parentId"],
                &["parent_id"],
                &["message", "parentId"],
                &["message", "parent_id"],
            ],
        ),
        level,
        level_raw,
        params: param_extraction.preview.clone(),
        args_preview: param_extraction.preview,
        args_raw: param_extraction.raw,
        args_truncated: param_extraction.truncated,
        message,
        raw_line: raw.to_string(),
    }
}

fn normalize_transcript_message(raw: &str, object: &Map<String, Value>) -> NormalizedEvent {
    let message = object
        .get("message")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let content = message
        .get("content")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let tool_calls = content
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("toolCall"))
        .cloned()
        .collect::<Vec<_>>();

    let outer = Value::Object(object.clone());
    let (timestamp, timestamp_raw) = parse_timestamp(&outer);
    let level_raw = first_string_from_paths(&outer, LEVEL_PATHS);
    let level = level_raw
        .as_deref()
        .map(Severity::from_string)
        .unwrap_or(Severity::Unknown);

    let session_id = first_string_from_paths(&outer, &[&["session"]]);
    let param_extraction = if !tool_calls.is_empty() {
        extract_params_from_value(&Value::Array(
            tool_calls
                .iter()
                .map(|call| {
                    let mut extracted = Map::new();
                    if let Some(name) = call.get("name") {
                        extracted.insert("name".to_string(), name.clone());
                    }
                    if let Some(arguments) =
                        call.get("arguments").or_else(|| call.get("partialJson"))
                    {
                        extracted.insert("arguments".to_string(), arguments.clone());
                    }
                    if let Some(id) = call.get("id") {
                        extracted.insert("id".to_string(), id.clone());
                    }
                    Value::Object(extracted)
                })
                .collect(),
        ))
    } else if let Some(details) = message.get("details") {
        extract_params_from_value(details)
    } else {
        extract_params_from_value(&Value::Object(message.clone()))
    };

    let result_extraction = extract_transcript_result(&message, &content);
    let call_ids = transcript_call_ids(&message, &tool_calls);
    let call_id = call_ids.first().cloned();
    let tool_name = transcript_tool_name(&message, &tool_calls);
    let status = transcript_status(&message, &role).or(result_extraction.status.clone());
    let kind = transcript_kind(&role, tool_calls.len(), status.as_deref());

    NormalizedEvent {
        kind,
        timestamp,
        timestamp_raw,
        source_path: None,
        source_kind: Some("transcript_v3".to_string()),
        session_key: session_id.clone(),
        session_id,
        session_source: Some("payload".to_string()),
        agent_id: None,
        agent_source: None,
        tool_name,
        status,
        result_summary: result_extraction.summary,
        result_preview: result_extraction.preview,
        result_raw: result_extraction.raw,
        result_metrics: result_extraction.metrics,
        exit_code: result_extraction.exit_code,
        duration_ms: result_extraction.duration_ms,
        is_error: result_extraction.is_error,
        call_id,
        call_ids: call_ids.clone(),
        correlation_ids: call_ids,
        message_id: first_string_from_paths(
            &Value::Object(object.clone()),
            &[&["message", "id"], &["id"]],
        ),
        parent_message_id: first_string_from_paths(
            &Value::Object(object.clone()),
            &[
                &["message", "parentId"],
                &["parentId"],
                &["message", "parent_id"],
            ],
        ),
        level,
        level_raw,
        params: param_extraction.preview.clone(),
        args_preview: param_extraction.preview,
        args_raw: param_extraction.raw,
        args_truncated: param_extraction.truncated,
        message: transcript_message_text(&message, &content),
        raw_line: raw.to_string(),
    }
}

fn is_transcript_message(object: &Map<String, Value>) -> bool {
    object.get("type").and_then(Value::as_str) == Some("message")
        && object.get("message").and_then(Value::as_object).is_some()
}

fn transcript_call_ids(message: &Map<String, Value>, tool_calls: &[Value]) -> Vec<String> {
    let mut ids = Vec::new();
    let mut seen = HashSet::new();
    if let Some(tool_call_id) = message.get("toolCallId").and_then(value_to_string) {
        if seen.insert(tool_call_id.clone()) {
            ids.push(tool_call_id);
        }
    }

    for tool_call in tool_calls {
        if let Some(id) = tool_call.get("id").and_then(value_to_string) {
            if seen.insert(id.clone()) {
                ids.push(id);
            }
        }
    }

    ids
}

fn transcript_tool_name(message: &Map<String, Value>, tool_calls: &[Value]) -> Option<String> {
    message
        .get("toolName")
        .and_then(value_to_string)
        .or_else(|| {
            tool_calls
                .iter()
                .find_map(|call| call.get("name").and_then(value_to_string))
        })
}

fn transcript_status(message: &Map<String, Value>, role: &str) -> Option<String> {
    message
        .get("details")
        .and_then(|value| value.get("status"))
        .and_then(value_to_string)
        .or_else(|| {
            message
                .get("isError")
                .and_then(Value::as_bool)
                .map(|is_error| if is_error { "error" } else { "ok" }.to_string())
        })
        .or_else(|| match role {
            "assistant" => Some("started".to_string()),
            "toolresult" => Some("completed".to_string()),
            _ => None,
        })
}

fn transcript_kind(role: &str, tool_call_count: usize, status: Option<&str>) -> ToolEventKind {
    match role {
        "assistant" if tool_call_count == 1 => ToolEventKind::ToolCallStart,
        "assistant" if tool_call_count > 1 => ToolEventKind::ToolCall,
        "toolresult" => {
            if matches!(status, Some("running" | "started" | "in_progress")) {
                ToolEventKind::ToolCall
            } else {
                ToolEventKind::ToolCallResult
            }
        }
        _ => ToolEventKind::Other,
    }
}

fn transcript_message_text(message: &Map<String, Value>, content: &[Value]) -> Option<String> {
    first_text_block(content).or_else(|| {
        first_string_from_paths(
            &Value::Object(message.clone()),
            &[&["content", "0", "text"]],
        )
    })
}

fn extract_result(value: &Value) -> ResultExtraction {
    let mut metrics = Vec::new();
    let exit_code = first_i64_from_paths(
        value,
        &[
            &["exit_code"],
            &["result", "exit_code"],
            &["details", "exitCode"],
        ],
    );
    let duration_ms = first_u64_from_paths(
        value,
        &[
            &["duration_ms"],
            &["result", "duration_ms"],
            &["details", "durationMs"],
        ],
    );
    if let Some(exit_code) = exit_code {
        metrics.push(("exit_code".to_string(), exit_code.to_string()));
    }
    if let Some(duration_ms) = duration_ms {
        metrics.push(("duration_ms".to_string(), duration_ms.to_string()));
    }

    let is_error = value
        .get("isError")
        .and_then(Value::as_bool)
        .or_else(|| value.get("ok").and_then(Value::as_bool).map(|ok| !ok));
    if let Some(is_error) = is_error {
        metrics.push(("is_error".to_string(), is_error.to_string()));
    }

    let raw = first_value_from_paths(value, RESULT_PATHS)
        .cloned()
        .or_else(|| first_value_from_paths(value, ERROR_PATHS).cloned());
    let preview = first_string_from_paths(value, RESULT_SUMMARY_PATHS)
        .or_else(|| raw.as_ref().map(value_summary))
        .or_else(|| first_string_from_paths(value, MESSAGE_PATHS));
    let summary = preview.clone();
    let status = extract_status(value);
    let message = first_string_from_paths(value, MESSAGE_PATHS);

    ResultExtraction {
        preview,
        summary,
        raw,
        metrics,
        status,
        is_error,
        exit_code,
        duration_ms,
        message,
    }
}

fn extract_transcript_result(message: &Map<String, Value>, content: &[Value]) -> ResultExtraction {
    let details = message.get("details").cloned();
    let aggregated = message
        .get("details")
        .and_then(|value| value.get("aggregated"))
        .cloned();
    let raw = aggregated.or_else(|| details.clone());

    let mut metrics = Vec::new();
    let exit_code = message
        .get("details")
        .and_then(|value| value.get("exitCode"))
        .and_then(Value::as_i64);
    let duration_ms = message
        .get("details")
        .and_then(|value| value.get("durationMs"))
        .and_then(Value::as_u64);
    let is_error = message.get("isError").and_then(Value::as_bool);
    let status = message
        .get("details")
        .and_then(|value| value.get("status"))
        .and_then(value_to_string)
        .or_else(|| is_error.map(|value| if value { "error" } else { "ok" }.to_string()));

    for key in ["command", "cwd", "path", "file_path", "query", "url"] {
        if let Some(value) = message
            .get("details")
            .and_then(|details| details.get(key))
            .and_then(value_to_string)
        {
            metrics.push((key.to_string(), truncate(&value, MAX_PARAM_VALUE_LENGTH)));
        }
    }

    if let Some(exit_code) = exit_code {
        metrics.push(("exit_code".to_string(), exit_code.to_string()));
    }
    if let Some(duration_ms) = duration_ms {
        metrics.push(("duration_ms".to_string(), duration_ms.to_string()));
    }

    let preview = first_text_block(content)
        .or_else(|| {
            message
                .get("details")
                .and_then(|value| value.get("aggregated"))
                .map(value_summary)
        })
        .or_else(|| status.clone());

    ResultExtraction {
        summary: preview.clone(),
        preview,
        raw,
        metrics,
        status,
        is_error,
        exit_code,
        duration_ms,
        message: first_text_block(content),
    }
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

fn extract_params(value: &Value) -> ParamExtraction {
    if let Some(args) = first_value_from_paths(value, PARAM_PATHS).cloned() {
        return extract_params_from_value(&args);
    }

    let mut preview = Vec::new();
    for key in ["command", "cmd", "path", "file_path", "query", "url"] {
        if let Some(value) = value.get(key).and_then(value_to_string) {
            preview.push((key.to_string(), truncate(&value, MAX_PARAM_VALUE_LENGTH)));
        }
    }

    ParamExtraction {
        truncated: false,
        raw: None,
        preview,
    }
}

fn extract_params_from_value(value: &Value) -> ParamExtraction {
    let mut preview = match value {
        Value::Object(map) => summarize_object(map),
        Value::Array(values) => values
            .iter()
            .enumerate()
            .map(|(index, value)| (format!("arg_{index}"), value_summary(value)))
            .collect::<Vec<_>>(),
        value => vec![("value".to_string(), value_summary(value))],
    };

    let truncated = preview.len() > MAX_PARAM_COUNT;
    preview.truncate(MAX_PARAM_COUNT);
    for (key, value) in &mut preview {
        *key = truncate(key, MAX_PARAM_VALUE_LENGTH);
        *value = truncate(value, MAX_PARAM_VALUE_LENGTH);
    }

    ParamExtraction {
        preview,
        raw: Some(value.clone()),
        truncated,
    }
}

fn summarize_object(map: &Map<String, Value>) -> Vec<(String, String)> {
    let preferred = [
        "command",
        "cmd",
        "cwd",
        "path",
        "file_path",
        "query",
        "url",
        "name",
        "status",
    ];
    let mut ordered = Vec::new();
    let mut seen = HashSet::new();

    for key in preferred {
        if let Some(value) = map.get(key) {
            seen.insert(key.to_string());
            ordered.push((key.to_string(), value_summary(value)));
        }
    }

    let mut rest = map
        .iter()
        .filter(|(key, _)| !seen.contains(*key))
        .map(|(key, value)| (key.clone(), value_summary(value)))
        .collect::<Vec<_>>();
    rest.sort_by(|a, b| a.0.cmp(&b.0));
    ordered.extend(rest);
    ordered
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

fn parse_timestamp(value: &Value) -> (Option<DateTime<Utc>>, Option<String>) {
    for path in TIMESTAMP_PATHS {
        if let Some(value) = first_value_from_paths(value, &[*path]) {
            if let Some(timestamp) = parse_timestamp_value(value) {
                return (
                    Some(timestamp),
                    Some(value.to_string().trim_matches('"').to_string()),
                );
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
        .find_map(|path| get_value_by_path(value, path).and_then(value_to_string))
}

fn first_i64_from_paths(value: &Value, paths: &[&[&str]]) -> Option<i64> {
    paths
        .iter()
        .find_map(|path| get_value_by_path(value, path).and_then(Value::as_i64))
}

fn first_u64_from_paths(value: &Value, paths: &[&[&str]]) -> Option<u64> {
    paths
        .iter()
        .find_map(|path| get_value_by_path(value, path).and_then(Value::as_u64))
}

fn first_value_from_paths<'a>(value: &'a Value, paths: &[&[&str]]) -> Option<&'a Value> {
    paths.iter().find_map(|path| get_value_by_path(value, path))
}

fn get_value_by_path<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for key in path {
        current = match current {
            Value::Object(map) => map.get(*key)?,
            Value::Array(values) => values.get(key.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

fn value_summary(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => truncate(value, MAX_PARAM_VALUE_LENGTH),
        Value::Array(values) => {
            if let Some(text) = first_text_block(values) {
                truncate(&text, MAX_PARAM_VALUE_LENGTH)
            } else {
                format!("array(len={})", values.len())
            }
        }
        Value::Object(map) => {
            summarize_nested_object(map).unwrap_or_else(|| format!("object(len={})", map.len()))
        }
    }
}

fn summarize_nested_object(map: &Map<String, Value>) -> Option<String> {
    for key in [
        "command",
        "cmd",
        "path",
        "file_path",
        "query",
        "url",
        "text",
        "status",
    ] {
        if let Some(value) = map.get(key).and_then(value_to_string) {
            return Some(format!(
                "{key}={}",
                truncate(&value, MAX_PARAM_VALUE_LENGTH)
            ));
        }
    }
    None
}

fn first_text_block(values: &[Value]) -> Option<String> {
    values.iter().find_map(|value| {
        value
            .get("text")
            .and_then(Value::as_str)
            .map(|text| text.to_string())
    })
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
    use crate::event::ToolEventKind;

    #[test]
    fn parse_tool_call_from_json() {
        let line = r#"{"event":"tool_call","timestamp":"2026-03-06T20:00:00Z","session_key":"session-123","tool":"search","call_id":"abc","params":{"query":"status check","depth":4},"level":"info"}"#;
        let normalized = normalize(line);
        assert_eq!(normalized.session_key.as_deref(), Some("session-123"));
        assert_eq!(normalized.tool_name.as_deref(), Some("search"));
        assert_eq!(normalized.call_id.as_deref(), Some("abc"));
        assert_eq!(normalized.call_ids, vec!["abc".to_string()]);
        assert_eq!(normalized.args_preview[0].0, "query");
        assert!(matches!(normalized.kind, ToolEventKind::ToolCallStart));
    }

    #[test]
    fn parse_tool_call_result_with_request_id_only() {
        let line = r#"{"event":"tool_call_result","tool":"shell","request_id":"req-1","status":"ok","params":{"value":"x"},"result":{"exit_code":0}}"#;
        let normalized = normalize(line);
        assert_eq!(normalized.call_id.as_deref(), Some("req-1"));
        assert_eq!(normalized.status.as_deref(), Some("ok"));
        assert_eq!(normalized.params.len(), 1);
        assert!(matches!(normalized.kind, ToolEventKind::ToolCallResult));
    }

    #[test]
    fn parse_nested_openclaw_error_event() {
        let line = r#"{"type":"tool","tool_call":{"tool":"shell","event":"error","id":"call-3","error":{"message":"quota exceeded","code":429}},"session":{"id":"session-3"}}"#;
        let normalized = normalize(line);
        assert_eq!(normalized.tool_name.as_deref(), Some("shell"));
        assert_eq!(normalized.call_id.as_deref(), Some("call-3"));
        assert!(matches!(normalized.kind, ToolEventKind::ToolCallResult));
        assert!(normalized.result_summary.is_some());
    }

    #[test]
    fn parse_non_object_json_as_malformed() {
        let normalized = normalize("\"plain string\"");
        assert!(matches!(normalized.kind, ToolEventKind::Malformed));
        assert!(normalized.result_summary.is_some());
    }

    #[test]
    fn transcript_tool_call_preserves_arguments_and_ids() {
        let line = r#"{"type":"message","session":"session-v3","timestamp":"2026-03-06T20:00:00Z","message":{"id":"msg-1","parentId":"msg-0","role":"assistant","content":[{"type":"toolCall","id":"call-1","name":"exec","arguments":{"command":"cargo test","cwd":"/repo"}},{"type":"toolCall","id":"call-2","name":"read","arguments":{"file_path":"src/main.rs"}}]}}"#;
        let normalized = normalize(line);
        assert!(matches!(normalized.kind, ToolEventKind::ToolCall));
        assert_eq!(
            normalized.call_ids,
            vec!["call-1".to_string(), "call-2".to_string()]
        );
        assert_eq!(normalized.tool_name.as_deref(), Some("exec"));
        assert_eq!(normalized.message_id.as_deref(), Some("msg-1"));
        assert_eq!(normalized.parent_message_id.as_deref(), Some("msg-0"));
        assert!(normalized.args_raw.is_some());
    }

    #[test]
    fn transcript_tool_result_extracts_metrics() {
        let line = r#"{"type":"message","session":"session-v3","timestamp":"2026-03-06T20:00:01Z","message":{"role":"toolResult","toolCallId":"call-1","toolName":"exec","isError":false,"details":{"status":"completed","exitCode":0,"durationMs":58,"cwd":"/repo","command":"cargo test"},"content":[{"type":"text","text":"tests passed"}]}}"#;
        let normalized = normalize(line);
        assert!(matches!(normalized.kind, ToolEventKind::ToolCallResult));
        assert_eq!(normalized.call_id.as_deref(), Some("call-1"));
        assert_eq!(normalized.exit_code, Some(0));
        assert_eq!(normalized.duration_ms, Some(58));
        assert_eq!(normalized.result_preview.as_deref(), Some("tests passed"));
    }
}
