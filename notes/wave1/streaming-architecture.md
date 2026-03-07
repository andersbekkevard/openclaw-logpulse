# Wave 1 Streaming Architecture: Multi-file Global Follow

## 1) Design Goals

- One-command startup: discover and follow all matching log files across user-specified roots.
- Low-latency: near real-time emission from file append to output.
- Resilient: tolerate file rotation, truncation, deletion, recreate, and temporary watcher gaps.
- Bounded memory: hard per-file and global limits with explicit backpressure.
- Deterministic ordering: consistent event ordering policy for correlated output.

## 2) High-Level Shape

```
Config/CLI
   ↓
Discovery Engine  ────────────────┐
   (globs + scans + events)        │
                                   ▼
                              Source Registry
                                   │ (active file list + state)
                                   ▼
        ┌──────────────┐   events   ┌──────────────┐   parsed   ┌─────────────────────┐
        │ Directory/FS │────────────▶│   Readers    │──────────▶│     Parsers         │
        │    Watcher    │            └──────────────┘           └─────────────────────┘
        └──────────────┘                 │                            │
                                         ▼                            ▼
                                  Reader Buffers                Parse Queue
                                         │                            │
                                         └──────────────┬─────────────┘
                                                        ▼
                                                  Correlator
                                                        │
                                                  Output Queue
                                                        │
                                               Output Dispatcher
                                                        │
                                                   Sinks
```

## 3) File Discovery and Concurrent Following

### 3.1 Discovery inputs
- Source roots from CLI/config (`--path`, `--path /var/log`, globs, include/exclude patterns).
- Optional file-name policy (`*.log`, timestamped log naming schemes, etc.).
- Optional hard filter stage to limit noisy files by size/age/min-line-rate.

### 3.2 Discovery Engine (single shared owner)
- Maintains a `CandidateIndex` of path → identity candidate metadata (`ino`, `dev`, `path`, `mtime`, policy hash).
- Performs **two discovery mechanisms**:
  1. **OS native watcher**: recursively watches directories for create/remove/move/attrib/modify events.
  2. **Periodic rescans**: full scan fallback for missed events and startup warm-up.
- Emits `CandidateAdded`, `CandidateRemoved`, and `CandidateRenamed` control events into a serialized control queue.

### 3.3 Reader provisioning
- Discovery events handled by orchestrator on a single control task.
- `CandidateAdded` transitions path to `Starting` state and provisions a Reader only once per file identity.
- Reader sharding strategy:
  - Hash by `dev/inode` or by path to spread read load.
  - Spawn one async task per active file with a small read buffer and bounded output channel.
- Reader lifecycle state machine:
  - `NEW` → `OPEN_SEEK_TAIL` → `FOLLOW` → `QUIESCENT/DEGRADED` → `CLOSED`.

### 3.4 Dynamic discovery semantics
- New file appears: discoverer creates/refreshes metadata and sends add event.
- File renamed away/rotated: watcher emits remove or move; reader keeps state until policy decides handoff.
- Same name reused by a different file: identity check prevents duplicate readers.

## 4) Rotation / Truncation / Deletion / Recreate Handling

### 4.1 Identity and state tracking
- Each open file identified by tuple `(dev, ino, ctime?)` (best-effort; platform-dependent fallback: path + mtime+size).
- Reader state keeps:
  - tracked identity
  - current byte offset
  - last stable size, last inode, last mtime
  - checksum salt/version for sanity checks

### 4.2 Rotation
Common rotation patterns:
1. `mv logfile logfile.1 && touch logfile` (rename + recreate)
2. copy+truncate
3. external tool truncating in-place

Behavior:
- On read loop, if file descriptor yields EOF and still active, continue polling with exponential backoff cap.
- If path exists but inode changes at same path:
  - emit `FileRecreated` control event
  - close old fd
  - start new Reader for new identity at EOF (or from configured start offset for backfill)
- If old inode still exists but now inaccessible, finish buffered bytes, then mark dormant and stop after grace period.

### 4.3 Truncation
- Detect by `(current_offset > file_size)` after stat/statx refresh.
- On truncation:
  - emit structured lifecycle event `FileTruncated`
  - reset local offset to 0.
  - behavior is configurable:
    - `from_start` (re-read entire file)
    - `to_end` (skip to current end)
- Pending partial line fragments from pre-truncation chunk are discarded to avoid corruption.

### 4.4 Deletion and recreate
- On delete/missing:
  - mark reader state `Orphaned` but retain last known checkpoint and dedupe window.
  - do not immediately allocate new reader if name reappears instantly.
- On recreate:
  - wait short settle window `recreate_debounce_ms` (default 50–250ms).
  - if same path with new identity exists -> reopen and start according to backfill policy.
- If no recreate before TTL, emit `FileDisappeared` and release resources.

### 4.5 Edge cases
- Symlink targets: follow policy choice (`follow_symlink=true|false`), if true, watch target identity not link path identity.
- Permission loss: reader moves to `DegradedPermission`, retries with jitter, optionally emits warning event.
- Network filesystems with unstable inode semantics: fallback identity strategy can include `(path, generation_id, fs-unique markers)` where available.

## 5) Buffering, Backpressure, Memory Bounds

### 5.1 Bounded queues
Every stage uses bounded queues with explicit capacities and drop/error policy:
- `Discovery → Reader control` queue (small, control-priority).
- `Reader → Parser` per-reader ring buffer.
- `Parser → Correlator` shared bounded batch queue.
- `Correlator → Output` bounded event sink queue.

Recommended defaults (initial baseline):
- Per-reader byte buffer: `256 KiB` (configurable 64KiB–1MiB).
- Parser worker inbox: `4,096` events.
- Correlator input queue: `16,384` events.
- Output queue: `8,192` events.

### 5.2 Backpressure behavior
- When queue is full, upstream pauses production in priority order:
  1. parser workers stop accepting new raw events
  2. readers stop reading new file chunks
  3. discovery still emits lifecycle/control events with higher priority channel
- Reader stop reading means no unbounded buffering; OS file descriptor remains open and offset preserved.
- Optional `read_ahead` limiter can cap concurrent active readers pulling data.

### 5.3 Memory model and hard limits
Let:
- `F` = active files
- `B_r` = per-reader read buffer bytes
- `E` = max event size (approx bytes before decode)
- `Q_p` = parser queue size
- `Q_o` = output queue size

Estimated upper bound:
`Mem <= F*B_r + Q_p*E + Q_o*E + overhead`

Hard guards:
- `F` capped by `max_active_files`.
- Per-file bytes cap to reduce tail latency at high count (`B_r` small default).
- Global byte cap `max_pipeline_memory_bytes` (e.g., 512 MiB default).
- If cap exceeded, apply **graceful shedding**:
  - stop new file onboarding
  - preserve existing high-priority lifecycle/control messages
  - log `BackpressureExceeded` with actionable status

### 5.4 Burst handling and catch-up mode
- Reader batches reads (e.g., up to N lines/chunks) before parse handoff.
- Correlator supports bounded-window buffering by correlation key with timeout-based flush.
- Catch-up mode for backlog events uses larger batch flush intervals while keeping per-key window bounded.

## 6) Module Plan

## 6.1 Watcher/Discovery Module
- File: `src/stream/watcher.rs` (proposed)
- Responsibilities:
  - start/stop FS watches
  - convert native events into control events
  - periodic rescan and diffing
  - emit lifecycle events with dedupe IDs
- Public events:
  - `CandidateAdded{path, identity_hint}`
  - `CandidateRemoved{path}`
  - `CandidateRenamed{from, to}`
  - `CandidateModified{path}`

## 6.2 Reader Module
- File: `src/stream/reader.rs` (proposed)
- Responsibilities:
  - maintain ownership of one file identity per task
  - incremental reads with minimal syscalls
  - split by lines/records and emit raw parsed records with sequence metadata
  - detect size regressions and inode transitions
- Public outputs:
  - `RawLine{source_id, path, offset, bytes, ts_monotonic, line_terminator}`
  - `ReaderLifecycle{source_id, kind, details}`

## 6.3 Parser Module
- File: `src/stream/parser.rs` (proposed)
- Responsibilities:
  - decode bytes to line/event format
  - apply schema/parser plugins
  - extract correlation keys (`request_id`, trace id, pod/container, etc.)
  - return parse errors as first-class events (not dropped)
- Outputs:
  - `ParsedEvent{...}`
  - `ParseError{source_id, raw_line_ref, reason}`

## 6.4 Correlator Module
- File: `src/stream/correlator.rs` (proposed)
- Responsibilities:
  - aggregate related events by correlation key
  - time-window completion and flush policy
  - ordering policy: global monotonic source seq + per-key arrival order
  - emit correlation lifecycle: started / update / completed / timeout

## 6.5 Output Module
- File: `src/stream/output.rs` (proposed)
- Responsibilities:
  - formatting and sink fan-out (stdout/json/structured)
  - non-blocking write with backpressure-aware ring
  - terminal/table mode should not block reader path
  - sink health and write-retry policy
- Outputs:
  - `LogFrame` for direct/uncorrelated output
  - `AggregateFrame` for correlation aggregates

## 7) Error and Observability Plan

- Structured lifecycle events for all file transitions (added, rotated, truncated, disappeared, recreated).
- Per-module metrics:
  - active files, queued bytes, queue saturation ratio, parser rate, correlation lag, output lag.
- Health probes:
  - `--stats`: live in-memory gauge snapshots (no blocking).
  - optional periodic `warn` on high-latency queues and backpressure state.
- Correlation of delays:
  - `read_latency = output_ts - first_read_ts`
  - `tail_lag = now - file_mtime`

## 8) Initial API Contracts (minimal)

### 8.1 Top-level config
- `paths: [string]`
- `include_globs/excludes`
- `follow_mode: tail | from_start | from_checkpoint`
- `buffer.read_bytes_per_reader`
- `buffer.reader_queue_depth`
- `buffer.parser_queue_depth`
- `buffer.output_queue_depth`
- `backpressure.mode: pause|drop_noncritical`

### 8.2 Lifecycle policy
- `max_active_files`
- `missing_file_ttl`
- `recreate_debounce_ms`
- `truncation_mode: from_start|to_end`

## 9) Why this meets the goal

- **Concurrency**: independent reader tasks + sharded parse/correlation/output paths means high fan-in and high parallelism.
- **Discovery**: watcher + periodic scan handles both real-time and recovery for missed events.
- **Correctness under mutation**: explicit rotation/truncation/deletion state machine keeps behavior deterministic and predictable.
- **No unbounded memory**: strict queue ceilings and staged backpressure create hard memory ceilings.
- **Practical extension point**: module boundaries map cleanly to future plugin parsers and alternate sinks.

