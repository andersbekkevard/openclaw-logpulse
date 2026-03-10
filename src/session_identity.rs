use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SessionIdentityConflict {
    pub field: String,
    pub preferred_source: String,
    pub preferred_value: String,
    pub conflicting_source: String,
    pub conflicting_value: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RoutingIssueKind {
    Missing,
    Conflict,
    Malformed,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SessionRoutingIssue {
    pub kind: RoutingIssueKind,
    pub field: String,
    pub detail: String,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct SessionRoutingMetadata {
    pub provider: Option<String>,
    pub provider_source: Option<String>,
    pub channel_id: Option<String>,
    pub channel_id_source: Option<String>,
    pub issues: Vec<SessionRoutingIssue>,
}

impl SessionRoutingMetadata {
    pub fn is_discord(&self) -> bool {
        self.provider.as_deref() == Some("discord")
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionIdentityState {
    pub session_id: Option<String>,
    pub session_source: Option<String>,
    pub session_label: Option<String>,
    pub session_label_source: Option<String>,
    pub conflicts: Vec<SessionIdentityConflict>,
}

pub fn build_session_identity(
    preferred_identity: Option<(&str, &str)>,
    payload_session_key: Option<&str>,
    payload_session_label: Option<&str>,
) -> SessionIdentityState {
    let mut conflicts = Vec::new();

    let (session_id, session_source) = match preferred_identity {
        Some((value, source)) => (Some(value.to_string()), Some(source.to_string())),
        None => (
            payload_session_key.map(str::to_string),
            payload_session_key.map(|_| "payload".to_string()),
        ),
    };

    if let (Some((preferred_value, preferred_source)), Some(payload_value)) =
        (preferred_identity, payload_session_key)
    {
        if preferred_value != payload_value {
            conflicts.push(SessionIdentityConflict {
                field: "session_id".to_string(),
                preferred_source: preferred_source.to_string(),
                preferred_value: preferred_value.to_string(),
                conflicting_source: "payload".to_string(),
                conflicting_value: payload_value.to_string(),
            });
        }
    }

    let (session_label, session_label_source) = if let Some(label) = payload_session_label {
        (Some(label.to_string()), Some("payload_label".to_string()))
    } else if let Some(label) = payload_session_key {
        (Some(label.to_string()), Some("payload".to_string()))
    } else {
        (session_id.clone(), session_source.clone())
    };

    SessionIdentityState {
        session_id,
        session_source,
        session_label,
        session_label_source,
        conflicts,
    }
}

pub fn derive_routing_metadata(value: &Value, session_key: Option<&str>) -> SessionRoutingMetadata {
    let mut issues = Vec::new();
    let mut provider_candidates = collect_candidates(value, PROVIDER_PATHS, false);
    let mut channel_candidates = collect_candidates(value, CHANNEL_ID_PATHS, true);
    collect_transcript_discord_candidates(
        value,
        &mut provider_candidates,
        &mut channel_candidates,
        &mut issues,
    );

    if let Some(session_key) = session_key {
        if session_key_mentions_discord(session_key) {
            provider_candidates.push(Candidate::new("discord", "session_key"));
        }
        if let Some(channel_id) = extract_discord_channel_from_session_key(session_key, &mut issues)
        {
            channel_candidates.push(Candidate::new(channel_id, "session_key"));
        }
    }

    let provider = select_candidate("provider", provider_candidates, &mut issues);
    let channel = select_candidate("channel_id", channel_candidates, &mut issues);

    let mut routing = SessionRoutingMetadata {
        provider: provider.as_ref().map(|candidate| candidate.value.clone()),
        provider_source: provider.as_ref().map(|candidate| candidate.source.clone()),
        channel_id: channel.as_ref().map(|candidate| candidate.value.clone()),
        channel_id_source: channel.as_ref().map(|candidate| candidate.source.clone()),
        issues,
    };

    if routing.is_discord() && routing.channel_id.is_none() {
        routing.issues.push(SessionRoutingIssue {
            kind: RoutingIssueKind::Missing,
            field: "channel_id".to_string(),
            detail: "discord routing metadata did not include a usable channel id".to_string(),
        });
    }

    routing
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn shorten_non_discord_session_label(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    if looks_like_uuid(trimmed) {
        return trimmed.chars().take(8).collect();
    }

    if trimmed.contains(':') {
        let parts = trimmed
            .split(':')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        if parts.len() >= 2 {
            let candidate = parts[parts.len().saturating_sub(2)..].join(":");
            if candidate.chars().count() <= 24 {
                return candidate;
            }
            return compact_head_tail(&candidate, 12, 6);
        }
    }

    if trimmed.chars().count() <= 16 {
        return trimmed.to_string();
    }

    compact_head_tail(trimmed, 8, 4)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Candidate {
    value: String,
    source: String,
}

impl Candidate {
    fn new(value: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            source: source.into(),
        }
    }
}

const PROVIDER_PATHS: &[(&[&str], &str)] = &[
    (&["provider"], "provider"),
    (&["transport"], "transport"),
    (&["routing", "provider"], "routing.provider"),
    (&["routing", "transport"], "routing.transport"),
    (&["metadata", "provider"], "metadata.provider"),
    (&["metadata", "transport"], "metadata.transport"),
    (
        &["metadata", "routing", "provider"],
        "metadata.routing.provider",
    ),
    (
        &["metadata", "routing", "transport"],
        "metadata.routing.transport",
    ),
    (&["context", "provider"], "context.provider"),
    (&["context", "transport"], "context.transport"),
    (&["payload", "provider"], "payload.provider"),
    (&["payload", "transport"], "payload.transport"),
    (&["session", "provider"], "session.provider"),
];

const CHANNEL_ID_PATHS: &[(&[&str], &str)] = &[
    (&["channel_id"], "channel_id"),
    (&["channelId"], "channelId"),
    (&["discord_channel_id"], "discord_channel_id"),
    (&["discordChannelId"], "discordChannelId"),
    (&["routing", "channel_id"], "routing.channel_id"),
    (&["routing", "channelId"], "routing.channelId"),
    (&["metadata", "channel_id"], "metadata.channel_id"),
    (&["metadata", "channelId"], "metadata.channelId"),
    (
        &["metadata", "discord_channel_id"],
        "metadata.discord_channel_id",
    ),
    (
        &["metadata", "discordChannelId"],
        "metadata.discordChannelId",
    ),
    (
        &["metadata", "discord", "channel_id"],
        "metadata.discord.channel_id",
    ),
    (
        &["metadata", "discord", "channelId"],
        "metadata.discord.channelId",
    ),
    (&["context", "channel_id"], "context.channel_id"),
    (&["context", "channelId"], "context.channelId"),
    (
        &["context", "discord_channel_id"],
        "context.discord_channel_id",
    ),
    (&["context", "discordChannelId"], "context.discordChannelId"),
    (&["payload", "channel_id"], "payload.channel_id"),
    (&["payload", "channelId"], "payload.channelId"),
    (
        &["payload", "discord_channel_id"],
        "payload.discord_channel_id",
    ),
    (&["payload", "discordChannelId"], "payload.discordChannelId"),
    (&["session", "channel_id"], "session.channel_id"),
    (&["session", "channelId"], "session.channelId"),
];

fn collect_candidates(
    value: &Value,
    specs: &[(&[&str], &str)],
    require_discord_channel_id: bool,
) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    for (path, source) in specs {
        let Some(raw) = get_value_by_path(value, path).and_then(value_to_string) else {
            continue;
        };
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }

        if require_discord_channel_id {
            if is_valid_discord_channel_id(raw) {
                candidates.push(Candidate::new(raw, *source));
            }
        } else {
            candidates.push(Candidate::new(raw.to_ascii_lowercase(), *source));
        }
    }
    candidates
}

fn select_candidate(
    field: &str,
    candidates: Vec<Candidate>,
    issues: &mut Vec<SessionRoutingIssue>,
) -> Option<Candidate> {
    let mut unique_values = BTreeSet::new();
    for candidate in &candidates {
        unique_values.insert(candidate.value.clone());
    }

    if unique_values.len() <= 1 {
        return candidates.into_iter().next();
    }

    let detail = candidates
        .iter()
        .map(|candidate| format!("{}={}", candidate.source, candidate.value))
        .collect::<Vec<_>>()
        .join(", ");
    issues.push(SessionRoutingIssue {
        kind: RoutingIssueKind::Conflict,
        field: field.to_string(),
        detail,
    });
    None
}

fn collect_transcript_discord_candidates(
    value: &Value,
    provider_candidates: &mut Vec<Candidate>,
    channel_candidates: &mut Vec<Candidate>,
    issues: &mut Vec<SessionRoutingIssue>,
) {
    let Some(message) = value.get("message") else {
        return;
    };

    if let Some(content) = message.get("content").and_then(Value::as_array) {
        for (idx, item) in content.iter().enumerate() {
            if item.get("type").and_then(Value::as_str) != Some("toolCall") {
                continue;
            }
            if item.get("name").and_then(Value::as_str) != Some("message") {
                continue;
            }

            let Some(arguments) = item.get("arguments") else {
                continue;
            };
            let Some(channel) = arguments.get("channel").and_then(value_to_string) else {
                continue;
            };
            if !channel.eq_ignore_ascii_case("discord") {
                continue;
            }

            let source_prefix = format!("message.content[{idx}].arguments");
            provider_candidates.push(Candidate::new(
                "discord",
                format!("{source_prefix}.channel"),
            ));
            collect_channel_candidate(
                arguments.get("target"),
                &format!("{source_prefix}.target"),
                channel_candidates,
                issues,
            );
        }
    }

    if message.get("toolName").and_then(Value::as_str) == Some("message") {
        if let Some(channel_id) = get_value_by_path(message, &["details", "result", "channelId"]) {
            let source = "message.details.result.channelId";
            provider_candidates.push(Candidate::new("discord", source));
            collect_channel_candidate(Some(channel_id), source, channel_candidates, issues);
        } else if let Some(channel_id) =
            get_value_by_path(message, &["details", "result", "channel_id"])
        {
            let source = "message.details.result.channel_id";
            provider_candidates.push(Candidate::new("discord", source));
            collect_channel_candidate(Some(channel_id), source, channel_candidates, issues);
        }
    }
}

fn collect_channel_candidate(
    raw_value: Option<&Value>,
    source: &str,
    channel_candidates: &mut Vec<Candidate>,
    issues: &mut Vec<SessionRoutingIssue>,
) {
    let Some(raw) = raw_value.and_then(value_to_string) else {
        return;
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return;
    }

    if is_valid_discord_channel_id(raw) {
        channel_candidates.push(Candidate::new(raw, source));
        return;
    }

    issues.push(SessionRoutingIssue {
        kind: RoutingIssueKind::Malformed,
        field: "channel_id".to_string(),
        detail: format!("{source} is not a numeric snowflake: {raw}"),
    });
}

fn session_key_mentions_discord(session_key: &str) -> bool {
    session_key
        .split(':')
        .any(|segment| segment.eq_ignore_ascii_case("discord"))
}

fn extract_discord_channel_from_session_key(
    session_key: &str,
    issues: &mut Vec<SessionRoutingIssue>,
) -> Option<String> {
    let parts = session_key.split(':').collect::<Vec<_>>();
    let mut idx = 0;
    while idx < parts.len() {
        if !parts[idx].eq_ignore_ascii_case("discord") {
            idx += 1;
            continue;
        }

        if parts
            .get(idx + 1)
            .map(|segment| segment.eq_ignore_ascii_case("channel"))
            != Some(true)
        {
            return None;
        }

        let Some(channel_id) = parts.get(idx + 2).map(|value| value.trim()) else {
            return None;
        };

        if is_valid_discord_channel_id(channel_id) {
            return Some(channel_id.to_string());
        }

        issues.push(SessionRoutingIssue {
            kind: RoutingIssueKind::Malformed,
            field: "channel_id".to_string(),
            detail: format!(
                "session_key discord channel id is not a numeric snowflake: {channel_id}"
            ),
        });
        return None;
    }

    None
}

#[cfg_attr(not(test), allow(dead_code))]
fn looks_like_uuid(value: &str) -> bool {
    let parts = value.split('-').collect::<Vec<_>>();
    let expected_lengths = [8, 4, 4, 4, 12];
    if parts.len() != expected_lengths.len() {
        return false;
    }

    parts
        .iter()
        .zip(expected_lengths)
        .all(|(part, len)| part.len() == len && part.chars().all(|ch| ch.is_ascii_hexdigit()))
}

#[cfg_attr(not(test), allow(dead_code))]
fn compact_head_tail(value: &str, head: usize, tail: usize) -> String {
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

fn is_valid_discord_channel_id(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|ch| ch.is_ascii_digit())
}

fn get_value_by_path<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_session_identity, derive_routing_metadata, shorten_non_discord_session_label,
        RoutingIssueKind,
    };
    use serde_json::json;

    #[test]
    fn prefers_durable_identity_and_keeps_payload_label() {
        let identity = build_session_identity(
            Some(("session-path", "path")),
            Some("friendly-session"),
            Some("Friendly Session"),
        );

        assert_eq!(identity.session_id.as_deref(), Some("session-path"));
        assert_eq!(identity.session_source.as_deref(), Some("path"));
        assert_eq!(identity.session_label.as_deref(), Some("Friendly Session"));
        assert_eq!(
            identity.session_label_source.as_deref(),
            Some("payload_label")
        );
        assert_eq!(identity.conflicts.len(), 1);
    }

    #[test]
    fn derives_discord_channel_from_session_key() {
        let routing = derive_routing_metadata(
            &json!({"session_key": "agent:main:discord:channel:1234567890"}),
            Some("agent:main:discord:channel:1234567890"),
        );

        assert_eq!(routing.provider.as_deref(), Some("discord"));
        assert_eq!(routing.channel_id.as_deref(), Some("1234567890"));
        assert_eq!(routing.channel_id_source.as_deref(), Some("session_key"));
        assert!(routing.issues.is_empty());
    }

    #[test]
    fn makes_missing_discord_channel_explicit() {
        let routing = derive_routing_metadata(&json!({"metadata": {"provider": "discord"}}), None);

        assert_eq!(routing.provider.as_deref(), Some("discord"));
        assert!(routing.channel_id.is_none());
        assert!(routing
            .issues
            .iter()
            .any(|issue| issue.kind == RoutingIssueKind::Missing && issue.field == "channel_id"));
    }

    #[test]
    fn bare_numeric_channel_id_does_not_imply_discord_provider() {
        let routing =
            derive_routing_metadata(&json!({"metadata": {"channel_id": "1234567890"}}), None);

        assert!(routing.provider.is_none());
        assert_eq!(routing.channel_id.as_deref(), Some("1234567890"));
        assert!(routing.issues.is_empty());
    }

    #[test]
    fn derives_discord_routing_from_message_tool_call_arguments() {
        let routing = derive_routing_metadata(
            &json!({
                "type": "message",
                "message": {
                    "role": "assistant",
                    "content": [{
                        "type": "toolCall",
                        "name": "message",
                        "arguments": {
                            "action": "send",
                            "channel": "discord",
                            "target": "123456789012345678"
                        }
                    }]
                }
            }),
            None,
        );

        assert_eq!(routing.provider.as_deref(), Some("discord"));
        assert_eq!(routing.channel_id.as_deref(), Some("123456789012345678"));
        assert_eq!(
            routing.provider_source.as_deref(),
            Some("message.content[0].arguments.channel")
        );
        assert_eq!(
            routing.channel_id_source.as_deref(),
            Some("message.content[0].arguments.target")
        );
        assert!(routing.issues.is_empty());
    }

    #[test]
    fn derives_discord_routing_from_message_tool_result_payload() {
        let routing = derive_routing_metadata(
            &json!({
                "type": "message",
                "message": {
                    "role": "toolResult",
                    "toolName": "message",
                    "details": {
                        "result": {
                            "channelId": "123456789012345678"
                        }
                    }
                }
            }),
            None,
        );

        assert_eq!(routing.provider.as_deref(), Some("discord"));
        assert_eq!(routing.channel_id.as_deref(), Some("123456789012345678"));
        assert_eq!(
            routing.provider_source.as_deref(),
            Some("message.details.result.channelId")
        );
        assert_eq!(
            routing.channel_id_source.as_deref(),
            Some("message.details.result.channelId")
        );
        assert!(routing.issues.is_empty());
    }

    #[test]
    fn shortens_non_discord_uuid_labels() {
        assert_eq!(
            shorten_non_discord_session_label("45b95685-dd1e-417f-9730-162a25f6e1b4"),
            "45b95685"
        );
    }

    #[test]
    fn shortens_non_discord_colon_delimited_labels() {
        assert_eq!(
            shorten_non_discord_session_label("agent:main:workspace:session-42"),
            "workspace:session-42"
        );
    }

    #[test]
    fn shortens_long_opaque_labels_with_head_tail_compaction() {
        assert_eq!(
            shorten_non_discord_session_label("abcdefghijklmnopqrstuvwx"),
            "abcdefgh…uvwx"
        );
    }
}
