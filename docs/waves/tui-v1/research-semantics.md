# TUI v1 Research: Semantics and State Foundations

Issue: `openclaw-logpulse-a5k.1.3`

## Scope

This brief recommends the semantic model for the planned tabbed TUI. It is based on the current normalizer, stale tracker, discovery flow, CLI/TUI filters, and transcript parsing behavior in this branch.

The intent is not to preserve every current implementation detail. The intent is to preserve what is already defensible, tighten ambiguous behavior, and identify gaps that would make a tabbed Sessions / Correlated Tool Calls experience misleading.

## Current behavior summary

### Normalized events

- Generic JSON logs are normalized into `NormalizedEvent` by extracting session, agent, tool, call IDs, status, params, summary, and severity from a broad set of known paths.
- Session discovery currently prefers `session_key` from payload fields, then falls back to the session UUID inferred from the source file path.
- Agent ID is inferred from either payload fields or the `agents/<agent>/sessions/...` path.
- Tool-call kind is inferred heuristically from event/status text plus the presence of result-like or call-like fields.

### Transcript v3 specifics

- Transcript `type=session` becomes a non-tool `Other` event with `session started`.
- Transcript `type=message` with `message.toolCallId` or `role=toolResult` becomes a `ToolCallResult`.
- Transcript `type=message` with one or more `content[].type == "toolCall"` items becomes a single `ToolCallStart` event.
- In that transcript case, the first tool call becomes `call_id`; additional tool-call IDs are stored only as `correlation_ids`.

### Stale and heartbeat

- The stale tracker stores in-flight calls only for `ToolCallStart` and removes them on `ToolCallResult`.
- Matching is by explicit call ID first, then by a fallback signature of `session + tool + agent + preferred param/message`.
- Heartbeat is currently just a periodic summary of in-flight state: `active_calls`, `stale_calls`, `active_sessions`.
- The current TUI inserts heartbeat summaries into the same timeline as tool events.

### Filters

- Implemented event filters are `session`, `agent`, `tool`, and `min-level`.
- Stale warnings are filtered only by `session` and `tool` in TUI; in line mode they are emitted unfiltered.
- `since` and `until` are accepted by CLI and shown in the TUI filter summary, but they are not enforced.

## Recommendations

### 1. Correlated tool call semantics

Define a correlated tool call as the smallest UI unit that represents one user-visible tool invocation attempt within one session.

Use this identity model:

- Primary key: `(session_id, canonical_call_id)` when an explicit call ID exists.
- Secondary key: `(session_id, fallback_signature, ordinal)` when no explicit call ID exists.
- Never correlate across sessions, even if call IDs collide.

Recommended canonical fields per correlated tool call:

- `session_id`
- `agent_id`
- `tool_name`
- `canonical_call_id`
- `status`: `running | succeeded | failed | incomplete | unknown`
- `started_at`
- `ended_at`
- `duration_ms`
- `start_event_refs[]`
- `result_event_refs[]`
- `related_event_refs[]`
- `matched_by`: `explicit_id | transcript_bundle | fallback_signature`

Mapping back to raw events:

- Every UI record should retain references to the normalized event(s) that produced it.
- A correlated tool call is not itself a raw event. It is a derived aggregate over one or more raw events.
- Start rows should typically come from `ToolCallStart`.
- Result rows should typically come from `ToolCallResult`.
- `ToolCall` and `Other` events should remain inspectable as supporting context but should not create a correlated-call row by default.

Transcript bundle rule:

- When one transcript assistant message contains multiple `toolCall` items, treat each `toolCall.id` as its own correlated tool call.
- The current behavior, which promotes only the first tool call to `call_id` and demotes the rest to `correlation_ids`, is not sufficient for a first-class Correlated Tool Calls tab.
- The normalizer or a later aggregation layer needs one normalized record per transcript tool call, or an equivalent fan-out representation.

Completion rule:

- A result with a matching explicit call ID closes exactly that correlated call.
- If explicit ID is missing, fallback matching may close only the oldest still-open call with the same fallback signature within the same session.
- Any fallback match should be marked as lower confidence in internal state to keep ambiguous joins debuggable.

### 2. Session semantics and drilldown

Define a session as the OpenClaw session UUID, preferably inferred from the session transcript/log file path or explicit session object ID.

Recommended session identity precedence:

1. Transcript/session object ID when `type=session`
2. Session UUID from source file path
3. Explicit payload session field only if it is known to be the same namespace as the OpenClaw session UUID
4. Otherwise treat payload session strings as `session_label`, not durable session identity

Reasoning:

- The current normalizer sets `session_id = session_key.clone().or(source_context.session_id)`, which allows arbitrary payload `session_key` values to override the path-derived UUID.
- For TUI drilldown, that is too weak. Session identity and human-readable label should be separate fields.

Recommended session model:

- `session_id`: durable identity used for grouping, filters, and drilldown
- `session_label`: display string, may come from `session_key` or a shortened UUID
- `agent_id`
- `last_activity_at`
- `open_call_count`
- `stale_call_count`
- `last_heartbeat_at`
- `health_status`

Ordering:

- Sessions tab should sort by `last_activity_at desc`.
- If two sessions tie, sort by `stale_call_count desc`, then `open_call_count desc`, then `session_id asc`.

Drilldown behavior:

- Sessions -> Correlated Tool Calls should always scope to a single `session_id`.
- Existing global filters should remain applied unless explicitly cleared.
- The drilldown should add a session scope, not replace unrelated filters such as time window, severity, or text search.

### 3. Heartbeat as observed health, not event spam

Heartbeat should represent observed log-stream health and session activity freshness, not just a periodic row appended to the timeline.

Recommended semantics:

- Heartbeat is a derived observation sampled from ingest state.
- It should power status badges/header summaries first.
- It should appear in the timeline only if the operator explicitly enables diagnostic/system events.

Recommended health dimensions:

- `ingest_status`: are logs being discovered/read right now?
- `activity_status`: has the session emitted any event recently?
- `call_status`: are there running or stale correlated calls?

Default status model per session:

- `active`: events observed within the heartbeat freshness window
- `idle`: no recent events, no open calls, session not yet stale
- `busy`: open calls exist and none are stale
- `stale`: one or more open calls exceeded the stale threshold
- `disconnected`: source previously existed but no new activity has been observed beyond a larger inactivity threshold, or the source disappeared
- `unknown`: session discovered but not enough data yet

Recommended global header defaults:

- Show counts of sessions by status.
- Highlight `stale` and `disconnected` first.
- Do not make repeated heartbeat rows the dominant visual element.

Threshold guidance:

- `heartbeat_seconds` should control UI refresh cadence or observation cadence.
- It should not itself define health state.
- Health should be based on separate freshness thresholds, for example:
  - recent activity window: about `2 x heartbeat_seconds` to `3 x heartbeat_seconds`
  - disconnected/inactive window: materially larger than stale threshold

### 4. Filter composition with tabs and drilldown

Filters need first-class state, not ad hoc string summaries.

Recommended filter set:

- `session_ids[]`
- `tool_names[]`
- `severity >=`
- `time_window: { since, until }`
- `text_query`
- `stale_only`
- `include_system_events`

Composition rules:

- Filters apply globally unless a tab explicitly defines a narrower derived view.
- Tabs should not each invent separate filter semantics.
- Drilldown adds scope. It does not silently discard global filters.

Recommended tab behavior:

- Sessions tab: filters sessions by session-level derived fields plus any matching underlying event/call facts.
- Correlated Tool Calls tab: filters correlated calls directly.
- Raw Events tab, if present later: filters normalized events directly, including system/heartbeat items when enabled.

Specific rules:

- `session` filter: exact identity match once a session is selected from the UI; free-text substring only for command-line bootstrap or search UX.
- `tool` filter: exact tool key in structured state, optional fuzzy match in search UX.
- `severity`: apply to raw events and to correlated calls using max severity observed on any member event.
- `time window`: apply by event timestamp for raw events, and by overlap for correlated calls and sessions.
- `text search`: match against tool name, message, summary, params preview, result preview, and session label.
- `stale_only`: on Sessions tab show sessions with at least one stale correlated call; on Correlated Tool Calls tab show only stale/open stale calls.

Important consistency rule:

- Stale warnings and derived session status must honor the same active filters as the tab being viewed.
- The current split behavior, where event filters and stale-warning filters differ and some outputs ignore filters entirely, will confuse the TUI.

### 5. Data-model and normalization gaps that block the UX

### Gap: session identity is conflated with display label

Current issue:

- `session_key` may replace the path-derived UUID in `session_id`.

Impact:

- Session grouping and drilldown can fragment or merge incorrectly.

Recommendation:

- Separate `session_id` from `session_label`.
- Preserve both source and confidence for each field.

### Gap: transcript multi-tool messages are collapsed

Current issue:

- Only the first transcript tool call gets `call_id`; the rest become loose `correlation_ids`.

Impact:

- The future Correlated Tool Calls tab cannot render one row per actual tool call.

Recommendation:

- Fan out transcript tool-call content into one normalized event per tool call, or add a structured child-call array that the aggregator expands deterministically.

### Gap: time-window filtering is not implemented

Current issue:

- `--since` and `--until` exist in CLI/TUI state but do nothing.

Impact:

- Any tabbed UX that shows active filters will misrepresent the dataset.

Recommendation:

- Implement time-window checks in the shared ingest/filter layer before building the TUI tab model.

### Gap: heartbeat has no notion of source freshness

Current issue:

- Heartbeat summarizes only currently open calls.

Impact:

- A quiet or disconnected source with zero open calls looks the same as a healthy idle session.

Recommendation:

- Track `last_event_at`, `last_source_seen_at`, and source disappearance independently from stale-call detection.

### Gap: fallback correlation is not confidence-aware

Current issue:

- Signature-based matching is useful, but ambiguous when the same tool/args repeat.

Impact:

- Calls can be closed incorrectly, especially in repetitive shell/read/search traffic.

Recommendation:

- Track `match_confidence` and surface ambiguous/fallback-only joins for debugging and tests.

### Gap: severity for derived entities is unspecified

Current issue:

- Sessions and correlated calls do not yet define derived severity.

Impact:

- Severity filters cannot compose cleanly with tabs.

Recommendation:

- For correlated calls, derive severity as max severity over member events.
- For sessions, derive severity as max severity over visible correlated calls/events within the active filter scope.

### 6. Testability and edge cases to protect

Add tests around these semantics before feature implementation:

- Two sessions emit the same `call_id`; they must not correlate together.
- One session emits repeated identical tool invocations without call IDs; fallback matching must close oldest-open only.
- A transcript message contains multiple `toolCall` items; each must become a distinct correlated call.
- A `toolResult` arrives without an explicit start; it should appear as a result-only correlated call or unmatched result, not vanish.
- A start event arrives with no result before stale threshold; status should progress `running -> stale`.
- A stale call later receives a result; status should resolve without leaving duplicate active state.
- `since/until` boundaries must be inclusive and deterministic.
- Session ordering must update when late-arriving out-of-order timestamps are ingested.
- Heartbeat/status should distinguish healthy idle from disconnected/no-source conditions.
- Derived filters must behave the same in TUI and non-TUI output paths where applicable.

## Recommended implementation order for later architecture/spec work

1. Split durable session identity from display label.
2. Define a first-class correlated-call aggregate type with raw-event references and confidence metadata.
3. Fan out transcript multi-tool messages into distinct call units.
4. Move all filters into one shared filter evaluator, including time windows and stale-only.
5. Introduce session/source freshness tracking separate from stale-call tracking.
6. Treat heartbeat as status state in the header/session rows, with optional diagnostic timeline visibility.

## Bottom line

The planned TUI should treat sessions and correlated tool calls as derived state built on normalized events, not as direct views over the current mixed timeline. The two biggest blockers are session identity conflation and transcript multi-tool-call collapse. The most misleading current behavior is heartbeat being represented as timeline noise instead of a health signal, combined with filters that do not consistently apply across event types.
