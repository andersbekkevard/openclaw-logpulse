# TUI v1 Verification Plan Adversarial Review

Issue: `openclaw-logpulse-a5k.9.2`

## Verdict

The current verification plan is directionally strong, but it still leaves several realistic escape hatches for an implementation that looks right in snapshots while lying about identity, freshness, or scoped datasets. The biggest weakness is that multiple items prove UI state and rendered text, but not enough items force cross-checks between rendered state and the underlying derived entities.

That matters in this repo because the current code already has the exact failure ingredients the future implementation is likely to inherit:

- `src/normalizer.rs` still lets `session_key` override path-derived session identity.
- `src/normalizer.rs` still collapses transcript multi-tool messages into one primary call plus loose `correlation_ids`.
- `src/tui.rs` still filters stale warnings on a separate path from normal events.
- `src/tui.rs` still inserts heartbeat summaries directly into the timeline.
- `src/stale.rs` heartbeat still derives “activity” from open calls, not source freshness.

If the implementation wave merely wraps those behaviors in prettier state and snapshots, the current plan will miss some wrong-but-plausible versions.

## Bypass 1: Breadcrumbs and tab rows can tell the truth while the scoped dataset is wrong

### How the bad implementation passes

An implementation can:

- keep a correct route stack
- render the correct tab, breadcrumb, and selected row labels
- open `Sessions -> Correlated Tool Calls -> Events` on `Enter`
- preserve parent cursor/scroll/live state exactly as V2 expects

while still populating the drilled dataset with the wrong rows.

Two realistic variants:

1. Scope drilldown by display label or substring instead of exact durable identity.
2. Apply session scope correctly in the route reducer, but forget to apply it in one projection path such as inspector detail, unseen-count updates, or stale-warning rows.

The repo is already primed for this failure. `src/normalizer.rs` sets `session_id = session_key.clone().or(source_context.session_id.clone())`, and `src/event.rs` filtering prefers `session_key` before `session_id`. A future implementation can therefore show the breadcrumb for the selected session row while actually grouping or filtering by the wrong namespace.

### Why it is still wrong

The operator believes drilldown is exact, but rows can leak in from:

- another session with the same human-friendly `session_key`
- the same session label across different source files
- stale warnings or derived notices that bypass the structured drilldown filter

This is exactly the kind of “looks plausible in snapshots” failure the verification plan is supposed to stop.

### Strengthening required

- Add entity-ID assertions for every drilldown fixture, not just route-stack assertions.
- For `Sessions -> Correlated Tool Calls`, assert the exact set of `call_entity_id`s visible after drilldown.
- For `Correlated Tool Calls -> Events`, assert the exact set of `event_ref`s visible after drilldown, including inspector/detail payload source.
- Add a fixture where two source files share the same `session_key` but have different path UUIDs, and require drilldown to isolate by path UUID.
- Add a negative assertion that stale warnings and other derived notices outside the selected session are absent from drilled views.

## Bypass 2: Correlated call rows can look right while correlation is still wrong

### How the bad implementation passes

An implementation can render one clean row per visible tool call and satisfy many of V7’s happy-path assertions while still being wrong in three ways:

- collapse a multi-tool transcript assistant message into one correlated row based on the first `toolCall`
- silently attach extra tool-call IDs as “related” metadata without ever materializing distinct rows
- mark ambiguous fallback matches as if they were explicit or bundle-backed

This is not speculative. The current transcript normalization in `src/normalizer.rs` promotes only the first `toolCall` item to `call_id` and turns later IDs into `correlation_ids`. A future aggregator can easily preserve that shape, render a convincing row, and still drop real invocations.

### Why it is still wrong

The architecture spec requires one correlated row per invocation within one session, with `match_confidence` exposing whether identity came from explicit ID, transcript bundle, or fallback signature. A row that stands in for an entire assistant message is false accounting. It hides:

- missing tool rows
- wrong per-call status
- wrong duration and severity derivation
- wrong drilldown from call row to event scope

### Strengthening required

- Upgrade V7 from “rows/statuses/event refs match the fixture oracle” to “materialized call identities exactly equal fixture oracle”.
- Require assertions on the full set of `(session_id, canonical_call_id or fallback_signature+ordinal)` identities, not just row counts.
- Add a fixture with one transcript message containing at least three `toolCall` items, where only the second receives a result first.
- Require assertions that each tool call gets its own row, its own status transition, and its own drilldown scope.
- Require assertions that any fallback-closed call is labeled `match_confidence=fallback_signature`, never upgraded silently.

## Bypass 3: Session health can look alive because heartbeat badges are decoupled from source freshness

### How the bad implementation passes

An implementation can:

- hide heartbeat rows by default
- render header counts for `busy`, `stale`, and `disconnected`
- show a plausible `health_status` badge per session
- satisfy snapshots for header/session rows

while deriving all of that from stale-tracker/open-call state instead of actual source freshness.

That bad implementation is a natural copy-forward of current behavior. `src/stale.rs` heartbeat only knows `active_calls`, `stale_calls`, and active sessions inferred from open calls. `src/tui.rs` then treats heartbeat as status text plus a timeline row. Nothing in the current code tracks “source still present and recently producing events” as a first-class session fact.

### Why it is still wrong

A quiet disconnected session with zero open calls can be mislabeled as healthy idle. A source that disappeared can keep looking alive if the last known session summary never decays. The operator sees reassuring status badges even though the data source is stale.

### Strengthening required

- Expand V8 fixtures to include: source disappeared, source still exists but silent, late event after disconnect, and no-open-call disconnected session.
- Assert the underlying session fields, not only rendered badges:
  - `last_event_at`
  - `last_source_seen_at`
  - `source_state`
  - `health_status`
- Require a time-advanced test where a session transitions `busy -> idle -> disconnected` without any new calls being opened.
- Require a negative assertion that open-call count alone cannot keep a session in `active` after freshness expiry.

## Bypass 4: `since` / `until` can look wired globally while one projection cheats

### How the bad implementation passes

An implementation can:

- show the active time window in the header
- apply it to raw events
- make event-tab snapshots and CLI integration pass
- even make some correlated/session fixtures pass

while still cheating in one of these ways:

- sessions are included if they have any historical activity, regardless of overlap
- correlated calls are filtered by `started_at` only
- stale warnings and derived notices ignore the time window
- inspector/detail mode still resolves the selected entity from the unfiltered global store

The repo already has a version of this split-brain problem. `src/tui.rs` filters normal events through `event.should_filter(...)`, but stale warnings use a separate session/tool-only filter path, and `since` / `until` are currently summary text only.

### Why it is still wrong

The UI advertises one global time scope but different tabs and overlays are actually showing different datasets. This is a classic verification failure because snapshots often prove only the visible list, not the selected-detail backing entity or hidden derived notices.

### Strengthening required

- Add one shared cross-projection oracle fixture and assert the visible entity IDs for Events, Calls, Sessions, stale notices, and detail view under the same time window.
- Require a call that starts before `since`, ends after `since`, and receives a stale warning after `since`.
- Require a session with only pre-window events except for a post-window stale/disconnect change, and define whether it should appear.
- Add a detail-mode assertion that the expanded event/call/session detail is also filtered/scoped, not looked up from the unfiltered store.

## Bypass 5: Contextual help can be mode-specific text that still advertises dead keys

### How the bad implementation passes

V4 currently proves that help changes by route/layer and that closing help does not mutate route state. That is necessary, but not sufficient. An implementation can generate help from a static per-mode template that is never checked against the actual active reducer/keymap.

Example bad behavior:

- help in session-scoped calls view says `Enter` drills into events, but current mode is pinned detail and `Enter` does nothing
- help in detail mode says `f` resumes `LIVE`, but detail mode intercepts that key or ignores it
- help advertises `q` as close-current-layer, but a conflicting handler exits the app from detail

Snapshots still pass because the text changes with mode. State tests still pass because they only exercise a subset of keys, not the full advertised contract.

### Why it is still wrong

A contextual help overlay that lies is worse than a static cheat sheet. It creates operator trust in key bindings that do not work in the active mode.

### Strengthening required

- For each help snapshot variant, derive the expected key list from the same source used by input dispatch, or assert it against a machine-readable keymap table.
- Add a test that every key advertised in contextual help for that mode has a non-noop handler with the documented effect.
- Add the inverse test: keys not active in that mode must not be advertised.
- Add at least one modal conflict fixture where detail mode, help overlay, and workspace each bind `q`/`Esc` differently.

## Bypass 6: Inspector and pinned state can drift under live updates while snapshots remain plausible

### How the bad implementation passes

An implementation can preserve the visible selected row in a pinned tab and keep the inspector rendering believable text, while the inspector is actually resolving data by current list index rather than stable entity ID. Under live ingest:

- newly inserted rows shift indices
- the pinned list selection appears unchanged
- the inspector silently starts showing the wrong call or event

This is realistic in this repo because the current TUI is index-driven: one `VecDeque<TimelineItem>`, one `TableState`, and selection adjustment on prepend. A naive v1 can replicate that behavior per tab and still satisfy many snapshot checks.

### Why it is still wrong

Pinned mode promises “selection remains fixed while new rows append.” That promise is about entity identity, not just cursor position. If the inspector/detail pane drifts to a different entity during prepend churn, the operator is reading lies.

### Strengthening required

- In V1, V3, and V5, assert stable selected entity IDs before and after ingest, not only selected indices.
- Add a live-update fixture where new rows are prepended ahead of the pinned selection while inspector and expanded detail are open.
- Require assertions that list row, inspector content, and detail content all resolve to the same entity ID after each ingest step.
- Add a pinning test for `Sessions -> Correlated Tool Calls` because that route intentionally starts `LIVE` while other entity-specific drilldowns start `PINNED`.

## What Already Looks Robust

Some parts of the verification plan are already shaped correctly.

### Route unwind and per-tab remembered state

V1, V2, and V5 explicitly call out wrong-but-plausible failures like one global `TableState`, wrong unwind target, and incorrect live/pinned defaults. That is good. Those items are grounded in the current `src/tui.rs` monolith and are already forcing the implementation away from a superficial tab shell.

### Narrow-screen layout as Tier 2 snapshot only

V11 is correctly scoped. Narrow-width rendering is exactly the kind of thing where buffer snapshots are the cheapest practical proof in this repo. The plan does not over-promise more than autonomous geometry plus readable snapshots can prove.

### Adversarial fixtures section

Section 6 is the strongest part of the document. It already names most of the high-risk fixture shapes this repo needs. The problem is not fixture selection. The problem is that several verification items still need stronger entity-level assertions so those fixtures cannot be “passed” by plausible-looking but dishonest projections.

## Highest-priority strengthenings

If only a few changes are made before implementation, make these:

1. Add entity-ID oracle assertions to every drilldown, pinning, and detail test. Do not stop at route state and snapshots.
2. Strengthen V7 so it proves exact correlated-call materialization, including transcript multi-tool fan-out and fallback-confidence labeling.
3. Strengthen V8 so health status is proven from source freshness fields, not inferred from open-call counts alone.
4. Add cross-projection time-window assertions so Events, Calls, Sessions, stale notices, and detail mode all honor the same `since` / `until` semantics.
5. Make contextual help test against the real active keymap, not hand-maintained snapshot text.

## Bottom line

The plan is not yet good enough to proceed unchanged. It is close, but still vulnerable to implementations that keep state and text coherent while projecting the wrong underlying entities. After the strengthenings above, especially the entity-ID, source-freshness, and cross-projection assertions, it should be good enough to proceed to implementation.
