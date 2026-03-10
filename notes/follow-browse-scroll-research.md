# Follow/Browse Scroll Research

## Scope

This note maps the current TUI navigation state in `src/tui.rs` and `src/projection.rs`, explains why repeated `j` produces jumpy back-and-forth scrolling, and outlines a minimal architecture for the requested FOLLOW/BROWSE semantics across Events, Tool Calls, and Sessions.

## 1. Current state ownership

### Per-tab navigation state

`TabStateModel` is the core navigation state for each pane. It owns:

- `selected`: current entity key
- `scroll_offset`: table viewport offset
- `follow_mode`: `Live` or `Pinned`
- `unseen_count`: count of new rows while pinned
- `search_match_index`
- `scope`: drilldown filters for session/call context

Citations:

- `src/tui.rs:93-123`
- `src/tui.rs:477-512`

At the app level, the active pane and navigation stack live in:

- `current_tab`
- `tabs`
- `route_stack`
- `detail`

Citations:

- `src/tui.rs:477-512`
- `src/tui.rs:529-542`
- `src/tui.rs:981-1003`

### Selection, offset, and mode transitions

The current list state is driven by these functions:

- `selected_index` resolves the selected entity into the current row index: `src/tui.rs:931-937`
- `move_selection` moves `selected`, forces `follow_mode = Pinned`, and sets `scroll_offset = next_index.saturating_sub(3)`: `src/tui.rs:948-959`
- `jump_to` does the same for absolute jumps: `src/tui.rs:961-971`
- `resume_live` switches back to `Live`, clears unseen, resets offset to `0`, and reselects the first row: `src/tui.rs:939-946`
- `reconcile_after_data_change` re-applies state after every ingest: `src/tui.rs:603-645`

Rendering consumes that state directly:

- `render_list` rebuilds a fresh `TableState` every frame
- `tab_state.scroll_offset` is passed to `TableState::with_offset(...)`
- `selected_index` is passed to `state.select(...)`

Citations:

- `src/tui.rs:1782-1851`

### Key handling and current semantics

Current bindings are single-key only:

- `j` / `Down` => next row
- `k` / `Up` => previous row
- `g` => first row
- `G` / `End` => last row
- `f` => resume live

Citations:

- `src/tui.rs:319-366`
- `src/tui.rs:403-409`

`resolve_action` matches a single `KeyEvent` against `KEY_BINDINGS`; there is no multi-key state machine, so `gg` is not currently representable.

Citations:

- `src/tui.rs:1509-1517`
- `src/tui.rs:1527-1544`

The UI currently labels the two modes as `LIVE` and `PINNED`, both in the help overlay and tab header.

Citations:

- `src/tui.rs:1358-1364`
- `src/tui.rs:1708-1728`

## 2. Which functions own each pane's rows

### Events

Events are built in `visible_event_rows`. That function:

- reads `Tab::Events` scope
- optionally constrains by `scope.session_id`
- optionally resolves `scope.call_entity_id`
- appends notice rows
- sorts the final list newest-first by `sort_at`

Citations:

- `src/tui.rs:655-730`

The underlying event data comes from `ProjectionStore::event_rows`.

Citation:

- `src/projection.rs:209-240`

### Tool Calls

Tool Calls are built in `visible_call_rows`. That function filters `ProjectionStore::correlated_calls(...)` by the current session scope and turns each call into an `EntityKey::Call`.

Citations:

- `src/tui.rs:831-877`

The underlying call list is sorted newest-first in `ProjectionStore::correlated_calls`, using `started_at.or(last_updated_at)`.

Citations:

- `src/projection.rs:242-260`

### Sessions

Sessions are built in `visible_session_rows`, which wraps `ProjectionStore::sessions(...)` and turns each row into an `EntityKey::Session`.

Citations:

- `src/tui.rs:879-918`

The underlying session list is sorted newest-first in `ProjectionStore::sessions`, using `last_activity_at` and then stale/open-call tie breakers.

Citations:

- `src/projection.rs:263-370`

## 3. The current implementation does mix follow updates with manual navigation

Yes. That is happening now.

Evidence:

1. Manual navigation switches a pane from `Live` to `Pinned` in `move_selection` and `jump_to`: `src/tui.rs:956-958`, `src/tui.rs:968-970`.
2. Incoming data still rebuilds the live row list for every tab via `reconcile_after_data_change`: `src/tui.rs:603-645`.
3. In pinned mode, reconciliation preserves the selected entity if it still exists, but it does not preserve a stable browse snapshot and it does not recompute `scroll_offset` when the selected row's index changes: `src/tui.rs:625-643`.
4. All three panes keep using live-sorted data sources while pinned:
   - Events: `src/tui.rs:724-728`
   - Calls: `src/projection.rs:254-259`
   - Sessions: `src/projection.rs:363-369`

So "manual navigation" does not isolate the pane from live reordering. It only changes how selection is reconciled.

## 4. Root cause of the jumpy repeated-`j` behavior

The jump is caused by stale viewport state during pinned browsing while live ingestion keeps reordering rows underneath the selection.

### Exact sequence

1. The main loop processes input first, then ingests new data, then draws.

   Citations:

   - multi-file path: `src/tui.rs:1441-1459`
   - single-file path: `src/tui.rs:1491-1503`

   This means one `j` press and one batch of live updates can affect the same frame.

2. Pressing `j` calls `move_selection(1)`.

   Citations:

   - `src/tui.rs:1549-1553`
   - `src/tui.rs:948-959`

   That function:

   - resolves the current selected index in the current live row ordering
   - switches to `Pinned`
   - moves selection to the next row
   - sets `scroll_offset = next_index.saturating_sub(3)`

3. Before the frame is drawn, ingestion can append new events or update call/session activity.

   Citations:

   - `src/tui.rs:549-576`
   - `src/tui.rs:603-645`
   - `src/projection.rs:177-207`
   - `src/projection.rs:242-260`
   - `src/projection.rs:263-370`

4. Because each pane is sorted newest-first, new activity inserts or reorders rows above the selected entity:

   - Events prepend by descending `sort_at`: `src/tui.rs:724-728`
   - Calls reorder by `started_at.or(last_updated_at)`: `src/projection.rs:254-259`
   - Sessions reorder by `last_activity_at`: `src/projection.rs:363-369`

5. In pinned mode, `reconcile_after_data_change` preserves the selected entity identity if it still exists, but leaves `scroll_offset` unchanged.

   Citation:

   - `src/tui.rs:625-643`

   So the selected row can silently move from index `n` to index `n + k`, while the viewport offset still reflects the old position.

6. On the next `j`, `selected_index` is recomputed against the shifted live list, and `move_selection` writes a new `scroll_offset` from that newer index.

   Citations:

   - `src/tui.rs:931-937`
   - `src/tui.rs:953-958`

That produces the visible oscillation:

- live updates push the selected row upward/downward relative to the viewport without a matching offset correction
- the next `j` snaps the viewport again from the newly shifted index

That is the back-and-forth effect.

### Why it is especially noticeable on repeated `j`

Repeated `j` is the fastest way to alternate between:

- user-driven offset updates in `move_selection`
- ingest-driven row reordering in `reconcile_after_data_change`

Because the loop interleaves input and ingestion on every tick, the viewport can drift between keypresses and then snap back on the next keypress.

### Additional correctness gap: `scrolloff`

The code hard-codes `3` as the browse offset margin in both `move_selection` and `jump_to`:

- `src/tui.rs:958`
- `src/tui.rs:970`

So the requested `scrolloff = 5` is not implemented today, and offset math is not centralized.

## 5. Minimal architecture plan for FOLLOW/BROWSE

The cleanest minimal plan is to separate "live projection data" from "browse presentation state" per tab.

### A. Replace current naming and semantics

Rename:

- `FollowMode::Live` => `FollowMode::Follow`
- `FollowMode::Pinned` => `FollowMode::Browse`

Update all visible labels from `LIVE` / `PINNED` to `FOLLOW` / `BROWSE`.

Affected surfaces:

- mode enum: `src/tui.rs:93-96`
- help label: `src/tui.rs:1358-1364`
- header badge: `src/tui.rs:1708-1728`
- `f` binding text: `src/tui.rs:403-409`

### B. Make manual navigation an explicit mode change

Any manual navigation action should call one shared helper like `enter_browse(...)` before changing selection:

- `j`
- `k`
- `g` / `gg`
- `G`
- arrow-key equivalents

`f` should be the only explicit path back into FOLLOW.

Current code already partly does this for `j/k/g/G` by switching to `Pinned`, but the behavior is incomplete because it still browses against a live-reordering row set.

Citations:

- `src/tui.rs:948-971`
- `src/tui.rs:1583-1585`

### C. Add a real browse buffer per tab

For each tab, store a browse snapshot keyed by entity IDs when entering BROWSE:

- ordered row keys for the current visible dataset
- selected browse index
- unseen count since the snapshot was taken

FOLLOW should render directly from the current live projection.

BROWSE should render from the snapshot order, not from the live-sorted order. New live data should only:

- increment unseen count
- mark keys as newly available for when FOLLOW is resumed or the snapshot is refreshed

This is the minimal way to satisfy the requested "vim-buffer-style navigation through entries" semantics. If BROWSE still uses the live-sorted row list, the buffer keeps moving underneath the cursor.

### D. Derive viewport offset from selection and scrolloff

Do not keep hand-written per-action offset math like `index - 3`.

Instead, centralize one viewport calculation that derives the top row from:

- selected index
- viewport height
- `scrolloff = 5`

That can either replace `scroll_offset` entirely or make `scroll_offset` a derived field recomputed in one place. The important constraint is that data changes and manual navigation must use the same offset rule.

Current offset ownership is fragmented across:

- `move_selection`: `src/tui.rs:948-959`
- `jump_to`: `src/tui.rs:961-971`
- `reconcile_after_data_change`: `src/tui.rs:603-645`
- `render_list`: `src/tui.rs:1782-1851`

### E. Add a small input-state layer for `gg`

Because `resolve_action` is single-key only, supporting `gg` requires a tiny pending-prefix state at the app/input layer.

Minimal change:

- keep `g` as a prefix candidate
- second `g` resolves to "top of buffer"
- `G` remains "bottom of buffer"
- both land in BROWSE

Citations:

- `src/tui.rs:347-357`
- `src/tui.rs:1509-1517`

### F. Apply the same model to all three panes

The same FOLLOW/BROWSE state machine should drive:

- Events
- Tool Calls
- Sessions

Only the row source differs:

- Events use `visible_event_rows`
- Calls use `visible_call_rows` / `ProjectionStore::correlated_calls`
- Sessions use `visible_session_rows` / `ProjectionStore::sessions`

That avoids the current inconsistency where route transitions already seed different modes:

- Sessions -> Calls sets target pane to live: `src/tui.rs:1014-1034`
- Calls -> Events sets target pane to pinned: `src/tui.rs:1044-1064`

The route logic should explicitly decide whether drill-in lands in FOLLOW or BROWSE, instead of inheriting older `Live/Pinned` quirks.

## 6. Tests that need to exist

The current tests cover some adjacent behavior, but they do not prove FOLLOW/BROWSE correctness.

### Existing related coverage

- per-tab state memory: `src/tui.rs:2247-2264`
- pinned selection survives prepends: `src/tui.rs:2312-2333`
- drilldown scope behavior: `src/tui.rs:2268-2308`
- stale notice scoping: `src/tui.rs:2381-2403`

### Missing tests required for the fix

1. Manual navigation exits FOLLOW in every pane.

   Assertions:

   - Events, Calls, and Sessions each start in FOLLOW
   - pressing `j`, `k`, `gg`, or `G` leaves FOLLOW and enters BROWSE

2. `f` explicitly re-enters FOLLOW in every pane.

   Assertions:

   - from BROWSE, pressing `f` returns to FOLLOW
   - selection returns to the live top row
   - unseen count resets

3. `gg` is a real two-key sequence and ends in BROWSE at the top.

   Assertions:

   - a single `g` does not incorrectly execute another action
   - `gg` selects row `0`
   - mode is BROWSE afterward

4. `G` remains bottom navigation, not follow.

   Assertions:

   - `G` selects the last row
   - mode is BROWSE afterward
   - `f` is still required to resume FOLLOW

5. BROWSE is stable under live ingest in Events.

   Assertions:

   - enter BROWSE
   - ingest newer events above the current selection
   - visible order and viewport stay stable for the browse snapshot
   - unseen count increments

6. BROWSE is stable under live ingest in Tool Calls.

   Assertions:

   - enter BROWSE in Calls
   - ingest related/result events that update `last_updated_at`
   - live call ordering changes underneath FOLLOW, but not underneath BROWSE

7. BROWSE is stable under live ingest in Sessions.

   Assertions:

   - enter BROWSE in Sessions
   - ingest activity that changes `last_activity_at`
   - live session ordering changes underneath FOLLOW, but not underneath BROWSE

8. Scrolloff is exactly 5.

   Assertions:

   - when enough rows exist, the selected row stays at least 5 rows away from the top/bottom viewport edges while moving through the middle of the list
   - clamping near the top and bottom behaves correctly

9. Re-entering FOLLOW flushes snapshot drift correctly.

   Assertions:

   - enter BROWSE
   - accumulate unseen rows
   - press `f`
   - pane snaps back to the current live top row with offset `0`

10. Drilldown routes use explicit FOLLOW/BROWSE semantics.

   Assertions:

   - Sessions -> Calls
   - Calls -> Events
   - mode choice is intentional and consistent with the new policy
   - subsequent manual navigation still leaves FOLLOW and enters BROWSE

## 7. Summary

The current bug is not in key handling alone. It comes from a deeper mismatch:

- manual navigation switches the mode flag to `Pinned`
- but the pane still renders against a live-reordering dataset
- and `scroll_offset` is only updated on manual actions, not when live reordering changes the selected row's index

That is why repeated `j` feels jumpy. The minimal durable fix is to turn FOLLOW and BROWSE into a real per-tab state machine, give BROWSE a stable row snapshot, and derive viewport positioning from one shared `scrolloff = 5` rule.
