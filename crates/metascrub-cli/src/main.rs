//! `metascrub` command-line tool.

mod args;
mod exit;
mod progress;
mod render;

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{CommandFactory, Parser, error::ErrorKind};
use metascrub_core::model::RunReport;
use metascrub_core::{
    CancelToken, Command as CoreCommand, Options, OutputMode, Request, TrackedChanges,
};

use args::{Cli, Command, KeepArgs, KeepItem, OutputArgs};
use progress::Progress;
use render::{Mode, Settings, Verbosity};

fn main() -> ExitCode {
    let cli = Cli::parse();
    if cli.output.no_color {
        anstream::ColorChoice::Never.write_global();
    }

    let (mode, request) = match cli.command {
        Command::Inspect {
            paths,
            recursive,
            keep,
        } => (
            Mode::Inspect,
            Request {
                options: options(&keep, TrackedChanges::Undecided),
                recursive,
                ..Request::new(CoreCommand::Inspect, paths)
            },
        ),
        Command::Clean {
            paths,
            recursive,
            output,
            in_place,
            dry_run,
            accept_changes,
            reject_changes,
            keep,
        } => {
            let tracked_changes = match (accept_changes, reject_changes) {
                (true, _) => TrackedChanges::Accept,
                (_, true) => TrackedChanges::Reject,
                _ => TrackedChanges::Undecided,
            };
            let output = match (output, in_place) {
                (Some(dir), _) => OutputMode::Directory(dir),
                (None, true) => OutputMode::InPlace,
                (None, false) => OutputMode::NextToOriginal,
            };
            (
                Mode::Clean { dry_run },
                Request {
                    options: options(&keep, tracked_changes),
                    output,
                    dry_run,
                    recursive,
                    ..Request::new(CoreCommand::Clean, paths)
                },
            )
        }
        Command::Verify {
            paths,
            recursive,
            keep,
        } => (
            Mode::Verify,
            Request {
                options: options(&keep, TrackedChanges::Undecided),
                recursive,
                ..Request::new(CoreCommand::Verify, paths)
            },
        ),
    };
    if !request.recursive {
        reject_folders(&request.paths);
    }

    let progress = Progress::new(!cli.output.quiet);
    let cancel = CancelToken::new();
    stop_on_ctrl_c(&cancel, &progress);
    let report = metascrub_core::run(&request, &progress, &cancel);
    progress.finish();
    let settings = settings(mode, &cli.output);
    // A closed pipe (e.g. `| head`) is not an error worth reporting.
    let _ = print(&report, &settings, cli.output.json);
    exit::exit_code(&report)
}

fn options(keep: &KeepArgs, tracked_changes: TrackedChanges) -> Options {
    Options {
        keep_color_profile: keep.keep.contains(&KeepItem::ColorProfile),
        keep_file_times: keep.keep.contains(&KeepItem::FileTimes),
        strip_orientation: keep.strip_orientation,
        keep_comments: keep.keep.contains(&KeepItem::Comments),
        tracked_changes,
    }
}

fn settings(mode: Mode, output: &OutputArgs) -> Settings {
    let verbosity = if output.quiet {
        Verbosity::Quiet
    } else if output.verbose {
        Verbosity::Verbose
    } else {
        Verbosity::Normal
    };
    Settings {
        mode,
        show_values: output.show_values,
        verbosity,
    }
}

fn print(report: &RunReport, settings: &Settings, json: bool) -> io::Result<()> {
    if json {
        let mut out = io::stdout().lock();
        serde_json::to_writer_pretty(&mut out, &render::json::render(report, settings))?;
        writeln!(out)
    } else {
        render::human::render(report, settings, &mut anstream::stdout().lock())
    }
}

fn reject_folders(paths: &[PathBuf]) {
    if let Some(folder) = paths.iter().find(|p| p.is_dir()) {
        Cli::command()
            .error(
                ErrorKind::InvalidValue,
                format!(
                    "'{}' is a folder. Use -r to process folders and everything inside them.",
                    render::messages::path(folder)
                ),
            )
            .exit();
    }
}

/// The first Ctrl+C lets the files in progress finish, so no output is left
/// half-written. A second one stops at once and may leave a temporary
/// `.metascrub-*.tmp` file behind.
fn stop_on_ctrl_c(cancel: &CancelToken, progress: &Progress) {
    let cancel = cancel.clone();
    let progress = progress.clone();
    // Without a handler, Ctrl+C still stops the program the usual way.
    let _ = ctrlc::set_handler(move || {
        if cancel.is_cancelled() {
            std::process::exit(exit::INTERRUPTED.into());
        }
        cancel.cancel();
        progress.notice("Stopping after the files in progress. Press Ctrl+C again to stop now.");
    });
}
