use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "metascrub",
    version,
    about = "Remove hidden metadata from your files.",
    after_help = "MetaScrub removes hidden metadata. It does not remove what is visible in the \
                  file itself (faces, places, text), and it can't guarantee anonymity.",
    after_long_help = "MetaScrub removes hidden metadata. It does not remove what is visible in \
                       the file itself (faces, places, text), and it can't guarantee anonymity.\n\n\
                       Exit codes:\n  \
                       0    success (skipped and copied files aren't failures)\n  \
                       1    some files failed\n  \
                       2    invalid usage\n  \
                       3    all files failed\n  \
                       4    verify found metadata that cleaning would remove\n  \
                       130  stopped with Ctrl+C"
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
        #[arg(required = true, value_name = "PATH")]
        paths: Vec<PathBuf>,

        /// Process folders and everything inside them.
        #[arg(short, long)]
        recursive: bool,

        #[command(flatten)]
        keep: KeepArgs,
    },
    /// Remove hidden metadata.
    ///
    /// Writes `name.clean.ext` next to each file unless --output or
    /// --in-place is given; originals are never changed by default.
    Clean {
        #[arg(required = true, value_name = "PATH")]
        paths: Vec<PathBuf>,

        /// Process folders and everything inside them.
        #[arg(short, long)]
        recursive: bool,

        /// Write cleaned files into this folder instead (created if missing),
        /// mirroring the folders given. Files MetaScrub can't read are copied
        /// there too, so the folder is complete.
        #[arg(short, long, value_name = "DIR", conflicts_with = "in_place")]
        output: Option<PathBuf>,

        /// Overwrite the original files.
        #[arg(long)]
        in_place: bool,

        /// Show what would be removed without writing anything.
        #[arg(long)]
        dry_run: bool,

        /// Accept all tracked changes in Word documents (keep the text as it
        /// reads now), so they can be cleaned.
        #[arg(long, conflicts_with = "reject_changes")]
        accept_changes: bool,

        /// Reject all tracked changes in Word documents (go back to the text
        /// before the changes), so they can be cleaned.
        #[arg(long)]
        reject_changes: bool,

        /// Delete junk files the operating system leaves in folders
        /// (.DS_Store, Thumbs.db, desktop.ini, ._ files, __MACOSX) with
        /// --in-place, or leave them out of the --output folder.
        #[arg(long)]
        remove_junk: bool,

        /// Give files whose names show a date, phone or app (IMG_20261102_143512.jpg,
        /// WhatsApp Image …) a neutral name such as image-001.jpg.
        #[arg(long)]
        rename: bool,

        #[command(flatten)]
        keep: KeepArgs,
    },
    /// Check that files contain no hidden metadata that cleaning would remove.
    Verify {
        #[arg(required = true, value_name = "PATH")]
        paths: Vec<PathBuf>,

        /// Process folders and everything inside them.
        #[arg(short, long)]
        recursive: bool,

        #[command(flatten)]
        keep: KeepArgs,
    },
    /// Print a completion script for your shell.
    ///
    /// With it installed, Tab completes MetaScrub's commands and options.
    /// For example, in Bash:
    /// `metascrub completions bash > ~/.local/share/bash-completion/completions/metascrub`
    Completions {
        #[arg(value_enum)]
        shell: Shell,
    },
    /// Print the manual page, or a command's.
    ///
    /// For example: `metascrub man clean > metascrub-clean.1`
    Man {
        #[arg(value_enum, value_name = "COMMAND")]
        page: Option<Page>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Powershell,
}

/// The commands that have their own manual page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Page {
    Inspect,
    Clean,
    Verify,
    Completions,
    Man,
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
    /// Comments in Office and PDF documents, with every author shown as "Author".
    Comments,
    /// Files attached to PDF documents (cleaned too when MetaScrub can read them).
    Attachments,
    /// The author, publisher and ISBN of e-books.
    BookInfo,
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

    /// Show the SHA-256 checksum of each file read and each file written,
    /// to prove exactly which file was shared.
    #[arg(long, global = true)]
    pub checksums: bool,

    /// Write a diagnostic log to this file, to attach to a problem report.
    /// It never contains metadata values, and names files by number unless
    /// --show-values is given. METASCRUB_LOG=debug prints it to the terminal.
    #[arg(long, global = true, value_name = "FILE")]
    pub log: Option<PathBuf>,
}
