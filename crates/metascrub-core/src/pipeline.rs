//! Runs a command over a list of files and collects one report per file.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;

use crate::format::{self, FormatError, FormatHandler, SNIFF_LEN};
use crate::limits::{MAX_IMAGE_BYTES, MAX_PARALLEL_FILES};
use crate::model::{
    Decision, Disposition, Failure, FailureKind, FileReport, Finding, FindingKind, FormatId,
    Location, Outcome, RunReport, SkipReason, Value,
};
use crate::options::Options;
use crate::output;
use crate::plan::{self, Job};

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
    /// Into this folder, keeping the file name and mirroring folders.
    /// Unsupported files are copied too.
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
    /// Process folders and everything inside them.
    pub recursive: bool,
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
            recursive: false,
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

/// Stops a run between files: files already started finish, the rest are
/// reported as cancelled. Clones share the same flag.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Reports come back in the order the files were found, whatever order they
/// were processed in.
pub fn run(request: &Request, observer: &dyn Observer, cancel: &CancelToken) -> RunReport {
    let jobs = plan::plan(request);
    observer.planned(jobs.len());
    let run_job = |job: &Job| {
        if cancel.is_cancelled() {
            return report(job, Outcome::Failed(cancelled()));
        }
        observer.file_started(&job.path);
        let report = process(job, request);
        observer.file_finished(&report);
        report
    };
    let threads = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(MAX_PARALLEL_FILES)
        .min(jobs.len());
    let pool = (threads > 1)
        .then(|| {
            rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .ok()
        })
        .flatten();
    let files = match pool {
        Some(pool) => pool.install(|| jobs.par_iter().map(run_job).collect()),
        None => jobs.iter().map(run_job).collect(),
    };
    RunReport { files }
}

fn report(job: &Job, outcome: Outcome) -> FileReport {
    FileReport {
        path: job.path.clone(),
        format: None,
        findings: Vec::new(),
        outcome,
        output: None,
    }
}

fn process(job: &Job, request: &Request) -> FileReport {
    if let Some(outcome) = &job.outcome {
        return report(job, outcome.clone());
    }
    let mut report = report(job, Outcome::Inspected);
    report.outcome = match process_file(job, request, &mut report) {
        Ok(outcome) => outcome,
        Err(error) => Outcome::Failed(failure(error)),
    };
    report
}

fn process_file(
    job: &Job,
    request: &Request,
    report: &mut FileReport,
) -> Result<Outcome, FormatError> {
    let path = job.path.as_path();
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
    report.format = format::detect(&head, &mut file)?;
    let settings = output::Settings {
        permissions: metadata.permissions(),
        neutral_times: !request.options.keep_file_times,
    };

    let Some((format, handler)) = report
        .format
        .and_then(|format| Some((format, format::handler(format)?)))
    else {
        return unsupported(job, &mut file, request, &settings, report);
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
            match decision_needed(&report.findings) {
                Some(decision) => Outcome::NeedsDecision(decision),
                None => Outcome::WouldClean,
            }
        }
        Command::Clean => {
            let (output, findings) = clean(job, file, handler, request, &settings)?;
            report.output = output;
            report.findings = findings;
            match decision_needed(&report.findings) {
                Some(decision) => Outcome::NeedsDecision(decision),
                None => Outcome::Cleaned,
            }
        }
    };
    if let Some(finding) = extension_mismatch(path, format) {
        report.findings.push(finding);
    }
    Ok(outcome)
}

/// Writes nothing when the findings say the user must decide first.
fn clean(
    job: &Job,
    mut file: File,
    handler: &dyn FormatHandler,
    request: &Request,
    settings: &output::Settings,
) -> Result<(Option<PathBuf>, Vec<Finding>), FormatError> {
    // Closed before writing: Windows can't replace a file that is still open.
    let input = format::read_limited(&mut file, MAX_IMAGE_BYTES)?;
    drop(file);
    let mut cleaned = Vec::new();
    let findings = handler.clean(&mut io::Cursor::new(&input), &mut cleaned, &request.options)?;
    if decision_needed(&findings).is_some() {
        return Ok((None, findings));
    }
    let write = |out: &mut dyn Write| {
        out.write_all(&cleaned)?;
        Ok(())
    };
    let written = match &job.target {
        Some(target) => output::write_new(target, settings, write)?,
        None => {
            output::replace(&job.path, settings, write)?;
            job.path.clone()
        }
    };
    Ok((Some(written), findings))
}

fn decision_needed(findings: &[Finding]) -> Option<Decision> {
    findings.iter().find_map(|f| match f.disposition {
        Disposition::Blocks(decision) => Some(decision),
        _ => None,
    })
}

/// Unsupported files are only copied when writing into an output folder,
/// so the folder is complete (requirements §13.13).
fn unsupported(
    job: &Job,
    file: &mut File,
    request: &Request,
    settings: &output::Settings,
    report: &mut FileReport,
) -> Result<Outcome, FormatError> {
    let (OutputMode::Directory(_), Some(target)) = (&request.output, &job.target) else {
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
    let written = output::write_new(target, settings, |out| {
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

fn cancelled() -> Failure {
    Failure {
        kind: FailureKind::Cancelled,
        detail: "the run was stopped before this file was processed".into(),
    }
}

fn failure(error: FormatError) -> Failure {
    let kind = match error {
        FormatError::Io(_) => FailureKind::Io,
        FormatError::Malformed(_) => FailureKind::Malformed,
        FormatError::Unsupported(_) => FailureKind::Unsupported,
        FormatError::TooLarge { .. } | FormatError::TooDeep { .. } => FailureKind::LimitExceeded,
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
        run(&request, &(), &CancelToken::new()).files.remove(0)
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
        let report = run(
            &Request::new(Command::Inspect, paths),
            &(),
            &CancelToken::new(),
        );
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
    fn flags_extensions_that_do_not_match_the_content() {
        let finding = extension_mismatch(Path::new("photo.png"), FormatId::Jpeg).expect("mismatch");
        assert_eq!(finding.kind, FindingKind::ExtensionMismatch);
        assert_eq!(finding.value, Some(Value::Text(".png".into())));
        assert!(extension_mismatch(Path::new("photo.JPG"), FormatId::Jpeg).is_none());
        assert!(extension_mismatch(Path::new("photo"), FormatId::Jpeg).is_none());
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
    fn tracked_changes_are_not_cleaned_until_the_user_decides() {
        let dir = tempfile::tempdir().unwrap();
        let original = copy_fixture(dir.path(), "office/tracked-changes.docx");
        let needs_decision = Outcome::NeedsDecision(Decision::TrackedChanges);
        let dry_run = Request {
            dry_run: true,
            ..Request::new(Command::Clean, vec![original.clone()])
        };
        assert_eq!(run_one(dry_run).outcome, needs_decision);
        let report = run_one(Request::new(Command::Clean, vec![original.clone()]));
        assert_eq!(report.outcome, needs_decision);
        assert_eq!(report.output, None);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);

        let accept = Request {
            options: Options {
                tracked_changes: crate::TrackedChanges::Accept,
                ..Options::default()
            },
            ..Request::new(Command::Clean, vec![original])
        };
        let report = run_one(accept);
        assert_eq!(report.outcome, Outcome::Cleaned);
        let verify = run_one(Request::new(Command::Verify, vec![report.output.unwrap()]));
        assert_eq!(verify.outcome, Outcome::Verified);
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
            &CancelToken::new(),
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
            &CancelToken::new(),
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
    fn cleans_a_folder_into_a_mirrored_output() {
        let dir = tempfile::tempdir().unwrap();
        let photos = dir.path().join("photos");
        fs::create_dir_all(photos.join("2024")).unwrap();
        copy_fixture(&photos.join("2024"), "jpeg/camera.jpg");
        copy_fixture(&photos, "png/text-chunks.png");
        copy_fixture(&photos, "other/notes.txt");
        let out = dir.path().join("out");

        let report = run(
            &Request {
                output: OutputMode::Directory(out.clone()),
                recursive: true,
                ..Request::new(Command::Clean, vec![photos.clone()])
            },
            &(),
            &CancelToken::new(),
        );
        let outputs: Vec<_> = report.files.iter().map(|f| f.output.clone()).collect();
        assert_eq!(
            outputs,
            [
                Some(out.join("photos/2024/camera.jpg")),
                Some(out.join("photos/notes.txt")),
                Some(out.join("photos/text-chunks.png")),
            ]
        );
        assert_eq!(report.files[1].outcome, Outcome::Copied);
        let verify = run(
            &Request {
                recursive: true,
                ..Request::new(Command::Verify, vec![out])
            },
            &(),
            &CancelToken::new(),
        );
        let verified = verify
            .files
            .iter()
            .filter(|f| f.outcome == Outcome::Verified);
        assert_eq!(verified.count(), 2);
    }

    #[test]
    fn cleaning_a_folder_again_skips_its_own_output() {
        let dir = tempfile::tempdir().unwrap();
        copy_fixture(dir.path(), "jpeg/camera.jpg");
        let clean = || {
            run(
                &Request {
                    recursive: true,
                    ..Request::new(Command::Clean, vec![dir.path().to_owned()])
                },
                &(),
                &CancelToken::new(),
            )
        };
        clean();
        let again = clean();
        assert_eq!(
            again.files[0].outcome,
            Outcome::Skipped(SkipReason::AlreadyCleaned)
        );
        assert_eq!(
            again.files[1].output,
            Some(dir.path().join("camera.clean-2.jpg"))
        );
    }

    #[test]
    fn a_cancelled_run_writes_nothing_more() {
        let dir = tempfile::tempdir().unwrap();
        let original = copy_fixture(dir.path(), "jpeg/camera.jpg");
        let cancel = CancelToken::new();
        cancel.cancel();
        let report = run(&Request::new(Command::Clean, vec![original]), &(), &cancel);
        assert!(matches!(
            &report.files[0].outcome,
            Outcome::Failed(f) if f.kind == FailureKind::Cancelled
        ));
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
