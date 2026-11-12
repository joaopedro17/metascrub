//! JPEG and JPEG XL streams stored as TIFF strips or tiles (DNG previews
//! and raw data). A JPEG stream can carry its own EXIF, XMP or comments
//! before the picture; their contents are zeroed at the same size, so no
//! offset in the TIFF changes.

use std::io::SeekFrom;
use std::ops::Range;

use crate::format::{FormatError, ReadSeek, drop_all, exif, xmp};
use crate::model::{Disposition, Finding, FindingKind, Location, Value};

/// Segments larger than this are reported by size only.
const MAX_READ: u64 = 1024 * 1024;
/// More segments than this before the picture isn't a real stream.
const MAX_SEGMENTS: usize = 1000;
const XMP_ID: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";

fn read_pair(input: &mut dyn ReadSeek, at: u64) -> Result<[u8; 2], FormatError> {
    input.seek(SeekFrom::Start(at))?;
    let mut pair = [0u8; 2];
    input.read_exact(&mut pair)?;
    Ok(pair)
}

fn read(input: &mut dyn ReadSeek, at: u64, len: u64) -> Result<Vec<u8>, FormatError> {
    input.seek(SeekFrom::Start(at))?;
    let mut data = vec![0u8; len as usize];
    input.read_exact(&mut data)?;
    Ok(data)
}

/// Walks the segments of the JPEG stream at `range` up to its picture data.
/// Returns the findings and the byte ranges to zero.
pub(super) fn jpeg(
    input: &mut dyn ReadSeek,
    range: Range<u64>,
    location: &Location,
) -> Result<(Vec<Finding>, Vec<Range<u64>>), FormatError> {
    let cut = || FormatError::malformed("a JPEG strip cut short");
    let mut findings = Vec::new();
    let mut zeros = Vec::new();
    if range.end - range.start < 2 || read_pair(input, range.start)? != [0xFF, 0xD8] {
        return Err(FormatError::malformed("a JPEG strip without its start"));
    }
    let mut at = range.start + 2;
    for _ in 0..MAX_SEGMENTS {
        if at + 2 > range.end {
            return Err(cut());
        }
        let [0xFF, kind] = read_pair(input, at)? else {
            return Err(FormatError::malformed("a JPEG strip with a broken segment"));
        };
        match kind {
            // Fill byte before a marker.
            0xFF => {
                at += 1;
                continue;
            }
            // The picture starts, or the stream ends (JPEG tables).
            0xDA | 0xD9 => return Ok((findings, zeros)),
            // Markers without a length.
            0x01 | 0xD0..=0xD7 => {
                at += 2;
                continue;
            }
            _ => {}
        }
        if at + 4 > range.end {
            return Err(cut());
        }
        let length = u64::from(u16::from_be_bytes(read_pair(input, at + 2)?));
        if length < 2 || at + 2 + length > range.end {
            return Err(cut());
        }
        let payload = at + 4..at + 2 + length;
        if matches!(kind, 0xE0..=0xEF | 0xFE) {
            segment(input, kind, payload, location, &mut findings, &mut zeros)?;
        }
        at += 2 + length;
    }
    Err(FormatError::malformed(
        "a JPEG strip with too many segments",
    ))
}

/// Reports an application segment or comment, and zeros it unless the
/// picture needs it (JFIF, Adobe's color transform) or it's already blank.
fn segment(
    input: &mut dyn ReadSeek,
    kind: u8,
    payload: Range<u64>,
    location: &Location,
    findings: &mut Vec<Finding>,
    zeros: &mut Vec<Range<u64>>,
) -> Result<(), FormatError> {
    let len = payload.end - payload.start;
    if len == 0 {
        return Ok(());
    }
    let data = read(input, payload.start, len.min(MAX_READ))?;
    let needed = match kind {
        0xE0 => data.starts_with(b"JFIF\0"),
        0xEE => data.starts_with(b"Adobe"),
        _ => false,
    };
    if needed || (len <= MAX_READ && data.iter().all(|&b| b == 0)) {
        return Ok(());
    }
    let unknown = || {
        vec![
            Finding::new(
                FindingKind::UnknownMetadata,
                location.child("JPEG"),
                Disposition::Remove,
            )
            .with_value(Value::Bytes(len)),
        ]
    };
    let mut found = if len > MAX_READ {
        unknown()
    } else if kind == 0xE1 && data.starts_with(b"Exif\0\0") {
        exif::inspect(data.get(6..).unwrap_or_default(), &location.child("EXIF"))
    } else if kind == 0xE1 && data.starts_with(XMP_ID) {
        xmp::inspect(
            data.get(XMP_ID.len()..).unwrap_or_default(),
            &location.child("XMP"),
        )
    } else if kind == 0xFE {
        let text = String::from_utf8_lossy(&data)
            .trim_matches(|c: char| c == '\0' || c.is_whitespace())
            .to_owned();
        vec![
            Finding::new(
                FindingKind::Comment,
                location.child("JPEG comment"),
                Disposition::Remove,
            )
            .with_value(Value::Text(text)),
        ]
    } else {
        unknown()
    };
    drop_all(&mut found);
    for finding in found {
        if !findings.contains(&finding) {
            findings.push(finding);
        }
    }
    zeros.push(payload);
    Ok(())
}

/// A JPEG XL strip must be a bare codestream: the container form can hold
/// EXIF and XMP boxes.
pub(super) fn is_bare_jpeg_xl(
    input: &mut dyn ReadSeek,
    at: u64,
    len: u64,
) -> Result<bool, FormatError> {
    Ok(len >= 2 && read_pair(input, at)? == [0xFF, 0x0A])
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn stream(segments: &[(u8, &[u8])]) -> Vec<u8> {
        let mut out = vec![0xFF, 0xD8];
        for (kind, payload) in segments {
            out.extend([0xFF, *kind]);
            out.extend(((payload.len() + 2) as u16).to_be_bytes());
            out.extend_from_slice(payload);
        }
        out.extend([0xFF, 0xDA, 0, 2, 1, 2, 3]);
        out
    }

    #[test]
    fn comments_and_exif_are_zeroed_and_jfif_kept() {
        let data = stream(&[
            (0xE0, b"JFIF\0\x01\x02"),
            (0xFE, b"Jane's camera"),
            (0xDB, &[0; 5]),
        ]);
        let len = data.len() as u64;
        let (findings, zeros) =
            jpeg(&mut Cursor::new(&data), 0..len, &Location::new(["DNG"])).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::Comment);
        assert_eq!(zeros, vec![Range { start: 17, end: 30 }]);
    }

    #[test]
    fn broken_streams_are_errors() {
        let data = stream(&[(0xE1, b"Exif\0\0")]);
        for end in [1, 5, 9] {
            assert!(
                jpeg(&mut Cursor::new(&data), 0..end, &Location::new(["DNG"])).is_err(),
                "{end}"
            );
        }
        assert!(jpeg(&mut Cursor::new(b"\0\0\0\0"), 0..4, &Location::new(["DNG"])).is_err());
    }
}
