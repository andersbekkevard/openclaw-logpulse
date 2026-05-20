use crate::cli::Args;
use crate::discovery;
use crate::history::{PersistedHistory, SourceCheckpointUpdate, SourceEventPosition};
use crate::normalizer::normalize_many_with_source;
use crate::tailer::{source_file_identity, SourceFileIdentity, TailedLine, Tailer};
use chrono::Utc;
use std::collections::HashSet;
use std::env;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const MISSING_TTL_SECONDS: u64 = 30;
const COMPACT_FREE_PAGE_THRESHOLD: usize = 1024;
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(600);

struct SourceReader {
    path: PathBuf,
    identity: SourceFileIdentity,
    tailer: Tailer,
    missing_since: Option<Instant>,
}

pub fn run(args: &Args, auto_discover: bool) -> io::Result<()> {
    let mut collector = Collector::new(args)?;
    let mut last_scan = Instant::now();

    if auto_discover {
        collector.sync(discover_initial_session_logs())?;
    } else if let Some(path) = args.log_file.clone() {
        collector.sync(vec![path])?;
    }

    loop {
        collector.run_maintenance()?;

        if auto_discover && !args.no_follow && last_scan.elapsed() >= collector.poll_interval() {
            collector.sync(discover_initial_session_logs())?;
            last_scan = Instant::now();
        }

        match collector.drain_once()? {
            true => {}
            false if args.no_follow => break,
            false => std::thread::sleep(collector.poll_interval()),
        }
    }

    Ok(())
}

struct Collector {
    history: PersistedHistory,
    sources: Vec<SourceReader>,
    follow: bool,
    poll_interval: Duration,
    missing_ttl: Duration,
    next_index: usize,
    last_maintenance: Instant,
}

impl Collector {
    fn new(args: &Args) -> io::Result<Self> {
        let history = PersistedHistory::open_default()?;
        history.compact_if_fragmented(COMPACT_FREE_PAGE_THRESHOLD)?;
        Ok(Self {
            history,
            sources: Vec::new(),
            follow: !args.no_follow,
            poll_interval: args.poll_duration(),
            missing_ttl: Duration::from_secs(MISSING_TTL_SECONDS),
            next_index: 0,
            last_maintenance: Instant::now(),
        })
    }

    fn sync(&mut self, paths: Vec<PathBuf>) -> io::Result<()> {
        let mut seen = HashSet::new();
        let now = Instant::now();

        for path in paths {
            let identity = match source_file_identity(&path) {
                Ok(identity) => identity,
                Err(_) => continue,
            };
            if !seen.insert(identity.key.clone()) {
                continue;
            }

            if let Some(existing) = self
                .sources
                .iter_mut()
                .find(|source| source.identity.key == identity.key)
            {
                existing.path = path.clone();
                existing.identity = identity;
                existing.tailer.set_path(path);
                existing.missing_since = None;
                continue;
            }

            let checkpoint = self.history.checkpoint(&identity.key)?;
            let start_position = checkpoint.map_or(0, |checkpoint| checkpoint.position);
            let mut tailer = Tailer::new_at(
                path.clone(),
                self.follow,
                start_position,
                self.poll_interval,
            )?;
            tailer.set_path(path.clone());
            self.sources.push(SourceReader {
                path,
                identity,
                tailer,
                missing_since: None,
            });
        }

        for source in &mut self.sources {
            if seen.contains(&source.identity.key) {
                source.missing_since = None;
            } else if source.missing_since.is_none() {
                source.missing_since = Some(now);
            }
        }

        self.sources.retain(|source| {
            if source.missing_since.is_none() {
                return true;
            }
            if source.tailer.has_reader() {
                return true;
            }
            self.follow
                && source
                    .missing_since
                    .is_none_or(|since| now.duration_since(since) < self.missing_ttl)
        });
        self.sources
            .sort_by(|left, right| left.path.cmp(&right.path));
        if self.next_index >= self.sources.len() {
            self.next_index = 0;
        }

        Ok(())
    }

    fn drain_once(&mut self) -> io::Result<bool> {
        let Some((source_index, line)) = self.next_line()? else {
            return Ok(false);
        };

        let source = &self.sources[source_index];
        let path = source.path.clone();
        let identity = source.identity.clone();
        self.ingest_line(path, identity, line)?;
        Ok(true)
    }

    fn next_line(&mut self) -> io::Result<Option<(usize, TailedLine)>> {
        if self.sources.is_empty() {
            return Ok(None);
        }

        let total = self.sources.len();
        for _ in 0..total {
            let idx = self.next_index;
            self.next_index = (idx + 1) % total;
            match self.sources[idx].tailer.next_line_with_offset() {
                Ok(Some(line)) => return Ok(Some((idx, line))),
                Ok(None) => {}
                Err(err) => {
                    return Err(io::Error::new(
                        err.kind(),
                        format!("{}: {err}", self.sources[idx].path.display()),
                    ));
                }
            }
        }

        Ok(None)
    }

    fn ingest_line(
        &mut self,
        path: PathBuf,
        identity: SourceFileIdentity,
        line: TailedLine,
    ) -> io::Result<()> {
        let now = Utc::now();
        let source_path = path.display().to_string();
        for (event_index, event) in normalize_many_with_source(&line.line, Some(path.as_path()))
            .into_iter()
            .enumerate()
        {
            self.history.append_source_event(
                now,
                &event,
                SourceEventPosition {
                    source_key: &identity.key,
                    source_path: &source_path,
                    offset: line.offset,
                    event_index,
                },
            )?;
        }
        self.history.update_checkpoint(SourceCheckpointUpdate {
            source_key: &identity.key,
            path: &source_path,
            position: line.next_offset,
            inode: identity.inode,
            device: identity.device,
            updated_at: now,
        })?;
        Ok(())
    }

    fn poll_interval(&self) -> Duration {
        self.poll_interval
    }

    fn run_maintenance(&mut self) -> io::Result<()> {
        if self.last_maintenance.elapsed() < MAINTENANCE_INTERVAL {
            return Ok(());
        }
        self.history
            .compact_if_fragmented(COMPACT_FREE_PAGE_THRESHOLD)?;
        self.last_maintenance = Instant::now();
        Ok(())
    }
}

fn discover_initial_session_logs() -> Vec<PathBuf> {
    let home_dir = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from);

    match home_dir {
        Some(home) => {
            let root = home.join(".openclaw");
            discovery::discover_session_logs(&root).unwrap_or_else(|_| Vec::new())
        }
        None => Vec::new(),
    }
}
