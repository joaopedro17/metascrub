//! MetaScrub core library: inspect, clean and verify hidden metadata in files.
//!
//! The core never prints and never talks to the user directly. It returns
//! structured results ([`model`]) that a user interface (the CLI today, a GUI
//! later) turns into text.


/// Version of the core library.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
