use crate::event::Severity;
use crate::output::OutputMode;
use clap::{Parser, ValueEnum};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum LevelArg {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

impl LevelArg {
    fn as_severity(self) -> Severity {
        match self {
            LevelArg::Trace => Severity::Trace,
            LevelArg::Debug => Severity::Debug,
            LevelArg::Info => Severity::Info,
            LevelArg::Warn => Severity::Warn,
            LevelArg::Error => Severity::Error,
            LevelArg::Fatal => Severity::Fatal,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "openclaw-logpulse",
    version,
    about = "Live visibility for OpenClaw tool calls"
)]
pub struct Args {
    #[arg(value_name = "LOG_FILE")]
    pub log_file: Option<PathBuf>,

    #[arg(short = 's', long = "session", value_name = "SUBSTRING")]
    pub session: Option<String>,

    #[arg(short = 't', long = "tool", value_name = "NAME")]
    pub tool: Option<String>,

    #[arg(short = 'a', long = "agent", value_name = "SUBSTRING")]
    pub agent: Option<String>,

    #[arg(long = "since", value_name = "TIMESTAMP")]
    pub since: Option<String>,

    #[arg(long = "until", value_name = "TIMESTAMP")]
    pub until: Option<String>,

    #[arg(long = "min-level", default_value = "trace", value_enum)]
    min_level: LevelArg,

    #[arg(long = "format", default_value = "tui", value_enum)]
    pub format: OutputMode,

    #[arg(long = "stale-seconds", default_value_t = 30)]
    pub stale_seconds: u64,

    #[arg(long = "heartbeat-seconds", default_value_t = 10)]
    pub heartbeat_seconds: u64,

    #[arg(long = "poll-millis", default_value_t = 300)]
    pub poll_millis: u64,

    #[arg(long = "from-start")]
    pub from_start: bool,

    #[arg(long = "no-follow")]
    pub no_follow: bool,
}

impl Args {
    pub fn poll_duration(&self) -> Duration {
        Duration::from_millis(self.poll_millis)
    }

    pub fn heartbeat_duration(&self) -> Duration {
        Duration::from_secs(self.heartbeat_seconds)
    }

    pub fn min_severity(&self) -> Severity {
        self.min_level.as_severity()
    }
}
