# openclaw-logpulse

openclaw-logpulse gives you a fast CLI for live OpenClaw log observation focused on tool-call lifecycle visibility.

## Install

```bash
cargo install --path /home/anders/.openclaw/workspace/dev/openclaw-logpulse
```

## Usage

```bash
# Follow the newest log content from file end
openclaw-logpulse /var/log/openclaw.log

# Follow from beginning
openclaw-logpulse --from-start /var/log/openclaw.log

# Human output filtered by session substring and tool
openclaw-logpulse --session "session-12" --tool shell /var/log/openclaw.log

# JSON output for machine ingestion
openclaw-logpulse --format json /var/log/openclaw.log

# Show stale warnings after 20 seconds and print heartbeat every 5 seconds
openclaw-logpulse --stale-seconds 20 --heartbeat-seconds 5 /var/log/openclaw.log

# One-shot parse (no follow)
openclaw-logpulse --no-follow /var/log/openclaw.log
```

### Arguments

- `<LOG_FILE>` path to OpenClaw log file.
- `--session <SUBSTRING>` show only events matching session key substring.
- `--tool <NAME>` filter by tool substring (case-insensitive).
- `--min-level <trace|debug|info|warn|error|fatal>` minimum level filter.
- `--format <human|json>` output format.
- `--stale-seconds N` threshold for stale in-flight calls.
- `--heartbeat-seconds N` heartbeat cadence.
- `--poll-millis N` file poll interval used when following.
- `--from-start` start from file start.
- `--no-follow` read existing content only.

## Troubleshooting

- If you only see a blank stream, confirm the file path and permissions.
- If malformed lines dominate, check that the log source format changed; non-JSON lines are preserved as MALFORMED events.
- If stale warnings flood, verify tool call completion markers are present in the log (`...tool_call_result...` or structured result fields).
- If heartbeat never appears, verify events are being emitted and/or inspect no-follow mode behavior.
