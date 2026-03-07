# Wave 1 Verification Plan

Date: 2026-03-06
Repo: /home/anders/.openclaw/workspace/dev/openclaw-logpulse
Goal: strict, command-driven checks proving `openclaw-logpulse` behavior across all sessions.

## 1) Tier-1 checks: dynamic source discovery + live streaming

### 1.1 Unit checks

```bash
cd /home/anders/.openclaw/workspace/dev/openclaw-logpulse
mkdir -p notes/wave1/artifacts

cargo build --release
cargo test -q --lib parser::
cargo test -q --lib normalizer::
cargo test -q --lib stale::
cargo test -q --lib event::
```

Acceptance:
- All commands return exit code `0`.
- Existing stale tests (`tracks_and_completes_inflight_calls`, `warns_when_stale`) remain green.
- Add/keep unit tests for any new discovery-related code before implementation changes land.

### 1.2 Integration checks

#### A) Dynamic session-source discovery from mixed sessions

```bash
cd /home/anders/.openclaw/workspace/dev/openclaw-logpulse
mkdir -p notes/wave1/artifacts/integration notes/wave1/fixtures
cat > notes/wave1/fixtures/session-mixed.fixture.jsonl <<'LOG'
{"event":"tool_call_start","timestamp":"2026-03-06T10:00:00Z","session_key":"alpha-001","tool_name":"shell","call_id":"c-a1","status":"started","level":"info"}
{"event":"tool_call_start","timestamp":"2026-03-06T10:00:01Z","session_key":"beta-002","tool_name":"search","call_id":"c-b1","status":"started","level":"info"}
{"event":"tool_call_result","timestamp":"2026-03-06T10:00:02Z","session_key":"alpha-001","tool_name":"shell","call_id":"c-a1","status":"ok","level":"info"}
{"event":"tool_call_result","timestamp":"2026-03-06T10:00:04Z","session_key":"beta-002","tool_name":"search","call_id":"c-b1","status":"ok","level":"info"}
{"event":"tool_call_start","timestamp":"2026-03-06T10:00:05Z","session_key":"alpha-001","tool_name":"http","call_id":"c-a2","status":"started","level":"warn"}
LOG

cargo run --release -- --no-follow --format json notes/wave1/fixtures/session-mixed.fixture.jsonl \
  > notes/wave1/artifacts/integration/session-mixed.out
jq -s 'map(select(.kind=="tool_event") | .event.session_key) | sort | unique | map(select(. != null)) | . == ["alpha-001","beta-002"]' notes/wave1/artifacts/integration/session-mixed.out
```

#### B) Dynamic source discovery during file switches/rotation

```bash
cd /home/anders/.openclaw/workspace/dev/openclaw-logpulse
tmpdir=$(mktemp -d)
log_a="$tmpdir/session-a.log"
log_b="$tmpdir/session-b.log"
current="$tmpdir/current.log"
mkdir -p "$tmpdir"

printf '{"event":"tool_call_start","session_key":"session-a","call_id":"a1","tool_name":"shell","status":"started","timestamp":"2026-03-06T10:00:00Z","level":"info"}\n' > "$log_a"
printf '{"event":"tool_call_start","session_key":"session-b","call_id":"b1","tool_name":"search","status":"started","timestamp":"2026-03-06T10:00:01Z","level":"info"}\n' > "$log_b"
ln -sfn "$log_a" "$current"

(
  sleep 0.5
  printf '{"event":"tool_call_result","session_key":"session-a","call_id":"a1","status":"ok","timestamp":"2026-03-06T10:00:02Z","level":"info"}\n' >> "$log_a"
  sleep 0.5
  ln -sfn "$log_b" "$current"
  printf '{"event":"tool_call_result","session_key":"session-b","call_id":"b1","status":"ok","timestamp":"2026-03-06T10:00:03Z","level":"info"}\n' >> "$log_b"
) &
writer=$!

timeout 6s cargo run --release -- --heartbeat-seconds 1 "$current" \
  > notes/wave1/artifacts/integration/rotation.out 2> notes/wave1/artifacts/integration/rotation.err
wait $writer || true

grep -q "session=session-a" notes/wave1/artifacts/integration/rotation.out
grep -q "session=session-b" notes/wave1/artifacts/integration/rotation.out
```

#### C) End-to-end live stream with follow mode and heartbeat

```bash
cd /home/anders/.openclaw/workspace/dev/openclaw-logpulse
tmpdir=$(mktemp -d)
log="$tmpdir/live.log"
: > "$log"

(
  for i in $(seq 1 5); do
    printf '{"event":"tool_call_start","session_key":"session-live","tool_name":"shell","call_id":"live-%s","status":"started","timestamp":"2026-03-06T10:00:%02dZ","level":"info"}\n' "$i" "$i"
    sleep 0.2
    printf '{"event":"tool_call_result","session_key":"session-live","tool_name":"shell","call_id":"live-%s","status":"ok","timestamp":"2026-03-06T10:00:%02dZ","level":"info"}\n' "$i" "$((i+1))"
    sleep 0.2
  done
) >> "$log" &
writer=$!

timeout 5s cargo run --release -- --heartbeat-seconds 1 --format human "$log" \
  > notes/wave1/artifacts/e2e/live-follow.out
wait $writer || true

[ -s notes/wave1/artifacts/e2e/live-follow.out ]
grep -q "session=session-live" notes/wave1/artifacts/e2e/live-follow.out
grep -q "\[HB\]" notes/wave1/artifacts/e2e/live-follow.out
```

## 2) Synthetic fixture generation strategy (from real logs, then sanitized)

### 2.1 Source collection and sampling

```bash
cd /home/anders/.openclaw/workspace/dev/openclaw-logpulse
mkdir -p notes/wave1/fixtures/raw notes/wave1/fixtures/sanitized notes/wave1/fixtures/synthetic

REAL_ROOTS=("$HOME/.openclaw" "/var/log/openclaw" "$HOME/.cache/openclaw")
for root in "${REAL_ROOTS[@]}"; do
  [ -d "$root" ] || continue
  find "$root" -type f \( -name "*.log" -o -name "*.jsonl" -o -name "*.ndjson" \) \
    | head -n 40

done | tee notes/wave1/fixtures/raw/sources.txt

while IFS= read -r src; do
  base=$(basename "$src")
  awk 'NR<=200 || NR%97==0 {print}' "$src" > "notes/wave1/fixtures/raw/$base.sample"
done < notes/wave1/fixtures/raw/sources.txt
```

### 2.2 Deterministic redaction/sanitization script

```bash
python3 - <<'PY'
#!/usr/bin/env python3
from pathlib import Path
import hashlib, json, re

in_dir = Path('notes/wave1/fixtures/raw')
out_dir = Path('notes/wave1/fixtures/sanitized')
out_dir.mkdir(parents=True, exist_ok=True)

SENSITIVE_KEYS = re.compile(r'(token|secret|api[_-]?key|authorization|password|bearer|session[_-]?id|call[_-]?id)$', re.I)
EMAIL_RE = re.compile(r'[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}', re.I)


def scrub_value(v):
    if isinstance(v, str):
        v = EMAIL_RE.sub('[REDACTED_EMAIL]', v)
        v = re.sub(r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}', '[REDACTED_UUID]', v, flags=re.I)
        if len(v) > 256:
            v = v[:256] + '...[TRUNC]'
        return v
    return v


def redact(obj):
    if isinstance(obj, dict):
        return {k: ('[REDACTED]' if SENSITIVE_KEYS.search(k) else redact(v)) for k, v in obj.items()}
    if isinstance(obj, list):
        return [redact(v) for v in obj]
    if isinstance(obj, str):
        return scrub_value(obj)
    return obj

for p in in_dir.glob('*.sample'):
    dst = out_dir / p.name.replace('.sample', '.jsonl')
    with p.open() as f, dst.open('w') as g:
        for line in f:
            l = line.strip()
            if not l:
                continue
            try:
                obj = json.loads(l)
                obj = redact(obj)
                g.write(json.dumps(obj, separators=(',', ':'), ensure_ascii=False) + '\n')
            except Exception:
                # keep non-JSON lines as-is only for traceability
                g.write((l[:1024] + ('...[TRUNC]' if len(l) > 1024 else '')) + '\n')
print('sanitized:', len(list(out_dir.glob('*.jsonl'))), 'files')
PY
```

### 2.3 Synthetic fixture synthesis for load + correctness tests

```bash
python3 - <<'PY'
#!/usr/bin/env python3
from pathlib import Path
import json, random
random.seed(2026)

out = Path('notes/wave1/fixtures/synthetic/openclaw-wave1.synthetic.ndjson')
sessions = ['session-alpha', 'session-beta', 'session-gamma']

with out.open('w') as f:
    for s in sessions:
        for i in range(200):
            call_id = f'{s}-call-{i:04d}'
            start = {
                'event':'tool_call_start', 'session_key': s, 'tool_name': random.choice(['shell','search','http']),
                'call_id': call_id, 'status':'started', 'timestamp': f'2026-03-06T10:{i//60:02d}:{i%60:02d}Z', 'level':'info'
            }
            result = {
                'event':'tool_call_result', 'session_key': s, 'tool_name': start['tool_name'],
                'call_id': call_id, 'status':'ok', 'timestamp': f'2026-03-06T10:{(i+1)//60:02d}:{(i+1)%60:02d}Z', 'level':'info'
            }
            f.write(json.dumps(start) + '\n')
            if random.random() < 0.98:
                f.write(json.dumps(result) + '\n')

# append a few malformed/non-standard lines for resilience coverage
with out.open('a') as f:
    f.write('not-json-line\n')
    f.write('{"event":"tool_call_start","session_key":"session-alpha","tool_name":"shell","level":"warn"}\n')
    f.write('{"not":"jsonish","status":"ok"}\n')

print('synthetic fixture written:', out)
PY
```

## 3) Performance checks

### 3.1 Throughput (single pass, no follow)

```bash
cd /home/anders/.openclaw/workspace/dev/openclaw-logpulse
mkdir -p notes/wave1/artifacts/perf
INPUT=notes/wave1/fixtures/synthetic/openclaw-wave1.synthetic.ndjson
OUT=notes/wave1/artifacts/perf/throughput.out
TIME=notes/wave1/artifacts/perf/throughput.time

start_ms=$(date +%s%3N)
/usr/bin/time -f '%e' cargo run --release -- --no-follow --format json "$INPUT" > "$OUT" 2> "$TIME"
end_ms=$(date +%s%3N)

events=$(wc -l < "$INPUT")
elapsed_ms=$((end_ms-start_ms))
python3 - <<PY
import os
lines = int(os.environ['events'])
elapsed = int(os.environ['elapsed_ms'])
print({'events': lines, 'elapsed_ms': elapsed, 'events_per_sec': round(lines / (elapsed / 1000.0), 2)})
PY
```

Acceptance target: `events_per_sec >= 20000` on dev laptop baseline.

### 3.2 Startup latency (first emit)

```bash
cd /home/anders/.openclaw/workspace/dev/openclaw-logpulse
INPUT=notes/wave1/fixtures/synthetic/openclaw-wave1.synthetic.ndjson
OUT=notes/wave1/artifacts/perf/startup.out

start_ms=$(date +%s%3N)
openclaw-logpulse --no-follow --format json "$INPUT" | head -n 1 > /tmp/startup_first_line.txt
end_ms=$(date +%s%3N)
echo "$((end_ms-start_ms))" > "$OUT"
```

Acceptance target: first-emission latency `< 1000ms`.

### 3.3 Memory ceiling under sustained follow

```bash
cd /home/anders/.openclaw/workspace/dev/openclaw-logpulse
tmpdir=$(mktemp -d)
log="$tmpdir/live.log"
: > "$log"

autofill(){
  i=0
  while true; do
    printf '{"event":"tool_call_start","session_key":"mem","call_id":"x-%d","tool_name":"shell","status":"started","timestamp":"2026-03-06T10:00:00Z","level":"info"}\n' "$i" >> "$log"
    printf '{"event":"tool_call_result","session_key":"mem","call_id":"x-%d","status":"ok","timestamp":"2026-03-06T10:00:01Z","level":"info"}\n' "$i" >> "$log"
    i=$((i+1))
    usleep 1000
  done
}

autofill &
writer=$!

/usr/bin/time -v timeout 20s openclaw-logpulse --heartbeat-seconds 2 --format json --from-start "$log" > notes/wave1/artifacts/perf/memory.out 2> notes/wave1/artifacts/perf/memory.time
kill $writer || true
```

Acceptance target: peak RSS in `memory.time` does not exceed `204800` KB.

## 4) Stale-detection correctness

### 4.1 Unit assertions

```bash
cd /home/anders/.openclaw/workspace/dev/openclaw-logpulse
cargo test -q stale::tests::tracks_and_completes_inflight_calls
cargo test -q stale::tests::warns_when_stale
```

Required additional stale tests to add:
- no warning before threshold
- warning emitted once per call
- completion removes in-flight immediately
- warnings require call_id
- `active_sessions` in heartbeat counts unique `session_key/session_id`

### 4.2 Integration-level stale behavior

```bash
cd /home/anders/.openclaw/workspace/dev/openclaw-logpulse
cat > notes/wave1/fixtures/stale-case.jsonl <<'LOG'
{"event":"tool_call_start","session_key":"session-stale-a","call_id":"stale-1","tool_name":"shell","status":"started","timestamp":"2026-03-06T10:00:00Z","level":"info"}
{"event":"tool_call_start","session_key":"session-stale-b","call_id":"ok-1","tool_name":"search","status":"started","timestamp":"2026-03-06T10:00:00Z","level":"info"}
{"event":"tool_call_result","session_key":"session-stale-b","call_id":"ok-1","status":"ok","timestamp":"2026-03-06T10:00:01Z","level":"info"}
{"event":"tool_call_result","session_key":"session-stale-a","call_id":"stale-1","status":"ok","timestamp":"2026-03-06T10:00:20Z","level":"info"}
LOG

openclaw-logpulse --stale-seconds 1 --heartbeat-seconds 2 --format json --no-follow \
  notes/wave1/fixtures/stale-case.jsonl > notes/wave1/artifacts/stale/stale.out

jq -s 'map(select(.kind=="stale_warning" and .call_id=="stale-1")) | length >= 1' notes/wave1/artifacts/stale/stale.out
jq -s 'map(select(.kind=="tool_event" and .event.call_id=="ok-1" and .event.kind=="tool_call_result")) | length == 1' notes/wave1/artifacts/stale/stale.out
jq -s 'map(select(.kind=="heartbeat")) | length > 0' notes/wave1/artifacts/stale/stale.out
```

### 4.3 Cross-session stale heartbeat smoke

```bash
cd /home/anders/.openclaw/workspace/dev/openclaw-logpulse
openclaw-logpulse --stale-seconds 2 --heartbeat-seconds 1 --format json notes/wave1/fixtures/synthetic/openclaw-wave1.synthetic.ndjson \
  > notes/wave1/artifacts/stale/cross-session-heartbeat.out

jq -s 'all(map(select(.kind=="heartbeat") | .active_calls | numbers) ) and all(map(select(.kind=="heartbeat") | .active_sessions | numbers))' notes/wave1/artifacts/stale/cross-session-heartbeat.out
```

## 5) Execution order and gate criteria

Execute in sequence and block on first failure:
1. Unit checks (Section 1.1)
2. Integration checks (Section 1.2)
3. Fixture generation (Section 2)
4. Performance checks (Section 3)
5. Stale checks (Section 4)

Blocking criteria:
- Any command exits non-zero.
- Missing expected keys in JSON outputs when assertions are applied.
- Throughput or memory regressions outside baseline thresholds.
