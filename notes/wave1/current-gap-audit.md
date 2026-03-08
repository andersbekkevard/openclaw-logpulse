# Wave 1 Gap Audit: One-line global OpenClaw action stream

## Requirement under review
User requirement: provide a one-line command that streams all OpenClaw actions dynamically across sessions from `~/.openclaw`.

Explicit baseline currently documented in this workspace (`notes/wave1/cli-ux.md`): zero-arg usage with discovery order `OPENCLAW_LOG_PATH` → `/var/log/openclaw/openclaw.log` → `$HOME/.openclaw/openclaw.log`, and follow-style stream.

## Current state vs requirement
Current implementation is a single-file tailer CLI. The executable requires an explicit log path argument and does not include built-in log-location discovery or multi-file aggregation. README examples also present only explicit file arguments and do not describe the zero-argument workflow.

## Gap list (prioritized)

### P0 — Critical
- `src/cli.rs` (`Args::log_file`):
  - `LOG_FILE` is required as a positional argument.
  - Impact: command cannot run as a one-line default invocation.

- `src/main.rs` (`main`):
  - `Args::parse()` flows directly into `Tailer::new(args.log_file.clone(), ...)`.
  - Impact: there is no fallback path resolution and no `.openclaw` preference.

- `src/tailer.rs` (`Tailer`, `Tailer::new`, `Tailer::next_line`, `Tailer::reopen`, `Tailer::handle_eof_or_rotation`):
  - Tailer owns a single `PathBuf` and reads one file only.
  - Impact: cannot discover/follow multiple log files dynamically, so cannot aggregate actions “across sessions” if sessions are split across files.

### P1 — High
- `README.md` (Usage + Arguments section):
  - documents required positional `<LOG_FILE>` and examples only using explicit paths (`/var/log/openclaw.log`).
  - Impact: runtime behavior promised by one-line default stream and `.openclaw` fallback is undocumented and undiscoverable.

- `src/main.rs` (`main`):
  - no path-resolution failure mode with usage/help-on-exit (exit code + concise diagnostic expected by spec; current behavior prints one error and returns with code 0 by implicit end of `main`).
  - Impact: user guidance and recovery behavior does not match zero-arg startup semantics.

### P2 — Medium
- `src/tailer.rs` and `src/main.rs`:
  - no support for path globbing/roots scanning, watch-list growth/shrink, or periodic rescans for new files in `~/.openclaw`.
  - Impact: dynamic multi-session addition/removal is unsupported even if multiple candidate logs exist under directory.

- `src/cli.rs`:
  - default follow/read mode is hardwired by booleans (`--no-follow` and `--from-start`) but there is no `--follow` flag and no explicit default-mode section tied to the zero-arg contract in code.
  - Impact: future parity with spec is limited and likely to drift from documented UX.

- `src/event.rs` / `src/normalizer.rs`:
  - session correlation relies on `session_key`/`session_id` extracted from each event; useful for filtering but not for dynamic source fan-in.
  - Impact: data model can represent session ids but does not provide source-level session discovery.

## What is working against requirement (lower risk)
- Session filtering (`--session`) is available and can include all sessions when omitted (`src/event.rs`, `src/main.rs`).
- Tool-level lifecycle and stale detection logic already works on a continuous stream once a source is being tailed.
- README names and examples include OpenClaw-focused usage and heartbeat/stale features.

## Proposed implementation wave

### Wave 1 (foundational behavior alignment)
1. Resolve log source path defaults in `src/cli.rs`:
   - Make `LOG_FILE` optional.
   - Resolve to `OPENCLAW_LOG_PATH` → `/var/log/openclaw/openclaw.log` → `$HOME/.openclaw/openclaw.log`.
   - On failure, print concise usage-like diagnostic and exit with failure.

2. Update startup wiring in `src/main.rs`:
   - Use resolved source from CLI config and keep follow semantics for default mode.
   - Add explicit error path and exit behavior for unresolved log source.

3. Update `README.md` usage:
   - Add `logpulse` no-arg example.
   - Document fallback resolution and required one-line behavior.

### Wave 2 (dynamic multi-file stream)
4. Introduce multi-file source discovery/management (new module, likely `src/stream` or equivalent):
   - Discover candidate log files under `~/.openclaw` / configured root.
   - Track add/remove/recreate events and avoid duplicate readers for identity-changed files.

5. Replace single `Tailer` usage in `src/main.rs` with an orchestrator that merges lines from multiple tailer readers while preserving event order/heartbeat behavior.

6. Expand stale tracking, heartbeat and filter pipeline to consume merged multi-source event stream without semantically breaking single-file behavior.

### Wave 3 (hardening + UX parity)
7. Add deterministic diagnostics and exit statuses per spec (invalid source args, recoverable IO behavior, graceful SIGPIPE handling if applicable).
8. Add docs/examples in README for dynamic source behavior and failure scenarios.
