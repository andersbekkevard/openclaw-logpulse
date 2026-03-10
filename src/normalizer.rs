use crate::event::{NormalizedEvent, Severity, ToolEventKind};
use crate::parser::ParsedLine;
use crate::session_identity::{
    build_session_identity, derive_routing_metadata, SessionIdentityState,
};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;

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

const SESSION_LABEL_PATHS: &[&[&str]] = &[
    &["session_label"],
    &["sessionLabel"],
    &["session", "label"],
    &["session", "session_label"],
    &["metadata", "session_label"],
    &["metadata", "sessionLabel"],
    &["context", "session_label"],
    &["context", "sessionLabel"],
    &["payload", "session_label"],
    &["payload", "sessionLabel"],
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

#[cfg_attr(not(test), allow(dead_code))]
pub fn normalize(line: &str) -> NormalizedEvent {
    normalize_with_source(line, None)
}

pub fn normalize_with_source(line: &str, source_path: Option<&Path>) -> NormalizedEvent {
    normalize_many_with_source(line, source_path)
        .into_iter()
        .next()
        .unwrap_or_else(|| empty_event(line.to_string()))
}

pub fn normalize_many_with_source(line: &str, source_path: Option<&Path>) -> Vec<NormalizedEvent> {
    match crate::parser::parse_line(line) {
        ParsedLine::Malformed { raw_line, reason } => vec![NormalizedEvent {
            kind: ToolEventKind::Malformed,
            result_summary: Some(reason),
            result_preview: None,
            ..empty_event(raw_line)
        }],
        ParsedLine::Json(value) => normalize_json(line, &value, source_path),
    }
}

fn empty_event(raw_line: String) -> NormalizedEvent {
    NormalizedEvent {
        kind: ToolEventKind::Other,
        timestamp: None,
        timestamp_raw: None,
        source_path: None,
        source_kind: None,
        session_key: None,
        session_label: None,
        session_id: None,
        session_source: None,
        session_label_source: None,
        session_identity_conflicts: Vec::new(),
        routing: Default::default(),
        agent_id: None,
        agent_source: None,
        tool_name: None,
        status: None,
        result_summary: None,
        result_preview: None,
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
        transcript_tool_call_index: None,
        transcript_tool_call_count: None,
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

fn normalize_json(raw: &str, value: &Value, source_path: Option<&Path>) -> Vec<NormalizedEvent> {
    if value.as_object().is_none() {
        return vec![NormalizedEvent {
            kind: ToolEventKind::Malformed,
            timestamp_raw: Some(raw.to_string()),
            result_summary: Some("non-object json entry".to_string()),
            message: Some(raw.to_string()),
            ..empty_event(raw.to_string())
        }];
    }

    let source_context = SourceContext::from_path(source_path);

    let transcript_events = normalize_transcript_event(raw, value, &source_context);
    if !transcript_events.is_empty() {
        return transcript_events;
    }

    let (timestamp, timestamp_raw) = parse_timestamp(value);
    let level_raw = first_string_from_paths(value, LEVEL_PATHS);
    let level = level_raw
        .as_deref()
        .map(Severity::from_string)
        .unwrap_or(Severity::Unknown);

    let session_key = first_string_from_paths(value, SESSION_PATHS);
    let payload_session_label = first_string_from_paths(value, SESSION_LABEL_PATHS);
    let session_identity = build_session_identity(
        source_context
            .session_id
            .as_deref()
            .map(|value| (value, "path")),
        session_key.as_deref(),
        payload_session_label.as_deref(),
    );
    let routing = derive_routing_metadata(value, session_key.as_deref());

    let agent_id = first_string_from_paths(value, AGENT_PATHS).or(source_context.agent_id.clone());
    let tool_name = first_string_from_paths(value, TOOL_PATHS);

    let correlation_ids = collect_call_ids(value);
    let call_id = correlation_ids.first().cloned();

    let status = extract_status(value);
    let result_summary = extract_result_summary(value);
    let message = first_string_from_paths(value, MESSAGE_PATHS);

    let kind = infer_kind(value, raw, status.as_deref(), &correlation_ids);
    let params = extract_params(value);

    vec![NormalizedEvent {
        kind,
        timestamp,
        timestamp_raw,
        source_path: source_path.map(|path| path.display().to_string()),
        source_kind: source_path.map(|_| "session_log".to_string()),
        session_key,
        session_label: session_identity.session_label,
        session_id: session_identity.session_id,
        session_source: session_identity.session_source,
        session_label_source: session_identity.session_label_source,
        session_identity_conflicts: session_identity.conflicts,
        routing,
        agent_id,
        agent_source: source_context.agent_id.as_ref().map(|_| "path".to_string()),
        tool_name,
        status,
        result_summary: result_summary.clone(),
        result_preview: result_summary,
        result_raw: None,
        result_metrics: Vec::new(),
        exit_code: None,
        duration_ms: None,
        is_error: None,
        call_id,
        call_ids: correlation_ids.clone(),
        correlation_ids,
        message_id: None,
        parent_message_id: None,
        transcript_tool_call_index: None,
        transcript_tool_call_count: None,
        level,
        level_raw,
        params: params.clone(),
        args_preview: params,
        args_raw: None,
        args_truncated: false,
        message,
        raw_line: raw.to_string(),
    }]
}

#[derive(Clone, Debug, Default)]
struct SourceContext {
    agent_id: Option<String>,
    session_id: Option<String>,
}

impl SourceContext {
    fn from_path(path: Option<&Path>) -> Self {
        let Some(path) = path else {
            return Self::default();
        };

        let components = path
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        let agent_id = components
            .iter()
            .position(|component| component == "agents")
            .and_then(|index| components.get(index + 1).cloned());

        let session_id = path
            .file_name()
            .and_then(|value| value.to_str())
            .and_then(session_id_from_file_name);

        Self {
            agent_id,
            session_id,
        }
    }
}

fn session_id_from_file_name(file_name: &str) -> Option<String> {
    if let Some(base) = file_name.strip_suffix(".jsonl") {
        return Some(base.to_string());
    }

    file_name
        .split_once(".jsonl.")
        .map(|(base, _)| base.to_string())
}

fn normalize_transcript_event(
    raw: &str,
    value: &Value,
    source_context: &SourceContext,
) -> Vec<NormalizedEvent> {
    let Some(entry_type) = value.get("type").and_then(Value::as_str) else {
        return Vec::new();
    };
    let (timestamp, timestamp_raw) = parse_timestamp(value);
    let routing = derive_routing_metadata(value, None);
    let session_identity = if entry_type == "session" {
        build_session_identity(
            value
                .get("id")
                .and_then(Value::as_str)
                .map(|value| (value, "transcript"))
                .or_else(|| {
                    source_context
                        .session_id
                        .as_deref()
                        .map(|value| (value, "path"))
                }),
            None,
            None,
        )
    } else {
        build_session_identity(
            source_context
                .session_id
                .as_deref()
                .map(|value| (value, "path")),
            None,
            None,
        )
    };
    let session_id = session_identity.session_id.clone();
    let agent_id = source_context.agent_id.clone();

    match entry_type {
        "session" => vec![NormalizedEvent {
            kind: ToolEventKind::Other,
            timestamp,
            timestamp_raw,
            session_label: session_identity.session_label,
            session_id,
            session_source: session_identity.session_source,
            session_label_source: session_identity.session_label_source,
            session_identity_conflicts: session_identity.conflicts,
            routing,
            agent_id,
            result_summary: Some("session started".to_string()),
            level: Severity::Info,
            level_raw: Some("info".to_string()),
            message: Some("session started".to_string()),
            source_kind: Some("transcript_v3".to_string()),
            message_id: value.get("id").and_then(Value::as_str).map(str::to_string),
            ..empty_event(raw.to_string())
        }],
        "message" => normalize_transcript_message(
            raw,
            value,
            timestamp,
            timestamp_raw,
            session_identity,
            routing,
            agent_id,
        ),
        _ => Vec::new(),
    }
}

fn normalize_transcript_message(
    raw: &str,
    value: &Value,
    timestamp: Option<DateTime<Utc>>,
    timestamp_raw: Option<String>,
    session_identity: SessionIdentityState,
    routing: crate::session_identity::SessionRoutingMetadata,
    agent_id: Option<String>,
) -> Vec<NormalizedEvent> {
    let Some(message) = value.get("message") else {
        return Vec::new();
    };
    let message_id = value.get("id").and_then(Value::as_str).map(str::to_string);
    let parent_message_id = value
        .get("parentId")
        .and_then(Value::as_str)
        .map(str::to_string);
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let content = message
        .get("content")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    if role == "toolResult" || message.get("toolCallId").is_some() {
        let level = transcript_level(message);
        let level_raw = Some(match level {
            Severity::Error => "error".to_string(),
            _ => "info".to_string(),
        });

        return vec![NormalizedEvent {
            kind: ToolEventKind::ToolCallResult,
            timestamp,
            timestamp_raw,
            session_label: session_identity.session_label.clone(),
            session_id: session_identity.session_id.clone(),
            session_source: session_identity.session_source.clone(),
            session_label_source: session_identity.session_label_source.clone(),
            session_identity_conflicts: session_identity.conflicts.clone(),
            routing: routing.clone(),
            agent_id,
            tool_name: first_string_from_paths(message, &[&["toolName"]]),
            status: first_string_from_paths(message, &[&["details", "status"]]).or_else(|| {
                message
                    .get("isError")
                    .and_then(Value::as_bool)
                    .map(|is_error| if is_error { "error" } else { "ok" }.to_string())
            }),
            result_summary: first_text_content(&content)
                .or_else(|| first_string_from_paths(message, &[&["details", "aggregated"]]))
                .or_else(|| first_string_from_paths(message, &[&["details", "status"]])),
            result_preview: first_text_content(&content)
                .or_else(|| first_string_from_paths(message, &[&["details", "aggregated"]]))
                .or_else(|| first_string_from_paths(message, &[&["details", "status"]])),
            call_id: first_string_from_paths(message, &[&["toolCallId"]]),
            call_ids: first_string_from_paths(message, &[&["toolCallId"]])
                .into_iter()
                .collect(),
            correlation_ids: Vec::new(),
            message_id,
            parent_message_id,
            transcript_tool_call_index: None,
            transcript_tool_call_count: None,
            level,
            level_raw,
            params: extract_transcript_result_params(message),
            args_preview: extract_transcript_result_params(message),
            source_kind: Some("transcript_v3".to_string()),
            message: first_string_from_paths(message, &[&["details", "aggregated"]])
                .or_else(|| first_text_content(&content)),
            ..empty_event(raw.to_string())
        }];
    }

    let tool_calls = content
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("toolCall"))
        .collect::<Vec<_>>();

    if !tool_calls.is_empty() {
        let tool_call_count = tool_calls.len();
        return tool_calls
            .iter()
            .enumerate()
            .map(|(index, tool_call)| NormalizedEvent {
                kind: ToolEventKind::ToolCallStart,
                timestamp,
                timestamp_raw: timestamp_raw.clone(),
                session_label: session_identity.session_label.clone(),
                session_id: session_identity.session_id.clone(),
                session_source: session_identity.session_source.clone(),
                session_label_source: session_identity.session_label_source.clone(),
                session_identity_conflicts: session_identity.conflicts.clone(),
                routing: routing.clone(),
                agent_id: agent_id.clone(),
                tool_name: first_string_from_paths(tool_call, &[&["name"]]),
                status: Some("started".to_string()),
                call_id: first_string_from_paths(tool_call, &[&["id"]]),
                call_ids: first_string_from_paths(tool_call, &[&["id"]])
                    .into_iter()
                    .collect(),
                correlation_ids: Vec::new(),
                message_id: message_id.clone(),
                parent_message_id: parent_message_id.clone(),
                transcript_tool_call_index: Some(index),
                transcript_tool_call_count: Some(tool_call_count),
                level: Severity::Info,
                level_raw: Some("info".to_string()),
                params: tool_call
                    .get("arguments")
                    .map(extract_params_from_value)
                    .unwrap_or_default(),
                args_preview: tool_call
                    .get("arguments")
                    .map(extract_params_from_value)
                    .unwrap_or_default(),
                source_kind: Some("transcript_v3".to_string()),
                message: first_string_from_paths(message, &[&["stopReason"]])
                    .or_else(|| first_text_content(&content)),
                ..empty_event(raw.to_string())
            })
            .collect();
    }

    vec![NormalizedEvent {
        kind: ToolEventKind::Other,
        timestamp,
        timestamp_raw,
        session_label: session_identity.session_label,
        session_id: session_identity.session_id,
        session_source: session_identity.session_source,
        session_label_source: session_identity.session_label_source,
        session_identity_conflicts: session_identity.conflicts,
        routing,
        agent_id,
        message_id,
        parent_message_id,
        transcript_tool_call_index: None,
        transcript_tool_call_count: None,
        status: first_string_from_paths(message, &[&["stopReason"]]).or_else(|| {
            if role.is_empty() {
                None
            } else {
                Some(role.to_string())
            }
        }),
        level: Severity::Info,
        level_raw: Some("info".to_string()),
        source_kind: Some("transcript_v3".to_string()),
        message: None,
        ..empty_event(raw.to_string())
    }]
}

fn transcript_level(message: &Value) -> Severity {
    if message
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        Severity::Error
    } else {
        Severity::Info
    }
}

fn first_text_content(content: &[Value]) -> Option<String> {
    content.iter().find_map(|item| {
        if item.get("type").and_then(Value::as_str) == Some("text") {
            item.get("text").and_then(Value::as_str).map(str::to_string)
        } else {
            None
        }
    })
}

fn extract_transcript_result_params(message: &Value) -> Vec<(String, String)> {
    let Some(details) = message.get("details") else {
        return Vec::new();
    };

    let mut params = Vec::new();

    if let Some(status) = details.get("status").and_then(value_to_string) {
        params.push((
            "status".to_string(),
            truncate(&status, MAX_PARAM_VALUE_LENGTH),
        ));
    }

    if let Some(exit_code) = details.get("exitCode").and_then(value_to_string) {
        params.push((
            "exit_code".to_string(),
            truncate(&exit_code, MAX_PARAM_VALUE_LENGTH),
        ));
    }

    if let Some(duration_ms) = details.get("durationMs").and_then(value_to_string) {
        params.push((
            "duration_ms".to_string(),
            truncate(&duration_ms, MAX_PARAM_VALUE_LENGTH),
        ));
    }

    params
}

fn extract_params_from_value(value: &Value) -> Vec<(String, String)> {
    let values = match value {
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
    };

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
    first_value_from_paths(value, PARAM_PATHS)
        .map(extract_params_from_value)
        .unwrap_or_default()
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
    use super::{normalize, normalize_many_with_source, normalize_with_source};
    use std::path::Path;

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

    #[test]
    fn parse_transcript_tool_call_with_source_context() {
        let line = r#"{"type":"message","id":"60167cca","parentId":"1f5ac5f2","timestamp":"2026-03-07T09:31:19.656Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"**Executing startup file reads**"},{"type":"toolCall","id":"call_KF57u6qJpwvTbrDMQnUr0Xcn|fc_09fca533eb0e7ec50169abf0677fa481918b205ee92908fae8","name":"read","arguments":{"file_path":"/home/anders/.openclaw/workspace/SOUL.md"}},{"type":"toolCall","id":"call_FePapL6miWnHFE838k8CvEE8|fc_09fca533eb0e7ec50169abf0677fb881919daea143ac903c9a","name":"read","arguments":{"file_path":"/home/anders/.openclaw/workspace/USER.md"}}],"stopReason":"toolUse","timestamp":1772875879655}}"#;
        let normalized = normalize_many_with_source(
            line,
            Some(Path::new(
                "/home/anders/.openclaw/agents/main/sessions/45b95685-dd1e-417f-9730-162a25f6e1b4.jsonl",
            )),
        );

        assert_eq!(normalized.len(), 2);
        assert!(normalized
            .iter()
            .all(|event| matches!(event.kind, crate::event::ToolEventKind::ToolCallStart)));
        assert_eq!(
            normalized[0].session_id.as_deref(),
            Some("45b95685-dd1e-417f-9730-162a25f6e1b4")
        );
        assert_eq!(normalized[0].agent_id.as_deref(), Some("main"));
        assert_eq!(normalized[0].tool_name.as_deref(), Some("read"));
        assert_eq!(normalized[0].params.len(), 1);
        assert!(normalized
            .iter()
            .all(|event| event.correlation_ids.is_empty()));
        assert_eq!(normalized[0].transcript_tool_call_index, Some(0));
        assert_eq!(normalized[1].transcript_tool_call_index, Some(1));
        assert_eq!(normalized[0].transcript_tool_call_count, Some(2));
    }

    #[test]
    fn parse_transcript_tool_result_with_status_and_metrics() {
        let line = r#"{"type":"message","id":"92a280c2","parentId":"a6814bd6","timestamp":"2026-03-07T09:39:11.230Z","message":{"role":"toolResult","toolCallId":"call_YP6f6I9GcXtQGQs2bvT0uNKB|fc_09fca533eb0e7ec50169abf23e6fc481918da107fce570cb4b","toolName":"exec","content":[{"type":"text","text":"331:anders    143629  132719  0 09:39 ?        00:00:00 /usr/bin/zsh -c ps -ef | rg \\\"agent-harness|ah daemon|harness-webhook|webhook-receiver\\\" -n"}],"details":{"status":"completed","exitCode":0,"durationMs":58,"aggregated":"331:anders    143629  132719  0 09:39 ?        00:00:00 /usr/bin/zsh -c ps -ef | rg \\\"agent-harness|ah daemon|harness-webhook|webhook-receiver\\\" -n","cwd":"/home/anders/.openclaw/workspace"},"isError":false,"timestamp":1772876351207}}"#;
        let normalized = normalize_with_source(
            line,
            Some(Path::new(
                "/home/anders/.openclaw/agents/main/sessions/45b95685-dd1e-417f-9730-162a25f6e1b4.jsonl",
            )),
        );

        assert!(matches!(
            normalized.kind,
            crate::event::ToolEventKind::ToolCallResult
        ));
        assert_eq!(normalized.agent_id.as_deref(), Some("main"));
        assert_eq!(
            normalized.session_id.as_deref(),
            Some("45b95685-dd1e-417f-9730-162a25f6e1b4")
        );
        assert_eq!(normalized.tool_name.as_deref(), Some("exec"));
        assert_eq!(normalized.status.as_deref(), Some("completed"));
        assert_eq!(normalized.params.len(), 3);
        assert!(normalized.result_summary.is_some());
    }

    #[test]
    fn path_session_id_is_not_overridden_by_session_key() {
        let line = r#"{"event":"tool_call_start","timestamp":"2026-03-06T20:00:00Z","session_key":"friendly-session","tool":"search","call_id":"abc","level":"info"}"#;
        let normalized = normalize_with_source(
            line,
            Some(Path::new(
                "/home/anders/.openclaw/agents/main/sessions/45b95685-dd1e-417f-9730-162a25f6e1b4.jsonl",
            )),
        );

        assert_eq!(normalized.session_key.as_deref(), Some("friendly-session"));
        assert_eq!(
            normalized.session_label.as_deref(),
            Some("friendly-session")
        );
        assert_eq!(
            normalized.session_id.as_deref(),
            Some("45b95685-dd1e-417f-9730-162a25f6e1b4")
        );
        assert_eq!(normalized.session_source.as_deref(), Some("path"));
        assert_eq!(normalized.session_label_source.as_deref(), Some("payload"));
        assert_eq!(normalized.session_identity_conflicts.len(), 1);
    }

    #[test]
    fn explicit_session_label_is_separate_from_durable_identity() {
        let line = r#"{"event":"tool_call_start","timestamp":"2026-03-06T20:00:00Z","session_key":"friendly-session","session_label":"Friendly Session","tool":"search","call_id":"abc","level":"info"}"#;
        let normalized = normalize_with_source(
            line,
            Some(Path::new(
                "/home/anders/.openclaw/agents/main/sessions/45b95685-dd1e-417f-9730-162a25f6e1b4.jsonl",
            )),
        );

        assert_eq!(
            normalized.session_id.as_deref(),
            Some("45b95685-dd1e-417f-9730-162a25f6e1b4")
        );
        assert_eq!(normalized.session_key.as_deref(), Some("friendly-session"));
        assert_eq!(
            normalized.session_label.as_deref(),
            Some("Friendly Session")
        );
        assert_eq!(
            normalized.session_label_source.as_deref(),
            Some("payload_label")
        );
    }

    #[test]
    fn discord_routing_is_extracted_from_session_key() {
        let line = r#"{"event":"tool_call_start","timestamp":"2026-03-06T20:00:00Z","session_key":"agent:main:discord:channel:1234567890","tool":"search","call_id":"abc","level":"info"}"#;
        let normalized = normalize(line);

        assert_eq!(normalized.routing.provider.as_deref(), Some("discord"));
        assert_eq!(normalized.routing.channel_id.as_deref(), Some("1234567890"));
        assert_eq!(
            normalized.routing.channel_id_source.as_deref(),
            Some("session_key")
        );
        assert!(normalized.routing.issues.is_empty());
    }

    #[test]
    fn discord_routing_conflicts_are_explicit() {
        let line = r#"{"event":"tool_call_start","timestamp":"2026-03-06T20:00:00Z","session_key":"agent:main:discord:channel:1234567890","metadata":{"channel_id":"9999999999"},"tool":"search","call_id":"abc","level":"info"}"#;
        let normalized = normalize(line);

        assert_eq!(normalized.routing.provider.as_deref(), Some("discord"));
        assert!(normalized.routing.channel_id.is_none());
        assert!(normalized
            .routing
            .issues
            .iter()
            .any(|issue| issue.field == "channel_id"));
    }

    #[test]
    fn transcript_normalization_preserves_discord_routing_metadata() {
        let line = r#"{"type":"message","id":"60167cca","parentId":"1f5ac5f2","timestamp":"2026-03-07T09:31:19.656Z","metadata":{"provider":"discord","channel_id":"1234567890"},"message":{"role":"assistant","content":[{"type":"toolCall","id":"call_a","name":"read","arguments":{"file_path":"/tmp/a"}}],"stopReason":"toolUse","timestamp":1772875879655}}"#;
        let normalized = normalize_with_source(
            line,
            Some(Path::new(
                "/home/anders/.openclaw/agents/main/sessions/45b95685-dd1e-417f-9730-162a25f6e1b4.jsonl",
            )),
        );

        assert_eq!(normalized.routing.provider.as_deref(), Some("discord"));
        assert_eq!(normalized.routing.channel_id.as_deref(), Some("1234567890"));
        assert_eq!(
            normalized.routing.channel_id_source.as_deref(),
            Some("metadata.channel_id")
        );
        assert!(normalized.routing.issues.is_empty());
    }
}
