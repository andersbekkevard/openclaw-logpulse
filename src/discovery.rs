use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

pub fn discover_session_logs(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();

    let sessions_root = root.join("agents");
    if !sessions_root.is_dir() {
        return Ok(paths);
    }

    for agent_entry in fs::read_dir(sessions_root)? {
        let agent_entry = match agent_entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };

        let agent_type = match agent_entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if !agent_type.is_dir() {
            continue;
        }

        let agent_path = agent_entry.path();
        let candidate_dir = agent_path.join("sessions");
        if !candidate_dir.is_dir() {
            collect_nested_codex_session_logs(&agent_path, &mut paths);
            continue;
        }

        for file_entry in match fs::read_dir(&candidate_dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        } {
            let file_entry = match file_entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };

            let file_path = file_entry.path();
            if !is_session_log_file(&file_path) {
                continue;
            }

            paths.push(file_path);
        }

        collect_nested_codex_session_logs(&agent_path, &mut paths);
    }

    paths.sort_by_key(|path| session_file_sort_key(path.as_path()));
    Ok(paths)
}

fn collect_nested_codex_session_logs(agent_path: &Path, paths: &mut Vec<PathBuf>) {
    let nested_agents_dir = agent_path.join("agent");
    let Ok(nested_agents) = fs::read_dir(nested_agents_dir) else {
        return;
    };

    for nested_agent in nested_agents.flatten() {
        let Ok(file_type) = nested_agent.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        collect_rollout_logs(&nested_agent.path().join("sessions"), paths);
    }
}

fn collect_rollout_logs(root: &Path, paths: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_rollout_logs(&path, paths);
        } else if is_codex_rollout_log_file(&path) {
            paths.push(path);
        }
    }
}

fn is_session_log_file(path: &Path) -> bool {
    let file_name = match path.file_name().and_then(|name| name.to_str()) {
        Some(name) => name,
        None => return false,
    };

    if file_name == "sessions.json" || file_name.starts_with("sessions.json.") {
        return false;
    }

    if file_name.ends_with(".jsonl.lock") {
        return false;
    }

    if let Some(base) = file_name.strip_suffix(".jsonl") {
        return is_uuid(base);
    }

    let Some((base, rest)) = file_name.split_once(".jsonl.") else {
        return false;
    };

    if !(rest.starts_with("deleted.") || rest.starts_with("reset.")) {
        return false;
    }

    is_uuid(base)
}

fn is_codex_rollout_log_file(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    file_name.starts_with("rollout-") && file_name.ends_with(".jsonl")
}

fn is_uuid(value: &str) -> bool {
    let segments: [usize; 5] = [8, 4, 4, 4, 12];
    let mut start = 0;

    for (index, size) in segments.iter().enumerate() {
        let segment_end = start + size;
        if value.len() < segment_end {
            return false;
        }

        let segment = &value[start..segment_end];
        if !segment.chars().all(|c| c.is_ascii_hexdigit()) {
            return false;
        }

        if *size < value.len() && index < segments.len() - 1 {
            if value.as_bytes().get(segment_end) != Some(&b'-') {
                return false;
            }
            start = segment_end + 1;
        } else {
            start = segment_end;
        }
    }

    value.len() == 36
}

fn session_file_sort_key(path: &Path) -> (String, u8, String) {
    let root = path
        .parent()
        .and_then(|parent| parent.to_str())
        .unwrap_or("")
        .to_string();

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");

    let file_type = if file_name.ends_with(".jsonl") {
        0
    } else if file_name.contains(".jsonl.deleted.") {
        1
    } else if file_name.contains(".jsonl.reset.") {
        2
    } else {
        3
    };

    (root, file_type, file_name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!("logpulse-discovery-{unique}"))
    }

    #[test]
    fn discovers_openclaw_sessions_and_nested_codex_rollouts() {
        let root = temp_root();
        let openclaw_sessions = root.join("agents").join("main").join("sessions");
        let codex_sessions = root
            .join("agents")
            .join("main")
            .join("agent")
            .join("codex-home")
            .join("sessions")
            .join("2026")
            .join("05")
            .join("20");
        fs::create_dir_all(&openclaw_sessions).expect("openclaw sessions dir");
        fs::create_dir_all(&codex_sessions).expect("codex sessions dir");

        let openclaw_log = openclaw_sessions.join("12345678-1234-1234-1234-123456789abc.jsonl");
        let trajectory =
            openclaw_sessions.join("12345678-1234-1234-1234-123456789abc.trajectory.jsonl");
        let rollout_log = codex_sessions.join("rollout-2026-05-20T17-06-34-019e465a.jsonl");
        let unrelated = codex_sessions.join("notes.jsonl");
        fs::write(&openclaw_log, "").expect("write openclaw log");
        fs::write(&trajectory, "").expect("write trajectory");
        fs::write(&rollout_log, "").expect("write rollout log");
        fs::write(&unrelated, "").expect("write unrelated");

        let discovered = discover_session_logs(&root).expect("discover logs");

        assert!(discovered.contains(&openclaw_log));
        assert!(discovered.contains(&rollout_log));
        assert!(!discovered.contains(&trajectory));
        assert!(!discovered.contains(&unrelated));

        fs::remove_dir_all(root).expect("cleanup");
    }
}
