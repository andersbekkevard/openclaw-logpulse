# Wave 1 Research: Repo Map For TUI v1

## Scope and method

This brief maps the current `openclaw-logpulse` crate surfaces that matter for a TUI v1 covering Events / Correlated Tool Calls / Sessions, drilldowns, heartbeat status, filtering, and navigation.

It is based on direct inspection of the current code in `src/` and `tests/`, plus non-destructive repo commands. No feature work was implemented in this wave.

## High-level repo map

### Crate/module inventory

| Module | Current responsibility | TUI v1 relevance |
| --- | --- | --- |
| `src/main.rs` | Entry point, mode switch, shared ingestion loop for CLI modes, heartbeat emission, auto-discovery bootstrap | High |
| `src/tui.rs` | Entire current TUI state, input handling, ingestion, rendering, filtering summary, detail panel formatting | Highest |
| `src/event.rs` | `NormalizedEvent`, severity, filtering, correlation helpers (`all_call_ids`, fallback signature) | Highest |
| `src/normalizer.rs` | JSON/transcript normalization, source-path context extraction, event kind inference, param/result extraction | Highest |
| `src/stale.rs` | In-flight tool-call tracking, stale warning generation, heartbeat summary | High |
| `src/tailer.rs` | Single-file and multi-file follow mode, rotation handling, discovery sync behavior | High |
| `src/discovery.rs` | Zero-arg session log discovery under `~/.openclaw/agents/*/sessions` | Medium |
| `src/output.rs` | Human/JSON emission for non-TUI modes | Medium |
| `src/cli.rs` | Argument surface | Medium |
| `src/parser.rs` | Line-level JSON parse / malformed preservation | Medium |

### Current data flow

1. `src/main.rs` parses args, resolves output mode, and dispatches to `tui::run` for `--format tui`.
2. Both TUI and non-TUI paths normalize each raw line through `normalize_with_source(...)`.
3. Normalized events feed `StaleTracker::on_event(...)` to maintain in-flight calls and generate stale warnings.
4. Filtering is applied on `NormalizedEvent::should_filter(...)`.
5. Non-TUI paths serialize through `src/output.rs`; TUI paths wrap items as `TimelineItem` and render them in a single timeline/detail screen.

The main architectural point: the TUI already shares the same ingestion/normalization/staleness model as CLI output, so a TUI v1 can evolve mostly by restructuring presentation state rather than replacing the pipeline.

## Current behavior and strengths

### What the current TUI already does well

- It uses the same normalization and stale-tracking path as CLI mode, so there is one event model and one heartbeat/stale implementation to extend rather than duplicate.
- It already supports zero-arg discovery mode by tailing multiple session logs and rescanning for new files.
- It handles file rotation and disappearing files through `Tailer` / `MultiTailer`, which is a solid base for long-running TUI sessions.
- It preserves malformed input as visible events instead of dropping it, which is useful for operator trust.
- It already has keyboard-driven selection, follow/freezing, detail scrolling, and a detail inspector with raw payload rendering.

### What the current TUI actually is

Today’s TUI is a single-screen timeline:

- One `VecDeque<TimelineItem>` backs everything in `src/tui.rs`.
- The upper pane is one flat table of mixed event types.
- The lower pane is one detail/inspector panel for the selected row.
- The header shows latest heartbeat summary and filter summary.
- The footer is the only built-in help surface.

There are no primary tabs, no derived session/correlation views, no drilldown routing, and no dedicated help mode.

## Exact code hotspots likely to change

### 1. Primary tabs: Events / Correlated Tool Calls / Sessions

Primary hotspots:

- `src/tui.rs:137-250`
  - `App` only stores a single `items` collection and one `TableState`.
  - TUI v1 tabs likely require either per-tab derived collections and selection state, or a richer route/view model.
- `src/tui.rs:481-500`
  - `render(...)` hardcodes one vertical layout with one table and one detail pane.
- `src/tui.rs:544-582`
  - `render_table(...)` assumes one flat timeline schema.
- `src/tui.rs:427-479`
  - `ingest_line(...)` always pushes matching events directly into timeline order; there is no intermediate session/call index.
- `src/event.rs:145-191`
  - Existing helpers (`all_call_ids`, `fallback_signature`, `preferred_params`) are the main reusable basis for building correlated-call rows.
- `src/stale.rs:18-23`, `src/stale.rs:100-183`
  - `StaleTracker` already keeps an in-flight map plus signature index, but it is private and optimized for stale detection, not for tabular correlated-call inspection.

Likely implementation consequence:

- TUI v1 will probably need a state split between raw event timeline, a derived correlated-call model, and a derived session model. Right now that derived state does not exist anywhere in the crate.

### 2. Drilldown transitions

Primary hotspots:

- `src/tui.rs:198-249`
  - Navigation is just row movement plus detail scrolling.
- `src/tui.rs:359-385`
  - `handle_input(...)` has no routing/state-machine concept beyond local key actions.
- `src/tui.rs:585-597`
  - The “detail view” is always a lower panel, never a dedicated route/screen.
- `src/tui.rs:641-716` and `src/tui.rs:872-938` (detail helpers and row styling/formatting)
  - Current formatting is item-centric, not screen-centric.

Likely implementation consequence:

- Drilldowns are easiest if `App` gains an explicit route enum, selected entity IDs, and per-route key handling. Without that, drilldown behavior will keep colliding with the single-table selection model.

### 3. Heartbeat status-bar behavior

Primary hotspots:

- `src/tui.rs:183-189`
  - `push_heartbeat(...)` both updates `latest_summary` and inserts a heartbeat row into the timeline.
- `src/tui.rs:299-303`, `src/tui.rs:346-349`
  - Heartbeat generation is timer-driven in both discovery and single-log loops.
- `src/tui.rs:503-541`
  - Header rendering is the current status-bar implementation.
- `src/stale.rs:63-92`, `src/stale.rs:212-218`
  - `HeartbeatSummary` is the only heartbeat data shape.
- `src/main.rs:57-65`, `src/main.rs:123-131`
  - Non-TUI heartbeat behavior mirrors the TUI timer logic.

Important current behavior:

- Heartbeats are both status data and timeline rows. If TUI v1 wants a cleaner status bar, this dual use may need to be separated.

### 4. Expandable inspector/detail view

Primary hotspots:

- `src/tui.rs:585-597`
  - Detail pane is fixed in the lower half of the screen.
- `src/tui.rs:634-716`
  - `detail_text(...)` / `detail_tool_event(...)` build the entire inspector presentation.
- `src/tui.rs:236-241`, `src/tui.rs:375-379`
  - Only vertical detail scrolling exists.
- `src/tui.rs:493-499`
  - Body layout hardcodes 56/44 table/detail split.

Likely implementation consequence:

- Expand/collapse or full-screen inspector work will mainly be a `src/tui.rs` state-and-layout change. There is no reusable abstraction for panels yet.

### 5. Contextual help / Vim navigation

Primary hotspots:

- `src/tui.rs:359-385`
  - Current keymap is limited to `q`, `j/k`, arrows, `g/G`, `PgUp/PgDn`, `Home`, `f`.
- `src/tui.rs:600-638`
  - Footer is the only help text and is always static.

Important current behavior:

- Basic Vim-style movement already exists (`j`, `k`, `g`, `G`), so v1 should extend an existing idiom rather than introducing a second navigation style.
- There is no discoverable “?” help surface, no per-tab help, and no key conflict resolution yet.

## Event normalization, correlation, stale logic, and filtering

### Event normalization

Main surfaces:

- `src/normalizer.rs:11-186`
  - Enumerates all supported field paths for session, agent, tool, correlation IDs, timestamp, params, result fields, level, and status.
- `src/normalizer.rs:242-318`
  - Generic JSON normalization path.
- `src/normalizer.rs:321-523`
  - Transcript-v3 source-context extraction and transcript-specific normalization.
- `src/normalizer.rs:608-705`
  - Event kind inference.

Key findings:

- The normalizer is intentionally heuristic and broad, which is good for ingestion resilience.
- Transcript-v3 support is stronger than the rest of the crate naming implies; it already maps assistant tool calls and tool results from session transcript logs.
- `source_path`, `source_kind`, `session_source`, and `agent_source` already exist on `NormalizedEvent`, giving TUI v1 useful provenance fields for session-oriented screens.

### Correlation model

Main surfaces:

- `src/event.rs:145-191`
  - Correlation helpers.
- `src/stale.rs:100-183`
  - In-flight matching by explicit call ID first, then fallback signature.

Key findings:

- There is enough information to build a correlated tool-call tab, but the current code only uses correlation to close stale-tracker entries.
- There is no persisted “tool call span” model combining start/result/warnings/heartbeat metadata into a stable TUI entity.
- `fallback_signature()` is practical but heuristic; any correlated-call view built on it should expect ambiguity for repeated same-session same-tool same-argument invocations.

### Heartbeat/stale model

Main surfaces:

- `src/stale.rs:53-60`
  - Event ingestion hook.
- `src/stale.rs:63-92`
  - Heartbeat summary.
- `src/stale.rs:185-209`
  - One-shot stale warning emission.

Key findings:

- Staleness is currently defined only for in-flight tool calls.
- Warnings are emitted once per call (`warned: bool`), which is simple and avoids UI spam.
- The summary only exposes counts; it does not expose which sessions or calls are stale without separately walking tracker internals.

### Filtering

Main surfaces:

- `src/event.rs:93-143`
  - Actual filter logic.
- `src/tui.rs:972-1001`
  - Filter summary string shown in the header.
- `src/cli.rs:40-74`
  - CLI surface for `session`, `tool`, `agent`, `since`, `until`, `min-level`.

Key findings:

- Working filters today: session substring, agent substring, tool substring, minimum severity.
- Non-working filters today: `--since` and `--until`. They exist in args and header text, but no code applies them during ingestion or rendering.
- TUI stale warning filtering only applies session/tool checks, not agent or time filters (`src/tui.rs:446-477`).

## Existing tests and gaps

### Existing test coverage

Unit tests:

- `src/parser.rs:27-44`
  - Valid JSON vs malformed line handling.
- `src/stale.rs:221-310`
  - In-flight tracking, synthetic IDs, fallback signature completion.
- `src/normalizer.rs:928-1038`
  - Generic tool call/result parsing, nested error events, transcript-v3 normalization.

Integration tests:

- `tests/logpulse_integration.rs:102-470`
  - Human output, JSON output, malformed preservation, transcript-v3 context, follow-mode rotation, zero-arg auto-discovery, stale warning emission, startup/throughput checks.

### Important gaps

- No tests for `src/tui.rs` at all: no rendering snapshots, no input handling tests, no state-transition tests.
- No tests for tabular derived models because those models do not exist yet.
- No tests for `--since` / `--until`, which matches the current implementation gap.
- No tests that verify stale warnings respect the same filter semantics as regular events.
- No tests for multi-call transcript messages beyond “first tool call becomes the main event and later IDs go into `correlation_ids`”.
- No tests for drilldown/navigation routing because there is no route model yet.
- No tests for discovery edge cases such as mixed valid/invalid session filenames, lockfiles during churn, or agent-directory races.

## Risks, ambiguities, and likely merge hotspots

### Risks and ambiguities

- `src/tui.rs` is a monolith. Tabs, drilldowns, inspector modes, and contextual help all currently land in one file, so design changes will stack on the same edit surface.
- The current event model is event-oriented, not entity-oriented. A Sessions tab and Correlated Tool Calls tab will likely need new derived data structures, not just new rendering functions.
- `fallback_signature()` correlation is heuristic. It is useful, but a first-class correlated-call screen should make ambiguity visible rather than assuming exact matching.
- Heartbeats are currently materialized as timeline rows. If the v1 UX wants a clean status bar without timeline noise, this behavior may need to be split.
- `since` / `until` semantics are undefined in code today. TUI v1 should not assume existing behavior.

### Likely merge/conflict hotspots

- `src/tui.rs`
  - Highest conflict risk; nearly every planned TUI feature touches it.
- `src/event.rs`
  - Likely to change if v1 introduces derived entities or richer correlation metadata.
- `src/normalizer.rs`
  - Likely to change if session/call aggregation needs more canonical IDs or transcript fields.
- `src/stale.rs`
  - Likely to change if heartbeat/status bars need richer stale/session breakdowns.
- `src/cli.rs`
  - Likely to change if time filtering is implemented or TUI-specific options expand.
- `tests/logpulse_integration.rs`
  - Likely to grow as missing behavior is covered.

## Recommended implementation map for the next wave

1. Introduce an explicit TUI route/view model in `src/tui.rs` (or a new `tui/` module split) before adding features.
2. Introduce derived state for:
   - event timeline rows
   - correlated tool call rows/entities
   - session rows/entities
3. Decide whether heartbeat stays as a timeline item, header-only status, or both.
4. Define exact semantics for `since` / `until`, then implement them in shared ingestion/filtering code rather than only inside the TUI.
5. Add TUI-focused state tests early, otherwise future refactors in the monolithic file will be fragile.

## Explicit unknowns / blockers

- `bd` issue-state operations are blocked in this worktree by a local Dolt/beads server configuration problem (`database "openclaw_logpulse" not found on Dolt server at 127.0.0.1:13334`), so I could not verify or update issue metadata from this environment.
- There is no existing design/spec in this branch defining what counts as a “session” row or a “correlated tool call” row in the UI, so those entity shapes still need to be nailed down in the next wave.
