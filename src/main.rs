mod cli;
mod discovery;
mod event;
mod normalizer;
mod output;
mod parser;
mod projection;
mod session_identity;
mod stale;
mod tailer;
mod tui;

use chrono::Utc;
use clap::Parser;
use std::env;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::path::PathBuf;
use std::time::Instant;

use crate::cli::Args;
use crate::normalizer::normalize_many_with_source;
use crate::output::{effective_mode, OutputMode};
use crate::stale::StaleTracker;

const MISSING_TTL_SECONDS: u64 = 30;

fn main() {
    let (mut args, auto_discover) = parse_args();
    args.format = effective_mode(args.format);
    let time_filter = match args.time_filter() {
        Ok(filter) => filter,
        Err(err) => {
            eprintln!("{err}");
            return;
        }
    };

    if args.format == OutputMode::Tui {
        if let Err(err) = tui::run(&args) {
            eprintln!("failed to start TUI: {}", err);
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
                if let Err(err) = output::emit_heartbeat(&summary, args.format, &mut stdout) {
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
                Ok(Some((path, raw_line))) => {
                    process_raw_line(
                        &raw_line,
                        Some(path.as_path()),
                        &args,
                        &time_filter,
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
            if let Err(err) = output::emit_heartbeat(&summary, args.format, &mut stdout) {
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
                    args.log_file.as_deref(),
                    &args,
                    &time_filter,
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
    source_path: Option<&Path>,
    args: &Args,
    time_filter: &crate::event::TimeFilter,
    tracker: &mut StaleTracker,
    stdout: &mut BufWriter<impl Write>,
    stderr: &mut BufWriter<impl Write>,
) {
    let now = Utc::now();
    for event in normalize_many_with_source(raw_line, source_path) {
        let notices = tracker.on_event(&event, now);

        if event.should_filter(
            args.session.as_ref(),
            args.agent.as_ref(),
            args.tool.as_ref(),
            args.min_severity(),
            Some(time_filter),
        ) {
            if let Err(err) = output::emit_tool_event(&event, args.format, stdout) {
                let _ = writeln!(stderr, "{}", err);
            }
        }

        for warning in notices {
            if !time_filter.contains(Some(now)) {
                continue;
            }
            if let Err(err) = output::emit_stale_warning(&warning, args.format, stdout) {
                let _ = writeln!(stderr, "{}", err);
            }
        }
    }

    if let Err(err) = stdout.flush() {
        let _ = writeln!(stderr, "{}", err);
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

fn parse_args() -> (Args, bool) {
    let args = Args::parse_from(env::args_os());
    let use_discovery = args.log_file.is_none();

    (args, use_discovery)
}
