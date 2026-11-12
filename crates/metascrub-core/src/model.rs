//! What MetaScrub found in a file, and what it did about it.
//!
//! Everything here is codes plus data. Turning a finding into a sentence such
//! as "This photo contains your GPS location" is the job of the user
//! interface, never of the core.

use std::fmt;
use std::path::PathBuf;

use serde::Serialize;

/// Ordered from least to most dangerous, so `Risk::High > Risk::Low`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    Low,
    Medium,
    High,
}

/// A GPS position is a [`FindingKind::GpsLocation`] whether it came from a
/// JPEG's EXIF block, a PNG text chunk or an iPhone video.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FindingKind {
    GpsLocation,
    PersonName,
    DeviceSerial,
    FilePath,
    DownloadUrl,
    Thumbnail,
    HiddenImage,
    TrailingData,
    TrackedChanges,
    Comment,
    EmbeddedFile,
    /// Macros (VBA) or JavaScript: programs stored in the document.
    Macros,
    /// Older versions of the document kept in the same file (PDF incremental updates).
    EarlierVersions,
    /// A file inside an archive that MetaScrub can't read, kept as it is.
    UnreadFile,
    /// A symbolic link inside an archive, kept and never followed; its
    /// target can be a path from the computer that made the archive.
    SymbolicLink,
    /// Values typed into form fields.
    FormData,
    /// Subtitles or chapter titles in a video, which MetaScrub doesn't check.
    TextTrack,
    HiddenContent,
    /// A file the operating system leaves in folders (`.DS_Store`,
    /// `Thumbs.db`, …), which can hold thumbnails, file names or download
    /// addresses.
    JunkFile,
    Description,
    /// A company or organization (Office "Company").
    Organization,
    /// Extra properties any program or person can add (Office custom properties).
    CustomProperty,
    /// Data the file system keeps next to the file (extended attributes,
    /// alternate data streams) that MetaScrub doesn't recognize.
    FileAttribute,
    DeviceModel,
    Software,
    Timestamp,
    /// A file name that shows when or with what the file was made
    /// (`IMG_20261102_143512.jpg`, `WhatsApp Image …`).
    FileName,
    DocumentId,
    UnknownMetadata,
    ColorProfile,
    Orientation,
    /// Album or cover artwork in a music or audio file. Players show it,
    /// so it isn't hidden, but it can be a personal photo.
    CoverArt,
    TechnicalInfo,
    ExtensionMismatch,
}

impl FindingKind {
    /// The risk level of this kind of finding (requirements §13.5).
    pub const fn risk(self) -> Risk {
        match self {
            Self::GpsLocation
            | Self::PersonName
            | Self::DeviceSerial
            | Self::FilePath
            | Self::DownloadUrl
            | Self::Thumbnail
            | Self::HiddenImage
            | Self::TrailingData
            | Self::TrackedChanges
            | Self::Comment
            | Self::EmbeddedFile
            | Self::Macros
            | Self::EarlierVersions
            | Self::FormData
            | Self::HiddenContent
            | Self::JunkFile => Risk::High,

            Self::Description
            | Self::Organization
            | Self::CustomProperty
            | Self::FileAttribute
            | Self::SymbolicLink
            | Self::UnreadFile
            | Self::TextTrack
            | Self::DeviceModel
            | Self::Software
            | Self::Timestamp
            | Self::FileName
            | Self::DocumentId
            | Self::UnknownMetadata => Risk::Medium,

            Self::ColorProfile
            | Self::Orientation
            | Self::CoverArt
            | Self::TechnicalInfo
            | Self::ExtensionMismatch => Risk::Low,
        }
    }
}

/// Where in the file a finding was found, from the outside in,
/// e.g. `["EXIF", "GPS"]` or `["docProps/core.xml", "dc:creator"]`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Location(Vec<String>);

impl Location {
    pub fn new<I, S>(parts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self(parts.into_iter().map(Into::into).collect())
    }

    /// A location one level deeper than this one.
    /// This location as seen from the file that contains it:
    /// `EXIF > GPS` within `word/media/image1.jpeg`.
    pub fn within(&self, outer: impl Into<String>) -> Self {
        let mut parts = vec![outer.into()];
        parts.extend(self.0.iter().cloned());
        Self(parts)
    }

    pub fn child(&self, part: impl Into<String>) -> Self {
        let mut parts = self.0.clone();
        parts.push(part.into());
        Self(parts)
    }

    pub fn parts(&self) -> &[String] {
        &self.0
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.join(" > "))
    }
}

/// The actual value of a finding. Always collected; the user interface hides
/// it unless the user asks to see values.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Value {
    Text(String),
    Integer(i64),
    Coordinates {
        latitude: f64,
        longitude: f64,
    },
    /// Size in bytes of a binary block (thumbnail, trailing data, …).
    Bytes(u64),
}

/// What cleaning does (or did) with a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "action", content = "reason", rename_all = "snake_case")]
pub enum Disposition {
    Remove,
    /// A mandatory field set to a fixed neutral value (never a random one).
    Normalize,
    Keep(KeepReason),
    /// Detected and reported, but left unchanged.
    Warn,
    /// The file can't be cleaned until the user makes this decision.
    Blocks(Decision),
}

impl Disposition {
    /// Whether a cleaned file may still contain a finding with this disposition.
    /// `verify` passes only when every finding is acceptable. Warnings are,
    /// because they describe something cleaning can't change.
    pub const fn is_acceptable_after_cleaning(self) -> bool {
        matches!(self, Self::Keep(_) | Self::Warn)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeepReason {
    /// The file wouldn't display correctly without it (e.g. orientation).
    RequiredToRender,
    /// A well-known color profile that reveals nothing about the user.
    StandardColorProfile,
    /// The user asked to keep it (`--keep …`).
    UserRequested,
    /// Nothing is written for this file, so the original keeps it.
    NotWritten,
    /// Part of the picture itself (a preview, depth map or HDR gain map),
    /// showing nothing the picture doesn't show.
    PartOfImage,
    /// Sound files need it to play correctly: length, seeking, volume, cue
    /// points, loops.
    RequiredToPlay,
}

/// A choice only the user can make, because every option changes the content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Accept or reject all tracked changes (`--accept-changes` / `--reject-changes`).
    TrackedChanges,
}

/// One piece of metadata found in a file.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Finding {
    pub kind: FindingKind,
    pub risk: Risk,
    pub location: Location,
    pub value: Option<Value>,
    pub disposition: Disposition,
}

impl Finding {
    /// A finding with the default risk for its kind and no value.
    pub fn new(kind: FindingKind, location: Location, disposition: Disposition) -> Self {
        Self {
            kind,
            risk: kind.risk(),
            location,
            value: None,
            disposition,
        }
    }

    /// The same finding, located inside the file `outer` (for embedded files).
    pub fn within(mut self, outer: impl Into<String>) -> Self {
        self.location = self.location.within(outer);
        self
    }

    pub fn with_value(mut self, value: Value) -> Self {
        self.value = Some(value);
        self
    }
}

/// A file format MetaScrub can read, detected from the file's content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum FormatId {
    Jpeg,
    Png,
    Webp,
    Gif,
    /// Windows bitmap (`.bmp`, `.dib`).
    Bmp,
    Tiff,
    /// Adobe's raw photo format, built on TIFF.
    Dng,
    /// Matroska video or audio (`.mkv`, `.mka`).
    Mkv,
    Webm,
    Pdf,
    /// Word document (also `.docm`, `.dotx`, `.dotm`).
    Docx,
    /// Excel workbook (also `.xlsm`, `.xltx`, `.xltm`, `.xlam`).
    Xlsx,
    /// PowerPoint presentation (also `.pptm`, `.ppsx`, `.potx`, …).
    Pptx,
    /// OpenDocument text (`.odt`, `.ott`), as LibreOffice writes.
    Odt,
    /// OpenDocument spreadsheet (`.ods`, `.ots`).
    Ods,
    /// OpenDocument presentation (`.odp`, `.otp`).
    Odp,
    /// OpenDocument drawing (`.odg`, `.otg`).
    Odg,
    /// EPUB e-book, version 2 or 3.
    Epub,
    /// A ZIP archive (not an Office or OpenDocument file, an app or an e-book).
    Zip,
    Tar,
    /// A gzip-compressed file, such as `.tar.gz`.
    Gzip,
    /// MPEG-4 video (also `.m4v`).
    Mp4,
    /// QuickTime video, as iPhones record.
    Mov,
    /// MPEG-4 audio (`.m4a`, `.m4b`).
    M4a,
    /// HEIC or HEIF photo, as iPhones take.
    Heic,
    Avif,
    /// MPEG audio, layers I to III.
    Mp3,
    Flac,
    /// Ogg Vorbis or Opus audio (`.ogg`, `.oga`, `.opus`).
    Ogg,
    /// WAV audio, Broadcast WAV included (`.wav`, `.bwf`).
    Wav,
    /// An OLE compound file: an old-format Office file (`.doc`, `.xls`,
    /// `.ppt`) or a password-protected Office file.
    Compound,
    /// Not a file format: a folder, of which only what the file system
    /// stores with it is inspected.
    Folder,
}

/// What happened to one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", content = "detail", rename_all = "snake_case")]
pub enum Outcome {
    /// `inspect` read the file; the findings say what it contains.
    Inspected,
    /// `clean` wrote a cleaned copy (or cleaned it in place).
    Cleaned,
    /// `clean --dry-run`: the file would be cleaned; nothing was written.
    WouldClean,
    /// `verify` found nothing that cleaning should have removed.
    Verified,
    /// `verify` found metadata that cleaning should have removed.
    RemainingMetadata,
    /// Unsupported format: copied as a fresh file, but not cleaned.
    Copied,
    /// `clean --dry-run`: the file would be copied; nothing was written.
    WouldCopy,
    /// A junk file deleted (`--in-place`) or left out of the output folder
    /// (`--remove-junk`).
    Removed,
    /// `clean --dry-run`: the junk file would be removed.
    WouldRemove,
    Skipped(SkipReason),
    /// Not cleaned until the user decides (counts as a failure).
    NeedsDecision(Decision),
    Failed(Failure),
}

impl Outcome {
    /// Whether this outcome counts as a failure for the summary and exit code.
    pub const fn is_failure(&self) -> bool {
        matches!(self, Self::NeedsDecision(_) | Self::Failed(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    UnsupportedFormat,
    /// FIFOs, devices and sockets are never opened.
    NotRegularFile,
    /// Symbolic links are not followed unless the user asks for it.
    SymbolicLink,
    /// The name looks like MetaScrub's own output (`*.clean.*`).
    AlreadyCleaned,
    /// Encrypted files can't be read, and MetaScrub never asks for passwords.
    Encrypted,
}

/// Why a file could not be processed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Failure {
    pub kind: FailureKind,
    /// Technical detail for troubleshooting (e.g. the operating system's error message).
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// Reading or writing failed (permissions, locked file, disk full, …).
    Io,
    /// The file is damaged or doesn't follow its format.
    Malformed,
    /// The file exceeds a safety limit (size, entry count, compression ratio, nesting).
    LimitExceeded,
    /// The file uses a feature MetaScrub can't handle without risking breaking it.
    Unsupported,
    /// An archive holds a file whose path leads outside it (`../evil.txt`),
    /// which could overwrite other files when extracted.
    UnsafePath,
    /// The run was cancelled before this file finished.
    Cancelled,
    /// The cleaned copy still held something that should have been removed,
    /// so it was thrown away: a bug in MetaScrub.
    Incomplete,
}

/// The result for one file.
#[derive(Debug, Clone, PartialEq)]
pub struct FileReport {
    pub path: PathBuf,
    pub format: Option<FormatId>,
    pub findings: Vec<Finding>,
    pub outcome: Outcome,
    /// Where the cleaned or copied file was written, if anywhere.
    pub output: Option<PathBuf>,
    /// Only when the request asks for them.
    pub checksums: Option<Checksums>,
}

/// SHA-256 of a file as it was read and as it was written, in lowercase
/// hex, so people can show exactly which file they shared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Checksums {
    pub original: String,
    /// The file written, if one was: the cleaned file, or the copy.
    pub output: Option<String>,
}

/// The result of a whole run.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunReport {
    pub files: Vec<FileReport>,
}

impl RunReport {
    /// Folders are listed for what the file system stores with them, but
    /// they aren't files: they count only when they fail or fail `verify`.
    pub fn summary(&self) -> Summary {
        let mut summary = Summary::default();
        for file in &self.files {
            let problem = file.outcome.is_failure() || file.outcome == Outcome::RemainingMetadata;
            if file.format == Some(FormatId::Folder) && !problem {
                continue;
            }
            match &file.outcome {
                Outcome::Failed(f) if f.kind == FailureKind::Cancelled => summary.cancelled += 1,
                Outcome::Inspected => summary.inspected += 1,
                Outcome::Cleaned | Outcome::WouldClean => summary.cleaned += 1,
                Outcome::Verified => summary.verified += 1,
                Outcome::RemainingMetadata => summary.remaining_metadata += 1,
                Outcome::Copied | Outcome::WouldCopy => summary.copied += 1,
                Outcome::Removed | Outcome::WouldRemove => summary.removed += 1,
                Outcome::Skipped(_) => summary.skipped += 1,
                Outcome::NeedsDecision(_) | Outcome::Failed(_) => summary.failed += 1,
            }
        }
        summary
    }
}

/// File counts per outcome, for the final summary line and the exit code.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub inspected: usize,
    pub cleaned: usize,
    pub verified: usize,
    pub remaining_metadata: usize,
    pub copied: usize,
    /// Junk files removed.
    pub removed: usize,
    pub skipped: usize,
    pub failed: usize,
    /// Not processed because the run was stopped; not counted in `failed`.
    pub cancelled: usize,
}

impl Summary {
    /// Every file counted.
    pub const fn total(&self) -> usize {
        self.inspected
            + self.cleaned
            + self.verified
            + self.remaining_metadata
            + self.copied
            + self.removed
            + self.skipped
            + self.failed
            + self.cancelled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn risk_is_ordered_from_low_to_high() {
        assert!(Risk::Low < Risk::Medium);
        assert!(Risk::Medium < Risk::High);
    }

    #[test]
    fn risk_table_matches_requirements() {
        assert_eq!(FindingKind::GpsLocation.risk(), Risk::High);
        assert_eq!(FindingKind::Thumbnail.risk(), Risk::High);
        assert_eq!(FindingKind::TrailingData.risk(), Risk::High);
        assert_eq!(FindingKind::DeviceModel.risk(), Risk::Medium);
        assert_eq!(FindingKind::DocumentId.risk(), Risk::Medium);
        assert_eq!(FindingKind::Organization.risk(), Risk::Medium);
        assert_eq!(FindingKind::CustomProperty.risk(), Risk::Medium);
        assert_eq!(FindingKind::ColorProfile.risk(), Risk::Low);
        assert_eq!(FindingKind::ExtensionMismatch.risk(), Risk::Low);
    }

    #[test]
    fn new_finding_takes_risk_from_its_kind() {
        let finding = Finding::new(
            FindingKind::DeviceSerial,
            Location::new(["EXIF", "BodySerialNumber"]),
            Disposition::Remove,
        );
        assert_eq!(finding.risk, Risk::High);
        assert_eq!(finding.value, None);
    }

    #[test]
    fn location_displays_from_outside_in() {
        let location = Location::new(["EXIF"]).child("GPS");
        assert_eq!(location.to_string(), "EXIF > GPS");
        assert_eq!(location.parts(), ["EXIF", "GPS"]);
    }

    #[test]
    fn only_kept_and_warned_findings_are_acceptable_after_cleaning() {
        assert!(Disposition::Keep(KeepReason::RequiredToRender).is_acceptable_after_cleaning());
        assert!(!Disposition::Remove.is_acceptable_after_cleaning());
        assert!(!Disposition::Normalize.is_acceptable_after_cleaning());
        assert!(Disposition::Warn.is_acceptable_after_cleaning());
        assert!(!Disposition::Blocks(Decision::TrackedChanges).is_acceptable_after_cleaning());
    }

    #[test]
    fn needs_decision_counts_as_failure() {
        assert!(Outcome::NeedsDecision(Decision::TrackedChanges).is_failure());
        assert!(!Outcome::Skipped(SkipReason::Encrypted).is_failure());
        assert!(!Outcome::Copied.is_failure());
    }

    #[test]
    fn summary_counts_each_outcome() {
        let file = |outcome| FileReport {
            path: PathBuf::from("photo.jpg"),
            format: Some(FormatId::Jpeg),
            findings: Vec::new(),
            outcome,
            output: None,
            checksums: None,
        };
        let report = RunReport {
            files: vec![
                file(Outcome::Cleaned),
                file(Outcome::WouldClean),
                file(Outcome::Copied),
                file(Outcome::Skipped(SkipReason::UnsupportedFormat)),
                file(Outcome::NeedsDecision(Decision::TrackedChanges)),
                file(Outcome::Failed(Failure {
                    kind: FailureKind::Malformed,
                    detail: "truncated JPEG segment".into(),
                })),
                file(Outcome::Failed(Failure {
                    kind: FailureKind::Cancelled,
                    detail: String::new(),
                })),
            ],
        };
        assert_eq!(
            report.summary(),
            Summary {
                cleaned: 2,
                copied: 1,
                skipped: 1,
                failed: 2,
                cancelled: 1,
                ..Summary::default()
            }
        );
    }

    #[test]
    fn folders_count_only_with_problems() {
        let folder = |outcome| FileReport {
            path: PathBuf::from("photos"),
            format: Some(FormatId::Folder),
            findings: Vec::new(),
            outcome,
            output: None,
            checksums: None,
        };
        let report = RunReport {
            files: vec![
                folder(Outcome::Cleaned),
                folder(Outcome::Removed),
                folder(Outcome::RemainingMetadata),
                folder(Outcome::Failed(Failure {
                    kind: FailureKind::Io,
                    detail: String::new(),
                })),
            ],
        };
        let summary = report.summary();
        assert_eq!(summary.remaining_metadata, 1);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.total(), 2);
    }

    #[test]
    fn finding_serializes_to_stable_json() {
        let finding = Finding::new(
            FindingKind::GpsLocation,
            Location::new(["EXIF", "GPS"]),
            Disposition::Remove,
        )
        .with_value(Value::Coordinates {
            latitude: 40.5,
            longitude: -74.25,
        });
        let json = serde_json::to_value(&finding).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "kind": "gps_location",
                "risk": "high",
                "location": ["EXIF", "GPS"],
                "value": {
                    "type": "coordinates",
                    "value": { "latitude": 40.5, "longitude": -74.25 }
                },
                "disposition": { "action": "remove" }
            })
        );
    }
}
