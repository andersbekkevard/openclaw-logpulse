use crate::event::NormalizedEvent;
use crate::stale::{HeartbeatSummary, StaleWarning};
use chrono::Utc;
use clap::ValueEnum;
use serde::Serialize;
use std::io::{self, IsTerminal, Write};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ValueEnum)]
pub enum OutputMode {
    Tui,
    Human,
    Json,
    Tui,
}

pub fn effective_mode(mode: OutputMode) -> OutputMode {
    if mode == OutputMode::Tui && !io::stdout().is_terminal() {
        OutputMode::Human
    } else {
        mode
    }
}

impl OutputMode {
    pub fn effective(self) -> Self {
        match self {
            OutputMode::Tui if !io::stdout().is_terminal() => OutputMode::Human,
            mode => mode,
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum OutputRecord<'a> {
    ToolEvent {
        event: &'a NormalizedEvent,
    },
    StaleWarning {
        call_id: &'a String,
        session_key: Option<&'a String>,
        tool_name: Option<&'a String>,
        age_seconds: u64,
        message: Option<&'a String>,
    },
    Heartbeat {
        active_calls: usize,
        stale_calls: usize,
        active_sessions: usize,
    },
}

pub fn emit_tool_event(
    event: &NormalizedEvent,
    mode: OutputMode,
    out: &mut impl Write,
) -> io::Result<()> {
    match mode {
        OutputMode::Tui => emit_tool_event(event, OutputMode::Human, out),
        OutputMode::Json => {
            let record = OutputRecord::ToolEvent { event };
            writeln!(
                out,
                "{}",
                serde_json::to_string(&record).unwrap_or_else(|_| "{}".to_string())
            )
        }
        OutputMode::Human => {
            let ts = event.timestamp.unwrap_or_else(Utc::now).to_rfc3339();
            let session = event
                .session_key
                .as_ref()
                .or(event.session_id.as_ref())
                .map_or("-", |value| value.as_str());
            let agent = event.agent_id.as_ref().map_or("-", |value| value.as_str());
            let tool = event.tool_name.as_ref().map_or("-", |value| value.as_str());
            let status = event
                .status
                .as_ref()
                .or(event.result_summary.as_ref())
                .map_or("-", |value| value.as_str());

            let params = if event.params.is_empty() {
                "-".to_string()
            } else {
                event
                    .params
                    .iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };

            writeln!(
                out,
                "[{ts}] {} session={session} agent={agent} tool={tool} status={status} params=[{params}]",
                event.kind.as_str(),
            )
        }
        OutputMode::Tui => Ok(()),
    }
}

pub fn emit_stale_warning(
    warning: &StaleWarning,
    mode: OutputMode,
    out: &mut impl Write,
) -> io::Result<()> {
    match mode {
        OutputMode::Tui => emit_stale_warning(warning, OutputMode::Human, out),
        OutputMode::Json => {
            let record = OutputRecord::StaleWarning {
                call_id: &warning.call_id,
                session_key: warning.session_key.as_ref(),
                tool_name: warning.tool_name.as_ref(),
                age_seconds: warning.age_seconds,
                message: warning.message.as_ref(),
            };
            writeln!(
                out,
                "{}",
                serde_json::to_string(&record).unwrap_or_else(|_| "{}".to_string())
            )
        }
        OutputMode::Human => {
            let session = warning.session_key.as_deref().unwrap_or("-");
            let tool = warning.tool_name.as_deref().unwrap_or("-");

            writeln!(
                out,
                "[WARN] stale call_id={} session={} tool={} age={}s",
                warning.call_id, session, tool, warning.age_seconds
            )
        }
        OutputMode::Tui => Ok(()),
    }
}

pub fn emit_heartbeat(
    summary: &HeartbeatSummary,
    mode: OutputMode,
    out: &mut impl Write,
) -> io::Result<()> {
    match mode {
        OutputMode::Tui => emit_heartbeat(summary, OutputMode::Human, out),
        OutputMode::Json => {
            let record = OutputRecord::Heartbeat {
                active_calls: summary.active_calls,
                stale_calls: summary.stale_calls,
                active_sessions: summary.active_sessions,
            };
            writeln!(
                out,
                "{}",
                serde_json::to_string(&record).unwrap_or_else(|_| "{}".to_string())
            )
        }
        OutputMode::Human => writeln!(out, "[HB] {}", summary.to_line()),
        OutputMode::Tui => Ok(()),
    }
}

impl crate::event::ToolEventKind {
    fn as_str(&self) -> &'static str {
        match self {
            crate::event::ToolEventKind::ToolCallStart => "START",
            crate::event::ToolEventKind::ToolCallResult => "RESULT",
            crate::event::ToolEventKind::ToolCall => "CALL",
            crate::event::ToolEventKind::Other => "OTHER",
            crate::event::ToolEventKind::Malformed => "MALFORMED",
        }
    }
}
