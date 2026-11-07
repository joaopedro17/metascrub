//! MetaScrub core library: inspect, clean and verify hidden metadata in files.
//!
//! The core never prints and never talks to the user directly. It returns
//! structured results ([`model`]) that a user interface (the CLI today, a GUI
//! later) turns into text.

mod budget;
pub mod format;
mod fsmeta;
pub mod limits;
mod memory;
pub mod model;
mod naming;
mod options;
mod output;
mod pipeline;
mod plan;

#[doc(hidden)]
pub use fsmeta::describe_attribute;
pub use options::{Options, TrackedChanges};
pub use pipeline::{CancelToken, Command, Observer, OutputMode, Request, run};

/// Version of the core library.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
