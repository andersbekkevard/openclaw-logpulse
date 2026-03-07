mod cli;
mod discovery;
mod event;
mod normalizer;
mod output;
mod parser;
mod stale;
mod tailer;
mod tui;

use chrono::Utc;
use clap::Parser;
use std::env;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::time::Instant;

use crate::cli::Args;
use crate::event::NormalizedEvent;
use crate::normalizer::normalize;
use crate::output::OutputMode;
use crate::stale::StaleTracker;

const MISSING_TTL_SECONDS: u64 = 30;

fn main() {
    let (args, auto_discover) = parse_args();
    let mode = args.format.effective();

    if matches!(mode, OutputMode::Tui) {
        if let Err(err) = tui::run(&args) {
            eprintln!("failed to start TUI: {err}");
        }
        return;
    }

    let mut tracker = StaleTracker::new(args.stale_seconds);
    let mut stdout = BufWriter::new(io::stdout());
    let mut stderr = BufWriter::new(io::stderr());
    let heartbeat_interval = args.heartbeat_duration();
    let mut last_heartbeat = Instant::now();

    if auto_discover {
        let mut discovered_paths = discover_initial_session_logs();
        let mut tailer = tailer::MultiTailer::new(
            discovered_paths,
            !args.no_follow,
            args.from_start,
            args.poll_duration(),
            std::time::Duration::from_secs(MISSING_TTL_SECONDS),
        );
        let mut last_scan = Instant::now();

        loop {
            let now = Instant::now();

            if now.duration_since(last_heartbeat) >= heartbeat_interval {
                let summary = tracker.heartbeat(Utc::now());
                if let Err(err) = output::emit_heartbeat(&summary, mode, &mut stdout) {
                    let _ = writeln!(stderr, "{}", err);
                }
                if let Err(err) = stdout.flush() {
                    let _ = writeln!(stderr, "{}", err);
                }
                last_heartbeat = now;
            }

            if !args.no_follow && now.duration_since(last_scan) >= tailer.poll_interval() {
                discovered_paths = discover_initial_session_logs();
                tailer.sync(discovered_paths);
                last_scan = now;
            }

            match tailer.next_line() {
                Ok(Some((_path, raw_line))) => {
                    process_raw_line(
                        &raw_line,
                        &args,
                        mode,
                        &mut tracker,
                        &mut stdout,
                        &mut stderr,
                    );
                }
                Ok(None) => {
                    if args.no_follow && tailer.is_done() {
                        break;
                    }
                    std::thread::sleep(tailer.poll_interval());
                }
                Err(err) => {
                    let _ = writeln!(stderr, "{}", err);
                    std::thread::sleep(tailer.poll_interval());
                }
            }
        }

        return;
    }

    let mut tailer = match tailer::Tailer::new(
        args.log_file.clone().expect("missing log file argument"),
        !args.no_follow,
        args.from_start,
        args.poll_duration(),
    ) {
        Ok(state) => state,
        Err(err) => {
            eprintln!(
                "failed to open log file {}: {}",
                args.log_file
                    .as_ref()
                    .expect("missing log file argument")
                    .display(),
                err
            );
            return;
        }
    };

    loop {
        let now = Instant::now();
        if now.duration_since(last_heartbeat) >= heartbeat_interval {
            let summary = tracker.heartbeat(Utc::now());
            if let Err(err) = output::emit_heartbeat(&summary, mode, &mut stdout) {
                let _ = writeln!(stderr, "{}", err);
            }
            if let Err(err) = stdout.flush() {
                let _ = writeln!(stderr, "{}", err);
            }
            last_heartbeat = now;
        }

        match tailer.next_line() {
            Ok(Some(raw_line)) => {
                process_raw_line(
                    &raw_line,
                    &args,
                    mode,
                    &mut tracker,
                    &mut stdout,
                    &mut stderr,
                );
            }
            Ok(None) => {
                if args.no_follow {
                    break;
                }
                std::thread::sleep(tailer.poll_interval());
            }
            Err(err) => {
                let _ = writeln!(stderr, "{}", err);
                std::thread::sleep(tailer.poll_interval());
            }
        }
    }
}

fn process_raw_line(
    raw_line: &str,
    args: &Args,
    mode: OutputMode,
    tracker: &mut StaleTracker,
    stdout: &mut BufWriter<impl Write>,
    stderr: &mut BufWriter<impl Write>,
) {
    let event = normalize(raw_line);
    let now = Utc::now();
    let notices = tracker.on_event(&event, now);

    if event_matches_filters(&event, args) {
        if let Err(err) = output::emit_tool_event(&event, mode, stdout) {
            let _ = writeln!(stderr, "{}", err);
        }
    }

    for warning in notices {
        if stale_warning_matches_filters(&warning, args) {
            if let Err(err) = output::emit_stale_warning(&warning, mode, stdout) {
                let _ = writeln!(stderr, "{}", err);
            }
        }
    }

    if let Err(err) = stdout.flush() {
        let _ = writeln!(stderr, "{}", err);
    }
}

pub(crate) fn event_matches_filters(event: &NormalizedEvent, args: &Args) -> bool {
    if !event.should_filter(
        args.session.as_ref(),
        args.tool.as_ref(),
        args.min_severity(),
    ) {
        return false;
    }

    if let Some(agent_filter) = args.agent.as_ref() {
        let needle = agent_filter.to_ascii_lowercase();
        let agent_matches = event
            .agent_id
            .as_ref()
            .map(|value| value.to_ascii_lowercase().contains(&needle))
            .unwrap_or(false);
        if !agent_matches {
            return false;
        }
    }

    true
}

pub(crate) fn stale_warning_matches_filters(
    warning: &crate::stale::StaleWarning,
    args: &Args,
) -> bool {
    if let Some(session_filter) = args.session.as_ref() {
        let needle = session_filter.to_ascii_lowercase();
        let session_matches = warning
            .session_key
            .as_ref()
            .map(|value| value.to_ascii_lowercase().contains(&needle))
            .unwrap_or(false);
        if !session_matches {
            return false;
        }
    }

    if let Some(tool_filter) = args.tool.as_ref() {
        let needle = tool_filter.to_ascii_lowercase();
        let tool_matches = warning
            .tool_name
            .as_ref()
            .map(|value| value.to_ascii_lowercase().contains(&needle))
            .unwrap_or(false);
        if !tool_matches {
            return false;
        }
    }

    if args.agent.is_some() {
        return false;
    }

    true
}

pub(crate) fn discover_initial_session_logs() -> Vec<PathBuf> {
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

fn parse_args() -> (Args, bool) {
    let args = Args::parse_from(env::args_os());
    let use_discovery = args.log_file.is_none();

    (args, use_discovery)
}
