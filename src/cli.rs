use crate::event::{Severity, TimeFilter};
use crate::output::OutputMode;
use chrono::{DateTime, Utc};
use clap::error::ErrorKind;
use clap::{CommandFactory, Parser, ValueEnum};
use std::ffi::OsString;
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
    name = "logpulse",
    version,
    about = "Live visibility for OpenClaw tool calls",
    after_help = "Special commands:\n  logpulse daemon         Run the collector explicitly (the TUI auto-starts it when following)\n  logpulse tui --fresh    Launch the TUI without restoring persisted history\n  logpulse tui clear      Delete the persisted TUI history store"
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

    #[arg(long = "fresh")]
    pub fresh: bool,
}

#[derive(Debug)]
pub(crate) enum CliCommand {
    Run { args: Args, auto_discover: bool },
    Daemon { args: Args, auto_discover: bool },
    TuiClear,
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

    pub fn time_filter(&self) -> Result<TimeFilter, String> {
        Ok(TimeFilter {
            since: self.since.as_deref().map(parse_cli_timestamp).transpose()?,
            until: self.until.as_deref().map(parse_cli_timestamp).transpose()?,
        })
    }
}

pub(crate) fn parse_command() -> Result<CliCommand, clap::Error> {
    parse_command_from(std::env::args_os())
}

pub(crate) fn parse_command_from<I, T>(input: I) -> Result<CliCommand, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let raw = input.into_iter().map(Into::into).collect::<Vec<OsString>>();
    let program = raw
        .first()
        .cloned()
        .unwrap_or_else(|| OsString::from("logpulse"));
    let tail = raw.get(1..).unwrap_or(&[]);

    if matches!(tail.first().and_then(|value| value.to_str()), Some("tui")) {
        if matches!(tail.get(1).and_then(|value| value.to_str()), Some("clear")) {
            return parse_tui_clear(&program, tail);
        }

        let mut rewritten = vec![program];
        rewritten.extend(tail[1..].iter().cloned());
        let args = Args::try_parse_from(rewritten)?;
        return Ok(CliCommand::Run {
            auto_discover: args.log_file.is_none(),
            args,
        });
    }

    if matches!(
        tail.first().and_then(|value| value.to_str()),
        Some("daemon")
    ) {
        let mut rewritten = vec![program];
        rewritten.extend(tail[1..].iter().cloned());
        let args = Args::try_parse_from(rewritten)?;
        return Ok(CliCommand::Daemon {
            auto_discover: args.log_file.is_none(),
            args,
        });
    }

    let args = Args::try_parse_from(raw)?;
    Ok(CliCommand::Run {
        auto_discover: args.log_file.is_none(),
        args,
    })
}

fn parse_tui_clear(program: &OsString, tail: &[OsString]) -> Result<CliCommand, clap::Error> {
    if tail.len() == 2 {
        return Ok(CliCommand::TuiClear);
    }

    if matches!(
        tail.get(2).and_then(|value| value.to_str()),
        Some("--help" | "-h")
    ) {
        return Err(clap::Error::raw(
            ErrorKind::DisplayHelp,
            format!(
                "Delete the persisted TUI history store.\n\nUsage: {} tui clear\n",
                program.to_string_lossy()
            ),
        ));
    }

    let extra = tail
        .get(2)
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "<unknown>".to_string());
    Err(Args::command().error(
        ErrorKind::UnknownArgument,
        format!("unexpected argument `{extra}` after `logpulse tui clear`"),
    ))
}

fn parse_cli_timestamp(value: &str) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|err| format!("invalid timestamp '{value}': {err}"))
}

#[cfg(test)]
mod tests {
    use super::{parse_command_from, CliCommand};

    #[test]
    fn parses_tui_clear_command() {
        let command = parse_command_from(["logpulse", "tui", "clear"]).expect("parse command");
        assert!(matches!(command, CliCommand::TuiClear));
    }

    #[test]
    fn parses_tui_fresh_alias_as_run_command() {
        let command = parse_command_from(["logpulse", "tui", "--fresh"]).expect("parse command");
        match command {
            CliCommand::Run {
                args,
                auto_discover,
            } => {
                assert!(args.fresh);
                assert!(auto_discover);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_daemon_command() {
        let command = parse_command_from(["logpulse", "daemon"]).expect("parse command");
        match command {
            CliCommand::Daemon {
                args,
                auto_discover,
            } => {
                assert!(args.log_file.is_none());
                assert!(auto_discover);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn keeps_existing_default_invocation_shape() {
        let command = parse_command_from(["logpulse", "/tmp/log.jsonl"]).expect("parse command");
        match command {
            CliCommand::Run {
                args,
                auto_discover,
            } => {
                assert_eq!(
                    args.log_file.as_deref(),
                    Some(std::path::Path::new("/tmp/log.jsonl"))
                );
                assert!(!auto_discover);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }
}
