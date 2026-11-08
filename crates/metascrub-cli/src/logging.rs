//! The diagnostic log (requirements §14.6). Nothing is written unless asked:
//! `--log FILE` writes everything down to the debug level into a new file,
//! and `METASCRUB_LOG=<level>` prints to the terminal. Only MetaScrub's own
//! messages are kept, since a library's could quote a file's content.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Mutex;
use std::time::Instant;

use log::{LevelFilter, Log, Metadata, Record};

use crate::render::messages;

const ENV: &str = "METASCRUB_LOG";
const MAX_NAME_ATTEMPTS: u32 = 1000;

struct Logger {
    start: Instant,
    file: Option<Mutex<File>>,
    terminal: LevelFilter,
}

impl Logger {
    fn to_file(&self, metadata: &Metadata<'_>) -> bool {
        self.file.is_some() && metadata.level() <= LevelFilter::Debug
    }
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.target().starts_with("metascrub")
            && (self.to_file(metadata) || metadata.level() <= self.terminal)
    }

    fn log(&self, record: &Record<'_>) {
        let metadata = record.metadata();
        if !self.enabled(metadata) {
            return;
        }
        let thread = format!("{:?}", std::thread::current().id());
        let thread = thread.trim_start_matches("ThreadId(").trim_end_matches(')');
        let line = format!(
            "{:>8.3}s {:<5} [{thread}] {}\n",
            self.start.elapsed().as_secs_f64(),
            record.level(),
            record.args()
        );
        // Written at once, so the log is complete up to a crash.
        if let Some(file) = &self.file
            && self.to_file(metadata)
            && let Ok(mut file) = file.lock()
        {
            let _ = file.write_all(line.as_bytes());
        }
        if metadata.level() <= self.terminal {
            let _ = io::stderr().write_all(line.as_bytes());
        }
    }

    fn flush(&self) {}
}

/// Sets up the log when asked for, and returns the file it's written to.
/// `names` is whether file names may appear in it.
pub fn init(path: Option<&Path>, names: bool) -> Result<Option<PathBuf>, String> {
    let terminal = match std::env::var(ENV) {
        Ok(level) => {
            LevelFilter::from_str(level.trim()).map_err(|_| messages::LOG_LEVEL.to_owned())?
        }
        Err(_) => LevelFilter::Off,
    };
    let file = match path {
        Some(path) => Some(create(path).map_err(|e| {
            format!(
                "{} '{}': {e}",
                messages::LOG_NOT_CREATED,
                messages::path(path)
            )
        })?),
        None => None,
    };
    if terminal == LevelFilter::Off && file.is_none() {
        return Ok(None);
    }
    let written = match file {
        Some((written, mut file)) => {
            let names = if names {
                messages::LOG_NAMES_SHOWN
            } else {
                messages::LOG_NAMES_HIDDEN
            };
            writeln!(
                file,
                "MetaScrub {} diagnostic log\n{names}\n{}\n",
                env!("CARGO_PKG_VERSION"),
                messages::LOG_NO_VALUES
            )
            .map_err(|e| {
                format!(
                    "{} '{}': {e}",
                    messages::LOG_NOT_CREATED,
                    messages::path(&written)
                )
            })?;
            Some((written, file))
        }
        None => None,
    };
    let max = if written.is_some() {
        terminal.max(LevelFilter::Debug)
    } else {
        terminal
    };
    let (path, file) = written.unzip();
    let logger = Logger {
        start: Instant::now(),
        file: file.map(Mutex::new),
        terminal,
    };
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(max);
        log_panics();
    }
    Ok(path)
}

/// A crash ends up in the log too, with where it happened.
fn log_panics() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("crashed: {info}");
        default(info);
    }));
}

/// A new file at `path`, or `name-2.log`, `name-3.log`, … when it's taken:
/// an existing file is never overwritten, like MetaScrub's outputs.
fn create(path: &Path) -> io::Result<(PathBuf, File)> {
    for n in 1..=MAX_NAME_ATTEMPTS {
        let candidate = numbered(path, n);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free file name for the log",
    ))
}

fn numbered(path: &Path, n: u32) -> PathBuf {
    if n == 1 {
        return path.to_owned();
    }
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let name = match path.extension() {
        Some(extension) => format!("{stem}-{n}.{}", extension.to_string_lossy()),
        None => format!("{stem}-{n}"),
    };
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_taken_name_gets_a_number() {
        assert_eq!(
            numbered(Path::new("logs/run.log"), 1),
            Path::new("logs/run.log")
        );
        assert_eq!(
            numbered(Path::new("logs/run.log"), 2),
            Path::new("logs/run-2.log")
        );
        assert_eq!(numbered(Path::new("run"), 3), Path::new("run-3"));
    }

    #[test]
    fn existing_files_are_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.log");
        std::fs::write(&path, "keep me").unwrap();
        let (written, _) = create(&path).unwrap();
        assert_eq!(written, dir.path().join("run-2.log"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep me");
    }
}
