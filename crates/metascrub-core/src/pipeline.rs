//! Runs a command over a list of files and collects one report per file.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::format::{self, FormatError, FormatHandler, SNIFF_LEN};
use crate::limits::MAX_IMAGE_BYTES;
use crate::model::{
    Disposition, Failure, FailureKind, FileReport, Finding, FindingKind, FormatId, Location,
    Outcome, RunReport, SkipReason, Value,
};
use crate::options::Options;
use crate::output;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Command {
    Inspect,
    Clean,
    Verify,
}

/// Where `clean` writes its output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputMode {
    /// `photo.jpg` → `photo.clean.jpg` in the same folder.
    NextToOriginal,
    /// Into this folder, keeping the file name. Unsupported files are copied too.
    Directory(PathBuf),
    /// Replace the original (`--in-place`).
    InPlace,
}

#[derive(Debug, Clone)]
pub struct Request {
    pub command: Command,
    pub paths: Vec<PathBuf>,
    pub options: Options,
    pub output: OutputMode,
    pub dry_run: bool,
}

impl Request {
    /// A request with the safe defaults: strict cleaning, output next to the original.
    pub fn new(command: Command, paths: Vec<PathBuf>) -> Self {
        Self {
            command,
            paths,
            options: Options::default(),
            output: OutputMode::NextToOriginal,
            dry_run: false,
        }
    }
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
            let report = process(path, request);
            observer.file_finished(&report);
            report
        })
        .collect();
    RunReport { files }
}

fn process(path: &Path, request: &Request) -> FileReport {
    let mut report = FileReport {
        path: path.to_owned(),
        format: None,
        findings: Vec::new(),
        outcome: Outcome::Inspected,
        output: None,
    };
    report.outcome = match process_file(path, request, &mut report) {
        Ok(outcome) => outcome,
        Err(error) => Outcome::Failed(failure(error)),
    };
    report
}

fn process_file(
    path: &Path,
    request: &Request,
    report: &mut FileReport,
) -> Result<Outcome, FormatError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(Outcome::Skipped(SkipReason::SymbolicLink));
    }
    // Checked before opening: opening a FIFO would block forever.
    if !metadata.is_file() {
        return Ok(Outcome::Skipped(SkipReason::NotRegularFile));
    }

    let mut file = File::open(path)?;
    ensure_same_file(&metadata, &file)?;
    let mut head = Vec::with_capacity(SNIFF_LEN);
    Read::by_ref(&mut file)
        .take(SNIFF_LEN as u64)
        .read_to_end(&mut head)?;
    file.seek(SeekFrom::Start(0))?;
    report.format = format::sniff(&head);
    let settings = output::Settings {
        permissions: metadata.permissions(),
        neutral_times: !request.options.keep_file_times,
    };

    let Some((format, handler)) = report
        .format
        .and_then(|format| Some((format, format::handler(format)?)))
    else {
        return unsupported(path, &mut file, request, &settings, report);
    };

    let options = &request.options;
    let outcome = match request.command {
        Command::Inspect => {
            report.findings = handler.inspect(&mut file, options)?;
            Outcome::Inspected
        }
        Command::Verify => {
            report.findings = handler.inspect(&mut file, options)?;
            let clean = report
                .findings
                .iter()
                .all(|f| f.disposition.is_acceptable_after_cleaning());
            if clean {
                Outcome::Verified
            } else {
                Outcome::RemainingMetadata
            }
        }
        Command::Clean if request.dry_run => {
            report.findings = handler.inspect(&mut file, options)?;
            Outcome::WouldClean
        }
        Command::Clean => {
            let (output, findings) = clean(path, file, handler, request, &settings)?;
            report.output = Some(output);
            report.findings = findings;
            Outcome::Cleaned
        }
    };
    if let Some(finding) = extension_mismatch(path, format) {
        report.findings.push(finding);
    }
    Ok(outcome)
}

fn clean(
    path: &Path,
    mut file: File,
    handler: &dyn FormatHandler,
    request: &Request,
    settings: &output::Settings,
) -> Result<(PathBuf, Vec<Finding>), FormatError> {
    // Closed before writing: Windows can't replace a file that is still open.
    let input = format::read_limited(&mut file, MAX_IMAGE_BYTES)?;
    drop(file);
    let mut findings = Vec::new();
    let write = |out: &mut dyn Write| {
        findings = handler.clean(&mut io::Cursor::new(&input), out, &request.options)?;
        Ok(())
    };
    let written = match &request.output {
        OutputMode::InPlace => {
            output::replace(path, settings, write)?;
            path.to_owned()
        }
        OutputMode::NextToOriginal => output::write_new(&cleaned_name(path), settings, write)?,
        OutputMode::Directory(dir) => {
            output::write_new(&dir.join(file_name(path)), settings, write)?
        }
    };
    Ok((written, findings))
}

/// Unsupported files are only copied when writing into an output folder,
/// so the folder is complete (requirements §13.13).
fn unsupported(
    path: &Path,
    file: &mut File,
    request: &Request,
    settings: &output::Settings,
    report: &mut FileReport,
) -> Result<Outcome, FormatError> {
    let OutputMode::Directory(dir) = &request.output else {
        return Ok(Outcome::Skipped(SkipReason::UnsupportedFormat));
    };
    if request.command != Command::Clean {
        return Ok(Outcome::Skipped(SkipReason::UnsupportedFormat));
    }
    if request.dry_run {
        return Ok(Outcome::WouldCopy);
    }
    // Copying the bytes ourselves, unlike `fs::copy`, never carries over
    // extended attributes or alternate data streams.
    let written = output::write_new(&dir.join(file_name(path)), settings, |out| {
        io::copy(file, out)?;
        Ok(())
    })?;
    report.output = Some(written);
    Ok(Outcome::Copied)
}

/// The path could be swapped (e.g. for a symlink) between the check and the
/// open; make sure the file opened is the one that was checked.
#[cfg(unix)]
fn ensure_same_file(checked: &fs::Metadata, file: &File) -> Result<(), FormatError> {
    use std::os::unix::fs::MetadataExt;
    let opened = file.metadata()?;
    if opened.dev() == checked.dev() && opened.ino() == checked.ino() {
        Ok(())
    } else {
        Err(io::Error::other("the file changed while it was being opened").into())
    }
}

#[cfg(not(unix))]
fn ensure_same_file(_checked: &fs::Metadata, file: &File) -> Result<(), FormatError> {
    if file.metadata()?.is_file() {
        Ok(())
    } else {
        Err(io::Error::other("the file changed while it was being opened").into())
    }
}

/// `photo.jpg` → `photo.clean.jpg`, `notes` → `notes.clean`.
fn cleaned_name(path: &Path) -> PathBuf {
    let mut name: OsString = path.file_stem().unwrap_or_default().to_owned();
    name.push(".clean");
    if let Some(extension) = path.extension() {
        name.push(".");
        name.push(extension);
    }
    path.with_file_name(name)
}

fn file_name(path: &Path) -> &std::ffi::OsStr {
    path.file_name().unwrap_or(std::ffi::OsStr::new("output"))
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
        FormatError::Unsupported(_) => FailureKind::Unsupported,
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

    fn fixture(relative: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/generated")
            .join(relative)
    }

    /// Copies a fixture into `dir` so tests can write next to it.
    fn copy_fixture(dir: &Path, relative: &str) -> PathBuf {
        let target = dir.join(Path::new(relative).file_name().unwrap());
        fs::copy(fixture(relative), &target).unwrap();
        target
    }

    fn run_one(request: Request) -> FileReport {
        run(&request, &()).files.remove(0)
    }

    #[test]
    fn reports_every_file_including_problems() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("notes.txt");
        fs::write(&text, "hello").unwrap();
        let broken = dir.path().join("broken.jpg");
        fs::write(&broken, [0xFF, 0xD8, 0xFF, 0xFE, 0x00]).unwrap();
        let missing = dir.path().join("missing.jpg");

        let paths = vec![text, broken, missing, dir.path().to_owned()];
        let report = run(&Request::new(Command::Inspect, paths), &());
        let outcomes: Vec<_> = report.files.iter().map(|f| f.outcome.clone()).collect();
        assert_eq!(outcomes[0], Outcome::Skipped(SkipReason::UnsupportedFormat));
        assert!(matches!(&outcomes[1], Outcome::Failed(f) if f.kind == FailureKind::Malformed));
        assert!(matches!(&outcomes[2], Outcome::Failed(f) if f.kind == FailureKind::Io));
        assert_eq!(outcomes[3], Outcome::Skipped(SkipReason::NotRegularFile));
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_links_are_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("link.jpg");
        std::os::unix::fs::symlink(fixture("jpeg/camera.jpg"), &link).unwrap();
        let report = run_one(Request::new(Command::Clean, vec![link]));
        assert_eq!(report.outcome, Outcome::Skipped(SkipReason::SymbolicLink));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn detected_but_unsupported_formats_keep_their_format() {
        let report = run_one(Request::new(
            Command::Inspect,
            vec![fixture("other/image.webp")],
        ));
        assert_eq!(report.format, Some(FormatId::Webp));
        assert_eq!(
            report.outcome,
            Outcome::Skipped(SkipReason::UnsupportedFormat)
        );
    }

    #[test]
    fn flags_extensions_that_do_not_match_the_content() {
        let finding = extension_mismatch(Path::new("photo.png"), FormatId::Jpeg).expect("mismatch");
        assert_eq!(finding.kind, FindingKind::ExtensionMismatch);
        assert_eq!(finding.value, Some(Value::Text(".png".into())));
        assert!(extension_mismatch(Path::new("photo.JPG"), FormatId::Jpeg).is_none());
        assert!(extension_mismatch(Path::new("photo"), FormatId::Jpeg).is_none());
    }

    #[test]
    fn names_the_output_next_to_the_original() {
        assert_eq!(
            cleaned_name(Path::new("a/photo.jpg")),
            Path::new("a/photo.clean.jpg")
        );
        assert_eq!(cleaned_name(Path::new("notes")), Path::new("notes.clean"));
    }

    #[test]
    fn cleans_next_to_the_original_without_touching_it() {
        let dir = tempfile::tempdir().unwrap();
        let original = copy_fixture(dir.path(), "jpeg/camera.jpg");
        let before = fs::read(&original).unwrap();

        let report = run_one(Request::new(Command::Clean, vec![original.clone()]));
        assert_eq!(report.outcome, Outcome::Cleaned);
        assert_eq!(report.output, Some(dir.path().join("camera.clean.jpg")));
        assert_eq!(fs::read(&original).unwrap(), before);

        let again = run_one(Request::new(Command::Clean, vec![original]));
        assert_eq!(again.output, Some(dir.path().join("camera.clean-2.jpg")));
    }

    #[test]
    fn cleaned_files_verify_and_originals_do_not() {
        let dir = tempfile::tempdir().unwrap();
        let original = copy_fixture(dir.path(), "jpeg/camera.jpg");
        let cleaned = run_one(Request::new(Command::Clean, vec![original.clone()]))
            .output
            .unwrap();

        let verify = |path: PathBuf| run_one(Request::new(Command::Verify, vec![path])).outcome;
        assert_eq!(verify(cleaned), Outcome::Verified);
        assert_eq!(verify(original), Outcome::RemainingMetadata);
    }

    #[test]
    fn in_place_replaces_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let original = copy_fixture(dir.path(), "png/text-chunks.png");
        let report = run_one(Request {
            output: OutputMode::InPlace,
            ..Request::new(Command::Clean, vec![original.clone()])
        });
        assert_eq!(report.output, Some(original.clone()));
        let verify = run_one(Request::new(Command::Verify, vec![original]));
        assert_eq!(verify.outcome, Outcome::Verified);
        let names: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(names.len(), 1);
    }

    #[test]
    fn dry_run_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let original = copy_fixture(dir.path(), "jpeg/camera.jpg");
        let notes = copy_fixture(dir.path(), "other/notes.txt");
        let out = dir.path().join("out");
        let report = run(
            &Request {
                output: OutputMode::Directory(out.clone()),
                dry_run: true,
                ..Request::new(Command::Clean, vec![original, notes])
            },
            &(),
        );
        assert_eq!(report.files[0].outcome, Outcome::WouldClean);
        assert!(!report.files[0].findings.is_empty());
        assert_eq!(report.files[1].outcome, Outcome::WouldCopy);
        assert!(!out.exists());
    }

    #[test]
    fn output_folder_receives_cleaned_and_copied_files() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let report = run(
            &Request {
                output: OutputMode::Directory(out.clone()),
                ..Request::new(
                    Command::Clean,
                    vec![fixture("jpeg/camera.jpg"), fixture("other/notes.txt")],
                )
            },
            &(),
        );
        assert_eq!(report.files[0].output, Some(out.join("camera.jpg")));
        assert_eq!(report.files[1].outcome, Outcome::Copied);
        assert_eq!(
            fs::read(out.join("notes.txt")).unwrap(),
            fs::read(fixture("other/notes.txt")).unwrap()
        );
    }

    #[test]
    fn unsupported_files_are_not_copied_next_to_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let notes = copy_fixture(dir.path(), "other/notes.txt");
        let report = run_one(Request::new(Command::Clean, vec![notes]));
        assert_eq!(
            report.outcome,
            Outcome::Skipped(SkipReason::UnsupportedFormat)
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn damaged_files_produce_no_output() {
        let dir = tempfile::tempdir().unwrap();
        let broken = copy_fixture(dir.path(), "jpeg/truncated.jpg");
        let report = run_one(Request::new(Command::Clean, vec![broken]));
        assert!(matches!(report.outcome, Outcome::Failed(_)));
        assert_eq!(report.output, None);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
