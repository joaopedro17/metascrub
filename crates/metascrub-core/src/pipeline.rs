//! Runs a command over a list of files and collects one report per file.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use log::{debug, info, warn};
use rayon::prelude::*;
use sha2::{Digest, Sha256};

use crate::budget::{self, Budget};
use crate::format::{self, FormatError, FormatHandler, SNIFF_LEN};
use crate::fsmeta::{self, Attribute};
use crate::limits::{MAX_PARALLEL_FILES, MEMORY_BUDGET_BYTES};
use crate::model::{
    Checksums, Decision, Disposition, Failure, FailureKind, FileReport, Finding, FindingKind,
    FormatId, KeepReason, Location, Outcome, RunReport, SkipReason, Value,
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
    /// Report the SHA-256 of every file read and written.
    pub checksums: bool,
    /// Name files by their path in log messages, not only by their number.
    /// Paths can show a user name or what a file is about.
    pub log_names: bool,
    /// Left out of the run wherever they're found (the log file itself).
    pub ignore: Vec<PathBuf>,
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
            checksums: false,
            log_names: false,
            ignore: Vec::new(),
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
    log_start(request, jobs.len());
    observer.planned(jobs.len());
    let budget = Budget::new(MEMORY_BUDGET_BYTES);
    let run_job = |(index, job): (usize, &Job)| {
        let label = Label::new(index, job, request);
        if cancel.is_cancelled() {
            let report = report(job, Outcome::Failed(cancelled()));
            log_finished(&label, &report, request, Instant::now());
            return report;
        }
        observer.file_started(&job.path);
        let started = Instant::now();
        let report = process(job, &label, request, &budget);
        log_finished(&label, &report, request, started);
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
    debug!("at most {threads} files at a time");
    let mut files: Vec<FileReport> = match pool {
        Some(pool) => pool.install(|| jobs.par_iter().enumerate().map(run_job).collect()),
        None => jobs.iter().enumerate().map(run_job).collect(),
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
    let report = RunReport { files };
    info!("finished: {:?}", report.summary());
    report
}

/// How the log names a file: `file 12`, followed by its path only when the
/// request allows names (`Request::log_names`).
struct Label<'a> {
    number: usize,
    folder: bool,
    path: Option<&'a Path>,
}

impl<'a> Label<'a> {
    fn new(index: usize, job: &'a Job, request: &Request) -> Self {
        Self {
            number: index + 1,
            folder: job.folder,
            path: request.log_names.then_some(job.path.as_path()),
        }
    }
}

impl fmt::Display for Label<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = if self.folder { "folder" } else { "file" };
        write!(f, "{kind} {}", self.number)?;
        match self.path {
            Some(path) => write!(f, " ({})", path.display()),
            None => Ok(()),
        }
    }
}

fn ms(since: Instant) -> u128 {
    since.elapsed().as_millis()
}

fn size(bytes: u64) -> String {
    const MB: u64 = 1024 * 1024;
    if bytes < MB {
        format!("{} KB", bytes.div_ceil(1024))
    } else {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    }
}

fn log_start(request: &Request, found: usize) {
    info!(
        "MetaScrub {} on {} ({})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let output = match &request.output {
        OutputMode::NextToOriginal => "next to the originals".to_owned(),
        OutputMode::InPlace => "in place".to_owned(),
        OutputMode::Directory(dir) if request.log_names => format!("into {}", dir.display()),
        OutputMode::Directory(_) => "into a folder".to_owned(),
    };
    info!(
        "{:?}{}: {} paths given, {found} files and folders found; output {output}; \
         recursive: {}, rename: {}, remove junk: {}, checksums: {}; {:?}",
        request.command,
        if request.dry_run { " (dry run)" } else { "" },
        request.paths.len(),
        request.recursive,
        request.rename,
        request.remove_junk,
        request.checksums,
        request.options,
    );
    if request.log_names {
        for path in &request.paths {
            debug!("path given: {}", path.display());
        }
    }
}

fn log_finished(label: &Label<'_>, report: &FileReport, request: &Request, started: Instant) {
    let format = report
        .format
        .map_or_else(|| "format not read".to_owned(), |f| format!("{f:?}"));
    let took = ms(started);
    match &report.outcome {
        Outcome::Failed(failure) => {
            // These can name files inside an archive or document.
            let named = matches!(
                failure.kind,
                FailureKind::UnsafePath | FailureKind::Incomplete
            );
            let detail = if named && !request.log_names {
                "details hidden because they can name files (add --show-values to include them)"
            } else {
                failure.detail.as_str()
            };
            warn!(
                "{label}: failed ({:?}): {detail} [{format}, {took} ms]",
                failure.kind
            );
        }
        outcome => info!(
            "{label}: {outcome:?} [{format}, {} findings, {took} ms]",
            report.findings.len()
        ),
    }
    if !log::log_enabled!(log::Level::Debug) {
        return;
    }
    // Counted, in the order first found: "Timestamp Remove ×3".
    let mut counted: Vec<(String, usize)> = Vec::new();
    for finding in &report.findings {
        let name = format!("{:?} {:?}", finding.kind, finding.disposition);
        match counted.iter_mut().find(|(n, _)| *n == name) {
            Some((_, count)) => *count += 1,
            None => counted.push((name, 1)),
        }
    }
    if !counted.is_empty() {
        let findings: Vec<String> = counted
            .into_iter()
            .map(|(name, count)| match count {
                1 => name,
                n => format!("{name} ×{n}"),
            })
            .collect();
        debug!("{label}: {}", findings.join(", "));
    }
}

fn report(job: &Job, outcome: Outcome) -> FileReport {
    FileReport {
        path: job.path.clone(),
        format: None,
        findings: Vec::new(),
        outcome,
        output: None,
        checksums: None,
    }
}

fn process(job: &Job, label: &Label<'_>, request: &Request, budget: &Budget) -> FileReport {
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
        process_file(job, label, request, budget, &mut report)
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
    label: &Label<'_>,
    request: &Request,
    budget: &Budget,
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
    if request.checksums {
        let step = Instant::now();
        report.checksums = Some(Checksums {
            original: sha256(&mut file)?,
            output: None,
        });
        file.seek(SeekFrom::Start(0))?;
        debug!("{label}: checksum of the original in {} ms", ms(step));
    }
    let mut head = Vec::with_capacity(SNIFF_LEN);
    Read::by_ref(&mut file)
        .take(SNIFF_LEN as u64)
        .read_to_end(&mut head)?;
    file.seek(SeekFrom::Start(0))?;
    report.format = format::detect(&head, &mut file)?;
    match report.format {
        Some(format) => debug!("{label}: {format:?}, {}", size(metadata.len())),
        None => debug!(
            "{label}: a format MetaScrub can't read, {}",
            size(metadata.len())
        ),
    }
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

    let estimate = budget::estimate(format, metadata.len());
    let waiting = Instant::now();
    let _memory = budget.reserve(estimate);
    if estimate > 0 {
        debug!(
            "{label}: {} of memory reserved, after waiting {} ms",
            size(estimate),
            ms(waiting)
        );
    }
    let step = Instant::now();
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
            let cleaned = clean(job, label, file, handler, request, &settings)?;
            report.output = cleaned.output;
            report.findings = cleaned.findings;
            if let Some(checksums) = &mut report.checksums {
                checksums.output = cleaned.checksum;
            }
            report.findings.extend(attributes);
            match decision_needed(&report.findings) {
                Some(decision) => Outcome::NeedsDecision(decision),
                None => Outcome::Cleaned,
            }
        }
    };
    if request.command != Command::Clean || request.dry_run {
        debug!("{label}: read in {} ms", ms(step));
    }
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
    label: &Label<'_>,
    file: File,
    handler: &dyn FormatHandler,
    request: &Request,
    settings: &output::Settings,
) -> Result<Cleaned, FormatError> {
    let mut draft = output::Draft::new(job.target.as_deref().unwrap_or(&job.path))?;
    let step = Instant::now();
    let findings = {
        let mut input = BufReader::new(file);
        let mut out = BufWriter::new(draft.file());
        let findings = handler.clean(&mut input, &mut out, &request.options)?;
        out.flush()?;
        findings
        // The input is closed here: Windows can't replace a file that is still open.
    };
    debug!("{label}: cleaned into a temporary file in {} ms", ms(step));
    if decision_needed(&findings).is_some() {
        debug!("{label}: not written, it needs the user's decision");
        return Ok(Cleaned {
            output: None,
            findings,
            checksum: None,
        });
    }
    let step = Instant::now();
    if handler.checks_its_output() {
        debug!("{label}: checked by its cleaner while cleaning");
    } else {
        self_check(draft.file(), handler, &request.options)?;
        debug!("{label}: self-check passed in {} ms", ms(step));
    }
    let checksum = if request.checksums {
        let step = Instant::now();
        let file = draft.file();
        file.seek(SeekFrom::Start(0))?;
        let checksum = sha256(file)?;
        debug!("{label}: checksum of the cleaned file in {} ms", ms(step));
        Some(checksum)
    } else {
        None
    };
    let step = Instant::now();
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
    debug!("{label}: written in {} ms", ms(step));
    Ok(Cleaned {
        output: Some(written),
        findings,
        checksum,
    })
}

#[derive(Debug)]
struct Cleaned {
    /// `None` when nothing was written because the user must decide first.
    output: Option<PathBuf>,
    findings: Vec<Finding>,
    /// Of the file written, with `--checksums`.
    checksum: Option<String>,
}

fn sha256(input: &mut impl Read) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 16];
    loop {
        match input.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => hasher.update(buffer.get(..n).unwrap_or_default()),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// A copy has the same bytes as the original.
fn copied(report: &mut FileReport) {
    if let Some(checksums) = &mut report.checksums {
        checksums.output = Some(checksums.original.clone());
    }
}

/// Reads the cleaned copy back as `verify` would, so a cleaner that missed
/// something fails instead of passing the file off as clean.
fn self_check(
    draft: &mut File,
    handler: &dyn FormatHandler,
    options: &Options,
) -> Result<(), FormatError> {
    draft.seek(SeekFrom::Start(0))?;
    let findings = handler
        .inspect(&mut BufReader::new(draft), options)
        .map_err(|e| {
            FormatError::Incomplete(format!("the cleaned copy can't be read back ({e})"))
        })?;
    format::ensure_clean(&findings)
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
            copied(report);
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
                    copied(report);
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
        FormatError::UnsafePath(_) => FailureKind::UnsafePath,
        FormatError::TooLarge { .. } | FormatError::TooDeep { .. } => FailureKind::LimitExceeded,
        FormatError::Incomplete(_) => FailureKind::Incomplete,
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

    /// A cleaner that copies the file unchanged and reads back whatever
    /// `left` says is still there.
    struct Leaky {
        left: Option<Disposition>,
    }

    impl FormatHandler for Leaky {
        fn inspect(
            &self,
            input: &mut dyn format::ReadSeek,
            _options: &Options,
        ) -> Result<Vec<Finding>, FormatError> {
            let mut bytes = Vec::new();
            input.read_to_end(&mut bytes)?;
            if bytes.is_empty() {
                return Err(FormatError::malformed("empty"));
            }
            Ok(self
                .left
                .map(|d| Finding::new(FindingKind::GpsLocation, Location::new(["EXIF"]), d))
                .into_iter()
                .collect())
        }

        fn clean(
            &self,
            input: &mut dyn format::ReadSeek,
            output: &mut dyn Write,
            _options: &Options,
        ) -> Result<Vec<Finding>, FormatError> {
            io::copy(input, output)?;
            Ok(Vec::new())
        }
    }

    fn check(handler: &Leaky, content: &[u8]) -> Result<(), FormatError> {
        let mut draft = tempfile::tempfile().unwrap();
        draft.write_all(content).unwrap();
        self_check(&mut draft, handler, &Options::default())
    }

    #[test]
    fn the_cleaned_copy_is_read_back_before_it_is_kept() {
        assert!(check(&Leaky { left: None }, b"data").is_ok());
        let kept = Leaky {
            left: Some(Disposition::Keep(KeepReason::RequiredToRender)),
        };
        assert!(check(&kept, b"data").is_ok());
        let leaky = Leaky {
            left: Some(Disposition::Remove),
        };
        assert!(matches!(
            check(&leaky, b"data"),
            Err(FormatError::Incomplete(detail)) if detail.contains("GpsLocation at EXIF")
        ));
        // A copy that can't be read back is no better.
        assert!(matches!(
            check(&Leaky { left: None }, b""),
            Err(FormatError::Incomplete(_))
        ));
    }

    #[test]
    fn a_copy_that_fails_the_check_is_thrown_away() {
        let dir = tempfile::tempdir().unwrap();
        let original = copy_fixture(dir.path(), "jpeg/camera.jpg");
        let job = plan::plan(&Request::new(Command::Clean, vec![original.clone()])).remove(0);
        let leaky = Leaky {
            left: Some(Disposition::Remove),
        };
        let settings = output::Settings {
            permissions: fs::metadata(&original).unwrap().permissions(),
            neutral_times: true,
        };
        let request = Request::new(Command::Clean, vec![original.clone()]);
        let file = File::open(&original).unwrap();
        let label = Label {
            number: 1,
            folder: false,
            path: None,
        };
        let error = clean(&job, &label, file, &leaky, &request, &settings).unwrap_err();
        assert_eq!(failure(error).kind, FailureKind::Incomplete);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    const HELLO_SHA256: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    #[test]
    fn checksums_only_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let original = copy_fixture(dir.path(), "jpeg/camera.jpg");
        let report = run_one(Request::new(Command::Inspect, vec![original]));
        assert_eq!(report.checksums, None);
    }

    #[test]
    fn checksums_of_the_original_and_the_cleaned_file() {
        let dir = tempfile::tempdir().unwrap();
        let original = copy_fixture(dir.path(), "jpeg/camera.jpg");
        let before = sha256(&mut File::open(&original).unwrap()).unwrap();
        let report = run_one(Request {
            checksums: true,
            ..Request::new(Command::Clean, vec![original])
        });
        let checksums = report.checksums.unwrap();
        assert_eq!(checksums.original, before);
        let after = sha256(&mut File::open(report.output.unwrap()).unwrap()).unwrap();
        assert_eq!(checksums.output, Some(after));
        assert_ne!(checksums.output, Some(before));
    }

    #[test]
    fn a_copy_has_the_checksum_of_its_original() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("notes.txt");
        fs::write(&text, "hello").unwrap();
        let report = run_one(Request {
            checksums: true,
            output: OutputMode::Directory(dir.path().join("out")),
            ..Request::new(Command::Clean, vec![text.clone()])
        });
        assert_eq!(report.outcome, Outcome::Copied);
        let checksums = report.checksums.unwrap();
        assert_eq!(checksums.original, HELLO_SHA256);
        assert_eq!(checksums.output.as_deref(), Some(HELLO_SHA256));

        // Nothing is written for it next to the original.
        let report = run_one(Request {
            checksums: true,
            ..Request::new(Command::Clean, vec![text])
        });
        let checksums = report.checksums.unwrap();
        assert_eq!(checksums.original, HELLO_SHA256);
        assert_eq!(checksums.output, None);
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
