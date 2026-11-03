//! File formats: detection by content, and the interface every format implements.

mod archive;
mod bytes;
mod exif;
mod gzip;
mod icc;
mod iptc;
mod jpeg;
mod office;
mod pdf;
mod png;
mod tar_archive;
mod unpack;
mod webp;
mod xmp;
mod zip_archive;

use std::cell::Cell;
use std::fmt;
use std::io::{self, Read, Seek, SeekFrom, Write};

use crate::limits::{MAX_EMBEDDING_DEPTH, MAX_IMAGE_BYTES, MAX_INFLATED_BYTES};
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

const ZIP_SIGNATURES: &[&[u8]] = &[b"PK\x03\x04", b"PK\x05\x06"];
const COMPOUND_SIGNATURE: &[u8] = &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

/// Detects the format from the file's first bytes. The extension is never
/// trusted. ZIP files are reported as [`FormatId::Zip`]; [`detect`] also
/// tells Office files apart.
pub fn sniff(head: &[u8]) -> Option<FormatId> {
    if ZIP_SIGNATURES.iter().any(|s| head.starts_with(s)) {
        Some(FormatId::Zip)
    } else if head.starts_with(COMPOUND_SIGNATURE) {
        Some(FormatId::Compound)
    } else if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(FormatId::Jpeg)
    } else if head.starts_with(png::SIGNATURE) {
        Some(FormatId::Png)
    } else if head.get(0..4) == Some(b"RIFF") && head.get(8..12) == Some(b"WEBP") {
        Some(FormatId::Webp)
    } else if head.starts_with(b"%PDF-") {
        Some(FormatId::Pdf)
    } else if head.starts_with(&[0x1F, 0x8B, 0x08]) {
        Some(FormatId::Gzip)
    } else {
        None
    }
}

/// Like [`sniff`], but looks inside ZIP files to recognize Word, Excel and
/// PowerPoint documents (and apps and e-books, which aren't archives), and
/// further into the file for TAR archives. Leaves `input` at the start.
pub fn detect(head: &[u8], input: &mut dyn ReadSeek) -> Result<Option<FormatId>, FormatError> {
    let format = match sniff(head) {
        Some(FormatId::Zip) => match office::identify(input)? {
            FormatId::Zip if zip_archive::is_package(input)? => None,
            format => Some(format),
        },
        Some(other) => Some(other),
        None => {
            let mut block = Vec::with_capacity(512);
            (&mut *input).take(512).read_to_end(&mut block)?;
            tar_archive::is_tar(&block).then_some(FormatId::Tar)
        }
    };
    input.seek(SeekFrom::Start(0))?;
    Ok(format)
}

/// `None` for formats that are detected but not supported yet.
pub fn handler(format: FormatId) -> Option<&'static dyn FormatHandler> {
    match format {
        FormatId::Jpeg => Some(&jpeg::Jpeg),
        FormatId::Png => Some(&png::Png),
        FormatId::Webp => Some(&webp::Webp),
        FormatId::Docx | FormatId::Xlsx | FormatId::Pptx => Some(&office::Office),
        FormatId::Pdf => Some(&pdf::Pdf),
        FormatId::Zip => Some(&zip_archive::Zip),
        FormatId::Tar => Some(&tar_archive::Tar),
        FormatId::Gzip => Some(&gzip::Gzip),
        FormatId::Compound | FormatId::Folder => None,
    }
}

/// Lower-case extensions that are normal for a format.
pub fn extensions(format: FormatId) -> &'static [&'static str] {
    match format {
        FormatId::Jpeg => &["jpg", "jpeg", "jpe", "jfif"],
        FormatId::Png => &["png"],
        FormatId::Webp => &["webp"],
        FormatId::Docx => &["docx", "docm", "dotx", "dotm"],
        FormatId::Xlsx => &["xlsx", "xlsm", "xltx", "xltm", "xlam"],
        FormatId::Pptx => &["pptx", "pptm", "potx", "potm", "ppsx", "ppsm", "ppam"],
        FormatId::Pdf => &["pdf"],
        FormatId::Zip => &["zip"],
        FormatId::Tar => &["tar"],
        FormatId::Gzip => &["gz", "tgz"],
        // Password-protected Office files keep their usual extension.
        FormatId::Compound => &[
            "doc", "dot", "xls", "xlt", "ppt", "pot", "pps", "docx", "docm", "xlsx", "xlsm",
            "pptx", "pptm",
        ],
        FormatId::Folder => &[],
    }
}

/// Detects and inspects a file held in memory.
pub fn inspect_bytes(
    data: &[u8],
    options: &Options,
) -> Result<(Option<FormatId>, Vec<Finding>), FormatError> {
    let Some(format) = detect(data, &mut io::Cursor::new(data))? else {
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
    let Some(handler) = detect(data, &mut io::Cursor::new(data))?.and_then(handler) else {
        return Ok(None);
    };
    let mut output = Vec::new();
    let findings = handler.clean(&mut io::Cursor::new(data), &mut output, options)?;
    Ok(Some(Cleaned {
        data: output,
        findings,
    }))
}

/// Cleans a file found inside another one. `None` when MetaScrub can't
/// read it, including when it's encrypted: the outer file is still cleaned.
fn clean_embedded(data: &[u8], options: &Options) -> Result<Option<Cleaned>, FormatError> {
    match clean_bytes(data, options) {
        Err(FormatError::Encrypted) => Ok(None),
        other => other,
    }
}

thread_local! {
    static DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// How deep the file being read is embedded in others (a chart's workbook
/// in a Word document, an attachment in a PDF). Each file is read on one
/// thread from start to end, so a thread-local count works.
struct Depth;

impl Depth {
    fn enter() -> Result<Self, FormatError> {
        let depth = DEPTH.with(|d| {
            d.set(d.get() + 1);
            d.get()
        });
        let guard = Self;
        if depth > MAX_EMBEDDING_DEPTH {
            return Err(FormatError::TooDeep {
                limit: MAX_EMBEDDING_DEPTH,
            });
        }
        Ok(guard)
    }
}

impl Drop for Depth {
    fn drop(&mut self) {
        DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

#[derive(Debug)]
pub enum FormatError {
    Io(io::Error),
    Malformed(String),
    /// A feature that can't be handled safely, so the file is left alone.
    Unsupported(String),
    TooLarge {
        limit: u64,
    },
    /// Files embedded in files, more levels deep than the limit allows.
    TooDeep {
        limit: usize,
    },
    /// MetaScrub never asks for passwords, so encrypted files are skipped.
    Encrypted,
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
            Self::Unsupported(detail) => write!(f, "unsupported feature: {detail}"),
            Self::TooLarge { limit } => write!(f, "larger than the {limit}-byte limit"),
            Self::TooDeep { limit } => write!(f, "files nested more than {limit} levels deep"),
            Self::Encrypted => f.write_str("encrypted"),
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

pub(crate) fn read_limited(input: &mut dyn Read, limit: u64) -> Result<Vec<u8>, FormatError> {
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
        assert_eq!(sniff(b"PK\x03\x04\x14\0"), Some(FormatId::Zip));
        assert_eq!(sniff(COMPOUND_SIGNATURE), Some(FormatId::Compound));
        assert_eq!(sniff(b"%PDF-1.7\n%"), Some(FormatId::Pdf));
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
