//! `metascrub` command-line tool.

mod args;
mod exit;
mod render;

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{CommandFactory, Parser, error::ErrorKind};
use metascrub_core::model::RunReport;
use metascrub_core::{CancelToken, Command as CoreCommand, Options, OutputMode, Request};

use args::{Cli, Command, KeepArgs, KeepItem, OutputArgs};
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
                options: options(&keep),
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
            keep,
        } => {
            let output = match (output, in_place) {
                (Some(dir), _) => OutputMode::Directory(dir),
                (None, true) => OutputMode::InPlace,
                (None, false) => OutputMode::NextToOriginal,
            };
            (
                Mode::Clean { dry_run },
                Request {
                    options: options(&keep),
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
                options: options(&keep),
                recursive,
                ..Request::new(CoreCommand::Verify, paths)
            },
        ),
    };
    if !request.recursive {
        reject_folders(&request.paths);
    }

    let report = metascrub_core::run(&request, &(), &CancelToken::new());
    let settings = settings(mode, &cli.output);
    // A closed pipe (e.g. `| head`) is not an error worth reporting.
    let _ = print(&report, &settings, cli.output.json);
    exit::exit_code(&report)
}

fn options(keep: &KeepArgs) -> Options {
    Options {
        keep_color_profile: keep.keep.contains(&KeepItem::ColorProfile),
        keep_file_times: keep.keep.contains(&KeepItem::FileTimes),
        strip_orientation: keep.strip_orientation,
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
