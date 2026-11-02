//! Progress bar on standard error while a run with several files is going.

use std::io::{self, IsTerminal};
use std::path::Path;

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use metascrub_core::Observer;
use metascrub_core::model::FileReport;

use crate::render::messages;

/// Clones share the same bar.
#[derive(Debug, Clone)]
pub struct Progress {
    bar: ProgressBar,
}

impl Progress {
    pub fn new(enabled: bool) -> Self {
        let bar = if enabled && io::stderr().is_terminal() {
            let bar = ProgressBar::with_draw_target(None, ProgressDrawTarget::stderr());
            if let Ok(style) = ProgressStyle::with_template("{bar:30} {pos}/{len} {wide_msg}") {
                bar.set_style(style);
            }
            bar
        } else {
            ProgressBar::hidden()
        };
        Self { bar }
    }

    /// Prints a line above the bar without breaking it.
    pub fn notice(&self, message: &str) {
        self.bar.suspend(|| eprintln!("{message}"));
    }

    pub fn finish(&self) {
        self.bar.finish_and_clear();
    }
}

impl Observer for Progress {
    fn planned(&self, total: usize) {
        // A single file is done before a bar would be worth reading.
        if total < 2 {
            self.bar.set_draw_target(ProgressDrawTarget::hidden());
        }
        self.bar.set_length(total as u64);
    }

    fn file_started(&self, path: &Path) {
        self.bar.set_message(messages::path(path));
    }

    fn file_finished(&self, _report: &FileReport) {
        self.bar.inc(1);
    }
}
