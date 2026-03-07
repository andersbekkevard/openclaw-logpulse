# Wave 1 Source Discovery: Dynamic OpenClaw session/tool log discovery

## 1) Observed ~/.openclaw layout (2026-03-06)

- Primary live session transcripts are under `~/.openclaw/agents/*/sessions`.
- `agents/main` is the only active transcript directory with rotating JSONL artifacts.
- Other directories also contain JSONL logs, but with different semantics.

### Core inventory snapshot

| Location | Matching JSONL files observed | Notes |
| --- | ---: | --- |
| `~/.openclaw/agents/main/sessions` | 121 | active + `.deleted` + `.reset` variants |
| `~/.openclaw/agents/default/sessions` | 0 | only `sessions.json` |
| `~/.openclaw/agents/codex/sessions` | 0 | only `sessions.json` |
| `~/.openclaw/agents/claude/sessions` | 0 | only `sessions.json` |
| `~/.openclaw/agents/email-specialist/sessions` | 0 | only `sessions.json` |
| `~/.openclaw/cron/runs/*.jsonl` | 29 | job run artifacts (different schema) |
| `~/.openclaw/workspace/logs/codex/**/history.jsonl` | 11 | wave history summaries |
| `~/.openclaw/workspace/logs/codex/**/sessions/**/*.jsonl` | 11 | rollout session transcripts (`rollout-*.jsonl`) |

Other paths under `~/.openclaw/workspace/memory/journal/sessions` contain Markdown notes, not JSONL.

## 2) Deterministic include/exclude rules

### 2.1 Candidate roots

#### Required (default scope: OpenClaw session logs)
1. `~/.openclaw/agents/*/sessions`
2. `~/.openclaw/workspace/logs/codex/*/sessions` (recursive to capture date directories)

#### Optional (explicit opt-in for broader observability)
1. `~/.openclaw/cron/runs`
2. `~/.openclaw/workspace/logs/codex/*` with `history.jsonl`

### 2.2 Canonical include patterns (core scope)

All patterns are anchored to file base name and should be case-sensitive.

1. **Active sessions**
   - `~/.openclaw/agents/*/sessions/[0-9a-fA-F-]{36}.jsonl`
2. **Deleted-rotated sessions**
   - `~/.openclaw/agents/*/sessions/[0-9a-fA-F-]{36}.jsonl.deleted.YYYY-mm-ddThh-mm-ss.SSSZ`
3. **Reset-rotated sessions**
   - `~/.openclaw/agents/*/sessions/[0-9a-fA-F-]{36}.jsonl.reset.YYYY-mm-ddThh-mm-ss.SSSZ`
4. **Workspace codex sessions (optional only if enabled)**
   - `~/.openclaw/workspace/logs/codex/*/sessions/*/*/*/rollout-*.jsonl`

### 2.3 Hard exclusions (must always be ignored)

1. `sessions.json` and `sessions.json.*`
2. `*.jsonl.lock`
3. `*.jsonl` that is not one of the canonical forms above
4. non-JSONL files
5. paths outside the allowed roots unless explicitly configured

### 2.4 Concrete examples from this host

- Included:
  - `~/.openclaw/agents/main/sessions/1ba00339-345a-4de2-ad8f-bc48085a77d8.jsonl`
  - `~/.openclaw/agents/main/sessions/1de6116a-bec0-4e00-a7a3-428f7092f7ea.jsonl.deleted.2026-03-06T13-49-59.740Z`
  - `~/.openclaw/agents/main/sessions/94e70de7-659e-4a78-a20a-9d9463681ebc.jsonl.reset.2026-03-06T15-32-29.532Z`
  - `~/.openclaw/workspace/logs/codex/ctankers/w1b-dashboard-research/sessions/2026/03/05/rollout-2026-03-05T15-14-33-019cbe59-abec-7ce3-a38c-4d000afa842b.jsonl`
- Excluded:
  - `~/.openclaw/agents/main/sessions/sessions.json`
  - `~/.openclaw/agents/main/sessions/sessions.json.993eba48-7b80-4faf-84d1-0b39a8fbecb1.tmp`
  - `~/.openclaw/agents/main/sessions/1ba00339-345a-4de2-ad8f-bc48085a77d8.jsonl.lock`
  - `~/.openclaw/workspace/memory/journal/sessions/2026-03-05-session-startup.md`

## 3) Live-follow semantics

### 3.1 New files during follow

1. Use both filesystem event notifications and periodic rescan.
2. A newly matching file should be added within:
   - immediate event notification when supported
   - or scan interval when rescan is the only path (target ≤2s)
3. On registration, record identity (`dev`,`ino`) and current offset.

### 3.2 Rotation/deletion/recreate behavior

1. **Rename-to `*.deleted.*` / `*.reset.*`**
   - track by identity (`dev`,`ino`) and update path metadata if the same inode moves.
   - continue using the old handle to consume any buffered tail, then close gracefully at EOF.
   - if the old identity is no longer readable, emit lifecycle event and stop after graceful drain.
2. **Same path, different inode**
   - close reader and reopen once if file ID changes and size semantics indicate truncate/rotation.
3. **Path temporarily disappears**
   - keep candidate in cache for `missing_ttl` (default 30s).
   - if file reappears (same UUID family or same inode), resume with correct offset policy.
4. **Permissions denied / transient FS errors**
   - emit warning event and retry with backoff, bounded by retry window.

### 3.3 De-duplication and ordering

1. De-duplicate by identity (`device`,`inode`) first, then canonical UUID path group.
2. If same inode is seen at multiple paths, keep one active reader.
3. Sort discovered streams deterministically before follow start:
   - root path
   - file type (`.jsonl`, `.jsonl.deleted`, `.jsonl.reset`)
   - UUID lexical order

## 4) Edge-case handling

1. Duplicate UUIDs across separate roots: keep as separate streams unless identity collision proves same inode.
2. Partial UTF-8/unfinished lines: emit malformed event instead of crashing.
3. Zero-byte created files: keep in pending state until first append.
4. Timestamp format mismatch on rotated suffix: ignore strict timestamp mismatch until manual opt-in override.
5. Symlinked roots:
   - policy default should be `follow_symlink=false`
   - if enabled, identity must include resolved target path.

## 5) Acceptance criteria

1. Startup includes all matches in required roots with deterministic ordering.
2. `sessions.json`, `sessions.json.*`, and `*.jsonl.lock` are never tailed.
3. On this host snapshot, core discovery must yield 121 files under `~/.openclaw/agents/*/sessions` (active + deleted + reset variants).
4. New matching file creation is discovered and tailed within the configured follow budget.
5. Rotated files ending in `.deleted.*`/`.reset.*` are discovered and tailed with consistent inode-based de-duplication.
6. A matching file removed and recreated within TTL resumes with a single reader (no duplicated emitted content).
7. Optional roots remain opt-in:
   - disabled by default
   - enabled only when explicit flag is set
