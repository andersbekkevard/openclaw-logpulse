use std::collections::HashSet;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::SystemTime;
use std::time::{Duration, Instant};

use serde_json::Value;

fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture(name: &str) -> PathBuf {
    project_root().join("tests").join("fixtures").join(name)
}

fn binary_path() -> PathBuf {
    env::var("CARGO_BIN_EXE_openclaw_logpulse")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            project_root()
                .join("target")
                .join("debug")
                .join("openclaw-logpulse")
        })
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        let mut path = env::temp_dir();
        let epoch_nanos = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let marker = format!(
            "openclaw-logpulse-tests-{}-{}-{}",
            prefix,
            std::process::id(),
            epoch_nanos
        );
        path.push(marker);
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn append_line(path: &Path, line: &str) {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("append fixture line");
    writeln!(file, "{line}").expect("write fixture line");
    file.flush().expect("flush fixture line");
}

fn run_cli(args: &[&str]) -> (String, String, i32) {
    run_cli_with_env(args, &[])
}

fn run_cli_with_env(args: &[&str], envs: &[(&str, &str)]) -> (String, String, i32) {
    let output = Command::new(binary_path())
        .args(args)
        .envs(envs.iter().map(|(k, v)| (*k, *v)))
        .output()
        .expect("run openclaw-logpulse");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let status = output.status.code().unwrap_or(1);
    (stdout, stderr, status)
}

fn parse_json_lines(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("invalid JSON output line: {error}: {line}"))
        })
        .collect()
}

#[test]
fn one_line_default_mode_emits_single_record() {
    let (stdout, stderr, status) = run_cli(&[
        "--no-follow",
        "--from-start",
        "--stale-seconds",
        "1000000",
        "--format",
        "human",
        fixture("one-line.fixture.jsonl")
            .to_str()
            .expect("fixture path"),
    ]);

    assert_eq!(status, 0, "stderr: {stderr}");
    let lines: Vec<&str> = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(lines.len(), 1, "expected one event line, got {lines:?}");
    assert!(lines[0].contains("session=session-single"));
    assert!(lines[0].contains("START"));
}

#[test]
fn json_mode_outputs_valid_records() {
    let (stdout, stderr, status) = run_cli(&[
        "--no-follow",
        "--from-start",
        "--stale-seconds",
        "1000000",
        "--format",
        "json",
        fixture("session-mixed.fixture.jsonl")
            .to_str()
            .expect("fixture path"),
    ]);

    assert_eq!(status, 0, "stderr: {stderr}");
    let records = parse_json_lines(&stdout);
    assert!(!records.is_empty());

    let mut sessions = Vec::new();
    for record in &records {
        let kind = record.get("kind").and_then(Value::as_str).unwrap_or("");
        assert_eq!(
            kind, "tool_event",
            "only tool events expected for static fixture"
        );
        let session_key = record
            .pointer("/event/session_key")
            .and_then(Value::as_str)
            .expect("session_key");
        sessions.push(session_key.to_string());
    }

    sessions.sort_unstable();
    sessions.dedup();
    assert!(sessions.contains(&"alpha-001".to_string()));
    assert!(sessions.contains(&"beta-002".to_string()));
}

#[test]
fn malformed_lines_are_preserved_in_json_output() {
    let (stdout, stderr, status) = run_cli(&[
        "--no-follow",
        "--from-start",
        "--format",
        "json",
        fixture("malformed-lines.fixture.jsonl")
            .to_str()
            .expect("fixture path"),
    ]);

    assert_eq!(status, 0, "stderr: {stderr}");
    let records = parse_json_lines(&stdout);
    assert_eq!(records.len(), 3);

    let has_malformed = records.iter().any(|record| {
        record.get("kind").and_then(Value::as_str) == Some("tool_event")
            && record
                .pointer("/event/kind/event_kind")
                .and_then(Value::as_str)
                == Some("malformed")
    });
    assert!(
        has_malformed,
        "malformed event not represented in JSON output"
    );
}

#[test]
fn follow_mode_handles_rotation_switching_files() {
    let tmp = TempDir::new("rotation");
    let current = tmp.path().join("session-live.jsonl");
    let rotated = tmp.path().join("session-live.jsonl.2026-03-06");

    append_line(
        &current,
        r#"{"event":"tool_call_start","session_key":"session-rotation-a","tool":"shell","call_id":"a-1","status":"started","level":"info"}"#,
    );

    let mut child = Command::new(binary_path())
        .args([
            "--from-start",
            "--heartbeat-seconds",
            "1",
            "--format",
            "human",
            current.to_str().expect("fixture path"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn follow mode");

    thread::sleep(Duration::from_millis(200));

    fs::rename(&current, &rotated).expect("rotate file");
    append_line(
        &current,
        r#"{"event":"tool_call_start","session_key":"session-rotation-b","tool":"search","call_id":"b-1","status":"started","level":"info"}"#,
    );

    thread::sleep(Duration::from_millis(900));
    let _ = child.kill();
    let output = child.wait_with_output().expect("collect output");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("session=session-rotation-a"));
    assert!(stdout.contains("session=session-rotation-b"));
}

#[test]
fn zero_arg_discovery_follows_session_logs() {
    let tmp = TempDir::new("auto-discovery");
    let home = tmp.path().to_path_buf();

    let session_a = auto_session_file(
        &home,
        "agent-one",
        "11111111-1111-1111-1111-111111111111.jsonl",
    );
    let session_b = auto_session_file(
        &home,
        "agent-two",
        "22222222-2222-2222-2222-222222222222.jsonl",
    );

    append_line(
        &session_a,
        r#"{"event":"tool_call_start","session_key":"session-a","tool":"shell","call_id":"a-1","status":"started","level":"info"}"#,
    );

    let mut child = Command::new(binary_path())
        .args(["--from-start", "--poll-millis", "50", "--format", "json"])
        .env("HOME", &home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn auto-discovery mode");

    thread::sleep(Duration::from_millis(180));

    append_line(
        &session_a,
        r#"{"event":"tool_call","session_key":"session-a","tool":"shell","call_id":"a-2","status":"ok","level":"info"}"#,
    );

    append_line(
        &session_b,
        r#"{"event":"tool_call_start","session_key":"session-b","tool":"search","call_id":"b-1","status":"started","level":"info"}"#,
    );

    thread::sleep(Duration::from_millis(420));
    let _ = child.kill();

    let output = child
        .wait_with_output()
        .expect("collect auto-discovery output");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let records = parse_json_lines(&stdout);

    assert!(
        records
            .iter()
            .any(|record| record.get("kind").and_then(Value::as_str) == Some("tool_event")),
        "auto discovery should emit tool events"
    );

    let session_keys: HashSet<String> = records
        .iter()
        .filter_map(|record| {
            if record.get("kind").and_then(Value::as_str) != Some("tool_event") {
                return None;
            }
            record
                .pointer("/event/session_key")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();

    assert!(session_keys.contains("session-a"));
    assert!(session_keys.contains("session-b"));
}

#[test]
fn stale_warning_emitted_once_per_long_running_call() {
    let tmp = TempDir::new("stale");
    let log = tmp.path().join("stale.jsonl");
    let _ = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log)
        .expect("create stale log");

    let mut child = Command::new(binary_path())
        .args([
            "--from-start",
            "--stale-seconds",
            "1",
            "--heartbeat-seconds",
            "10",
            "--format",
            "json",
            log.to_str().expect("log path"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn follow mode");

    thread::sleep(Duration::from_millis(150));
    append_line(
        &log,
        r#"{"event":"tool_call_start","timestamp":"2025-12-31T23:59:50Z","session_key":"session-stale","tool_name":"shell","call_id":"stale-1","status":"started","level":"info"}"#,
    );
    thread::sleep(Duration::from_millis(150));
    append_line(
        &log,
        r#"{"event":"tool_call","session_key":"session-stale","tool_name":"shell","status":"in_progress","level":"info"}"#,
    );
    thread::sleep(Duration::from_millis(120));
    append_line(
        &log,
        r#"{"event":"tool_call_result","session_key":"session-stale","tool_name":"shell","call_id":"stale-1","status":"ok","level":"info"}"#,
    );

    thread::sleep(Duration::from_millis(180));
    let _ = child.kill();
    let output = child.wait_with_output().expect("collect output");
    let records = parse_json_lines(&String::from_utf8_lossy(&output.stdout));

    let warnings: Vec<_> = records
        .iter()
        .filter(|record| {
            record.get("kind").and_then(Value::as_str) == Some("stale_warning")
                && record.get("call_id").and_then(Value::as_str) == Some("stale-1")
        })
        .collect();
    assert_eq!(warnings.len(), 1);
}

#[test]
fn startup_latency_and_throughput_are_reasonable() {
    let startup_start = Instant::now();
    let (_, stderr, startup_status) = run_cli(&[
        "--no-follow",
        "--from-start",
        "--stale-seconds",
        "1000000",
        "--format",
        "json",
        fixture("one-line.fixture.jsonl")
            .to_str()
            .expect("fixture path"),
    ]);
    assert_eq!(startup_status, 0, "stderr: {stderr}");
    assert!(
        startup_start.elapsed() < Duration::from_secs(3),
        "startup should be under 3s"
    );

    let throughput_events = 2500usize;
    let tmp = TempDir::new("perf");
    let perf = tmp.path().join("throughput.jsonl");
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&perf)
        .expect("create throughput fixture");

    for i in 0..throughput_events {
        writeln!(
            file,
            "{{\"event\":\"tool_call\",\"session_key\":\"session-perf\",\"tool_name\":\"shell\",\"call_id\":\"perf-{i}\",\"status\":\"started\",\"level\":\"info\"}}"
        )
        .expect("write throughput line");
    }
    file.flush().expect("flush throughput fixture");

    let throughput_start = Instant::now();
    let (throughput_stdout, throughput_stderr, throughput_status) = run_cli(&[
        "--no-follow",
        "--from-start",
        "--format",
        "json",
        perf.to_str().expect("perf fixture path"),
    ]);
    let elapsed = throughput_start.elapsed();
    assert_eq!(throughput_status, 0, "stderr: {throughput_stderr}");
    assert!(
        elapsed < Duration::from_secs(8),
        "throughput run should finish quickly"
    );

    let parsed = parse_json_lines(&throughput_stdout);
    assert_eq!(parsed.len(), throughput_events);
    assert!(
        (throughput_events as f64) / elapsed.as_secs_f64() > 150.0,
        "throughput too low: {} lines/sec",
        (throughput_events as f64) / elapsed.as_secs_f64()
    );
}

fn auto_session_file(home: &Path, agent: &str, file_name: &str) -> PathBuf {
    let sessions_root = home
        .join(".openclaw")
        .join("agents")
        .join(agent)
        .join("sessions");

    fs::create_dir_all(&sessions_root).expect("create session directory");
    sessions_root.join(file_name)
}
