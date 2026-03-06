use std::fs::{self, File, Metadata};
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::Duration;

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
        let bytes = reader.read_line(&mut line)?;
        if bytes > 0 {
            self.position += bytes as u64;
            return Ok(Some(line));
        }

        self.handle_eof_or_rotation()?;

        Ok(None)
    }

    fn handle_eof_or_rotation(&mut self) -> io::Result<()> {
        let meta = match fs::metadata(&self.path) {
            Ok(meta) => meta,
            Err(_) => {
                self.reader = None;
                return Ok(());
            }
        };

        let (inode, device, size) = file_id_and_size(&meta);
        if self.inode != inode || self.device != device || self.position > size {
            self.reopen(true)?;
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

    fn try_open_follow_mode(&mut self) -> io::Result<()> {
        match fs::metadata(&self.path) {
            Ok(meta) => {
                let mut file = File::open(&self.path)?;
                let (_inode, _device, size) = file_id_and_size(&meta);
                file.seek(SeekFrom::End(0))?;
                self.reader = Some(BufReader::new(file));
                self.position = size;
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
