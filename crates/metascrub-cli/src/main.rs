//! `metascrub` command-line tool.
//!
//! Argument parsing and rendering arrive in v0.1 unit 0.1-11; this is the
//! placeholder that keeps the workspace building until then.

fn main() {
    println!(
        "metascrub {} (core {}) — not usable yet",
        env!("CARGO_PKG_VERSION"),
        metascrub_core::VERSION
    );
}
