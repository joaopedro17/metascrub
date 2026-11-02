//! Every sentence a user reads. The core only returns codes.

use std::path::Path;

use metascrub_core::model::{
    Disposition, FailureKind, Finding, FindingKind, FormatId, KeepReason, SkipReason, Value,
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
        TrailingData => "Hidden data after the end of the image (such as a Motion Photo video)",
        TrackedChanges => "Tracked changes (may contain deleted text)",
        Comment => "A comment",
        EmbeddedFile => "Another file embedded inside it",
        HiddenContent => "Hidden content",
        Description => "A title, caption or keywords",
        DeviceModel => "Make and model of the camera or phone",
        Software => "Software or app used",
        Timestamp => "Date and time",
        DocumentId => "A unique ID that can link copies of this file",
        UnknownMetadata => "Metadata MetaScrub doesn't recognize",
        ColorProfile => "Color profile",
        Orientation => "Rotation setting",
        TechnicalInfo => "Technical details about the camera or encoding",
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

pub fn disposition(disposition: Disposition, tense: Tense) -> &'static str {
    match (disposition, tense) {
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
        (Disposition::Warn, _) => "can't be fixed automatically",
        (Disposition::Blocks(_), _) => "needs your decision",
    }
}

pub const COPIED: &str = "copied without cleaning: MetaScrub can't read this type of file, \
                          so it may still contain hidden information";
pub const WOULD_COPY: &str = "would be copied without cleaning: MetaScrub can't read this type \
                              of file, so it may still contain hidden information";
pub const VERIFIED: &str = "no hidden metadata left";
pub const REMAINING: &str = "still contains hidden metadata";
pub const LIMITATIONS: &str = "MetaScrub removes hidden metadata only. It can't remove what's \
                               visible in a file (faces, places, text), and it doesn't make you anonymous.";

pub fn value(finding: &Finding) -> Option<String> {
    let text = match finding.value.as_ref()? {
        Value::Text(text) => text.clone(),
        Value::Integer(n) => match finding.kind {
            FindingKind::Orientation => orientation(*n).to_owned(),
            FindingKind::TechnicalInfo | FindingKind::UnknownMetadata => {
                format!("{n} field{}", if *n == 1 { "" } else { "s" })
            }
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
        _ => "file",
    }
}

pub fn skip_reason(reason: SkipReason, format: Option<FormatId>) -> String {
    match (reason, format) {
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
