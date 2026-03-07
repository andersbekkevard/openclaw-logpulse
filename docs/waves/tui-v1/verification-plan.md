# TUI v1 Verification Plan

Issue: `openclaw-logpulse-a5k.9.1`

This document is the binding verification contract for TUI v1. It defines the cheapest practical checks that must prove the implementation matches the TUI v1 architecture spec, with preference for autonomous verification over manual review.

## 1. Verification principles

### Tier policy

- `Tier 1 Autonomous`: deterministic checks runnable in CI without a human interpreting UI behavior.
- `Tier 2 Good Proxy`: deterministic artifact checks that validate a close proxy for the intended UX but do not fully prove operator readability.
- `Tier 3 Human-Minimal`: a bounded human review of a named artifact with explicit pass/fail criteria.

Required policy:

- Every deliverable starts at Tier 1.
- Tier 2 is allowed only when ratatui layout or rendering behavior is expensive to prove end-to-end without a proxy artifact.
- Tier 3 is allowed only for final visual/operator sanity that cannot be reduced to state transitions or snapshots.
- A Tier 2 or Tier 3 item must explain why Tier 1 alone is insufficient.

### Repo-grounded strategy

The current repo has strong coverage for normalization, stale tracking, and CLI integration in [`src/normalizer.rs`](/home/anders/.openclaw/workspace/dev/openclaw-logpulse-worktrees/w25-verification-plan/src/normalizer.rs), [`src/stale.rs`](/home/anders/.openclaw/workspace/dev/openclaw-logpulse-worktrees/w25-verification-plan/src/stale.rs), and [`tests/logpulse_integration.rs`](/home/anders/.openclaw/workspace/dev/openclaw-logpulse-worktrees/w25-verification-plan/tests/logpulse_integration.rs), but no TUI tests and no derived entity model yet.

Cheapest practical verification in this repo:

- Pure state-machine/unit tests for route, follow/pin, filter, correlation, and session derivation.
- Small fixture-driven integration tests for ingest plus derived projection correctness.
- Ratatui snapshot/golden tests for header, tab rows, inspector, expanded detail, and help overlay.
- Very limited human-minimal review only if snapshot tests cannot adequately prove layout intent.

### Expected future test seams

Implementation may organize modules differently, but the verification plan assumes the implementation exposes seams equivalent to:

- a derived store/index layer for normalized events, correlated calls, and session summaries
- a route/state reducer for tabs, drilldown, overlays, and follow/pin behavior
- a machine-readable keymap/action table shared by input dispatch and contextual help generation
- a rendering surface that can render ratatui buffers off-screen for snapshot tests

Required invariant across all seams:

- Any test that renders a selected row, inspector, expanded detail, or drilled dataset must assert stable entity IDs and backing derived entities, not only route state, row counts, or snapshot text.

If implementation keeps all behavior inside [`src/tui.rs`](/home/anders/.openclaw/workspace/dev/openclaw-logpulse-worktrees/w25-verification-plan/src/tui.rs), the same checks still apply, but the tests should target extracted internal helpers instead of only black-box terminal sessions.

## 2. Deliverable coverage matrix

| Deliverable from architecture spec | Primary verification shape | Minimum tier |
| --- | --- | --- |
| Fixed tabs, default tab, per-tab remembered state | state-machine tests + buffer snapshots | Tier 1 |
| Sessions -> Calls -> Events drilldown | state-machine tests + scoped dataset tests | Tier 1 |
| Right inspector vs expanded detail mode | state-machine tests + rendering snapshots | Tier 1 |
| Contextual help by mode/layer | snapshot tests + keymap state tests | Tier 1 |
| LIVE vs PINNED behavior and unseen counts | state-machine tests | Tier 1 |
| Session ordering and scoping | derived-model unit tests + fixture integration | Tier 1 |
| Correlated tool-call aggregation and confidence | derived-model unit tests + fixture integration | Tier 1 |
| Heartbeat hidden from timeline by default, derived health visible elsewhere | derived-model tests + snapshots | Tier 1 |
| Real `since` / `until` filtering | shared filter tests + CLI integration | Tier 1 |
| `stale_only` and other first-class filter semantics | filter-composition tests + tab projection tests | Tier 1 |
| Narrow-screen inspector collapse behavior | off-screen render snapshots at multiple widths | Tier 2 |
| Final operator readability sanity for dense views | optional screenshot review artifact | Tier 3 only if snapshots prove flaky |

## 3. Required verification items

Each item below is binding. If implementation changes structure, it must still satisfy the same checks.

### V1. Tab model and remembered state

- What it checks:
  - Startup lands on `Events`.
  - Tab order is exactly `Events`, `Correlated Tool Calls`, `Sessions`.
  - `h`/`l` and `1`/`2`/`3` switch tabs correctly.
  - Each tab remembers cursor, scroll offset, follow state, unseen count, and search match index independently.
  - Pinned selection is anchored to stable entity identity, not current list index.
- How it runs:
  - State-machine tests feed a fixed event/call/session dataset into app state, simulate key sequences, and assert route plus per-tab state.
  - Live-update tests prepend new rows ahead of a pinned selection and assert the selected entity ID is unchanged before and after ingest.
  - Snapshot tests render the header/body/footer after tab switches to prove tab labels, status badges, and remembered selection state appear in the buffer.
- Pass/fail signal:
  - Pass if state assertions, selected entity IDs, and snapshots are unchanged.
  - Fail on any tab order drift, state leakage between tabs, missing indicator text, or pinned-selection identity drift under prepend churn.
- Tier:
  - `Tier 1 Autonomous`.
- Wrong-but-plausible failures this must catch:
  - One global `TableState` reused across tabs.
  - Tab jump keys switching content but losing pinned/live state.
  - Search position or unseen counts resetting on every tab visit.

### V2. Drilldown routing and breadcrumb restoration

- What it checks:
  - `Sessions -> Enter -> Correlated Tool Calls` applies exact `session_id` scope.
  - `Correlated Tool Calls -> Enter -> Events` applies exact correlated-call scope.
  - `Events -> Enter` and `o` open expanded detail.
  - `Esc`/`q` unwind one layer and restore previous tab, cursor, scroll, and follow state.
  - Breadcrumb text matches the route contract.
  - The drilled datasets themselves are correct, including inspector/detail backing entities and absence of out-of-scope derived notices.
- How it runs:
  - State-machine tests drive route transitions over a deterministic multi-session fixture with repeated tool names, repeated call IDs across sessions, and two source files sharing the same `session_key` but different source-path UUIDs.
  - For `Sessions -> Correlated Tool Calls`, tests assert the exact visible `call_entity_id` set after drilldown.
  - For `Correlated Tool Calls -> Events`, tests assert the exact visible `event_ref` set after drilldown and the same `event_ref` or entity ID in list row, inspector, and expanded detail.
  - Negative assertions prove stale warnings and other derived notices from outside the selected session/call are absent from drilled views.
  - Snapshot tests render header breadcrumbs for root, session-scoped, call-scoped, and detail routes.
- Pass/fail signal:
  - Pass if route stack, exact visible entity-ID sets, restored state, inspector/detail backing entities, and breadcrumb strings match expectations.
  - Fail if drilldown replaces unrelated filters, loses parent selection, uses label-based rather than durable-ID scope, or shows an out-of-scope event/warning anywhere in the drilled route.
- Tier:
  - `Tier 1 Autonomous`.
- Wrong-but-plausible failures this must catch:
  - Using substring session filtering for drilldown instead of exact identity.
  - Returning from detail to the wrong tab or the newest row.
  - Breadcrumbs showing a session label while the underlying scope uses a different session.

### V3. Inspector vs expanded detail mode

- What it checks:
  - Default workspace uses split view with list plus read-only inspector.
  - Inspector updates with list selection but never steals focus.
  - Expanded detail is a separate fullscreen layer with independent scroll behavior.
  - `q` quits only from workspace; otherwise it closes detail/overlay first.
  - Under live ingest, inspector and expanded detail remain bound to the same selected entity ID as the pinned list row.
- How it runs:
  - State-machine tests assert focus layer and key handling.
  - Live-update tests open inspector and expanded detail on a pinned entity, prepend newer rows, and assert that list row, inspector payload, and detail payload still resolve to the same entity ID after each ingest step.
  - Snapshot tests render split mode and expanded detail mode from the same selected entity.
  - Buffer assertions verify inspector title/content and fullscreen detail title/content are distinct.
- Pass/fail signal:
  - Pass if split and detail layers coexist with the required unwind behavior and entity identity remains stable under live updates.
  - Fail if inspector becomes a second interactive pane, if detail reuses the split layout, if `q` exits the app from detail, or if inspector/detail silently drift to a different entity after prepends.
- Tier:
  - `Tier 1 Autonomous`.
- Wrong-but-plausible failures this must catch:
  - Making `Enter` merely scroll the inspector longer instead of opening detail.
  - Accidentally sharing one scroll offset between list preview and fullscreen detail.
  - Allowing focus to land in the inspector and breaking `j/k` list navigation.

### V4. Contextual help by mode and layer

- What it checks:
  - `?` opens help overlay from workspace and detail.
  - Help content includes global bindings, active-tab bindings, active-layer bindings, and one-line state summary.
  - `Esc`, `q`, and `?` close help without mutating underlying route state.
  - Help is generated from the real active keymap/actions, not a hand-maintained text template.
- How it runs:
  - State-machine tests open help from multiple routes and verify overlay stacking/unwinding.
  - Help assertions derive the expected advertised key set from the same machine-readable keymap table used by input dispatch, then compare that set to rendered help content.
  - Action tests prove every advertised key in that mode has a non-noop handler with the documented effect.
  - Inverse tests prove inactive keys for that mode are not advertised.
  - Modal conflict tests cover `q` and `Esc` across workspace, detail, and help overlay.
  - Snapshot tests capture help overlay content for `Events`, session-scoped `Correlated Tool Calls`, and expanded detail.
- Pass/fail signal:
  - Pass if mode-specific help text changes with state, matches the active keymap table exactly, and overlay close leaves underlying state untouched.
  - Fail if help is a static cheat sheet, advertises dead keys, omits active keys, or omits breadcrumb/live state context.
- Tier:
  - `Tier 1 Autonomous`.
- Wrong-but-plausible failures this must catch:
  - One global help modal that never changes across tabs.
  - Help closing by resetting the route to workspace root.
  - Missing `LIVE` or `PINNED` context, leaving users unable to interpret current state.

### V5. LIVE vs PINNED behavior

- What it checks:
  - Every tab starts `LIVE`.
  - Manual navigation flips only the current tab to `PINNED`.
  - `f` resumes `LIVE` and clears unseen count for the current tab.
  - While pinned, new rows append without moving selection and unseen count increments only for the affected tab.
  - `Sessions -> Correlated Tool Calls` starts `LIVE` within the scoped session; other entity-specific drilldowns start `PINNED`.
  - While pinned, inspector and expanded detail stay bound to the selected entity ID even as live ingest prepends rows ahead of it.
- How it runs:
  - State-machine tests simulate ingest while tabs are live or pinned.
  - Integration-style reducer tests append events after manual navigation and assert selected entity IDs, inspector/detail entity IDs, and unseen counters.
  - A dedicated route test covers `Sessions -> Correlated Tool Calls` starting `LIVE`, manual navigation flipping that route to `PINNED`, and `f` resuming `LIVE` without losing session scope.
- Pass/fail signal:
  - Pass if selection movement, entity-ID stability, and unseen counts match the contract exactly.
  - Fail if unseen counts increment in `LIVE`, if one tab’s manual movement pins all tabs, if drilldown default follow mode is wrong, or if pinned inspector/detail content drifts while the row appears stable.
- Tier:
  - `Tier 1 Autonomous`.
- Wrong-but-plausible failures this must catch:
  - Tracking pin/live as one global bool, like the current `follow_tail` in [`src/tui.rs`](/home/anders/.openclaw/workspace/dev/openclaw-logpulse-worktrees/w25-verification-plan/src/tui.rs).
  - Incrementing unseen counts for rows filtered out of the current tab.
  - Resetting unseen counts on tab switch instead of on `f`.

### V6. Session ordering, identity, and scoping

- What it checks:
  - Session grouping uses durable `session_id`, not arbitrary `session_key` override.
  - Session rows expose the required fields from the architecture spec.
  - Default ordering is `last_activity_at desc`, `stale_call_count desc`, `open_call_count desc`, `session_id asc`.
  - Session drilldown scopes by exact session identity and preserves unrelated filters.
- How it runs:
  - Derived-model unit tests build session summaries from mixed fixtures where `session_key` conflicts with the source-path UUID.
  - Fixture-driven integration tests ingest transcript-v3 and generic JSON from multiple session files and assert grouping/order.
- Pass/fail signal:
  - Pass if row identities and ordering stay stable across ingest order permutations.
  - Fail if sessions fragment by label, merge by shared label, or reorder nondeterministically.
- Tier:
  - `Tier 1 Autonomous`.
- Wrong-but-plausible failures this must catch:
  - Grouping by `session_key` because it looks human-friendly.
  - Sorting only by last visible row insertion order.
  - Losing sessions with no current open calls but visible historical activity in the filtered window.

### V7. Correlated tool-call aggregation and match confidence

- What it checks:
  - One correlated row exists per tool invocation within a session.
  - Explicit `(session_id, canonical_call_id)` identity wins over fallback heuristics.
  - Calls never correlate across sessions, even with colliding call IDs.
  - Result-only events appear as `unknown` or `incomplete`, not dropped.
  - Fallback correlation closes only the oldest matching open call and marks `match_confidence=fallback_signature`.
  - Multi-tool transcript assistant messages fan out to one correlated call per tool call.
  - Materialized correlated-call identities exactly match the fixture oracle, not just row count or visible labels.
  - Transcript-bundle rows are labeled distinctly from explicit-ID and fallback-signature rows.
- How it runs:
  - Derived-model unit tests for explicit-ID, fallback-signature, transcript-bundle, repeated no-ID calls, and result-only events.
  - Fixture integration tests ingest synthetic transcript-v3 fixtures with at least one assistant message containing three `toolCall` items where only the second receives a result first.
  - Tests assert the full materialized identity set, using exact `(session_id, canonical_call_id)` or `(session_id, fallback_signature, ordinal)` keys.
  - Tests assert each materialized call row has its own status, severity, event refs, confidence label, and drilldown scope.
- Pass/fail signal:
  - Pass if the exact materialized identity set, statuses, severities, event refs, confidence labels, and per-call drilldown scopes match the fixture oracle.
  - Fail if correlation crosses sessions, if later no-ID calls close before older ones, if extra transcript tool calls disappear into loose `correlation_ids`, or if any fallback-closed call is silently upgraded to a stronger confidence.
- Tier:
  - `Tier 1 Autonomous`.
- Wrong-but-plausible failures this must catch:
  - A naive hash on `call_id` only.
  - Treating `fallback_signature` matches as high-confidence without exposing that fact.
  - Rendering one row for a whole transcript message instead of one row per tool call.

### V8. Heartbeat-derived health and hidden-from-timeline default

- What it checks:
  - Heartbeat remains a derived observation layer by default, not a default `Events` row.
  - Header surfaces global session counts for `busy`, `stale`, and `disconnected`.
  - Session rows surface `health_status`.
  - `include_system_events=true` reveals heartbeat/system rows in `Events`.
  - `idle` vs `disconnected` depends on freshness/source state, not just open-call count.
  - Session freshness/state fields are correct under time advancement:
    - `last_event_at`
    - `last_source_seen_at`
    - `source_state`
    - `health_status`
- How it runs:
  - Derived-model tests simulate active, idle, stale, disconnected, and unknown sessions using controllable timestamps and source disappearance signals.
  - Fixture coverage includes source disappeared, source still exists but silent, late event after disconnect, and disconnected sessions with zero open calls.
  - Time-advanced tests assert a session can transition `busy -> idle -> disconnected` without opening any new calls.
  - Negative assertions prove open-call count alone cannot keep a session `active` after freshness expiry.
  - Snapshot tests render headers and session rows with system events hidden and visible.
- Pass/fail signal:
  - Pass if default `Events` excludes heartbeat rows while the underlying freshness/state fields and rendered health status stay correct.
  - Fail if heartbeat rows always flood the event timeline, if quiet/disconnected sessions collapse into the same state, or if rendered health badges disagree with the underlying freshness/state fields.
- Tier:
  - `Tier 1 Autonomous`.
- Wrong-but-plausible failures this must catch:
  - Reusing current `push_heartbeat()` behavior from [`src/tui.rs`](/home/anders/.openclaw/workspace/dev/openclaw-logpulse-worktrees/w25-verification-plan/src/tui.rs) and merely recoloring the row.
  - Computing health solely from `StaleTracker::heartbeat()` counts.
  - Marking a session `active` forever because its source was once seen.

### V9. Real `since` / `until` filtering

- What it checks:
  - `since` and `until` are enforced, inclusive.
  - The same time-window semantics apply in non-TUI output and all TUI tabs.
  - Correlated calls and sessions use overlap semantics rather than only start time.
  - Events, correlated calls, sessions, stale notices, and detail mode all honor the same time-window oracle.
- How it runs:
  - Shared filter unit tests for raw events, correlated calls, and session summaries with edge timestamps at exact bounds.
  - CLI integration tests invoke the binary with `--since` and `--until` against controlled fixtures and assert output changes.
  - One shared cross-projection oracle fixture is evaluated under the same `since` / `until` values and asserts exact visible entity-ID sets for `Events`, `Correlated Tool Calls`, `Sessions`, stale notices, and expanded detail mode.
  - The oracle fixture must include:
    - a call that starts before `since`, ends after `since`, and emits a stale warning after `since`
    - a session with only pre-window events except for a post-window stale or disconnect transition
    - detail-mode selection looked up from the filtered/scoped store, not an unfiltered global store
  - Derived projection tests assert a long-running call that overlaps the window still appears in `Correlated Tool Calls` and `Sessions`.
- Pass/fail signal:
  - Pass if the dataset shrinks/expands exactly at inclusive boundaries and every projection, stale notice, and detail payload matches the same oracle.
  - Fail if header text advertises time filters that do nothing, if any projection cheats with different overlap rules, or if detail mode resolves an out-of-window backing entity.
- Tier:
  - `Tier 1 Autonomous`.
- Wrong-but-plausible failures this must catch:
  - Applying the window to event ingestion in one path but not another.
  - Filtering correlated calls by `started_at` only and dropping overlapping calls.
  - Applying time filters to events but not stale warnings or session summaries.

### V10. First-class filter semantics, including `stale_only`

- What it checks:
  - Structured filters cover session scope, tool scope, severity, time window, text query, `stale_only`, and `include_system_events`.
  - Filter semantics are consistent across `Events`, `Correlated Tool Calls`, and `Sessions`.
  - Stale warnings honor the same active filters as the current tab.
  - Severity on correlated calls/sessions derives from member events, not a separate ad hoc status.
- How it runs:
  - Filter-composition unit tests feed the same base dataset through all three projections.
  - Integration tests assert tab projections after combining severity, stale-only, time-window, and text filters.
  - Snapshot tests verify visible filter summary text matches the actual active structured state.
- Pass/fail signal:
  - Pass if each projection returns the expected entity IDs and counts under composed filters.
  - Fail if stale warnings bypass agent/time filters, if sessions use different text-search semantics than events, or if summaries advertise filters inconsistently.
- Tier:
  - `Tier 1 Autonomous`.
- Wrong-but-plausible failures this must catch:
  - Keeping the current split where stale warnings only honor session/tool filters.
  - Implementing `stale_only` as a visual highlight instead of a dataset restriction.
  - Letting each tab invent its own text-search field set.

### V11. Narrow-screen split fallback

- What it checks:
  - When terminal width is too small, the inspector collapses below the list while preserving the logical split-view contract.
  - Header, tabs, and focus markers remain legible at narrow widths.
- How it runs:
  - Off-screen render snapshots at representative widths, for example 80x24, 120x30, and one narrow width below the side-by-side threshold.
- Pass/fail signal:
  - Pass if snapshots show the inspector moved below the list without truncating route/status context beyond the expected snapshot baseline.
  - Fail if the inspector disappears completely, overlaps the list, or loses selection context.
- Tier:
  - `Tier 2 Good Proxy`.
- Why not Tier 1 only:
  - Buffer geometry can be asserted autonomously, but a snapshot is the cheapest repo-local proof that the collapse still reads correctly.
- Wrong-but-plausible failures this must catch:
  - Hiding the inspector with no alternate detail surface.
  - Keeping the horizontal split until columns become unreadable.

## 4. Recommended test inventory by repo surface

### Add or expand unit/state tests

- Extend [`src/stale.rs`](/home/anders/.openclaw/workspace/dev/openclaw-logpulse-worktrees/w25-verification-plan/src/stale.rs) tests for oldest-open fallback completion and session-isolated correlation.
- Add derived-model tests near the future aggregation layer for:
  - correlated-call fan-out from transcript multi-tool messages
  - exact materialized correlated-call identity sets and confidence labeling
  - result-only call rows
  - session identity precedence and ordering
  - session/source freshness and health-status derivation
  - inclusive time-window overlap behavior
- Add route reducer/state tests near the future TUI state layer for:
  - tab switching
  - drilldown/unwind
  - per-tab remembered state
  - live/pinned unseen behavior
  - stable selected entity IDs across live prepends
  - help/detail overlay stacking
  - contextual help parity with the machine-readable keymap table

### Add integration tests

- Extend [`tests/logpulse_integration.rs`](/home/anders/.openclaw/workspace/dev/openclaw-logpulse-worktrees/w25-verification-plan/tests/logpulse_integration.rs) with binary-level coverage for:
  - real `--since` / `--until`
  - transcript multi-tool-call inputs
  - result-only correlated calls surviving to output or internal projection harness
  - multi-session call ID collision isolation
  - a shared cross-projection time-window oracle fixture

### Add snapshot/golden tests

- Add ratatui buffer snapshots for:
  - workspace root on each tab
  - session-scoped calls tab
  - call-scoped events tab
  - expanded detail
  - contextual help
  - narrow-width split fallback

Implementation detail:

- Prefer text/buffer snapshots over pixel screenshots. They are cheaper, deterministic, and fit this Rust repo better.

## 5. Human-minimal review requirement

Default position:

- No Tier 3 human review is required if Tier 1 and Tier 2 checks above are implemented with stable snapshots.

Conditional Tier 3 only if needed:

- Artifact:
  - one captured terminal transcript or screenshot set showing `Events`, `Correlated Tool Calls`, `Sessions`, expanded detail, and help overlay at one normal width and one narrow width
- Pass/fail criteria:
  - breadcrumbs are readable
  - `LIVE` / `PINNED` status is immediately visible
  - right inspector and fullscreen detail are visually distinct
  - stale/disconnected emphasis is visible without showing heartbeat spam in default events
  - no clipped or overlapping text obscures selection context

Tier 3 is only justified if buffer snapshots prove too brittle to judge final layout readability.

## 6. Adversarial fixture requirements

The implementation wave must not rely only on happy-path fixtures. Verification must include fixtures with:

- two sessions sharing the same explicit `call_id`
- repeated same-session same-tool same-argument calls with no explicit IDs
- a result arriving before any matching start
- one transcript assistant message containing multiple `toolCall` items
- conflicting `session_key` vs source-path session UUID
- heartbeat/system events interleaved with normal events
- stale calls inside and outside active filters
- calls spanning `since`/`until` boundaries
- two source files sharing the same `session_key` but different path UUIDs
- a transcript message with at least three `toolCall` items and out-of-order results
- live prepends while a pinned inspector and expanded detail are open

Without these fixtures, naive implementations will appear correct while violating the architecture contract.

## 7. Residual blind spots

These are the only acceptable remaining blind spots after Tier 1 and Tier 2 automation:

- Terminal-specific font rendering, color theme interpretation, and emulator quirks are not fully provable in CI.
- Ratatui snapshot tests prove layout structure and visible text, but not subjective readability under every terminal theme.
- Extremely timing-sensitive races between file-discovery churn and live ingest may still require separate implementation-wave stress coverage beyond this doc-focused contract.

Those blind spots do not justify broad manual testing. They justify only the narrow Tier 3 artifact described above if snapshot evidence is insufficient.
