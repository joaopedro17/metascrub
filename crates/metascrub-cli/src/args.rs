use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

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

        #[command(flatten)]
        keep: KeepArgs,
    },
    /// Remove hidden metadata. Writes `name.clean.ext` next to each file
    /// unless --output or --in-place is given; originals are never changed by default.
    Clean {
        #[arg(required = true, value_name = "FILE")]
        paths: Vec<PathBuf>,

        /// Write cleaned files into this folder instead (created if missing).
        /// Files MetaScrub can't read are copied there too, so the folder is complete.
        #[arg(short, long, value_name = "DIR", conflicts_with = "in_place")]
        output: Option<PathBuf>,

        /// Overwrite the original files.
        #[arg(long)]
        in_place: bool,

        /// Show what would be removed without writing anything.
        #[arg(long)]
        dry_run: bool,

        #[command(flatten)]
        keep: KeepArgs,
    },
    /// Check that files contain no hidden metadata that cleaning would remove.
    Verify {
        #[arg(required = true, value_name = "FILE")]
        paths: Vec<PathBuf>,

        #[command(flatten)]
        keep: KeepArgs,
    },
}

#[derive(Debug, Args)]
pub struct KeepArgs {
    /// Keep something cleaning would otherwise remove. Can be repeated.
    #[arg(long, value_enum, value_name = "ITEM")]
    pub keep: Vec<KeepItem>,

    /// Remove the rotation setting too (the image may then display sideways).
    #[arg(long)]
    pub strip_orientation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum KeepItem {
    /// Any color profile, not only standard ones (sRGB, Display P3, Adobe RGB).
    ColorProfile,
    /// The current date on output files, instead of 1980-01-01.
    FileTimes,
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
