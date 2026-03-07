# Wave 1 CLI UX Spec: One-line global OpenClaw action stream

## 1) Command surface and defaults

### Goal
Provide a zero-argument default workflow for common on-call and ops usage while preserving explicit control for advanced filtering.

### Top-level command

```bash
openclaw-logpulse [OPTIONS] [LOG_PATH]
```

### Zero-arg behavior

When `LOG_PATH` is omitted, default log source resolution is:

1. `OPENCLAW_LOG_PATH` env var if set and readable
2. `/var/log/openclaw/openclaw.log`
3. `$HOME/.openclaw/openclaw.log`

If none are available, print a concise diagnostic and exit with usage help plus `exit code 2`.

### Default mode

- `--follow` enabled (tail-like stream)
- `--from-start` false
- `--format` `human`
- `--time-format` relative (`1m32s ago`)
- `--severity` default `info`
- `--heartbeat` `10s`
- `--stale-seconds` `60s`
- `--poll` `250ms`
- `--stats` off
- `--no-color` false (human output may use color; deterministic JSON unaffected)

### Help synopsis

```text
USAGE:
  openclaw-logpulse [OPTIONS] [LOG_PATH]

OPTIONS:
  --from-start                 Read from beginning of file instead of tail position
  --no-follow                  One-shot read only (no follow)
  --poll <duration>            Poll interval when following (default 250ms)
  --session <substring>        Filter by session id contains
  --agent <substring>          Filter by agent name/id contains
  --tool <substring>           Filter by tool name contains
  --severity <level>[..<level>] Level or range filter. Example: warn, info..error
  --severity <list>            Comma list of severities
  --severity-glob <pattern>     Glob match on severity labels
  --since <time>               Lower bound timestamp (RFC3339, epoch, or -5m)
  --until <time>               Upper bound timestamp (RFC3339, epoch, or +5m)
  --window <duration>           Relative window from now (e.g., 24h)
  --format <human|json|json-pretty>
  --output-separator <char>     Separator for human mode (default: space)
  --stats                      Emit running metrics every heartbeat
  --heartbeat <duration>        Interval for heartbeat + stats; default 10s, 0 disables
  --stale-seconds <duration>   Warn if action still running beyond this threshold
  --stale-once                  Emit one stale warning per action (default)
  --stale-interval <duration>  Re-emit stale warnings every N while still open
  --max-stale-warning-count N   Cap repeated stale warnings per action
  --json-ns-seconds             Include ns precision timestamps in JSON output
  --no-color                    Disable ANSI styling in human mode
  -h, --help                   Show help and exit
```

---

## 2) Filter semantics

### Session filter

- `--session` accepts case-insensitive substring by default.
- Multiple flags may be repeated.
- Multiple values are ORed within one kind (match if any token matches).

### Agent filter

- `--agent` matches normalized agent identifier fields:
  - `agent.id`
  - `agent.name`
- Substring match is case-insensitive.

### Tool filter

- `--tool` matches tool identifiers, including dotted and alias names.
- Matching is case-insensitive substring.

### Severity filter

Supported canonical severities: `trace`, `debug`, `info`, `warn`, `error`, `fatal`.

Accepted forms:

- Single level: `--severity error`
- Inclusive range: `--severity info..error`
- List: `--severity warn,error`
- Multiple flags: `--severity warn --severity error`

If parser cannot map a raw severity token, event is included only when `--severity-glob '*'` or explicit passthrough mode is enabled (future flag).

### Time window filter

Any one of:

- `--since` (`--since 10m`, `--since 2026-03-05T10:30:00Z`, `--since 1710000000`)
- `--until` (`--until now`, `--until -1h`)
- `--window` (`--window 15m`) as shorthand for `--since -15m` when reading from tail mode

Window filters apply after severity/session/agent/tool filters.

---

## 3) Stale detection behavior

### Options

- `--stale-seconds` (default `60s`): action stale threshold.
- `--stale-interval` (default `30s`): re-alert cadence while stale.
- `--max-stale-warning-count` (default `3`): `0` = unlimited, `1` = only first, etc.

### Behavior

- Track currently in-flight actions (`tool_call` started but no matching completion).
- Emit a synthetic line/event at `max(stale-seconds, heartbeat?)` cadence.
- Stale event includes:
  - session identifier
  - agent
  - tool
  - age
  - state (`running`)
  - start timestamp
- Stale output can be suppressed with `--stale-seconds 0` (explicitly disables stale checks).

---

## 4) Stats behavior

### `--stats`

When set, stream periodic metrics at every `--heartbeat`:

- currently open actions
- matched events/s
- dropped events (invalid JSON / malformed)
- stale actions count
- age of oldest running action
- ingest lag estimate (if tail offset is measurable)

Stats are printed in human mode as a gray summary line; in JSON mode they are emitted as `type: "stats"` events.

`--heartbeat 0` with `--stats` prints one final stats line at exit after no-follow completion.

---

## 5) Human output format

### Single event line (default)

```text
[TIME] [SEV] [SESSION] [AGENT] [TOOL] MESSAGE
```

### Example

```text
12:04:12 [INFO] [sess:9b1a2c] [agent:coder-1] [tool:shell.exec] started tool_call=tsk-7
```

### Human fields

- `TIME`: local timestamp or relative if `--time-format=relative`
- `SEV`: severity abbreviation `TRC/DBG/INF/WAR/ERR/FAT`
- `SESSION`: `session:<id>` if present
- `AGENT`: `agent:<name>` if present
- `TOOL`: `tool:<name>` if present
- `MESSAGE`: canonicalized action summary

### Additional tokens for lifecycle events

- `duration=<ms>` shown on completion
- `correlation=<id>` from `action_id` when present
- `latency_p95` included only in stats lines
- `stale=AGE` for stale warnings

---

## 6) JSON event schema

### Human-readable schema overview

All JSON events are newline-delimited objects (`json` mode). Schema supports machine ingestion and stable downstream parsing.

```json
{
  "schema_version": "1.0",
  "event_kind": "action|heartbeat|stale|stats|error|malformed",
  "ts": "2026-03-06T12:04:12.123456Z",
  "ts_ns": 1710000000000000000,
  "severity": "info|warn|error|...",
  "session": "optional",
  "agent": "optional",
  "tool": "optional",
  "action_id": "optional",
  "message": "human summary string",
  "details": {},
  "duration_ms": 0,
  "metadata": {}
}
```

### JSON per kind

`action`

- `event_type` lifecycle: `started|progress|completed|failed|cancelled`
- Includes `correlation_id`, `input_summary`, `output_summary` when known

`stale`

- `event_type`: `stale`
- `age_ms`, `threshold_ms`, `last_seen_ts`, `stale_count`

`heartbeat`

- `event_type`: `heartbeat`
- `poll_count`, `runtime_ms`

`stats`

- `event_type`: `stats`
- `metrics`: object including:
  - `throughput_events_per_sec`
  - `matched_events_total`
  - `dropped_events_total`
  - `open_actions_total`
  - `stale_actions_total`

`error`

- `error_code`, `error_message`, optional `fatal` boolean

`malformed`

- `raw_line`, `parse_error`, optional `line_no`

### JSON output ordering

When streaming, objects are emitted in event time order with heartbeat/stats after all event emission for that interval.

---

## 7) Exit and status behavior

| Condition | Exit code | Behavior |
| --- | --- | --- |
| Success, finite one-shot consumed all matching events | `0` | returns after EOF (or timeout not set) |
| Success, follow mode stopped by user (SIGINT/SIGTERM) | `0` | prints final stats when enabled |
| Invalid CLI args or unreadable log source | `2` | prints short usage and diagnostic |
| Runtime parsing/IO error (recoverable) | `3` | stream pauses/retries if follow; no-follow returns immediately |
| Internal unexpected failure | `4` | single `error` JSON or stderr message; abort stream |
| Stale alerts detected only | `0` | stale events do not alter exit code |

`SIGPIPE` handling: suppress stack traces and exit `0`.

---

## 8) Operational examples

### Follow global stream with zero args (default log path)

```bash
openclaw-logpulse
```

### Follow from standard start with strict severity filter

```bash
openclaw-logpulse --from-start --severity error --format human
```

### Track one session and one tool for last 20m only

```bash
openclaw-logpulse --session 9b1a2c --tool shell.exec --window 20m
```

### JSON for SIEM ingestion with one-off parse

```bash
openclaw-logpulse --no-follow --format json --since 2026-03-06T00:00:00Z /var/log/openclaw/openclaw.log
```

### Stale action monitoring with stats heartbeat

```bash
openclaw-logpulse --stale-seconds 45 --stale-interval 15 --heartbeat 10 --stats
```

### Multi-filter command for on-call triage

```bash
openclaw-logpulse \
  --session checkout \
  --agent agent:worker-3 \
  --tool kubernetes \
  --severity warn,error \
  --since -15m \
  --stats
```

### One-shot health slice (past hour), JSON pretty for review

```bash
openclaw-logpulse --no-follow --window 1h --json-pretty
```
