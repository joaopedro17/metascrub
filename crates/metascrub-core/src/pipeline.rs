//! Runs a command over a list of files and collects one report per file.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;

use crate::format::{self, FormatError, FormatHandler, SNIFF_LEN};
use crate::fsmeta::{self, Attribute};
use crate::limits::MAX_PARALLEL_FILES;
use crate::model::{
    Decision, Disposition, Failure, FailureKind, FileReport, Finding, FindingKind, FormatId,
    KeepReason, Location, Outcome, RunReport, SkipReason, Value,
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
    /// Delete junk files in place, or leave them out of the output folder.
    /// Ignored when writing next to the originals, which are never deleted.
    pub remove_junk: bool,
    /// Give files whose names reveal a date, device or app a neutral name
    /// (`image-001.jpg`).
    pub rename: bool,
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
            remove_junk: false,
            rename: false,
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
    let mut files: Vec<FileReport> = match pool {
        Some(pool) => pool.install(|| jobs.par_iter().map(run_job).collect()),
        None => jobs.iter().map(run_job).collect(),
    };
    if request.command == Command::Clean && !request.dry_run && !cancel.is_cancelled() {
        // Last, and deepest first: writing into a folder changes its date.
        for (job, report) in jobs.iter().zip(&mut files).rev() {
            if job.folder
                && !report.outcome.is_failure()
                && let Err(error) = finish_folder(job, request)
            {
                report.outcome = Outcome::Failed(failure(error));
            }
        }
    }
    // Folders are listed only when there is something to say about them.
    files.retain(|f| {
        f.format != Some(FormatId::Folder) || !f.findings.is_empty() || f.outcome.is_failure()
    });
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
    let result = if job.folder {
        report.format = Some(FormatId::Folder);
        process_folder(job, request, &mut report)
    } else if job.junk && removes_junk(request) && is_regular_file(&job.path) {
        remove_junk(job, request, &mut report)
    } else {
        process_file(job, request, &mut report)
    };
    report.outcome = match result {
        Ok(outcome) => outcome,
        Err(FormatError::Encrypted) => Outcome::Skipped(SkipReason::Encrypted),
        Err(error) => Outcome::Failed(failure(error)),
    };
    let reported = report
        .findings
        .iter()
        .any(|f| f.kind == FindingKind::JunkFile);
    if job.junk && !reported && !report.outcome.is_failure() {
        report.findings.insert(0, junk(Disposition::Warn));
    }
    if job.name_leaks
        && let Some(finding) = name_finding(job, request, &report.outcome)
    {
        report.findings.insert(0, finding);
    }
    report
}

/// The file name, when it reveals something. Renamed only if a file is
/// written under the new name.
fn name_finding(job: &Job, request: &Request, outcome: &Outcome) -> Option<Finding> {
    let written = match outcome {
        Outcome::Cleaned | Outcome::WouldClean | Outcome::Copied | Outcome::WouldCopy => true,
        Outcome::Skipped(SkipReason::UnsupportedFormat) => request.output == OutputMode::InPlace,
        Outcome::Inspected | Outcome::Verified | Outcome::RemainingMetadata => false,
        _ => return None,
    };
    let disposition = if job.renamed && written {
        Disposition::Remove
    } else {
        Disposition::Warn
    };
    let name = job.path.file_name()?.to_string_lossy().into_owned();
    Some(
        Finding::new(
            FindingKind::FileName,
            Location::new(["File name"]),
            disposition,
        )
        .with_value(Value::Text(name)),
    )
}

/// Junk files go only when asked, and never when writing next to the
/// originals, which MetaScrub doesn't delete.
fn removes_junk(request: &Request) -> bool {
    request.command == Command::Clean
        && request.remove_junk
        && request.output != OutputMode::NextToOriginal
}

/// Symbolic links and special files named like junk are skipped as usual.
fn is_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_file())
}

fn junk(disposition: Disposition) -> Finding {
    Finding::new(
        FindingKind::JunkFile,
        Location::new(["File name"]),
        disposition,
    )
}

/// Leaves a junk file out of the output folder, or deletes it in place.
fn remove_junk(
    job: &Job,
    request: &Request,
    report: &mut FileReport,
) -> Result<Outcome, FormatError> {
    report.findings = vec![junk(Disposition::Remove)];
    if request.dry_run {
        return Ok(Outcome::WouldRemove);
    }
    if request.output == OutputMode::InPlace {
        fs::remove_file(&job.path)?;
    }
    Ok(Outcome::Removed)
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
    let attributes = fsmeta::list(path)?;

    let Some((format, handler)) = report
        .format
        .and_then(|format| Some((format, format::handler(format)?)))
    else {
        return unsupported(job, &mut file, request, &settings, attributes, report);
    };

    // Outputs are new files, which never carry the original's attributes.
    let attributes = fsmeta::findings(attributes, Disposition::Remove);
    let options = &request.options;
    let outcome = match request.command {
        Command::Inspect => {
            report.findings = handler.inspect(&mut file, options)?;
            report.findings.extend(attributes);
            Outcome::Inspected
        }
        Command::Verify => {
            report.findings = handler.inspect(&mut file, options)?;
            report.findings.extend(attributes);
            verdict(&report.findings)
        }
        Command::Clean if request.dry_run => {
            report.findings = handler.inspect(&mut file, options)?;
            report.findings.extend(attributes);
            match decision_needed(&report.findings) {
                Some(decision) => Outcome::NeedsDecision(decision),
                None => Outcome::WouldClean,
            }
        }
        Command::Clean => {
            let (output, findings) = clean(job, file, handler, request, &settings)?;
            report.output = output;
            report.findings = findings;
            report.findings.extend(attributes);
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

/// Folders are never copied: an output folder's copies are new folders.
fn process_folder(
    job: &Job,
    request: &Request,
    report: &mut FileReport,
) -> Result<Outcome, FormatError> {
    let path = job.path.as_path();
    let strip = request.command == Command::Clean
        && !request.dry_run
        && request.output == OutputMode::InPlace;
    let attributes = if strip {
        fsmeta::strip(path)?
    } else {
        fsmeta::list(path)?
    };
    report.findings = fsmeta::findings(attributes, Disposition::Remove);
    // A `__MACOSX` folder: removed in place after its contents.
    if job.junk && removes_junk(request) {
        report.findings.insert(0, junk(Disposition::Remove));
        return Ok(if request.dry_run {
            Outcome::WouldRemove
        } else {
            Outcome::Removed
        });
    }
    Ok(match request.command {
        Command::Inspect => Outcome::Inspected,
        Command::Verify => verdict(&report.findings),
        Command::Clean if request.dry_run => Outcome::WouldClean,
        Command::Clean => Outcome::Cleaned,
    })
}

/// The folder cleaned in place, or its copy in the output folder, once
/// everything inside it is written.
fn finish_folder(job: &Job, request: &Request) -> Result<(), FormatError> {
    if job.junk && removes_junk(request) && request.output == OutputMode::InPlace {
        // Fails if something inside couldn't be removed.
        fs::remove_dir(&job.path)?;
        return Ok(());
    }
    let path = match (&request.output, &job.target) {
        (OutputMode::InPlace, _) => job.path.as_path(),
        // Folders with nothing written into them aren't created.
        (OutputMode::Directory(_), Some(target)) if target.is_dir() => {
            fsmeta::strip(target)?;
            target.as_path()
        }
        _ => return Ok(()),
    };
    if !request.options.keep_file_times {
        output::set_neutral_times_at(path)?;
    }
    Ok(())
}

/// Writes nothing when the findings say the user must decide first.
/// Streams from the file into the output's temporary file, which only
/// becomes the output once nothing needs a decision.
fn clean(
    job: &Job,
    file: File,
    handler: &dyn FormatHandler,
    request: &Request,
    settings: &output::Settings,
) -> Result<(Option<PathBuf>, Vec<Finding>), FormatError> {
    let mut draft = output::Draft::new(job.target.as_deref().unwrap_or(&job.path))?;
    let findings = {
        let mut input = BufReader::new(file);
        let mut out = BufWriter::new(draft.file());
        let findings = handler.clean(&mut input, &mut out, &request.options)?;
        out.flush()?;
        findings
        // The input is closed here: Windows can't replace a file that is still open.
    };
    if decision_needed(&findings).is_some() {
        return Ok((None, findings));
    }
    let written = match &job.target {
        Some(target) => draft.finish_new(target, settings)?,
        None => {
            draft.finish_replace(&job.path, settings)?;
            job.path.clone()
        }
    };
    // In case the system added any to the new file.
    fsmeta::strip(&written)?;
    if job.renamed && request.output == OutputMode::InPlace {
        fs::remove_file(&job.path)?;
    }
    Ok((Some(written), findings))
}

fn verdict(findings: &[Finding]) -> Outcome {
    if findings
        .iter()
        .all(|f| f.disposition.is_acceptable_after_cleaning())
    {
        Outcome::Verified
    } else {
        Outcome::RemainingMetadata
    }
}

fn decision_needed(findings: &[Finding]) -> Option<Decision> {
    findings.iter().find_map(|f| match f.disposition {
        Disposition::Blocks(decision) => Some(decision),
        _ => None,
    })
}

/// Unsupported files are only copied when writing into an output folder,
/// so the folder is complete (requirements §13.13). In place, their
/// contents stay as they are, but the file system's data about them goes.
fn unsupported(
    job: &Job,
    file: &mut File,
    request: &Request,
    settings: &output::Settings,
    attributes: Vec<Attribute>,
    report: &mut FileReport,
) -> Result<Outcome, FormatError> {
    let skipped = Outcome::Skipped(SkipReason::UnsupportedFormat);
    if request.command != Command::Clean {
        report.findings = fsmeta::findings(attributes, Disposition::Remove);
        return Ok(match request.command {
            Command::Verify if verdict(&report.findings) == Outcome::RemainingMetadata => {
                Outcome::RemainingMetadata
            }
            _ => skipped,
        });
    }
    match (&request.output, &job.target) {
        (OutputMode::Directory(_), Some(target)) => {
            report.findings = fsmeta::findings(attributes, Disposition::Remove);
            if request.dry_run {
                return Ok(Outcome::WouldCopy);
            }
            // Copying the bytes ourselves, unlike `fs::copy`, never carries
            // over extended attributes or alternate data streams.
            let written = output::write_new(target, settings, |out| {
                io::copy(file, out)?;
                Ok(())
            })?;
            fsmeta::strip(&written)?;
            report.output = Some(written);
            Ok(Outcome::Copied)
        }
        (OutputMode::InPlace, target) => {
            report.findings = fsmeta::findings(attributes, Disposition::Remove);
            if request.dry_run {
                return Ok(skipped);
            }
            match target {
                // Renamed: a fresh copy under the new name replaces it.
                Some(target) => {
                    let written = output::write_new(target, settings, |out| {
                        io::copy(file, out)?;
                        Ok(())
                    })?;
                    fsmeta::strip(&written)?;
                    fs::remove_file(&job.path)?;
                    report.output = Some(written);
                }
                None => {
                    fsmeta::strip(&job.path)?;
                    if settings.neutral_times {
                        output::set_neutral_times_at(&job.path)?;
                    }
                }
            }
            Ok(skipped)
        }
        _ => {
            report.findings =
                fsmeta::findings(attributes, Disposition::Keep(KeepReason::NotWritten));
            Ok(skipped)
        }
    }
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
        FormatError::Unsupported(_) | FormatError::Encrypted => FailureKind::Unsupported,
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
    fn encrypted_files_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let original = copy_fixture(dir.path(), "pdf/encrypted.pdf");
        let report = run_one(Request::new(Command::Clean, vec![original]));
        assert_eq!(report.outcome, Outcome::Skipped(SkipReason::Encrypted));
        assert_eq!(report.format, Some(FormatId::Pdf));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
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

    const URL: &str = "https://example.com/camera.jpg";

    /// A copy of `relative` that a browser marked as downloaded, or `None`
    /// where the file system can't record it.
    fn downloaded(dir: &Path, relative: &str) -> Option<PathBuf> {
        let path = copy_fixture(dir, relative);
        fsmeta::mark_downloaded(&path, URL).then_some(path)
    }

    fn download_urls(report: &FileReport) -> Vec<Disposition> {
        report
            .findings
            .iter()
            .filter(|f| f.kind == FindingKind::DownloadUrl)
            .map(|f| f.disposition)
            .collect()
    }

    fn has_attributes(path: &Path) -> bool {
        fsmeta::findings(fsmeta::list(path).unwrap(), Disposition::Remove)
            .iter()
            .any(|f| f.disposition == Disposition::Remove)
    }

    #[test]
    fn reports_where_a_file_was_downloaded_from() {
        let dir = tempfile::tempdir().unwrap();
        let Some(photo) = downloaded(dir.path(), "jpeg/camera.jpg") else {
            return;
        };
        let report = run_one(Request::new(Command::Inspect, vec![photo.clone()]));
        assert_eq!(download_urls(&report), [Disposition::Remove]);

        let report = run_one(Request::new(Command::Clean, vec![photo.clone()]));
        assert_eq!(download_urls(&report), [Disposition::Remove]);
        let output = report.output.unwrap();
        assert!(!has_attributes(&output));
        assert!(has_attributes(&photo), "the original is left alone");

        let report = run_one(Request::new(Command::Verify, vec![output]));
        assert_eq!(report.outcome, Outcome::Verified);
    }

    #[test]
    fn cleaning_in_place_drops_the_attributes() {
        let dir = tempfile::tempdir().unwrap();
        let Some(photo) = downloaded(dir.path(), "jpeg/camera.jpg") else {
            return;
        };
        let report = run_one(Request {
            output: OutputMode::InPlace,
            ..Request::new(Command::Clean, vec![photo.clone()])
        });
        assert_eq!(report.outcome, Outcome::Cleaned);
        assert!(!has_attributes(&photo));
    }

    #[test]
    fn unsupported_files_lose_their_attributes_in_place_only() {
        let dir = tempfile::tempdir().unwrap();
        let notes = dir.path().join("notes.txt");
        fs::write(&notes, "hello").unwrap();
        if !fsmeta::mark_downloaded(&notes, URL) {
            return;
        }
        let verify = run_one(Request::new(Command::Verify, vec![notes.clone()]));
        assert_eq!(verify.outcome, Outcome::RemainingMetadata);

        let next_to = run_one(Request::new(Command::Clean, vec![notes.clone()]));
        assert_eq!(
            download_urls(&next_to),
            [Disposition::Keep(KeepReason::NotWritten)]
        );
        assert!(has_attributes(&notes));

        let dry_run = run_one(Request {
            output: OutputMode::InPlace,
            dry_run: true,
            ..Request::new(Command::Clean, vec![notes.clone()])
        });
        assert_eq!(download_urls(&dry_run), [Disposition::Remove]);
        assert!(has_attributes(&notes));

        let in_place = run_one(Request {
            output: OutputMode::InPlace,
            ..Request::new(Command::Clean, vec![notes.clone()])
        });
        assert_eq!(
            in_place.outcome,
            Outcome::Skipped(SkipReason::UnsupportedFormat)
        );
        assert_eq!(download_urls(&in_place), [Disposition::Remove]);
        assert!(!has_attributes(&notes));
        assert_eq!(fs::read_to_string(&notes).unwrap(), "hello");
        let modified = fs::metadata(&notes).unwrap().modified().unwrap();
        assert_eq!(modified, std::time::UNIX_EPOCH + output::NEUTRAL_TIME);
    }

    #[test]
    fn copies_in_an_output_folder_have_no_attributes() {
        let dir = tempfile::tempdir().unwrap();
        let notes = dir.path().join("notes.txt");
        fs::write(&notes, "hello").unwrap();
        if !fsmeta::mark_downloaded(&notes, URL) {
            return;
        }
        let report = run_one(Request {
            output: OutputMode::Directory(dir.path().join("out")),
            ..Request::new(Command::Clean, vec![notes])
        });
        assert_eq!(report.outcome, Outcome::Copied);
        assert_eq!(download_urls(&report), [Disposition::Remove]);
        assert!(!has_attributes(&report.output.unwrap()));
    }

    fn is_neutral(path: &Path) -> bool {
        let modified = fs::metadata(path).unwrap().modified().unwrap();
        modified == std::time::UNIX_EPOCH + output::NEUTRAL_TIME
    }

    /// `photos/` (marked as downloaded) holding `trip/camera.jpg`.
    fn photo_folder(dir: &Path) -> Option<PathBuf> {
        let photos = dir.join("photos");
        fs::create_dir_all(photos.join("trip")).unwrap();
        copy_fixture(&photos.join("trip"), "jpeg/camera.jpg");
        fsmeta::mark_downloaded(&photos, URL).then_some(photos)
    }

    #[test]
    fn folders_are_reported_only_with_something_to_say() {
        let dir = tempfile::tempdir().unwrap();
        let Some(photos) = photo_folder(dir.path()) else {
            return;
        };
        let report = run(
            &Request {
                recursive: true,
                ..Request::new(Command::Inspect, vec![photos.clone()])
            },
            &(),
            &CancelToken::new(),
        );
        let paths: Vec<_> = report.files.iter().map(|f| f.path.clone()).collect();
        assert_eq!(paths, [photos.clone(), photos.join("trip/camera.jpg")]);
        assert_eq!(report.files[0].format, Some(FormatId::Folder));
        assert_eq!(download_urls(&report.files[0]), [Disposition::Remove]);
    }

    #[test]
    fn cleaning_a_folder_in_place_cleans_the_folders_too() {
        let dir = tempfile::tempdir().unwrap();
        let Some(photos) = photo_folder(dir.path()) else {
            return;
        };
        let report = run(
            &Request {
                recursive: true,
                output: OutputMode::InPlace,
                ..Request::new(Command::Clean, vec![photos.clone()])
            },
            &(),
            &CancelToken::new(),
        );
        assert_eq!(report.files[0].outcome, Outcome::Cleaned);
        assert!(!has_attributes(&photos));
        for path in [&photos, &photos.join("trip")] {
            assert!(is_neutral(path), "{}", path.display());
        }
        assert!(
            !is_neutral(dir.path()),
            "only the folder given and what's inside"
        );
    }

    #[test]
    fn folders_in_an_output_folder_get_the_neutral_date() {
        let dir = tempfile::tempdir().unwrap();
        let photos = dir.path().join("photos");
        fs::create_dir_all(photos.join("trip")).unwrap();
        copy_fixture(&photos.join("trip"), "jpeg/camera.jpg");
        let out = dir.path().join("out");
        let request = |keep_file_times| Request {
            recursive: true,
            output: OutputMode::Directory(out.clone()),
            options: Options {
                keep_file_times,
                ..Options::default()
            },
            ..Request::new(Command::Clean, vec![photos.clone()])
        };
        run(&request(false), &(), &CancelToken::new());
        for path in [out.join("photos"), out.join("photos/trip")] {
            assert!(is_neutral(&path), "{}", path.display());
        }
        assert!(!is_neutral(&out), "the output folder itself is the user's");
        assert!(!is_neutral(&photos), "the originals are left alone");

        fs::remove_dir_all(&out).unwrap();
        run(&request(true), &(), &CancelToken::new());
        assert!(!is_neutral(&out.join("photos")));
    }

    /// `photos/` with a photo, a `.DS_Store` and a `__MACOSX` folder.
    fn folder_with_junk(dir: &Path) -> PathBuf {
        let photos = dir.join("photos");
        fs::create_dir_all(photos.join("__MACOSX")).unwrap();
        copy_fixture(&photos, "jpeg/camera.jpg");
        fs::write(photos.join(".DS_Store"), "Bud1").unwrap();
        fs::write(photos.join("__MACOSX/._camera.jpg"), "\0\x05\x16\x07").unwrap();
        photos
    }

    fn clean_folder(
        photos: &Path,
        output: OutputMode,
        remove_junk: bool,
        dry_run: bool,
    ) -> RunReport {
        let request = Request {
            recursive: true,
            output,
            remove_junk,
            dry_run,
            ..Request::new(Command::Clean, vec![photos.to_owned()])
        };
        run(&request, &(), &CancelToken::new())
    }

    fn outcome_of<'a>(report: &'a RunReport, path: &Path) -> &'a Outcome {
        &report
            .files
            .iter()
            .find(|f| f.path == path)
            .unwrap()
            .outcome
    }

    #[test]
    fn junk_is_deleted_in_place_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let photos = folder_with_junk(dir.path());

        let dry_run = clean_folder(&photos, OutputMode::InPlace, true, true);
        assert_eq!(
            outcome_of(&dry_run, &photos.join(".DS_Store")),
            &Outcome::WouldRemove
        );
        assert!(photos.join(".DS_Store").exists());

        let report = clean_folder(&photos, OutputMode::InPlace, true, false);
        for junk in [".DS_Store", "__MACOSX/._camera.jpg", "__MACOSX"] {
            assert_eq!(
                outcome_of(&report, &photos.join(junk)),
                &Outcome::Removed,
                "{junk}"
            );
            assert!(!photos.join(junk).exists(), "{junk}");
        }
        // Two files; the folder is listed but not counted.
        assert_eq!(report.summary().removed, 2);
        assert!(photos.join("camera.jpg").exists());
        assert!(is_neutral(&photos), "dated after the junk is gone");
    }

    #[test]
    fn junk_is_left_out_of_an_output_folder_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let photos = folder_with_junk(dir.path());
        let out = dir.path().join("out");
        let report = clean_folder(&photos, OutputMode::Directory(out.clone()), true, false);
        assert_eq!(report.summary().removed, 2);
        assert!(out.join("photos/camera.jpg").exists());
        assert!(!out.join("photos/.DS_Store").exists());
        assert!(!out.join("photos/__MACOSX").exists());
        assert!(photos.join(".DS_Store").exists(), "the originals stay");
    }

    #[test]
    fn junk_is_only_reported_unless_asked() {
        let dir = tempfile::tempdir().unwrap();
        let photos = folder_with_junk(dir.path());
        let out = dir.path().join("out");
        let report = clean_folder(&photos, OutputMode::Directory(out.clone()), false, false);
        let ds_store = report
            .files
            .iter()
            .find(|f| f.path == photos.join(".DS_Store"))
            .unwrap();
        assert_eq!(ds_store.outcome, Outcome::Copied);
        assert_eq!(ds_store.findings[0].kind, FindingKind::JunkFile);
        assert_eq!(ds_store.findings[0].disposition, Disposition::Warn);
        assert!(out.join("photos/.DS_Store").exists());

        // Next to the originals, asking changes nothing: originals are never deleted.
        let report = clean_folder(&photos, OutputMode::NextToOriginal, true, false);
        assert_eq!(report.summary().removed, 0);
        assert!(photos.join(".DS_Store").exists());
    }

    fn name_findings(report: &FileReport) -> Vec<Disposition> {
        report
            .findings
            .iter()
            .filter(|f| f.kind == FindingKind::FileName)
            .map(|f| f.disposition)
            .collect()
    }

    #[test]
    fn leaking_names_are_reported_and_replaced_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let photo = dir.path().join("IMG_20261102_143512.jpg");
        fs::copy(fixture("jpeg/camera.jpg"), &photo).unwrap();

        let report = run_one(Request::new(Command::Clean, vec![photo.clone()]));
        assert_eq!(name_findings(&report), [Disposition::Warn]);
        assert_eq!(
            report.output,
            Some(dir.path().join("IMG_20261102_143512.clean.jpg"))
        );

        let report = run_one(Request {
            output: OutputMode::InPlace,
            rename: true,
            ..Request::new(Command::Clean, vec![photo.clone()])
        });
        assert_eq!(report.outcome, Outcome::Cleaned);
        assert_eq!(name_findings(&report), [Disposition::Remove]);
        let renamed = dir.path().join("image-001.jpg");
        assert_eq!(report.output, Some(renamed.clone()));
        assert!(!photo.exists());
        let verify = run_one(Request::new(Command::Verify, vec![renamed]));
        assert_eq!(verify.outcome, Outcome::Verified);
    }

    #[test]
    fn unsupported_files_are_renamed_in_place_too() {
        let dir = tempfile::tempdir().unwrap();
        let notes = dir.path().join("notes 2026-11-02.txt");
        fs::write(&notes, "hello").unwrap();
        let marked = fsmeta::mark_downloaded(&notes, URL);
        let report = run_one(Request {
            output: OutputMode::InPlace,
            rename: true,
            ..Request::new(Command::Clean, vec![notes.clone()])
        });
        assert_eq!(name_findings(&report), [Disposition::Remove]);
        let renamed = dir.path().join("document-001.txt");
        assert_eq!(report.output, Some(renamed.clone()));
        assert!(!notes.exists());
        assert_eq!(fs::read_to_string(&renamed).unwrap(), "hello");
        if marked {
            assert!(!has_attributes(&renamed));
        }
    }

    #[test]
    fn nothing_is_renamed_when_nothing_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let notes = dir.path().join("notes 2026-11-02.txt");
        fs::write(&notes, "hello").unwrap();
        let report = run_one(Request {
            rename: true,
            ..Request::new(Command::Clean, vec![notes.clone()])
        });
        assert_eq!(name_findings(&report), [Disposition::Warn]);
        assert_eq!(report.output, None);
        assert!(notes.exists());
    }
}
