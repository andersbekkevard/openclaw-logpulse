# TUI v1 Architecture Spec

Issue: `openclaw-logpulse-a5k.1`

This document is the binding implementation contract for TUI v1 in `openclaw-logpulse`. It is grounded in the current crate layout, the existing ingestion pipeline, and the completed Wave 1 research briefs.

## 1. Product scope

TUI v1 is a keyboard-first operational viewer for OpenClaw session activity. It builds on the existing normalization, tailing, discovery, and stale-tracking pipeline, but replaces the current single mixed timeline UI in [`src/tui.rs`](/home/anders/.openclaw/workspace/dev/openclaw-logpulse-worktrees/w2-architecture/src/tui.rs) with a derived multi-tab workspace.

### In scope

- Three primary tabs:
  - `Events`: default landing tab, one row per normalized event.
  - `Correlated Tool Calls`: one row per derived tool invocation aggregate.
  - `Sessions`: one row per derived session summary ordered by recent activity.
- Default split view with list on the left and read-only inspector on the right.
- Fullscreen expanded detail mode for the selected entity.
- Drilldown transitions:
  - `Sessions` -> `Enter` -> `Correlated Tool Calls` scoped to that session.
  - `Correlated Tool Calls` -> `Enter` -> `Events` scoped to that call.
  - `Events` -> `Enter` -> expanded detail mode.
- Live-follow vs pinned-selection behavior.
- Contextual help overlay on `?`.
- Structured filter state covering session, tool, severity, time window, text search, and stale-only.
- Heartbeat-derived health surfaced in headers and session rows instead of flooding the default event list.

### Non-goals

- No mouse-first interactions.
- No command palette, ex mode, or free-form query language in v1.
- No editable inspector widgets or multi-pane focus choreography beyond the main list and temporary overlays.
- No separate stale-only primary tab.
- No attempt to preserve the current mixed `TimelineItem` model as the top-level TUI abstraction.
- No feature implementation in this wave; this document defines the contract for later implementation waves.

## 2. Repo grounding and architectural direction

The implementation must continue to reuse the current shared ingestion path:

- [`src/main.rs`](/home/anders/.openclaw/workspace/dev/openclaw-logpulse-worktrees/w2-architecture/src/main.rs) and [`src/tui.rs`](/home/anders/.openclaw/workspace/dev/openclaw-logpulse-worktrees/w2-architecture/src/tui.rs) already normalize raw lines through `normalize_with_source(...)`.
- [`src/normalizer.rs`](/home/anders/.openclaw/workspace/dev/openclaw-logpulse-worktrees/w2-architecture/src/normalizer.rs) is the source of raw event normalization and source-path context extraction.
- [`src/event.rs`](/home/anders/.openclaw/workspace/dev/openclaw-logpulse-worktrees/w2-architecture/src/event.rs) owns normalized event shape plus current filtering helpers.
- [`src/stale.rs`](/home/anders/.openclaw/workspace/dev/openclaw-logpulse-worktrees/w2-architecture/src/stale.rs) already tracks in-flight calls and produces heartbeat summaries.

TUI v1 must introduce a state split between:

- Raw normalized events.
- Derived correlated tool-call aggregates.
- Derived session summaries.
- UI route, focus, and filter state.

The existing `VecDeque<TimelineItem>` in [`src/tui.rs`](/home/anders/.openclaw/workspace/dev/openclaw-logpulse-worktrees/w2-architecture/src/tui.rs) is not sufficient for v1. Implementation should move toward an app state where ingestion updates a durable store/index, and each tab renders a derived projection of that store.

## 3. Primary navigation model

### Tab set

The primary tabs are fixed for v1 and appear in this order:

1. `Events`
2. `Correlated Tool Calls`
3. `Sessions`

`Events` is the default active tab at startup.

### Tab responsibilities

#### Events

- Shows individual normalized events in reverse chronological order by effective timestamp.
- Hidden by default:
  - heartbeat/system diagnostic rows
- Visible by default:
  - tool starts
  - tool results
  - other normalized events
  - malformed events
  - stale warnings derived from tracker state
- When scoped from another tab, the header and breadcrumb must make the active scope explicit.

#### Correlated Tool Calls

- Shows one row per derived tool invocation within one session.
- Default sort: newest `started_at` or `last_updated_at` first.
- Represents status progression cleanly without requiring the user to mentally stitch start/result lines together.

#### Sessions

- Shows one row per durable session identity.
- Default sort: `last_activity_at desc`, then `stale_call_count desc`, then `open_call_count desc`, then `session_id asc`.
- Sessions are active-session oriented. Idle sessions may still appear if they satisfy active filters, but the default emphasis is recent activity.

### Tab switching

- `h` and `l` switch previous/next tab.
- `1`, `2`, `3` jump directly to `Events`, `Correlated Tool Calls`, `Sessions`.
- Each tab remembers its own cursor position, scroll offset, live/pinned state, unseen count, and search match index.

## 4. Drilldown contract

### Required transitions

- `Sessions` row + `Enter`
  - navigates to `Correlated Tool Calls`
  - applies a drilldown-derived session scope for the selected `session_id`
- `Correlated Tool Calls` row + `Enter`
  - navigates to `Events`
  - applies a drilldown-derived correlated-call scope for the selected call entity
- `Events` row + `Enter`
  - opens expanded detail mode for the selected event

### Scope preservation

- Drilldown adds scope; it does not replace unrelated filters.
- Returning from a drilldown restores the previous tab, selection, scroll, and live/pinned state.
- Breadcrumb text is required in the header for drilled routes.

### Breadcrumb format

Use plain hierarchical text, for example:

- `Events`
- `Correlated Tool Calls / session 4b2f0d8c`
- `Events / session 4b2f0d8c / call shell.exec:call_123`
- `Event Detail / session 4b2f0d8c / call shell.exec:call_123`

## 5. UI state and focus model

### Core layers

The app has four layers:

1. Workspace
2. Inspector
3. Expanded detail
4. Overlay

Only the workspace list is focusable in v1. The inspector is read-only and never steals focus. Expanded detail and overlays temporarily replace the normal keymap, then unwind back to the workspace.

### Default layout

- Default workspace layout is a horizontal split:
  - left: active tab list, about 58-66% width
  - right: inspector, about 34-42% width
- No user-configurable layout system in v1.
- If terminal width is too small, inspector may collapse below the list, but the logical split-view contract remains.

### Expanded detail mode

- Trigger: `Enter` on `Events`, or `o` as an alias.
- Fullscreen layer above the workspace.
- Preserves parent tab selection and scroll state.
- Supports vertical scrolling with the same reading keys used elsewhere.
- Shows normalized fields, derived metadata, and raw payload/raw line when available.

### Overlay semantics

Overlays are limited to:

- contextual help
- search entry / result summary
- future filter editor placeholder only if implemented in the same wave

No overlay is allowed to become the main inspection surface.

### Universal unwind rule

- `Esc` closes the topmost active layer.
- `q` behaves like `Esc` when an overlay or expanded detail is open.
- `q` quits only when the workspace layer is active and no higher layer is open.

### Live vs pinned

Each tab maintains its own follow state:

- `LIVE`
  - selection tracks the newest row in that tab's current dataset
  - unseen count is always zero
- `PINNED`
  - selection remains fixed while new rows append
  - unseen count increments for rows arriving after the pin transition

Rules:

- Every tab starts in `LIVE`.
- Any manual navigation action switches the current tab to `PINNED`.
- `f` resumes `LIVE` for the current tab and clears that tab's unseen count.
- Drilldown-derived tabs start in `PINNED` if they open on a specific selected entity, except `Sessions -> Correlated Tool Calls`, which starts `LIVE` within the scoped session because the user is still tracking ongoing session activity.

## 6. Data and entity model

TUI v1 requires explicit entity types separate from raw rendering rows.

### Event row entity

The event list is built from normalized events plus selected derived notices. At minimum each event row must expose:

- `event_ref`
- `timestamp`
- `session_id`
- `session_label`
- `agent_id`
- `tool_name`
- `kind`
- `status`
- `severity`
- `call_ids[]`
- `preview`
- `is_system_event`

Source of truth:

- normalized events from [`src/event.rs`](/home/anders/.openclaw/workspace/dev/openclaw-logpulse-worktrees/w2-architecture/src/event.rs)
- system/derived notices from stale and ingest state

### Correlated tool-call summary

Each correlated call row must expose:

- `call_entity_id`
- `session_id`
- `session_label`
- `agent_id`
- `tool_name`
- `canonical_call_id`
- `status`: `running | succeeded | failed | stale | incomplete | unknown`
- `match_confidence`: `explicit_id | transcript_bundle | fallback_signature`
- `started_at`
- `ended_at`
- `last_updated_at`
- `duration_ms`
- `event_refs.start[]`
- `event_refs.result[]`
- `event_refs.related[]`
- `severity`
- `message_preview`

Identity rules:

- Primary identity is `(session_id, canonical_call_id)` when an explicit call ID exists.
- Fallback identity is `(session_id, fallback_signature, ordinal)`.
- Calls never correlate across sessions.
- If a result arrives without a start, create a result-only correlated call with `status=unknown` or `incomplete`; do not drop it.

### Session summary

Each session row must expose:

- `session_id`
- `session_label`
- `agent_id`
- `last_activity_at`
- `last_event_at`
- `last_source_seen_at`
- `open_call_count`
- `stale_call_count`
- `derived_severity`
- `health_status`
- `source_state`
- `latest_tool_name`
- `latest_preview`

### Heartbeat health/status model

Heartbeat is a derived observation layer, not a default timeline row. Required model:

- global header summary:
  - total tracked sessions
  - sessions `busy`
  - sessions `stale`
  - sessions `disconnected`
- per-session status:
  - `unknown`
  - `active`
  - `idle`
  - `busy`
  - `stale`
  - `disconnected`

Status rules:

- `busy`: open calls exist and none exceed stale threshold.
- `stale`: one or more open calls exceed stale threshold.
- `active`: recent non-heartbeat activity observed and no open calls.
- `idle`: no recent activity, no open calls, source still considered present.
- `disconnected`: source previously existed or session was observed, but `last_source_seen_at` or discovery state indicates disappearance/inactivity beyond the disconnected threshold.
- `unknown`: insufficient evidence yet.

Threshold rules:

- `heartbeat_seconds` continues to control observation cadence.
- Freshness thresholds are separate derived constants/config:
  - recent activity window: `2 * heartbeat_seconds`
  - disconnected window: `max(4 * heartbeat_seconds, stale_seconds * 2)`

### Filter state

The TUI filter state is first-class structured state:

- `session_scope: Option<Vec<SessionId>>`
- `tool_scope: Option<Vec<ToolName>>`
- `min_severity: Severity`
- `time_window: { since: Option<DateTime<Utc>>, until: Option<DateTime<Utc>> }`
- `text_query: Option<String>`
- `stale_only: bool`
- `include_system_events: bool`
- `drilldown_scope: Option<DrilldownScope>`

This replaces the current header-only string summary pattern in [`src/tui.rs`](/home/anders/.openclaw/workspace/dev/openclaw-logpulse-worktrees/w2-architecture/src/tui.rs).

## 7. Filter semantics and composition rules

### Global vs tab-scoped vs drilldown-derived

- Global filters:
  - session
  - tool
  - severity
  - time window
  - text search
  - stale-only
  - include-system-events
- Tab-scoped behavior:
  - tabs interpret the same structured filter state against their own entity type
- Drilldown-derived scope:
  - added automatically when entering a child route from a selected row

The same visible filter summary must mean the same dataset restriction regardless of tab. No tab may silently invent incompatible semantics.

### Specific filter rules

#### Session

- CLI bootstrap may still accept substring session filters.
- Once inside the TUI, selected-session drilldowns use exact durable `session_id` matching.
- `session_label` is searchable text, but not identity.

#### Tool

- Tool filter matches exact structured tool names for derived entities.
- Text search may still fuzzy-match tool names.

#### Severity

- On `Events`, filter directly by event severity.
- On `Correlated Tool Calls`, filter by max severity across member events.
- On `Sessions`, filter by max visible severity across the session's currently visible calls/events.

#### Time window

- `since` and `until` are inclusive.
- On `Events`, apply to event timestamp.
- On `Correlated Tool Calls`, apply by overlap:
  - include a call if any member event falls within the window, or if its running span overlaps the window.
- On `Sessions`, include a session if any visible event/call overlaps the window.

#### Text search

Matches against:

- session label
- session id
- tool name
- agent id
- message
- preview
- params preview
- result preview

#### Stale-only

- On `Events`, show stale warnings and events associated with stale correlated calls.
- On `Correlated Tool Calls`, show only rows with `status=stale`.
- On `Sessions`, show only sessions with `stale_call_count > 0`.

### System-event visibility

- Heartbeat rows are hidden from `Events` by default.
- They become visible only when `include_system_events=true`.
- Stale warnings remain visible by default because they are operator-relevant, but they must honor the same active filter state as the current tab.

## 8. Session identity and correlation decisions

These decisions are explicit to remove ambiguity during implementation.

### Session identity

Current gap:

- [`src/normalizer.rs`](/home/anders/.openclaw/workspace/dev/openclaw-logpulse-worktrees/w2-architecture/src/normalizer.rs) currently sets `session_id = session_key.clone().or(source_context.session_id.clone())`, which allows arbitrary payload values to override the path-derived session UUID.

Decision:

- Durable `session_id` must prefer source/path or transcript session UUID.
- `session_key` becomes `session_label` input, not durable identity, unless future evidence proves it is the same namespace.
- The normalizer or a post-normalization adapter must preserve both:
  - `session_id`
  - `session_label`
  - source/confidence metadata for each

### Transcript multi-tool-call fan-out

Current gap:

- transcript assistant messages with multiple `toolCall` items currently elevate only the first tool call to `call_id`, leaving the rest in `correlation_ids`.

Decision:

- TUI v1 requires one correlated-call unit per transcript tool call.
- Implementation may either:
  - fan out one normalized event per transcript tool call in [`src/normalizer.rs`](/home/anders/.openclaw/workspace/dev/openclaw-logpulse-worktrees/w2-architecture/src/normalizer.rs), or
  - preserve a structured child-call array that the aggregation layer expands deterministically
- The first option is preferred because it simplifies downstream indexing and testing.

### Fallback correlation confidence

Current gap:

- [`src/stale.rs`](/home/anders/.openclaw/workspace/dev/openclaw-logpulse-worktrees/w2-architecture/src/stale.rs) closes calls by fallback signature without surfacing confidence.

Decision:

- Any fallback-signature match must carry `match_confidence=fallback_signature`.
- If multiple repeated same-session same-tool same-argument calls exist without IDs, completion closes the oldest open call only.
- The correlated-call detail view must show the match confidence.

## 9. Heartbeat handling

### Default behavior

- Do not inject heartbeat summaries into the default `Events` tab.
- Continue computing heartbeat/refresh cadence from the existing tracker and timer loop.
- Surface the derived health signal in:
  - workspace header
  - session rows
  - correlated-call status where relevant

### Diagnostic visibility

- If system events are enabled, heartbeat summaries may appear in `Events`.
- Diagnostic rows must be visually distinct from raw tool events.
- The user-visible default remains a clean operator timeline, not periodic heartbeat spam.

### Source freshness

Implementation must track freshness beyond open-call counts:

- `last_event_at`
- `last_source_seen_at`
- whether a discovered source disappeared or stopped yielding events

Without this, idle and disconnected sessions remain indistinguishable.

## 10. Keybinding contract for v1

The v1 keymap is intentionally small and Vim-friendly.

### Global workspace keys

- `j` / `Down`: move selection down
- `k` / `Up`: move selection up
- `g`: first row
- `G`: last row
- `Ctrl-u`: half-page up
- `Ctrl-d`: half-page down
- `h`: previous tab
- `l`: next tab
- `1`: jump to `Events`
- `2`: jump to `Correlated Tool Calls`
- `3`: jump to `Sessions`
- `Enter`: drill down or open expanded detail
- `o`: open expanded detail for current row where supported
- `f`: resume `LIVE`
- `/`: start in-screen text search
- `n`: next match
- `N`: previous match
- `?`: open contextual help overlay
- `Esc`: unwind one layer / cancel
- `q`: unwind one layer or quit from workspace

### Layer-specific rules

- In expanded detail:
  - `j/k`, arrows, `g/G`, `Ctrl-u`, `Ctrl-d` scroll the detail body
  - `Esc` / `q` closes detail
- In help overlay:
  - `Esc`, `q`, `?` close help

### `?` contextual help contract

The help overlay must show:

- global keybindings
- tab-specific actions for the active tab
- active layer-specific actions
- one-line state summary:
  - active tab
  - current breadcrumb
  - `LIVE` or `PINNED`
  - unseen count if pinned

## 11. Verification hooks for later implementation waves

Implementation is not complete until later waves prove the following through tests and interaction coverage.

### State and semantics

- Session identity uses durable `session_id`, not `session_key` override.
- Two sessions with the same explicit `call_id` do not correlate together.
- Repeated no-ID calls in one session close oldest-open only.
- Transcript multi-tool-call messages produce one correlated-call row per tool call.
- Result-only events remain visible as unmatched/incomplete correlated calls.
- `since` and `until` are enforced, inclusive, and consistent across TUI and non-TUI paths.
- Stale warnings and session health honor active filters consistently.
- Idle and disconnected sessions are distinguishable.

### UI behavior

- Each tab preserves independent cursor, scroll, follow state, and unseen count.
- Manual navigation flips a tab from `LIVE` to `PINNED`.
- `f` resumes live follow and clears unseen count only for the current tab.
- Drilldown preserves parent state and applies additive scope.
- `Esc`/`q` unwind correctly from help and expanded detail before quitting.
- Heartbeats are hidden by default from `Events`.

### Suggested test surface

- unit tests for normalization/correlation/filter helpers
- reducer/state-transition tests for route and follow behavior
- rendering smoke or snapshot tests for tab headers and inspector/detail content
- integration tests that ingest transcript and generic logs through the shared pipeline

## 12. Implementation decomposition

The implementation should be split into parallel tracks with minimal file conflict.

### Track 1: normalization and identity foundation

Ownership:

- [`src/normalizer.rs`](/home/anders/.openclaw/workspace/dev/openclaw-logpulse-worktrees/w2-architecture/src/normalizer.rs)
- [`src/event.rs`](/home/anders/.openclaw/workspace/dev/openclaw-logpulse-worktrees/w2-architecture/src/event.rs)

Responsibilities:

- separate `session_id` from `session_label`
- add confidence/source metadata as needed
- fan out transcript multi-tool calls or expose deterministic expansion data
- add shared time-window filter support

Risk:

- touches the shared ingest contract used by both TUI and non-TUI code

### Track 2: derived aggregation and health model

Ownership:

- new aggregation module, likely `src/model.rs` or `src/view_model.rs`
- [`src/stale.rs`](/home/anders/.openclaw/workspace/dev/openclaw-logpulse-worktrees/w2-architecture/src/stale.rs)

Responsibilities:

- correlated-call aggregation
- session summary derivation
- heartbeat/source freshness model
- derived severity and stale-only semantics

Risk:

- depends on Track 1 identity decisions

### Track 3: TUI route/state/render refactor

Ownership:

- [`src/tui.rs`](/home/anders/.openclaw/workspace/dev/openclaw-logpulse-worktrees/w2-architecture/src/tui.rs)
- any new `src/tui/*` modules if the file is split

Responsibilities:

- replace single mixed timeline state with tabbed workspace state
- implement route/layer/focus model
- render tabs, inspector, detail, contextual help, and status/header
- wire live/pinned behavior and drilldown transitions

Risk:

- highest merge hotspot because `src/tui.rs` currently contains nearly all TUI behavior

### Track 4: test harness and interaction coverage

Ownership:

- `tests/`
- targeted module tests in `src/*`

Responsibilities:

- cover identity, correlation, filters, route transitions, and heartbeat visibility defaults

Risk:

- should land after Tracks 1-3 expose stable APIs

### Suggested merge order

1. Track 1
2. Track 2
3. Track 3
4. Track 4

Track 3 should avoid starting from the old `TimelineItem` design after Track 2 lands; otherwise it will create rework.

## 13. High-risk gaps that implementation must address

- `since` / `until` are exposed in [`src/cli.rs`](/home/anders/.openclaw/workspace/dev/openclaw-logpulse-worktrees/w2-architecture/src/cli.rs) and shown in the TUI filter summary, but not enforced.
- Session identity is currently conflated with payload display labels in [`src/normalizer.rs`](/home/anders/.openclaw/workspace/dev/openclaw-logpulse-worktrees/w2-architecture/src/normalizer.rs).
- Transcript multi-tool-call correlation is incomplete.
- Heartbeat freshness currently reflects only in-flight call counts and cannot distinguish idle from disconnected.
- Stale warning filtering is inconsistent with event filtering.
- [`src/tui.rs`](/home/anders/.openclaw/workspace/dev/openclaw-logpulse-worktrees/w2-architecture/src/tui.rs) is a hotspot concentration for state, input, rendering, and formatting, so implementation should expect a structural split rather than incremental patching.

## 14. Unresolved blockers

There are no design blockers that justify deferring core UX decisions in this spec.

The only operational blocker observed in this wave is local issue-tracker connectivity: `bd` cannot currently open the expected Dolt database in this worktree. That does not block architecture decisions, but it may block issue status updates until the local `bd` server/database configuration is corrected.
