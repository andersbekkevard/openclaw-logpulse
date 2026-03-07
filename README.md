# openclaw-logpulse

openclaw-logpulse gives you a fast CLI for live OpenClaw log observation focused on tool-call lifecycle visibility. On an interactive terminal it opens a colored TUI by default; when stdout is piped it falls back to one-line human output.

## Install

```bash
# Install from source
cargo install --path /home/anders/.openclaw/workspace/dev/openclaw-logpulse
```

## Usage

```bash
# Default usage opens the TUI and auto-discovers session logs
openclaw-logpulse

# Force one-line human output
openclaw-logpulse --format human

# Follow from beginning of discovered source
openclaw-logpulse --from-start

# Human output filtered by session, agent, and tool
openclaw-logpulse --session "session-12" --agent "agent-7" --tool shell

# JSON output for machine ingestion
openclaw-logpulse --format json

# One-line with explicit log file path
openclaw-logpulse /var/log/openclaw.log

# Optional time window + heartbeat/stale controls
openclaw-logpulse --since 2026-01-01T12:00:00Z --until 2026-01-02T12:00:00Z \
  --stale-seconds 20 --heartbeat-seconds 5

# One-shot parse (no follow)
openclaw-logpulse --no-follow
```

### Arguments

- `[LOG_FILE]` optional path to OpenClaw log file. Omit for auto-discovery.
- `--session <SUBSTRING>` show only events matching session key substring.
- `--tool <NAME>` filter by tool substring (case-insensitive).
- `--agent <SUBSTRING>` filter by agent id substring (case-insensitive).
- `--min-level <trace|debug|info|warn|error|fatal>` minimum level filter.
- `--format <tui|human|json>` output format. `tui` is the default and automatically falls back to `human` when stdout is not a terminal.
- `--since <TIMESTAMP>` emit events at or after this time (`RFC3339` or unix seconds).
- `--until <TIMESTAMP>` emit events at or before this time (`RFC3339` or unix seconds).
- `--stale-seconds N` threshold for stale in-flight calls.
- `--heartbeat-seconds N` heartbeat cadence.
- `--poll-millis N` file poll interval used when following.
- `--from-start` start from file start.
- `--no-follow` read existing content only.

Auto-discovery checks these sources, in order:

- `OPENCLAW_LOG_FILE`
- `OPENCLAW_LOG_PATH`
- `OPENCLAW_LOG_DIR/openclaw.log`
- `OPENCLAW_LOG_DIR/logs/openclaw.log`
- `OPENCLAW_HOME/openclaw.log`
- `OPENCLAW_HOME/logs/openclaw.log`
- `OPENCLAW_BASE_DIR/openclaw.log`
- `OPENCLAW_BASE_DIR/logs/openclaw.log`
- `~/.openclaw/openclaw.log`
- `~/.openclaw/logs/openclaw.log`
- `~/.cache/openclaw/openclaw.log`
- `/var/log/openclaw.log`
- `/var/log/openclaw/openclaw.log`

## Troubleshooting

- If auto-discovery fails, pass a log path directly and confirm permissions: `openclaw-logpulse <LOG_FILE>`.
- If malformed lines dominate, check that the log source format changed; non-JSON lines are preserved as MALFORMED events.
- If stale warnings flood, verify tool call completion markers are present in the log (`...tool_call_result...` or structured result fields).
- If heartbeat never appears, verify events are being emitted and/or inspect no-follow mode behavior.
