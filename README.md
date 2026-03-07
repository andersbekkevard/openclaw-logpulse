# openclaw-logpulse

openclaw-logpulse gives you a fast CLI for live OpenClaw log observation focused on tool-call lifecycle visibility.

By default, it now launches a color TUI dashboard for humans. If stdout is not a terminal, it automatically falls back to line-oriented human output so pipes and scripts still behave.

## Install

```bash
# Install globally from source
cd /home/anders/.openclaw/workspace/dev/openclaw-logpulse
cargo install --path .
```

`cargo install` places the binary in `~/.cargo/bin`. If that directory is not already on your shell `PATH`, add it:

```bash
echo 'export PATH="$HOME/.cargo/bin:$PATH"' >> ~/.zshrc
source ~/.zshrc
```

If you want a shorter shell alias:

```bash
echo 'alias logpulse="openclaw-logpulse"' >> ~/.zshrc
source ~/.zshrc
```

If you want `logpulse` as a real command name instead of a shell alias:

```bash
mkdir -p ~/.local/bin
ln -sf ~/.cargo/bin/openclaw-logpulse ~/.local/bin/logpulse
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc
source ~/.zshrc
```

## Update

After pulling repo changes, reinstall the binary globally:

```bash
cd /home/anders/.openclaw/workspace/dev/openclaw-logpulse
git pull
cargo install --path . --force
```

Use `--force` so the installed binary is replaced even when the package version in `Cargo.toml` has not changed.

## Usage

```bash
# Default: launch the interactive TUI (auto-discovers OpenClaw session logs)
openclaw-logpulse

# Follow from beginning of discovered source in the TUI
openclaw-logpulse --from-start

# Filter the TUI by session, agent, and tool
openclaw-logpulse --session "session-12" --agent "agent-7" --tool shell

# Force line-oriented human output
openclaw-logpulse --format human

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
- `--format <tui|human|json>` output format (`tui` is the interactive default for terminals).
- `--since <TIMESTAMP>` emit events at or after this time (`RFC3339` or unix seconds).
- `--until <TIMESTAMP>` emit events at or before this time (`RFC3339` or unix seconds).
- `--stale-seconds N` threshold for stale in-flight calls.
- `--heartbeat-seconds N` heartbeat cadence.
- `--poll-millis N` file poll interval used when following.
- `--from-start` start from file start.
- `--no-follow` read existing content only.

### TUI controls

- `q` quit
- `↑/↓` or `j/k` move through events
- `f` toggle auto-follow of the newest event
- `PgUp/PgDn` scroll the detail pane
- `g` jump to newest event, `G` jump to oldest retained event

The timeline is intentionally compact while the detail pane shows the decoded event metadata plus a pretty-printed raw payload, so you can read things like exec commands, memory queries, and tool results without staring at compressed JSON soup.

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
