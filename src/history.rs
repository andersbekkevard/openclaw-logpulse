use crate::event::NormalizedEvent;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const HISTORY_LIMIT: usize = 10_000;

#[derive(Clone, Debug)]
pub struct PersistedEvent {
    pub observed_at: DateTime<Utc>,
    pub event: NormalizedEvent,
}

pub struct PersistedHistory {
    conn: Connection,
    path: PathBuf,
}

impl PersistedHistory {
    pub fn open_default() -> io::Result<Self> {
        Self::open(default_history_path()?)
    }

    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(&path).map_err(sqlite_error)?;
        let history = Self { conn, path };
        history.init()?;
        Ok(history)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(
        &mut self,
        observed_at: DateTime<Utc>,
        event: &NormalizedEvent,
    ) -> io::Result<()> {
        let payload = serde_json::to_string(event).map_err(json_error)?;
        let tx = self.conn.transaction().map_err(sqlite_error)?;
        tx.execute(
            "INSERT INTO persisted_history (observed_at, event_json) VALUES (?1, ?2)",
            params![observed_at.to_rfc3339(), payload],
        )
        .map_err(sqlite_error)?;
        trim_history(&tx, HISTORY_LIMIT)?;
        tx.commit().map_err(sqlite_error)?;
        Ok(())
    }

    pub fn load_recent(&self) -> io::Result<Vec<PersistedEvent>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT observed_at, event_json
                 FROM persisted_history
                 ORDER BY id DESC
                 LIMIT ?1",
            )
            .map_err(sqlite_error)?;
        let rows = stmt
            .query_map([HISTORY_LIMIT as i64], |row| {
                let observed_at = row.get::<_, String>(0)?;
                let event_json = row.get::<_, String>(1)?;
                Ok((observed_at, event_json))
            })
            .map_err(sqlite_error)?;

        let mut restored = Vec::new();
        for row in rows {
            let (observed_at, event_json) = row.map_err(sqlite_error)?;
            restored.push(PersistedEvent {
                observed_at: parse_timestamp(&observed_at)?,
                event: serde_json::from_str(&event_json).map_err(json_error)?,
            });
        }
        restored.reverse();
        Ok(restored)
    }

    pub fn clear(&self) -> io::Result<usize> {
        self.conn
            .execute("DELETE FROM persisted_history", [])
            .map_err(sqlite_error)
    }

    fn init(&self) -> io::Result<()> {
        self.conn
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                PRAGMA synchronous = NORMAL;
                PRAGMA temp_store = MEMORY;
                CREATE TABLE IF NOT EXISTS persisted_history (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    observed_at TEXT NOT NULL,
                    event_json TEXT NOT NULL
                );",
            )
            .map_err(sqlite_error)
    }

    #[cfg(test)]
    fn len(&self) -> io::Result<usize> {
        self.conn
            .query_row("SELECT COUNT(*) FROM persisted_history", [], |row| {
                row.get(0)
            })
            .map_err(sqlite_error)
    }
}

pub fn clear_default_history() -> io::Result<PathBuf> {
    let history = PersistedHistory::open_default()?;
    history.clear()?;
    Ok(history.path().to_path_buf())
}

fn trim_history(tx: &Transaction<'_>, limit: usize) -> io::Result<()> {
    let cutoff = tx
        .query_row(
            "SELECT id
             FROM persisted_history
             ORDER BY id DESC
             LIMIT 1 OFFSET ?1",
            [limit as i64],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(sqlite_error)?;

    if let Some(cutoff) = cutoff {
        tx.execute("DELETE FROM persisted_history WHERE id <= ?1", [cutoff])
            .map_err(sqlite_error)?;
    }

    Ok(())
}

fn default_history_path() -> io::Result<PathBuf> {
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
    Ok(home
        .join(".openclaw")
        .join("logpulse")
        .join("history.sqlite3"))
}

fn parse_timestamp(value: &str) -> io::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))
}

fn sqlite_error(err: rusqlite::Error) -> io::Error {
    io::Error::other(err.to_string())
}

fn json_error(err: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, err.to_string())
}

#[cfg(test)]
mod tests {
    use super::PersistedHistory;
    use crate::event::{NormalizedEvent, Severity, ToolEventKind};
    use crate::session_identity::SessionRoutingMetadata;
    use chrono::{Duration, Utc};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_db_path() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir()
            .join(format!("logpulse-history-{unique}"))
            .join("history.sqlite3")
    }

    fn event(sequence: usize) -> NormalizedEvent {
        NormalizedEvent {
            kind: ToolEventKind::ToolCallStart,
            timestamp: Some(Utc::now()),
            timestamp_raw: None,
            source_path: Some("/tmp/session.jsonl".to_string()),
            source_kind: Some("session_log".to_string()),
            session_key: Some("session".to_string()),
            session_label: Some("session".to_string()),
            session_id: Some("session".to_string()),
            session_source: Some("path".to_string()),
            session_label_source: Some("payload".to_string()),
            session_identity_conflicts: Vec::new(),
            routing: SessionRoutingMetadata::default(),
            agent_id: Some("agent".to_string()),
            agent_source: Some("path".to_string()),
            tool_name: Some("shell".to_string()),
            status: Some("started".to_string()),
            result_summary: None,
            result_preview: None,
            result_raw: None,
            result_metrics: Vec::new(),
            exit_code: None,
            duration_ms: None,
            is_error: None,
            call_id: Some(format!("call-{sequence}")),
            call_ids: vec![format!("call-{sequence}")],
            correlation_ids: Vec::new(),
            message_id: None,
            parent_message_id: None,
            transcript_tool_call_index: None,
            transcript_tool_call_count: None,
            level: Severity::Info,
            level_raw: Some("info".to_string()),
            params: vec![("command".to_string(), format!("echo {sequence}"))],
            args_preview: vec![("command".to_string(), format!("echo {sequence}"))],
            args_raw: None,
            args_truncated: false,
            message: Some(format!("event {sequence}")),
            raw_line: format!("{{\"sequence\":{sequence}}}"),
        }
    }

    #[test]
    fn restores_events_in_append_order() {
        let path = temp_db_path();
        let mut history = PersistedHistory::open(&path).expect("open history");
        let first_at = Utc::now();
        let second_at = first_at + Duration::seconds(1);
        history.append(first_at, &event(1)).expect("append first");
        history.append(second_at, &event(2)).expect("append second");

        let restored = history.load_recent().expect("load recent");
        assert_eq!(restored.len(), 2);
        assert_eq!(restored[0].observed_at, first_at);
        assert_eq!(restored[0].event.call_id.as_deref(), Some("call-1"));
        assert_eq!(restored[1].observed_at, second_at);
        assert_eq!(restored[1].event.call_id.as_deref(), Some("call-2"));

        fs::remove_dir_all(path.parent().expect("parent")).expect("cleanup");
    }

    #[test]
    fn retains_only_the_newest_ten_thousand_events() {
        let path = temp_db_path();
        let mut history = PersistedHistory::open(&path).expect("open history");
        let base = Utc::now();
        for index in 0..10_001 {
            history
                .append(base + Duration::seconds(index as i64), &event(index))
                .expect("append");
        }

        let restored = history.load_recent().expect("load recent");
        assert_eq!(history.len().expect("count"), 10_000);
        assert_eq!(restored.len(), 10_000);
        assert_eq!(restored[0].event.call_id.as_deref(), Some("call-1"));
        assert_eq!(
            restored
                .last()
                .and_then(|entry| entry.event.call_id.as_deref()),
            Some("call-10000")
        );

        fs::remove_dir_all(path.parent().expect("parent")).expect("cleanup");
    }

    #[test]
    fn clear_deletes_persisted_history() {
        let path = temp_db_path();
        let mut history = PersistedHistory::open(&path).expect("open history");
        history.append(Utc::now(), &event(1)).expect("append");
        history.append(Utc::now(), &event(2)).expect("append");

        let deleted = history.clear().expect("clear");
        assert_eq!(deleted, 2);
        assert_eq!(history.len().expect("count"), 0);
        assert!(history.load_recent().expect("load recent").is_empty());

        fs::remove_dir_all(path.parent().expect("parent")).expect("cleanup");
    }
}
