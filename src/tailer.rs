use std::collections::HashSet;
use std::fs::{self, File, Metadata};
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct SourceFileIdentity {
    pub key: String,
    pub inode: u64,
    pub device: u64,
}

#[derive(Clone, Debug)]
pub struct TailedLine {
    pub offset: u64,
    pub next_offset: u64,
    pub line: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
enum SourceIdentity {
    #[cfg(unix)]
    Unix { inode: u64, device: u64 },
    #[cfg(not(unix))]
    Path(PathBuf),
}

impl SourceIdentity {
    #[cfg(unix)]
    fn from_meta(_path: &Path, meta: &Metadata) -> Self {
        let (inode, device, _) = file_id_and_size(meta);
        Self::Unix { inode, device }
    }

    #[cfg(not(unix))]
    fn from_meta(path: &Path, _meta: &Metadata) -> Self {
        Self::Path(path.to_path_buf())
    }
}

pub struct Tailer {
    path: PathBuf,
    reader: Option<BufReader<File>>,
    position: u64,
    inode: u64,
    device: u64,
    follow: bool,
    poll_interval: Duration,
}

impl Tailer {
    pub fn new(
        path: PathBuf,
        follow: bool,
        from_start: bool,
        poll_interval: Duration,
    ) -> io::Result<Self> {
        let mut state = Self {
            path,
            reader: None,
            position: 0,
            inode: 0,
            device: 0,
            follow,
            poll_interval,
        };
        state.reopen(from_start)?;
        Ok(state)
    }

    pub fn next_line(&mut self) -> io::Result<Option<String>> {
        Ok(self.next_line_with_offset()?.map(|line| line.line))
    }

    pub fn next_line_with_offset(&mut self) -> io::Result<Option<TailedLine>> {
        if self.reader.is_none() {
            if !self.follow {
                return Ok(None);
            }
            if self.try_open_follow_mode().is_err() {
                return Ok(None);
            }
        }

        if self.reader.is_none() {
            return Ok(None);
        }

        let reader = self.reader.as_mut().expect("reader initialized");
        let mut line = String::new();
        let offset = self.position;
        let bytes = reader.read_line(&mut line)?;
        if bytes > 0 {
            self.position += bytes as u64;
            return Ok(Some(TailedLine {
                offset,
                next_offset: self.position,
                line,
            }));
        }

        self.handle_eof_or_rotation()?;

        Ok(None)
    }

    fn handle_eof_or_rotation(&mut self) -> io::Result<()> {
        let reader_meta = self.reader_meta();
        let path_meta = fs::metadata(&self.path).ok();

        match (reader_meta, path_meta) {
            (Some(reader_meta), Some(path_meta)) => {
                let (path_inode, path_device, path_size) = file_id_and_size(&path_meta);
                let (_reader_inode, _reader_device, reader_size) = file_id_and_size(&reader_meta);

                if self.inode != path_inode || self.device != path_device {
                    self.reopen(true)?;
                    return Ok(());
                }

                if self.position > path_size || self.position > reader_size {
                    self.reopen(true)?;
                }
            }
            (Some(reader_meta), None) => {
                let (_, _, reader_size) = file_id_and_size(&reader_meta);
                if self.position >= reader_size {
                    self.reader = None;
                }
            }
            _ => {
                self.reader = None;
            }
        }

        Ok(())
    }

    fn reopen(&mut self, from_start: bool) -> io::Result<()> {
        let mut file = File::open(&self.path)?;
        let metadata = file.metadata()?;
        let (inode, device, size) = file_id_and_size(&metadata);
        let start = if from_start { 0 } else { size };
        file.seek(SeekFrom::Start(start))?;
        self.reader = Some(BufReader::new(file));
        self.position = start;
        self.inode = inode;
        self.device = device;
        Ok(())
    }

    pub fn new_at(
        path: PathBuf,
        follow: bool,
        start_position: u64,
        poll_interval: Duration,
    ) -> io::Result<Self> {
        let mut state = Self {
            path,
            reader: None,
            position: 0,
            inode: 0,
            device: 0,
            follow,
            poll_interval,
        };
        state.reopen_at(start_position)?;
        Ok(state)
    }

    fn reopen_at(&mut self, start_position: u64) -> io::Result<()> {
        let mut file = File::open(&self.path)?;
        let metadata = file.metadata()?;
        let (inode, device, size) = file_id_and_size(&metadata);
        let start = start_position.min(size);
        file.seek(SeekFrom::Start(start))?;
        self.reader = Some(BufReader::new(file));
        self.position = start;
        self.inode = inode;
        self.device = device;
        Ok(())
    }

    fn reader_meta(&self) -> Option<Metadata> {
        self.reader
            .as_ref()
            .and_then(|reader| reader.get_ref().metadata().ok())
    }

    fn try_open_follow_mode(&mut self) -> io::Result<()> {
        match fs::metadata(&self.path) {
            Ok(meta) => {
                let mut file = File::open(&self.path)?;
                let (_inode, _device, size) = file_id_and_size(&meta);
                let start = self.position.min(size);
                file.seek(SeekFrom::Start(start))?;
                self.reader = Some(BufReader::new(file));
                self.position = start;
                let (inode, device, _) = file_id_and_size(&meta);
                self.inode = inode;
                self.device = device;
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    pub fn poll_interval(&self) -> Duration {
        self.poll_interval
    }

    pub(crate) fn has_reader(&self) -> bool {
        self.reader.is_some()
    }

    pub(crate) fn set_path(&mut self, path: PathBuf) {
        self.path = path;
    }
}

pub fn source_file_identity(path: &Path) -> io::Result<SourceFileIdentity> {
    let meta = fs::metadata(path)?;
    let (inode, device, _) = file_id_and_size(&meta);
    let key = if inode == 0 && device == 0 {
        format!("path:{}", path.display())
    } else {
        format!("dev:{device}:ino:{inode}")
    };
    Ok(SourceFileIdentity { key, inode, device })
}

pub struct MultiTailer {
    sources: Vec<TrackedSource>,
    follow: bool,
    from_start: bool,
    poll_interval: Duration,
    missing_ttl: Duration,
    next_index: usize,
}

struct TrackedSource {
    path: PathBuf,
    identity: SourceIdentity,
    tailer: Tailer,
    missing_since: Option<Instant>,
}

impl MultiTailer {
    pub fn new(
        paths: Vec<PathBuf>,
        follow: bool,
        from_start: bool,
        poll_interval: Duration,
        missing_ttl: Duration,
    ) -> Self {
        let mut state = Self {
            sources: Vec::new(),
            follow,
            from_start,
            poll_interval,
            missing_ttl,
            next_index: 0,
        };
        state.sync(paths);
        state
    }

    pub fn sync(&mut self, paths: Vec<PathBuf>) {
        let mut seen = HashSet::new();
        let now = Instant::now();

        for path in paths {
            let metadata = match fs::metadata(&path) {
                Ok(meta) => meta,
                Err(_) => continue,
            };

            if !metadata.is_file() {
                continue;
            }

            let identity = SourceIdentity::from_meta(&path, &metadata);
            if !seen.insert(identity.clone()) {
                continue;
            }

            if let Some(index) = self
                .sources
                .iter()
                .position(|source| source.identity == identity)
            {
                let source = &mut self.sources[index];
                source.path = path.clone();
                source.tailer.set_path(path);
                source.missing_since = None;
                source.tailer.set_path(source.path.clone());
                continue;
            }

            if let Ok(mut tailer) = Tailer::new(
                path.clone(),
                self.follow,
                self.from_start,
                self.poll_interval,
            ) {
                tailer.set_path(path.clone());
                self.sources.push(TrackedSource {
                    path,
                    identity,
                    tailer,
                    missing_since: None,
                });
            }
        }

        for source in &mut self.sources {
            if seen.contains(&source.identity) {
                source.missing_since = None;
                continue;
            }

            if source.missing_since.is_none() {
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

            if !self.follow {
                return false;
            }

            source
                .missing_since
                .is_none_or(|since| now.duration_since(since) < self.missing_ttl)
        });

        self.sources.sort_by(|a, b| a.path.cmp(&b.path));
        if self.next_index >= self.sources.len() {
            self.next_index = 0;
        }
    }

    pub fn next_line(&mut self) -> io::Result<Option<(PathBuf, String)>> {
        if self.sources.is_empty() {
            return Ok(None);
        }

        let mut last_error: Option<io::Error> = None;
        let total = self.sources.len();

        for _ in 0..total {
            let idx = self.next_index;
            self.next_index = (idx + 1) % total;

            match self.sources[idx].tailer.next_line() {
                Ok(Some(line)) => {
                    let path = self.sources[idx].path.clone();
                    return Ok(Some((path, line)));
                }
                Ok(None) => {}
                Err(err) => {
                    last_error = Some(io::Error::new(
                        err.kind(),
                        format!("{}: {}", self.sources[idx].path.display(), err),
                    ));
                }
            }
        }

        if let Some(err) = last_error {
            return Err(err);
        }

        Ok(None)
    }

    pub fn is_done(&self) -> bool {
        !self.follow
            && self
                .sources
                .iter()
                .all(|source| !source.tailer.has_reader())
    }

    pub fn poll_interval(&self) -> Duration {
        self.poll_interval
    }
}

fn file_id_and_size(meta: &Metadata) -> (u64, u64, u64) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (meta.ino(), meta.dev(), meta.len())
    }
    #[cfg(not(unix))]
    {
        let (ino, dev) = (0, 0);
        (ino, dev, meta.len())
    }
}
