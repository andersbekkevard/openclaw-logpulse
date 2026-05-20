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
    pub id: i64,
    pub observed_at: DateTime<Utc>,
    pub event: NormalizedEvent,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct SourceCheckpoint {
    pub source_key: String,
    pub path: String,
    pub position: u64,
    pub inode: u64,
    pub device: u64,
    pub updated_at: DateTime<Utc>,
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

    #[allow(dead_code)]
    pub fn append(
        &mut self,
        observed_at: DateTime<Utc>,
        event: &NormalizedEvent,
    ) -> io::Result<bool> {
        self.append_with_source(observed_at, event, None)
    }

    pub fn append_source_event(
        &mut self,
        observed_at: DateTime<Utc>,
        event: &NormalizedEvent,
        source: SourceEventPosition<'_>,
    ) -> io::Result<bool> {
        self.append_with_source(observed_at, event, Some(source))
    }

    fn append_with_source(
        &mut self,
        observed_at: DateTime<Utc>,
        event: &NormalizedEvent,
        source: Option<SourceEventPosition<'_>>,
    ) -> io::Result<bool> {
        let payload = serde_json::to_string(event).map_err(json_error)?;
        let event_key = source.as_ref().map(|source| {
            format!(
                "{}:{}:{}",
                source.source_key, source.offset, source.event_index
            )
        });
        let tx = self.conn.transaction().map_err(sqlite_error)?;
        let inserted = tx
            .execute(
                "INSERT OR IGNORE INTO persisted_history
                    (observed_at, event_json, source_key, source_path, source_offset, event_index, event_key)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    observed_at.to_rfc3339(),
                    payload,
                    source.as_ref().map(|source| source.source_key),
                    source.as_ref().map(|source| source.source_path),
                    source.as_ref().map(|source| source.offset as i64),
                    source.as_ref().map(|source| source.event_index as i64),
                    event_key,
                ],
            )
            .map_err(sqlite_error)?
            > 0;
        if inserted {
            trim_history(&tx, HISTORY_LIMIT)?;
        }
        tx.commit().map_err(sqlite_error)?;
        Ok(inserted)
    }

    pub fn update_checkpoint(&self, checkpoint: SourceCheckpointUpdate<'_>) -> io::Result<()> {
        self.conn
            .execute(
                "INSERT INTO source_checkpoints
                    (source_key, path, position, inode, device, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(source_key) DO UPDATE SET
                    path = excluded.path,
                    position = excluded.position,
                    inode = excluded.inode,
                    device = excluded.device,
                    updated_at = excluded.updated_at",
                params![
                    checkpoint.source_key,
                    checkpoint.path,
                    checkpoint.position as i64,
                    checkpoint.inode as i64,
                    checkpoint.device as i64,
                    checkpoint.updated_at.to_rfc3339(),
                ],
            )
            .map_err(sqlite_error)?;
        Ok(())
    }

    pub fn checkpoint(&self, source_key: &str) -> io::Result<Option<SourceCheckpoint>> {
        self.conn
            .query_row(
                "SELECT source_key, path, position, inode, device, updated_at
                 FROM source_checkpoints
                 WHERE source_key = ?1",
                [source_key],
                |row| {
                    let updated_at = row.get::<_, String>(5)?;
                    Ok(SourceCheckpoint {
                        source_key: row.get(0)?,
                        path: row.get(1)?,
                        position: row.get::<_, i64>(2)?.max(0) as u64,
                        inode: row.get::<_, i64>(3)?.max(0) as u64,
                        device: row.get::<_, i64>(4)?.max(0) as u64,
                        updated_at: parse_timestamp_rusqlite(&updated_at)?,
                    })
                },
            )
            .optional()
            .map_err(sqlite_error)
    }

    pub fn load_after_id(&self, id: i64, limit: usize) -> io::Result<Vec<PersistedEvent>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, observed_at, event_json
                 FROM persisted_history
                 WHERE id > ?1
                 ORDER BY id ASC
                 LIMIT ?2",
            )
            .map_err(sqlite_error)?;
        let rows = stmt
            .query_map(params![id, limit as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(sqlite_error)?;
        load_events_from_rows(rows)
    }

    pub fn load_recent(&self) -> io::Result<Vec<PersistedEvent>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, observed_at, event_json
                 FROM persisted_history
                 ORDER BY id DESC
                 LIMIT ?1",
            )
            .map_err(sqlite_error)?;
        let mut restored = load_events_from_rows(
            stmt.query_map([HISTORY_LIMIT as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(sqlite_error)?,
        )?;
        restored.reverse();
        Ok(restored)
    }

    pub fn clear(&self) -> io::Result<usize> {
        let tx = self.conn.unchecked_transaction().map_err(sqlite_error)?;
        let deleted = tx
            .execute("DELETE FROM persisted_history", [])
            .map_err(sqlite_error)?;
        tx.execute("DELETE FROM source_checkpoints", [])
            .map_err(sqlite_error)?;
        tx.commit().map_err(sqlite_error)?;
        Ok(deleted)
    }

    pub fn max_id(&self) -> io::Result<i64> {
        self.conn
            .query_row(
                "SELECT COALESCE(MAX(id), 0) FROM persisted_history",
                [],
                |row| row.get(0),
            )
            .map_err(sqlite_error)
    }

    pub fn compact_if_fragmented(&self, min_free_pages: usize) -> io::Result<bool> {
        let free_pages = self
            .conn
            .query_row("PRAGMA freelist_count", [], |row| row.get::<_, i64>(0))
            .map_err(sqlite_error)?
            .max(0) as usize;
        if free_pages < min_free_pages {
            return Ok(false);
        }

        self.conn
            .execute_batch(
                "PRAGMA wal_checkpoint(TRUNCATE);
                 VACUUM;",
            )
            .map_err(sqlite_error)?;
        Ok(true)
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
                );
                CREATE TABLE IF NOT EXISTS source_checkpoints (
                    source_key TEXT PRIMARY KEY,
                    path TEXT NOT NULL,
                    position INTEGER NOT NULL,
                    inode INTEGER NOT NULL,
                    device INTEGER NOT NULL,
                    updated_at TEXT NOT NULL
                );",
            )
            .map_err(sqlite_error)?;
        self.ensure_column("persisted_history", "source_key", "TEXT")?;
        self.ensure_column("persisted_history", "source_path", "TEXT")?;
        self.ensure_column("persisted_history", "source_offset", "INTEGER")?;
        self.ensure_column("persisted_history", "event_index", "INTEGER")?;
        self.ensure_column("persisted_history", "event_key", "TEXT")?;
        self.conn
            .execute_batch(
                "CREATE UNIQUE INDEX IF NOT EXISTS idx_persisted_history_event_key
                    ON persisted_history(event_key)
                    WHERE event_key IS NOT NULL;
                 CREATE INDEX IF NOT EXISTS idx_persisted_history_source
                    ON persisted_history(source_key, source_offset);",
            )
            .map_err(sqlite_error)
    }

    fn ensure_column(&self, table: &str, column: &str, definition: &str) -> io::Result<()> {
        let mut stmt = self
            .conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .map_err(sqlite_error)?;
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_error)?;
        if columns.iter().any(|candidate| candidate == column) {
            return Ok(());
        }
        self.conn
            .execute(
                &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
                [],
            )
            .map_err(sqlite_error)?;
        Ok(())
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

#[derive(Clone, Copy, Debug)]
pub struct SourceEventPosition<'a> {
    pub source_key: &'a str,
    pub source_path: &'a str,
    pub offset: u64,
    pub event_index: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct SourceCheckpointUpdate<'a> {
    pub source_key: &'a str,
    pub path: &'a str,
    pub position: u64,
    pub inode: u64,
    pub device: u64,
    pub updated_at: DateTime<Utc>,
}

pub fn clear_default_history() -> io::Result<PathBuf> {
    let history = PersistedHistory::open_default()?;
    history.clear()?;
    history.compact_if_fragmented(1)?;
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

fn parse_timestamp_rusqlite(value: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
        })
}

fn load_events_from_rows<F>(rows: rusqlite::MappedRows<'_, F>) -> io::Result<Vec<PersistedEvent>>
where
    F: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<(i64, String, String)>,
{
    let mut restored = Vec::new();
    for row in rows {
        let (id, observed_at, event_json) = row.map_err(sqlite_error)?;
        restored.push(PersistedEvent {
            id,
            observed_at: parse_timestamp(&observed_at)?,
            event: serde_json::from_str(&event_json).map_err(json_error)?,
        });
    }
    Ok(restored)
}

fn sqlite_error(err: rusqlite::Error) -> io::Error {
    io::Error::other(err.to_string())
}

fn json_error(err: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, err.to_string())
}

#[cfg(test)]
mod tests {
    use super::{PersistedHistory, SourceCheckpointUpdate, SourceEventPosition};
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

    #[test]
    fn source_events_are_idempotent_and_checkpointed() {
        let path = temp_db_path();
        let mut history = PersistedHistory::open(&path).expect("open history");
        let observed_at = Utc::now();
        let item = event(1);

        let inserted = history
            .append_source_event(
                observed_at,
                &item,
                SourceEventPosition {
                    source_key: "dev:1:ino:2",
                    source_path: "/tmp/session.jsonl",
                    offset: 42,
                    event_index: 0,
                },
            )
            .expect("append source event");
        let duplicate = history
            .append_source_event(
                observed_at,
                &item,
                SourceEventPosition {
                    source_key: "dev:1:ino:2",
                    source_path: "/tmp/session.jsonl",
                    offset: 42,
                    event_index: 0,
                },
            )
            .expect("append duplicate source event");

        assert!(inserted);
        assert!(!duplicate);
        assert_eq!(history.len().expect("count"), 1);

        history
            .update_checkpoint(SourceCheckpointUpdate {
                source_key: "dev:1:ino:2",
                path: "/tmp/session.jsonl",
                position: 99,
                inode: 2,
                device: 1,
                updated_at: observed_at,
            })
            .expect("checkpoint");
        let checkpoint = history
            .checkpoint("dev:1:ino:2")
            .expect("load checkpoint")
            .expect("checkpoint exists");
        assert_eq!(checkpoint.position, 99);

        let restored = history.load_after_id(0, 10).expect("load after");
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].id, history.max_id().expect("max id"));

        fs::remove_dir_all(path.parent().expect("parent")).expect("cleanup");
    }
}
