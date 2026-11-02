use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "metascrub",
    version,
    about = "Remove hidden metadata from your files.",
    after_help = "MetaScrub removes hidden metadata. It does not remove what is visible in the \
                  file itself (faces, places, text), and it can't guarantee anonymity."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List the hidden metadata in files, without changing anything.
    Inspect {
        #[arg(required = true, value_name = "FILE")]
        paths: Vec<PathBuf>,
    },
}

#[derive(Debug, Args)]
pub struct OutputArgs {
    /// Print machine-readable JSON instead of text.
    #[arg(long, global = true)]
    pub json: bool,

    /// Show the actual metadata values (hidden by default, so reports can be shared safely).
    #[arg(long, global = true)]
    pub show_values: bool,

    /// Disable colored output (the NO_COLOR environment variable also works).
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Show where in the file each finding is, and technical error details.
    #[arg(short, long, global = true, conflicts_with = "quiet")]
    pub verbose: bool,

    /// Only print the summary and problems.
    #[arg(short, long, global = true)]
    pub quiet: bool,
}
