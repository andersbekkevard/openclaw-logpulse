mod cli;
mod event;
mod normalizer;
mod output;
mod parser;
mod stale;
mod tailer;

use chrono::Utc;
use clap::Parser;
use std::io::{self, BufWriter, Write};
use std::time::Instant;

use crate::cli::Args;
use crate::normalizer::normalize;
use crate::stale::StaleTracker;

fn main() {
    let args = Args::parse();
    let mut tailer = match tailer::Tailer::new(
        args.log_file.clone(),
        !args.no_follow,
        args.from_start,
        args.poll_duration(),
    ) {
        Ok(state) => state,
        Err(err) => {
            eprintln!(
                "failed to open log file {}: {}",
                args.log_file.display(),
                err
            );
            return;
        }
    };

    let mut tracker = StaleTracker::new(args.stale_seconds);
    let mut stdout = BufWriter::new(io::stdout());
    let mut stderr = BufWriter::new(io::stderr());
    let heartbeat_interval = args.heartbeat_duration();
    let mut last_heartbeat = Instant::now();

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
                let event = normalize(&raw_line);
                let now = Utc::now();
                let notices = tracker.on_event(&event, now);

                if event.should_filter(
                    args.session.as_ref(),
                    args.tool.as_ref(),
                    args.min_severity(),
                ) {
                    if let Err(err) = output::emit_tool_event(&event, args.format, &mut stdout) {
                        let _ = writeln!(stderr, "{}", err);
                    }
                }

                for warning in notices {
                    if let Err(err) = output::emit_stale_warning(&warning, args.format, &mut stdout)
                    {
                        let _ = writeln!(stderr, "{}", err);
                    }
                }

                if let Err(err) = stdout.flush() {
                    let _ = writeln!(stderr, "{}", err);
                }
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
