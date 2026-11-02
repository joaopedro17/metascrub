//! JPEG: a sequence of marker segments, compressed scan data, and an
//! end-of-image marker. Anything after that marker is not part of the image.

use super::bytes::Cursor;
use super::{FormatError, FormatHandler, ReadSeek, exif, icc, iptc, read_image, xmp};
use crate::model::{Disposition, Finding, FindingKind, Location, Value};

const SOI: u8 = 0xD8;
const EOI: u8 = 0xD9;
const SOS: u8 = 0xDA;
const COM: u8 = 0xFE;
const APP0: u8 = 0xE0;
const APP1: u8 = 0xE1;
const APP2: u8 = 0xE2;
const APP13: u8 = 0xED;
const APP14: u8 = 0xEE;
const APP15: u8 = 0xEF;

const JFIF: &[u8] = b"JFIF\0";
const JFXX: &[u8] = b"JFXX\0";
const EXIF: &[u8] = b"Exif\0\0";
const XMP: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
const XMP_EXTENSION: &[u8] = b"http://ns.adobe.com/xmp/extension/\0";
const ICC: &[u8] = b"ICC_PROFILE\0";
const MPF: &[u8] = b"MPF\0";
const PHOTOSHOP: &[u8] = b"Photoshop 3.0\0";
const ADOBE: &[u8] = b"Adobe";

#[derive(Debug)]
pub(super) struct Jpeg;

impl FormatHandler for Jpeg {
    fn inspect(&self, input: &mut dyn ReadSeek) -> Result<Vec<Finding>, FormatError> {
        let data = read_image(input)?;
        let parsed = parse(&data)?;
        Ok(inspect_segments(&parsed))
    }
}

#[derive(Debug)]
pub(super) struct Segment<'a> {
    pub(super) marker: u8,
    /// Payload after the 2-byte length.
    pub(super) data: &'a [u8],
}

#[derive(Debug)]
pub(super) struct Parsed<'a> {
    pub(super) segments: Vec<Segment<'a>>,
    pub(super) trailing: &'a [u8],
}

pub(super) fn parse(data: &[u8]) -> Result<Parsed<'_>, FormatError> {
    let mut cursor = Cursor::new(data);
    if cursor.array::<2>()? != [0xFF, SOI] {
        return Err(FormatError::malformed("missing JPEG start of image"));
    }
    let mut segments = Vec::new();
    loop {
        if cursor.u8()? != 0xFF {
            return Err(FormatError::malformed("expected a JPEG marker"));
        }
        // Any number of 0xFF fill bytes may precede a marker.
        let mut marker = cursor.u8()?;
        while marker == 0xFF {
            marker = cursor.u8()?;
        }
        match marker {
            EOI => {
                return Ok(Parsed {
                    segments,
                    trailing: cursor.remaining(),
                });
            }
            // Markers without a payload.
            SOI | 0x01 | 0xD0..=0xD7 => {}
            _ => {
                let length = usize::from(cursor.u16_be()?);
                let payload = length
                    .checked_sub(2)
                    .ok_or_else(|| FormatError::malformed("invalid JPEG segment length"))?;
                segments.push(Segment {
                    marker,
                    data: cursor.take(payload)?,
                });
                if marker == SOS {
                    skip_scan_data(&mut cursor)?;
                }
            }
        }
    }
}

/// Compressed data ends at the first 0xFF that isn't byte stuffing (`FF 00`)
/// or a restart marker (`FF D0`–`FF D7`).
fn skip_scan_data(cursor: &mut Cursor<'_>) -> Result<(), FormatError> {
    let rest = cursor.remaining();
    let end = rest
        .windows(2)
        .position(
            |pair| matches!(pair, [0xFF, next] if *next != 0x00 && !(0xD0..=0xD7).contains(next)),
        )
        .ok_or_else(|| FormatError::malformed("JPEG image data is truncated"))?;
    cursor.skip(end)
}

fn inspect_segments(parsed: &Parsed<'_>) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut icc_chunks: Vec<(u8, &[u8])> = Vec::new();
    let mut extended_xmp: Vec<(u32, &[u8])> = Vec::new();
    let mut has_mpf = false;

    for segment in &parsed.segments {
        let data = segment.data;
        let after = |prefix: &[u8]| data.strip_prefix(prefix);
        match segment.marker {
            APP0 if data.starts_with(JFIF) => {}
            APP0 if data.starts_with(JFXX) => findings.push(
                Finding::new(
                    FindingKind::Thumbnail,
                    Location::new(["JFIF extension"]),
                    Disposition::Remove,
                )
                .with_value(Value::Bytes(data.len() as u64)),
            ),
            APP1 if data.starts_with(EXIF) => {
                findings.extend(exif::inspect(
                    after(EXIF).unwrap_or_default(),
                    &Location::new(["EXIF"]),
                ));
            }
            APP1 if data.starts_with(XMP) => {
                findings.extend(xmp::inspect(
                    after(XMP).unwrap_or_default(),
                    &Location::new(["XMP"]),
                ));
            }
            APP1 if data.starts_with(XMP_EXTENSION) => {
                // 32-byte GUID, 4-byte full length, 4-byte offset, then the chunk.
                let mut chunk = Cursor::new(after(XMP_EXTENSION).unwrap_or_default());
                if let (Ok(_guid), Ok(_len), Ok(offset)) =
                    (chunk.skip(32), chunk.u32_be(), chunk.u32_be())
                {
                    extended_xmp.push((offset, chunk.remaining()));
                }
            }
            APP2 if data.starts_with(ICC) => {
                // Sequence number (1-based) and chunk count, then the chunk.
                let mut chunk = Cursor::new(after(ICC).unwrap_or_default());
                if let (Ok(sequence), Ok(_count)) = (chunk.u8(), chunk.u8()) {
                    icc_chunks.push((sequence, chunk.remaining()));
                }
            }
            APP2 if data.starts_with(MPF) => has_mpf = true,
            APP13 if data.starts_with(PHOTOSHOP) => {
                findings.extend(iptc::inspect_photoshop(
                    after(PHOTOSHOP).unwrap_or_default(),
                    &Location::new(["Photoshop"]),
                ));
            }
            // Needed to decode CMYK and YCCK images correctly.
            APP14 if data.starts_with(ADOBE) => {}
            APP0..=APP15 => findings.push(
                Finding::new(
                    FindingKind::UnknownMetadata,
                    Location::new([format!("APP{}", segment.marker - APP0)]),
                    Disposition::Remove,
                )
                .with_value(Value::Bytes(data.len() as u64)),
            ),
            COM => findings.push(
                Finding::new(
                    FindingKind::Comment,
                    Location::new(["Comment"]),
                    Disposition::Remove,
                )
                .with_value(Value::Text(String::from_utf8_lossy(data).trim().to_owned())),
            ),
            // Tables, frame and scan headers: needed to decode the image.
            _ => {}
        }
    }

    if !extended_xmp.is_empty() {
        extended_xmp.sort_by_key(|(offset, _)| *offset);
        let packet: Vec<u8> = extended_xmp
            .into_iter()
            .flat_map(|(_, c)| c.iter().copied())
            .collect();
        findings.extend(xmp::inspect(&packet, &Location::new(["XMP", "Extended"])));
    }
    if !icc_chunks.is_empty() {
        icc_chunks.sort_by_key(|(sequence, _)| *sequence);
        let profile: Vec<u8> = icc_chunks
            .into_iter()
            .flat_map(|(_, c)| c.iter().copied())
            .collect();
        findings.push(icc::inspect(&profile, Location::new(["ICC"])));
    }

    let trailing = parsed.trailing;
    let is_padding = trailing.iter().all(|&b| b == 0x00 || b == 0xFF);
    if has_mpf {
        // The extra images of a Multi-Picture file follow the first image.
        let finding = Finding::new(
            FindingKind::HiddenImage,
            Location::new(["MPF"]),
            Disposition::Remove,
        );
        findings.push(if trailing.is_empty() {
            finding
        } else {
            finding.with_value(Value::Bytes(trailing.len() as u64))
        });
    } else if !is_padding {
        findings.push(
            Finding::new(
                FindingKind::TrailingData,
                Location::new(["After end of image"]),
                Disposition::Remove,
            )
            .with_value(Value::Bytes(trailing.len() as u64)),
        );
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(marker: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0xFF, marker];
        out.extend(((payload.len() + 2) as u16).to_be_bytes());
        out.extend(payload);
        out
    }

    /// SOI, the given segments, a scan with stuffed bytes and a restart marker, EOI.
    fn jpeg(segments: &[Vec<u8>], trailing: &[u8]) -> Vec<u8> {
        let mut out = vec![0xFF, SOI];
        for s in segments {
            out.extend(s);
        }
        out.extend(segment(SOS, &[1, 1, 0, 0, 63, 0]));
        out.extend([0x12, 0xFF, 0x00, 0x34, 0xFF, 0xD0, 0x56]);
        out.extend([0xFF, EOI]);
        out.extend(trailing);
        out
    }

    fn kinds(data: &[u8]) -> Vec<FindingKind> {
        let parsed = parse(data).unwrap();
        inspect_segments(&parsed).iter().map(|f| f.kind).collect()
    }

    #[test]
    fn scan_data_with_stuffing_and_restarts_is_skipped() {
        let data = jpeg(&[segment(APP0, b"JFIF\0\x01\x02")], b"");
        let parsed = parse(&data).unwrap();
        assert_eq!(parsed.segments.len(), 2);
        assert!(parsed.trailing.is_empty());
    }

    #[test]
    fn reports_comments_and_unknown_app_segments() {
        let data = jpeg(
            &[segment(COM, b"made by Jane"), segment(0xEC, b"Ducky")],
            b"",
        );
        assert_eq!(
            kinds(&data),
            [FindingKind::Comment, FindingKind::UnknownMetadata]
        );
    }

    #[test]
    fn keeps_jfif_and_adobe_segments_silently() {
        let data = jpeg(
            &[
                segment(APP0, b"JFIF\0\x01\x02"),
                segment(APP14, b"Adobe\0d"),
            ],
            b"",
        );
        assert!(kinds(&data).is_empty());
    }

    #[test]
    fn reports_data_after_end_of_image() {
        let motion_photo = b"\0\0\0\x18ftypmp42 video data";
        let data = jpeg(&[], motion_photo);
        assert_eq!(kinds(&data), [FindingKind::TrailingData]);
    }

    #[test]
    fn padding_after_end_of_image_is_not_a_finding() {
        assert!(kinds(&jpeg(&[], &[0, 0, 0, 0])).is_empty());
    }

    #[test]
    fn multi_picture_images_are_hidden_images() {
        let second_image = [0xFF, SOI, 0xFF, EOI];
        let data = jpeg(&[segment(APP2, b"MPF\0MM\0\x2a")], &second_image);
        assert_eq!(kinds(&data), [FindingKind::HiddenImage]);
    }

    #[test]
    fn reassembles_icc_profiles_split_across_segments() {
        let profile = icc::tests::profile(
            "sRGB",
            [
                [0.4361, 0.2225, 0.0139],
                [0.3851, 0.7169, 0.0971],
                [0.1431, 0.0606, 0.7141],
            ],
            None,
        );
        let (first, second) = profile.split_at(100);
        let chunk = |seq: u8, data: &[u8]| {
            let mut payload = ICC.to_vec();
            payload.extend([seq, 2]);
            payload.extend(data);
            segment(APP2, &payload)
        };
        // Out of order on purpose.
        let data = jpeg(&[chunk(2, second), chunk(1, first)], b"");
        let findings = inspect_segments(&parse(&data).unwrap());
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::ColorProfile);
        assert_eq!(findings[0].value, Some(Value::Text("sRGB".into())));
    }

    #[test]
    fn truncated_files_are_errors_not_panics() {
        let data = jpeg(&[segment(COM, b"hello")], b"");
        for len in 0..data.len() - 2 {
            assert!(parse(&data[..len]).is_err(), "length {len}");
        }
    }

    #[test]
    fn segment_length_below_two_is_an_error() {
        assert!(parse(&[0xFF, SOI, 0xFF, COM, 0x00, 0x01]).is_err());
    }
}
