# TUI V1 Research Brief: UX and Navigation Patterns

## Recommendation in one sentence

Adopt a mostly modeless, Vim-friendly tabbed workspace where the main list remains the primary navigation surface, the right pane is a live inspector for the current selection, fullscreen detail is a temporary drilldown layer, overlays are reserved for transient tasks like help/filter/jump, and `Esc`/`q` always backs out one layer before quitting.

## Goals this model should optimize for

- Fast scanning of active tool activity across sessions.
- Stable keyboard-first navigation without forcing users into many hard modes.
- Clear distinction between "following the live stream" and "inspecting a pinned item".
- Dense information layout that still preserves orientation for first-time users.
- Progressive disclosure: summary first, payload depth on demand.

## Recommended state model

### Core layers

Treat the UI as four stacked layers, not many peer modes:

1. Workspace layer
   - Persistent tabs across the top.
   - Each tab owns its own list state, filters, cursor position, and follow/pin state.
   - Example tabs for v1: `Timeline`, `Sessions`, `Tools`, `Stale`.

2. Inspector layer
   - Right-side pane for the currently selected row.
   - Always contextual to the focused list.
   - Never changes the app's navigation mode by itself.

3. Expanded detail layer
   - Fullscreen drilldown for the selected entity or event.
   - Opens above the workspace when the payload is too wide/tall for the inspector.
   - Keeps the underlying tab and cursor position intact.

4. Overlay layer
   - Temporary modal surfaces for help, filter editing, jump/search, and confirm dialogs.
   - Dismissible with `Esc`.
   - Should not be used for ordinary inspection.

### Back-navigation rule

Use one universal unwind rule:

- `Esc` or `q` closes the topmost active layer.
- If an overlay is open, close overlay.
- Else if expanded detail is open, close expanded detail.
- Else if focus is in a transient input state, cancel it and return to list focus.
- Else `q` quits the app.

This keeps navigation understandable without a deep mode machine.

### Focus model

Keep one primary focus target at a time:

- Default focus is always the main list in the active tab.
- The inspector is read-only in v1 and should not steal focus by default.
- If later adding interactive inspector widgets, focus transfer should be explicit with `Tab`.

This is the safest way to preserve Vim-style list movement and avoid "where did my keys go?" confusion.

## Recommended tab model

Tabs should represent different aggregation lenses, not arbitrary panels:

- `Timeline`: all matching events in time order. Default landing tab.
- `Sessions`: one row per session with status, active call count, last event, staleness.
- `Tools`: one row per tool or tool-call group with frequency, failure count, active count.
- `Stale`: only in-flight calls past threshold.

Recommended behavior:

- `h/l` or `[`/`]` switch tabs.
- Number keys `1-4` jump directly to tabs.
- Each tab remembers its own cursor, scroll, filters, and follow/pin state.
- Cross-tab drilldown should preserve context. Example: opening a session from `Sessions` can drill into a filtered `Timeline` view for that session.

Avoid tabs that represent pure UI layout states. Layout state should be orthogonal to content state.

## Inspector vs expanded detail

### Right inspector

The right inspector should answer "what is this row?" immediately:

- Show concise metadata first: timestamp, session, agent, tool, status, call id.
- Show a short structured summary next.
- Show truncated payload or decoded fields last.
- Update instantly as the list selection changes.
- Scroll independently if content overflows.

The inspector should stay visible during normal navigation because it lowers the cost of browsing dense rows.

### Fullscreen expanded detail

Fullscreen detail should answer "let me read or compare the whole thing":

- Open with `Enter` or `o`.
- Use when content is long, nested, wrapped badly, or needs side-by-side sections.
- Include raw JSON, normalized fields, and any derived summaries.
- Support section jumping within the detail view with `j/k`, `/`, `n/N`, and `g/G`.

### Recommended split of responsibility

- Inspector: selection preview and short-form interpretation.
- Fullscreen detail: long-form reading and deep inspection.

Do not let the inspector become a second full detail screen. That produces redundant layouts and weakens the value of drilldown.

## Live vs pinned behavior

This is the most important interaction choice. Follow behavior must be explicit and visible.

### Recommended defaults

- New tabs start in `Live` mode.
- `Live` means the cursor tracks the newest relevant row and the inspector updates with it.
- Any manual row navigation (`j/k`, mouse select, search result jump) switches that tab into `Pinned` mode.
- `f` resumes `Live` mode for the current tab.

### Indicator language

Use plain language, not ambiguous icons alone:

- `LIVE` when following newest matching row.
- `PINNED` when user has frozen selection.
- Optional suffixes:
  - `LIVE +12 unseen` should not exist, because unseen rows are impossible while following.
  - `PINNED +12 new` is useful when new rows arrive while the user is inspecting older content.

### New-data behavior while pinned

- Do not move the cursor.
- Append new rows to the dataset.
- Show a small count badge in header/footer for unseen rows since pinning.
- `f` jumps to newest and clears the unseen count.

This mirrors the mental model users already know from tailing tools, editors, and log viewers.

## Vim-friendly keybinding model

### Principles

- Use Vim motions where they match terminal-list behavior.
- Keep global bindings small and memorable.
- Prefer screen-local bindings over overloading the same key everywhere.
- Expose every non-obvious binding in `?`.

### Recommended global keys

- `j/k` or `Up/Down`: move selection.
- `g/G`: first/last item in current list.
- `Ctrl-d` / `Ctrl-u`: half-page.
- `h/l`: previous/next tab.
- `1-4`: direct tab jump.
- `Enter` or `o`: open fullscreen detail.
- `Esc`: back/cancel/close top layer.
- `q`: close current layer or quit from workspace.
- `f`: resume live follow.
- `/`: search within current screen.
- `n/N`: next/previous result.
- `:` optional later; skip in v1 unless command mode is genuinely needed.

### Recommended current-screen-specific keys

- `?`: open help overlay scoped to the active screen.
- `Tab`: if needed later, move focus between list and interactive side panels.
- `p`: toggle pin only if it differs meaningfully from `f`; otherwise avoid duplicate semantics.
- `z`: expand/collapse inspector width or toggle detail density, if implemented.

### Help overlay design for `?`

`?` should not show one giant static cheat sheet. It should be composed from:

- Global bindings.
- Bindings specific to the active tab.
- Bindings specific to the active layer.
- A one-line explanation of the current state. Example: "Timeline tab, list focused, PINNED, 12 unseen rows."

This follows the strongest terminal UX pattern from tools that remain learnable under high key density.

## Drilldown transitions

Use direct, low-ceremony transitions:

- From list row to fullscreen detail: `Enter`.
- From aggregate rows (`Sessions`, `Tools`) to filtered timeline: `l` or `Enter`.
- From fullscreen detail back to parent list: `Esc`.
- Preserve cursor and scroll state on return.

Recommended breadcrumb language in fullscreen detail:

- `Timeline / session abc123 / tool shell.exec / call tsk-7`

This gives users confidence that drilldown is reversible and local, not a context reset.

## Prior art to copy

### ratatui ecosystem

- Copy the common pattern of stable layout + temporary popup overlays rather than deeply nested modal flows.
- Copy explicit highlight state and visible focus markers.
- Avoid overusing popups for core reading tasks; popups are best for help/filter dialogs, not full data inspection.

### LazyGit

- Copy context-sensitive help and view-specific keymaps.
- Copy the idea that list navigation is always the primary interaction plane.
- Avoid excessive pane interactivity in v1; LazyGit works because each pane has a clear role, but it also carries a large keybinding surface that would be too much for a first logpulse TUI.

### k9s

- Copy the "resource list plus detail/describe drilldown" mental model.
- Copy visible mode/status indicators.
- Avoid excessive shortcut sprawl and hidden command vocabulary unless there is strong evidence users need it.

### Helix

- Copy the discipline of having a small number of real modes with clear escape semantics.
- Copy in-context keymap discoverability.
- Avoid importing editor-style modality wholesale; log inspection is not text editing, so a heavy normal/select/insert split would be unnecessary friction.

## Risks and tradeoffs

### Density vs clarity

Risk:

- A timeline table plus right inspector plus tabs plus footer help can become visually busy very quickly.

Recommendation:

- Keep the default row schema compact.
- Move secondary metadata into inspector and fullscreen detail.
- Use color for state reinforcement, not as the only state carrier.

### Too many modes

Risk:

- If tabs, inspector focus, expanded detail, search, filters, and live/pinned all become separate modes, users will lose track of which keys work.

Recommendation:

- Treat only overlays and expanded detail as temporary layers.
- Keep the list as the default focus target everywhere else.

### Ambiguous live behavior

Risk:

- Users often cannot tell whether a log UI will move under them when new data arrives.

Recommendation:

- Always show `LIVE` or `PINNED`.
- Make manual movement switch to `PINNED`.
- Give `f` one obvious job: resume follow.

### Over-investing in tabs too early

Risk:

- Too many tabs can fragment the information architecture.

Recommendation:

- Start with four or fewer tabs.
- Add new tabs only when they represent distinct operator questions, not just another way to render the same rows.

## Concrete v1 interaction model

If the team wants a single recommended model to build, it should be:

- Default screen: `Timeline` tab with list on left, inspector on right, footer hint row, and explicit `LIVE` status.
- Manual navigation pins the selection.
- `f` resumes live.
- `Enter` opens fullscreen detail for the selected row.
- `?` opens a context-specific help overlay.
- `h/l` switch tabs; each tab keeps its own state.
- `Esc` always backs out one layer.
- Aggregate tabs drill back into a filtered timeline instead of inventing new navigation semantics.

## Suggested v1 footer/help copy

Footer:

`j/k move  h/l tab  Enter inspect  f live  / search  ? help  Esc back  q quit`

Header status example:

`Timeline  PINNED  +12 new  filters: session=abc tool=shell`

## Unresolved questions that likely need human input

- Should `Sessions` or `Timeline` be the default landing tab for the primary target user?
- Does the team want `Enter` to open fullscreen detail, or should `Enter` drill into filtered timeline while `o` opens fullscreen detail?
- Is search intended to be simple local find within the visible tab, or a cross-tab/query-builder concept?

## Sources and references

- ratatui examples and docs: https://ratatui.rs/
- LazyGit repository and documentation: https://github.com/jesseduffield/lazygit
- k9s documentation: https://k9scli.io/ and https://github.com/derailed/k9s
- Helix editor documentation: https://docs.helix-editor.com/
