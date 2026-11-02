//! MetaScrub core library: inspect, clean and verify hidden metadata in files.
//!
//! The core never prints and never talks to the user directly. It returns
//! structured results ([`model`]) that a user interface (the CLI today, a GUI
//! later) turns into text.

pub mod format;
pub mod limits;
pub mod model;
mod options;
mod output;
mod pipeline;
mod plan;

pub use options::Options;
pub use pipeline::{CancelToken, Command, Observer, OutputMode, Request, run};

/// Version of the core library.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
