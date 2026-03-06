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

        let candidate_dir = agent_entry.path().join("sessions");
        if !candidate_dir.is_dir() {
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
    }

    paths.sort_by_key(|path| session_file_sort_key(path.as_path()));
    Ok(paths)
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
