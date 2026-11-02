//! File formats: detection by content, and the interface every format implements.

mod bytes;
mod exif;
mod icc;
mod iptc;
mod jpeg;
mod png;
mod xmp;

use std::fmt;
use std::io::{self, Read, Seek, Write};

use crate::limits::{MAX_IMAGE_BYTES, MAX_INFLATED_BYTES};
use crate::model::{Disposition, Finding, FindingKind, FormatId, KeepReason, Location, Value};
use crate::options::Options;

pub trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

pub trait FormatHandler: Send + Sync {
    fn inspect(
        &self,
        input: &mut dyn ReadSeek,
        options: &Options,
    ) -> Result<Vec<Finding>, FormatError>;

    /// Writes the cleaned file and returns the same findings `inspect` would,
    /// so the report says exactly what was removed and what was kept.
    fn clean(
        &self,
        input: &mut dyn ReadSeek,
        output: &mut dyn Write,
        options: &Options,
    ) -> Result<Vec<Finding>, FormatError>;
}

/// How many leading bytes [`sniff`] needs.
pub const SNIFF_LEN: usize = 12;

/// Detects the format from the file's first bytes. The extension is never trusted.
pub fn sniff(head: &[u8]) -> Option<FormatId> {
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(FormatId::Jpeg)
    } else if head.starts_with(png::SIGNATURE) {
        Some(FormatId::Png)
    } else if head.get(0..4) == Some(b"RIFF") && head.get(8..12) == Some(b"WEBP") {
        Some(FormatId::Webp)
    } else {
        None
    }
}

/// `None` for formats that are detected but not supported yet.
pub fn handler(format: FormatId) -> Option<&'static dyn FormatHandler> {
    match format {
        FormatId::Jpeg => Some(&jpeg::Jpeg),
        FormatId::Png => Some(&png::Png),
        FormatId::Webp => None,
    }
}

/// Lower-case extensions that are normal for a format.
pub fn extensions(format: FormatId) -> &'static [&'static str] {
    match format {
        FormatId::Jpeg => &["jpg", "jpeg", "jpe", "jfif"],
        FormatId::Png => &["png"],
        FormatId::Webp => &["webp"],
    }
}

/// Detects and inspects a file held in memory.
pub fn inspect_bytes(
    data: &[u8],
    options: &Options,
) -> Result<(Option<FormatId>, Vec<Finding>), FormatError> {
    let Some(format) = sniff(data) else {
        return Ok((None, Vec::new()));
    };
    let findings = match handler(format) {
        Some(handler) => handler.inspect(&mut io::Cursor::new(data), options)?,
        None => Vec::new(),
    };
    Ok((Some(format), findings))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cleaned {
    pub data: Vec<u8>,
    pub findings: Vec<Finding>,
}

/// Detects and cleans a file held in memory. `None` for unsupported formats.
pub fn clean_bytes(data: &[u8], options: &Options) -> Result<Option<Cleaned>, FormatError> {
    let Some(handler) = sniff(data).and_then(handler) else {
        return Ok(None);
    };
    let mut output = Vec::new();
    let findings = handler.clean(&mut io::Cursor::new(data), &mut output, options)?;
    Ok(Some(Cleaned {
        data: output,
        findings,
    }))
}

#[derive(Debug)]
pub enum FormatError {
    Io(io::Error),
    Malformed(String),
    TooLarge { limit: u64 },
}

impl FormatError {
    pub(crate) fn malformed(detail: impl Into<String>) -> Self {
        Self::Malformed(detail.into())
    }
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::Malformed(detail) => write!(f, "malformed file: {detail}"),
            Self::TooLarge { limit } => write!(f, "larger than the {limit}-byte limit"),
        }
    }
}

impl std::error::Error for FormatError {}

impl From<io::Error> for FormatError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// For blocks that cleaning keeps or rebuilds, applies the user's choices.
fn apply_options(findings: &mut [Finding], options: &Options) {
    for finding in findings {
        match (finding.kind, finding.disposition) {
            (FindingKind::Orientation, Disposition::Keep(_)) if options.strip_orientation => {
                finding.disposition = Disposition::Remove;
            }
            (FindingKind::ColorProfile, Disposition::Remove) if options.keep_color_profile => {
                finding.disposition = Disposition::Keep(KeepReason::UserRequested);
            }
            _ => {}
        }
    }
}

/// For blocks that cleaning drops whole: nothing inside them survives.
fn drop_all(findings: &mut [Finding]) {
    for finding in findings {
        if let Disposition::Keep(_) = finding.disposition {
            finding.disposition = Disposition::Remove;
        }
    }
}

/// Whether the finding at `location` says the block is kept.
fn is_kept(findings: &[Finding], kind: FindingKind, location: &Location) -> bool {
    findings.iter().any(|f| {
        f.kind == kind && &f.location == location && matches!(f.disposition, Disposition::Keep(_))
    })
}

/// The orientation to rebuild, if the finding at `location` says it's kept.
fn kept_orientation(findings: &[Finding], location: &Location) -> Option<u16> {
    findings
        .iter()
        .find_map(|f| match (&f.value, f.disposition) {
            (Some(Value::Integer(n)), Disposition::Keep(_))
                if f.kind == FindingKind::Orientation && &f.location == location =>
            {
                u16::try_from(*n).ok()
            }
            _ => None,
        })
}

fn read_limited(input: &mut dyn ReadSeek, limit: u64) -> Result<Vec<u8>, FormatError> {
    let mut data = Vec::new();
    input.take(limit.saturating_add(1)).read_to_end(&mut data)?;
    if data.len() as u64 > limit {
        return Err(FormatError::TooLarge { limit });
    }
    Ok(data)
}

fn read_image(input: &mut dyn ReadSeek) -> Result<Vec<u8>, FormatError> {
    read_limited(input, MAX_IMAGE_BYTES)
}

fn inflate(compressed: &[u8]) -> Result<Vec<u8>, FormatError> {
    let mut decoder = flate2::read::ZlibDecoder::new(compressed);
    let mut data = Vec::new();
    decoder
        .by_ref()
        .take(MAX_INFLATED_BYTES.saturating_add(1))
        .read_to_end(&mut data)
        .map_err(|e| FormatError::malformed(format!("invalid compressed data: {e}")))?;
    if data.len() as u64 > MAX_INFLATED_BYTES {
        return Err(FormatError::TooLarge {
            limit: MAX_INFLATED_BYTES,
        });
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_by_content() {
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(FormatId::Jpeg));
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n\0\0\0\x0d"), Some(FormatId::Png));
        assert_eq!(sniff(b"RIFF\x10\0\0\0WEBPVP8 "), Some(FormatId::Webp));
        assert_eq!(sniff(b"RIFF\x10\0\0\0WAVEfmt "), None);
        assert_eq!(sniff(b"hello"), None);
        assert_eq!(sniff(&[]), None);
    }

    #[test]
    fn refuses_input_over_the_limit() {
        let mut input = io::Cursor::new(vec![0u8; 11]);
        assert!(matches!(
            read_limited(&mut input, 10),
            Err(FormatError::TooLarge { limit: 10 })
        ));
        let mut input = io::Cursor::new(vec![0u8; 10]);
        assert_eq!(read_limited(&mut input, 10).unwrap().len(), 10);
    }

    #[test]
    fn inflate_stops_at_the_limit() {
        use flate2::{Compression, write::ZlibEncoder};
        use std::io::Write;

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
        let bomb = vec![0u8; (MAX_INFLATED_BYTES + 1) as usize];
        encoder.write_all(&bomb).unwrap();
        let compressed = encoder.finish().unwrap();
        assert!(compressed.len() < 100_000);
        assert!(matches!(
            inflate(&compressed),
            Err(FormatError::TooLarge { .. })
        ));
    }
}
