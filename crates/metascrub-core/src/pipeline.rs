//! Runs a command over a list of files and collects one report per file.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::format::{self, FormatError, SNIFF_LEN};
use crate::model::{
    Disposition, Failure, FailureKind, FileReport, Finding, FindingKind, FormatId, Location,
    Outcome, RunReport, SkipReason, Value,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Command {
    Inspect,
}

#[derive(Debug, Clone)]
pub struct Request {
    pub command: Command,
    pub paths: Vec<PathBuf>,
}

/// Progress callbacks, so a user interface can show progress while a run is going.
pub trait Observer: Sync {
    fn planned(&self, _total: usize) {}
    fn file_started(&self, _path: &Path) {}
    fn file_finished(&self, _report: &FileReport) {}
}

impl Observer for () {}

pub fn run(request: &Request, observer: &dyn Observer) -> RunReport {
    observer.planned(request.paths.len());
    let files = request
        .paths
        .iter()
        .map(|path| {
            observer.file_started(path);
            let report = match request.command {
                Command::Inspect => inspect_file(path),
            };
            observer.file_finished(&report);
            report
        })
        .collect();
    RunReport { files }
}

fn inspect_file(path: &Path) -> FileReport {
    let mut report = FileReport {
        path: path.to_owned(),
        format: None,
        findings: Vec::new(),
        outcome: Outcome::Inspected,
        output: None,
    };
    match inspect_file_inner(path, &mut report) {
        Ok(outcome) => report.outcome = outcome,
        Err(error) => report.outcome = Outcome::Failed(failure(error)),
    }
    report
}

fn inspect_file_inner(path: &Path, report: &mut FileReport) -> Result<Outcome, FormatError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(Outcome::Skipped(SkipReason::SymbolicLink));
    }
    // Checked before opening: opening a FIFO would block forever.
    if !metadata.is_file() {
        return Ok(Outcome::Skipped(SkipReason::NotRegularFile));
    }

    let mut file = File::open(path)?;
    let mut head = Vec::with_capacity(SNIFF_LEN);
    file.by_ref()
        .take(SNIFF_LEN as u64)
        .read_to_end(&mut head)?;
    report.format = format::sniff(&head);
    let Some(format) = report.format else {
        return Ok(Outcome::Skipped(SkipReason::UnsupportedFormat));
    };
    let Some(handler) = format::handler(format) else {
        return Ok(Outcome::Skipped(SkipReason::UnsupportedFormat));
    };

    file.seek(SeekFrom::Start(0))?;
    report.findings = handler.inspect(&mut file)?;
    if let Some(finding) = extension_mismatch(path, format) {
        report.findings.push(finding);
    }
    Ok(Outcome::Inspected)
}

fn extension_mismatch(path: &Path, format: FormatId) -> Option<Finding> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    if format::extensions(format).contains(&extension.as_str()) {
        return None;
    }
    Some(
        Finding::new(
            FindingKind::ExtensionMismatch,
            Location::new(["File name"]),
            Disposition::Warn,
        )
        .with_value(Value::Text(format!(".{extension}"))),
    )
}

fn failure(error: FormatError) -> Failure {
    let kind = match error {
        FormatError::Io(_) => FailureKind::Io,
        FormatError::Malformed(_) => FailureKind::Malformed,
        FormatError::TooLarge { .. } => FailureKind::LimitExceeded,
    };
    Failure {
        kind,
        detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("metascrub-pipeline-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn inspect(paths: Vec<PathBuf>) -> RunReport {
        run(
            &Request {
                command: Command::Inspect,
                paths,
            },
            &(),
        )
    }

    #[test]
    fn reports_every_file_including_problems() {
        let dir = scratch_dir("every-file");
        let text = dir.join("notes.txt");
        fs::write(&text, "hello").unwrap();
        let broken = dir.join("broken.jpg");
        fs::write(&broken, [0xFF, 0xD8, 0xFF, 0xFE, 0x00]).unwrap();
        let missing = dir.join("missing.jpg");

        let report = inspect(vec![text, broken, missing, dir.clone()]);
        let outcomes: Vec<_> = report.files.iter().map(|f| f.outcome.clone()).collect();
        assert_eq!(outcomes[0], Outcome::Skipped(SkipReason::UnsupportedFormat));
        assert!(matches!(&outcomes[1], Outcome::Failed(f) if f.kind == FailureKind::Malformed));
        assert!(matches!(&outcomes[2], Outcome::Failed(f) if f.kind == FailureKind::Io));
        assert_eq!(outcomes[3], Outcome::Skipped(SkipReason::NotRegularFile));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn detected_but_unsupported_formats_keep_their_format() {
        let dir = scratch_dir("webp");
        let webp = dir.join("image.webp");
        fs::write(&webp, b"RIFF\x04\0\0\0WEBP").unwrap();
        let report = inspect(vec![webp]);
        assert_eq!(report.files[0].format, Some(FormatId::Webp));
        assert_eq!(
            report.files[0].outcome,
            Outcome::Skipped(SkipReason::UnsupportedFormat)
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn flags_extensions_that_do_not_match_the_content() {
        let finding = extension_mismatch(Path::new("photo.png"), FormatId::Jpeg).expect("mismatch");
        assert_eq!(finding.kind, FindingKind::ExtensionMismatch);
        assert_eq!(finding.value, Some(Value::Text(".png".into())));
        assert!(extension_mismatch(Path::new("photo.JPG"), FormatId::Jpeg).is_none());
        assert!(extension_mismatch(Path::new("photo"), FormatId::Jpeg).is_none());
    }
}
