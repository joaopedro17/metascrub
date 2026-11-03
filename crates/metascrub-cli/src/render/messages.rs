//! Every sentence a user reads. The core only returns codes.

use std::path::Path;

use metascrub_core::model::{
    Decision, Disposition, FailureKind, Finding, FindingKind, FormatId, KeepReason, SkipReason,
    Value,
};

const MAX_VALUE_CHARS: usize = 80;

pub fn finding(kind: FindingKind) -> &'static str {
    use FindingKind::*;
    match kind {
        GpsLocation => "Location where it was created",
        PersonName => "A person's name (author, artist or owner)",
        DeviceSerial => "Serial number of the camera or device",
        FilePath => "A file path from the computer that made it",
        DownloadUrl => "The web address it was downloaded from",
        Thumbnail => "A small preview image (may show the original before cropping or edits)",
        HiddenImage => "Extra hidden images (such as depth maps)",
        TrailingData => "Hidden data added after the file's content (such as a Motion Photo video)",
        TrackedChanges => "Tracked changes (may contain deleted text)",
        Comment => "A comment",
        EmbeddedFile => "Another file embedded inside it",
        Macros => "Macros or scripts (programs that can run when the file is opened)",
        EarlierVersions => "Earlier versions of the document (may include removed text)",
        UnreadFile => {
            "A file inside that MetaScrub can't read, kept as it is (it may still contain \
             hidden information)"
        }
        SymbolicLink => "A shortcut (symbolic link) inside, kept and not followed",
        FormData => "Values typed into form fields",
        TextTrack => "Subtitles or chapter titles, which MetaScrub doesn't check",
        HiddenContent => "Hidden content (hidden text, sheets or slides, or speaker notes)",
        JunkFile => {
            "A junk file left by the operating system (may hold thumbnails, file names \
             or download addresses)"
        }
        Description => "A title, caption or keywords",
        Organization => "A company or organization name",
        CustomProperty => "A custom property (extra details added by a program or person)",
        FileAttribute => "Extra data the file system stores with the file",
        DeviceModel => "Make and model of a device (camera, phone, printer)",
        Software => "Software or app used",
        Timestamp => "Date and time",
        FileName => "The file name (shows when it was made, or which phone or app made it)",
        DocumentId => "A unique ID that can link copies of this file",
        UnknownMetadata => "Metadata MetaScrub doesn't recognize",
        ColorProfile => "Color profile",
        Orientation => "Rotation setting",
        TechnicalInfo => "Technical details (encoding, settings, statistics)",
        ExtensionMismatch => "The file name's extension doesn't match the real file type",
        _ => "Other metadata",
    }
}

/// When the action happens, relative to the command being run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tense {
    /// `inspect` and `verify`: what `clean` would do.
    Future,
    /// `clean --dry-run`.
    Conditional,
    /// `clean`.
    Past,
}

pub fn disposition(kind: FindingKind, disposition: Disposition, tense: Tense) -> &'static str {
    match (disposition, tense) {
        // Left alone only because the option that fixes them wasn't given.
        (Disposition::Warn, _) if kind == FindingKind::JunkFile => {
            "left alone: --remove-junk removes it"
        }
        (Disposition::Warn, _) if kind == FindingKind::FileName => {
            "left alone: --rename replaces it"
        }
        (Disposition::Remove, Tense::Future) => "will be removed",
        (Disposition::Remove, Tense::Conditional) => "would be removed",
        (Disposition::Remove, Tense::Past) => "removed",
        (Disposition::Normalize, Tense::Future) => "will be reset to a neutral value",
        (Disposition::Normalize, Tense::Conditional) => "would be reset to a neutral value",
        (Disposition::Normalize, Tense::Past) => "reset to a neutral value",
        (Disposition::Keep(KeepReason::RequiredToRender), _) => "kept: needed to display correctly",
        (Disposition::Keep(KeepReason::StandardColorProfile), _) => {
            "kept: a standard profile that reveals nothing"
        }
        (Disposition::Keep(KeepReason::UserRequested), _) => "kept: you asked to keep it",
        (Disposition::Keep(KeepReason::NotWritten), _) => "kept: no copy of this file is written",
        (Disposition::Keep(KeepReason::PartOfImage), _) => {
            "kept: part of the picture, shows nothing the picture doesn't"
        }
        (Disposition::Warn, _) => "can't be fixed automatically",
        (Disposition::Blocks(_), _) => "needs your decision",
    }
}

pub const COPIED: &str = "copied without cleaning: MetaScrub can't read this type of file, \
                          so it may still contain hidden information";
pub const WOULD_COPY: &str = "would be copied without cleaning: MetaScrub can't read this type \
                              of file, so it may still contain hidden information";
pub const ATTRIBUTES_HINT: &str = "With --in-place or --output, MetaScrub removes what the \
                                   file system stores with it, even though it can't read the file.";
pub const JUNK_HINT: &str = "Junk files can be removed with --remove-junk, together with \
                             --in-place or --output.";
pub const NAME_HINT: &str = "File names that show a date, phone or app can be replaced with \
                             neutral ones (like image-001.jpg) with --rename.";
pub const REMOVED: &str = "removed: a junk file left by the operating system";
pub const WOULD_REMOVE: &str = "would be removed: a junk file left by the operating system";
pub const VERIFIED: &str = "no hidden metadata left";
pub const REMAINING: &str = "still contains hidden metadata";
pub const LIMITATIONS: &str = "MetaScrub removes hidden metadata only. It can't remove what's \
                               visible in a file (faces, places, text), and it doesn't make you anonymous.";

/// What to do about a file that needs a decision.
pub fn decision(decision: Decision) -> &'static str {
    match decision {
        Decision::TrackedChanges => {
            "Tracked changes can hide deleted text. Clean again with --accept-changes \
             (keep the text as it reads now) or --reject-changes (go back to the text \
             before the changes)."
        }
    }
}

pub fn value(finding: &Finding) -> Option<String> {
    let text = match finding.value.as_ref()? {
        Value::Text(text) => text.clone(),
        Value::Integer(n) => match finding.kind {
            FindingKind::Orientation => orientation(*n).to_owned(),
            FindingKind::TechnicalInfo
            | FindingKind::UnknownMetadata
            | FindingKind::DocumentId
            | FindingKind::CustomProperty => {
                format!("{n} field{}", if *n == 1 { "" } else { "s" })
            }
            FindingKind::TrackedChanges => format!("{n} change{}", if *n == 1 { "" } else { "s" }),
            FindingKind::EarlierVersions => {
                format!("{n} earlier version{}", if *n == 1 { "" } else { "s" })
            }
            FindingKind::Macros => format!("{n} script{}", if *n == 1 { "" } else { "s" }),
            _ => n.to_string(),
        },
        Value::Coordinates {
            latitude,
            longitude,
        } => format!("{latitude:.5}, {longitude:.5}"),
        Value::Bytes(len) => size(*len),
    };
    Some(truncate(&text))
}

fn orientation(value: i64) -> &'static str {
    match value {
        1 => "upright",
        3 => "rotated 180°",
        6 => "rotated 90° clockwise",
        8 => "rotated 90° counterclockwise",
        2 | 4 | 5 | 7 => "mirrored",
        _ => "unknown",
    }
}

fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["bytes", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    match (unit, UNITS.get(unit)) {
        (0, _) => format!("{bytes} bytes"),
        (_, Some(name)) => format!("{value:.1} {name}"),
        _ => format!("{bytes} bytes"),
    }
}

/// Text from a file (values, names, paths) can contain control characters or
/// bidirectional overrides that would rewrite the terminal or disguise the
/// report. They are shown as `�` instead.
pub fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() || is_bidi_control(c) {
                char::REPLACEMENT_CHARACTER
            } else {
                c
            }
        })
        .collect()
}

fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

pub fn path(path: &Path) -> String {
    sanitize(&path.display().to_string())
}

fn truncate(text: &str) -> String {
    let single_line = sanitize(&text.split_whitespace().collect::<Vec<_>>().join(" "));
    if single_line.chars().count() <= MAX_VALUE_CHARS {
        return single_line;
    }
    let mut short: String = single_line.chars().take(MAX_VALUE_CHARS - 1).collect();
    short.push('…');
    short
}

pub fn format(format: FormatId) -> &'static str {
    match format {
        FormatId::Jpeg => "JPEG image",
        FormatId::Png => "PNG image",
        FormatId::Webp => "WebP image",
        FormatId::Pdf => "PDF document",
        FormatId::Docx => "Word document",
        FormatId::Xlsx => "Excel workbook",
        FormatId::Pptx => "PowerPoint presentation",
        FormatId::Zip => "ZIP archive",
        FormatId::Tar => "TAR archive",
        FormatId::Gzip => "gzip-compressed file",
        FormatId::Mp4 => "MP4 video",
        FormatId::Mov => "QuickTime video",
        FormatId::M4a => "MPEG-4 audio",
        FormatId::Heic => "HEIC image",
        FormatId::Avif => "AVIF image",
        FormatId::Compound => "old-format or password-protected Office file",
        FormatId::Folder => "folder",
        _ => "file",
    }
}

pub fn skip_reason(reason: SkipReason, format: Option<FormatId>) -> String {
    match (reason, format) {
        (SkipReason::UnsupportedFormat, Some(FormatId::Compound)) => {
            "old-format (.doc, .xls, .ppt) and password-protected Office files aren't supported"
                .into()
        }
        (SkipReason::UnsupportedFormat, Some(format)) => {
            format!("{} files aren't supported yet", self::format(format))
        }
        (SkipReason::UnsupportedFormat, None) => "MetaScrub can't read this type of file".into(),
        (SkipReason::NotRegularFile, _) => "not a regular file".into(),
        (SkipReason::SymbolicLink, _) => "it's a shortcut (symbolic link), not followed".into(),
        (SkipReason::AlreadyCleaned, _) => "looks already cleaned".into(),
        (SkipReason::Encrypted, _) => "it's encrypted".into(),
    }
}

pub fn failure(kind: FailureKind) -> &'static str {
    match kind {
        FailureKind::Io => "couldn't read the file",
        FailureKind::Malformed => "the file is damaged or not what it claims to be",
        FailureKind::LimitExceeded => "the file exceeds MetaScrub's safety limits",
        FailureKind::Unsupported => {
            "the file uses a feature MetaScrub can't handle safely, so it was left alone"
        }
        FailureKind::UnsafePath => {
            "it contains a file whose path leads outside it (like ../), which could \
             overwrite other files when extracted, so it was left alone"
        }
        FailureKind::Cancelled => "cancelled before it finished",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes_for_people() {
        assert_eq!(size(512), "512 bytes");
        assert_eq!(size(12_345), "12.3 KB");
        assert_eq!(size(3_400_000), "3.4 MB");
    }

    #[test]
    fn control_characters_cannot_reach_the_terminal() {
        assert_eq!(
            sanitize("\u{1b}]0;title\u{7}\u{1b}[2Jtext"),
            "\u{FFFD}]0;title\u{FFFD}\u{FFFD}[2Jtext"
        );
        assert_eq!(sanitize("photo\u{202E}gpj.exe"), "photo\u{FFFD}gpj.exe");
        assert_eq!(sanitize("café ✓"), "café ✓");
    }

    #[test]
    fn long_values_are_shortened_to_one_line() {
        assert_eq!(truncate("two\nlines"), "two lines");
        let long = "x".repeat(200);
        assert_eq!(truncate(&long).chars().count(), MAX_VALUE_CHARS);
        assert!(truncate(&long).ends_with('…'));
    }
}
