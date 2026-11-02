//! `metascrub` command-line tool.

mod args;
mod exit;
mod render;

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{CommandFactory, Parser, error::ErrorKind};
use metascrub_core::model::RunReport;
use metascrub_core::{Command as CoreCommand, Request};

use args::{Cli, Command, OutputArgs};
use render::{Settings, Verbosity};

fn main() -> ExitCode {
    let cli = Cli::parse();
    if cli.output.no_color {
        anstream::ColorChoice::Never.write_global();
    }
    let settings = settings(&cli.output);

    let (name, report) = match cli.command {
        Command::Inspect { paths } => {
            reject_folders(&paths);
            let request = Request {
                command: CoreCommand::Inspect,
                paths,
            };
            ("inspect", metascrub_core::run(&request, &()))
        }
    };

    // A closed pipe (e.g. `| head`) is not an error worth reporting.
    let _ = print(&report, name, &settings, cli.output.json);
    exit::exit_code(&report)
}

fn settings(output: &OutputArgs) -> Settings {
    let verbosity = if output.quiet {
        Verbosity::Quiet
    } else if output.verbose {
        Verbosity::Verbose
    } else {
        Verbosity::Normal
    };
    Settings {
        show_values: output.show_values,
        verbosity,
    }
}

fn print(report: &RunReport, command: &str, settings: &Settings, json: bool) -> io::Result<()> {
    if json {
        let mut out = io::stdout().lock();
        serde_json::to_writer_pretty(&mut out, &render::json::render(report, command, settings))?;
        writeln!(out)
    } else {
        render::human::render(report, settings, &mut anstream::stdout().lock())
    }
}

/// Folder support (`-r`) arrives in v0.2.
fn reject_folders(paths: &[PathBuf]) {
    if let Some(folder) = paths.iter().find(|p| p.is_dir()) {
        Cli::command()
            .error(
                ErrorKind::InvalidValue,
                format!(
                    "'{}' is a folder. Folders aren't supported yet; pass the files inside it instead.",
                    folder.display()
                ),
            )
            .exit();
    }
}
