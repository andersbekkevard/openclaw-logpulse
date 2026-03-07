# OpenClaw Event Schema (Tool-call Lifecycle, wave1)

Scope:
- Repository: `/home/anders/.openclaw/workspace/dev/openclaw-logpulse`
- Sample source: `~/.openclaw/agents/*/sessions/*.jsonl`
- Practical sampling achieved from `~/.openclaw/agents/main/sessions/*.jsonl` (40+ files total in current workspace snapshot)
- Objective: infer real event lifecycle fields for tool calls and tool results from sessions JSONL

## 1) Observed top-level event families

Real events in session exports include:
- `message`
- `custom`
- `session`
- `thinking_level_change`
- `model_change`
- `compaction`

Tool-call lifecycle is represented through `message` events.

## 2) Tool-call start shape (tool invocation)

Observed as an assistant message with `toolCall` content blocks:

```json
{
  "type": "message",
  "session": "<session-id>",
  "timestamp": "2026-02-xxT..Z",
  "message": {
    "role": "assistant",
    "content": [
      {
        "type": "toolCall",
        "id": "call_..." | "fc_...",
        "name": "exec|read|write|web_search|web_fetch|edit|...",
        "arguments": { ... },
        "partialJson": "{...}"   // optional
      }
    ],
    "api": "openai",
    "provider": "openai",
    "model": "..."
  }
}
```

Notes:
- Tool name in `name` is reliable for grouping.
- Correlation ID for matching is `id`.
- `arguments` is usually an object. In a few cases `partialJson` appears during tool call streaming.

## 3) Tool-result shape (tool completion/error/etc.)

Observed as assistant role `toolResult` message with `toolCallId`:

```json
{
  "type": "message",
  "session": "<session-id>",
  "timestamp": "2026-02-xxT..Z",
  "stopReason": "tool",
  "message": {
    "role": "toolResult",
    "toolCallId": "call_..." | "fc_...",
    "toolName": "exec|read|web_search|...",
    "content": [
      { "type": "text", "text": "..." }
    ],
    "details": {
      "status": "completed|running|error|failed|forbidden|timeout|ok|approval-pending|200",
      "exitCode": 0,
      "durationMs": 1234,
      "aggregated": {...},
      "cwd": "...",
      "command": "..."
    },
    "isError": false
  }
}
```

Notes:
- `details` is optional in many logs.
- `isError` is often omitted; when present it can be false/true depending on result.
- `status` appears in `details`, but values are not uniform across tools.
- In-process long-running tools can emit intermediate `status: running` followed by terminal `status: completed`.

## 4) Correlation keys observed

Primary key:
- `toolCall.id` (from role `assistant` + `toolCall` content)
- `toolResult.toolCallId` (from role `toolResult`)

When both are present in same timeline, they match exactly for expected pairings.

Observed call/result names (partial, representative):
- `exec`, `read`, `write`, `edit`, `web_search`, `web_fetch`, `process`, `memory_search`, `memory_get`, `memory_store`, `message`, `cron`, `sessions_list`, `sessions_history`, `sessions_spawn`, `sessions_send`, `session_status`, `pdf`

## 5) Normalized output schema for analysis

Recommended normalized row schema (deterministic output):

- `timestamp`
  - `start_timestamp`: first seen `message.timestamp` on toolCall (ISO string, optional fallback to parseable numeric ms if present)
  - `result_timestamp`: first seen corresponding toolResult timestamp
- `session`
  - `session_id`: top-level `session`
- `agent_runtime`
  - `api`: `message.api`
  - `provider`: `message.provider`
  - `model`: `message.model`
  - `runtime_ts`: top-level `timestamp` if provided
- `tool`
  - `name`: `toolCall.name` / `toolResult.toolName`
  - `call_id`: `toolCall.id` or `toolResult.toolCallId`
  - `arguments`: sanitized summary from `toolCall.arguments`
- `params_summary`
  - `params_keys`: list of top-level argument keys
  - `main_params`: a compact subset, e.g. `command`, `path`, `query`, `workdir`, `sessionId`, etc.
- `result`
  - `status`: prefer `details.status`, then inferred from `isError`, else `unknown`
  - `is_error`: `toolResult.isError`
  - `http_status_like`: any numeric status-like string/value in details
  - `result_summary`: shortened text from content or details
  - `aggregated`: optional raw/selected fields from `details.aggregated`
- `latency_ms`
  - Compute only when both timestamps are available: `result_timestamp - start_timestamp`
  - If either timestamp missing, leave null and mark reason (`missing_start` / `missing_result`)

## 6) Ambiguous / unstable cases and fallback logic

1. Missing toolResult
- Not every call has a matching completion in a file slice (premature session termination or truncated capture).
- Fallback: emit row with status `in_flight` and null latency.

2. Result without tool details
- `toolResult.message.details` can be absent.
- Fallback: use `content` as result summary and set status to `unknown` unless `isError` is present.

3. Non-standard status vocabularies
- `status` values vary by tool/runtime (`completed`, `running`, `error`, `failed`, `forbidden`, `timeout`, `ok`, `approval-pending`, `200`).
- Fallback: normalize to enum-like families, keep raw `details.status` as `status_raw`.

4. In-flight / polling flows
- `process` tool can emit intermediate `running` states (with or without final output in same session window).
- Fallback: keep latest terminal state when `latency` can only be computed on the terminal/completion message.

5. `partialJson` on start events
- Presence is optional and represents streaming fragments.
- Fallback: parse when valid JSON; otherwise ignore and use the parsed `arguments` object.

6. Multiple tool calls in one assistant message
- `message.content` may contain more than one `toolCall` block.
- Fallback: emit one canonical row per toolCall with its own `toolCall.id`.

7. Ambiguous `status` from outer fields
- Occasionally outer `stopReason`/`toolResult` metadata can conflict with `details.status`.
- Fallback: prefer inner `details.status` > `isError` > outer reason.

## 7) Practical extraction rules (recommended)

- Iterate all `message` events in chronological order.
- On each `assistant` message with `content[].type == toolCall`, emit/start a call record keyed by `id`.
- On each `toolResult`, match to call by `toolCallId`.
- Build canonical output row from merged call+result record.
- If no result is found by end of stream, keep unresolved call with status `in_flight`.

This schema intentionally avoids mutating production code and is constrained to observed fields only.
