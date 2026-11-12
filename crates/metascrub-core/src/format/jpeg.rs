//! JPEG: a sequence of marker segments, compressed scan data, and an
//! end-of-image marker. Anything after that marker is not part of the image.

use std::io::{self, Write};

use super::bytes::Cursor;
use super::{
    FormatError, FormatHandler, ReadSeek, apply_options, drop_all, exif, icc, iptc, is_kept,
    kept_orientation, read_image, ultra_hdr, xmp,
};
use crate::model::{Disposition, Finding, FindingKind, KeepReason, Location, Risk, Value};
use crate::options::Options;

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
const MPF: &[u8] = ultra_hdr::MPF;
const ISO_GAIN_MAP: &[u8] = ultra_hdr::ISO;
const PHOTOSHOP: &[u8] = b"Photoshop 3.0\0";
const ADOBE: &[u8] = b"Adobe";

/// JFIF header up to and including the thumbnail size bytes.
const JFIF_HEADER_LEN: usize = 14;

#[derive(Debug)]
pub(super) struct Jpeg;

impl FormatHandler for Jpeg {
    fn inspect(
        &self,
        input: &mut dyn ReadSeek,
        options: &Options,
    ) -> Result<Vec<Finding>, FormatError> {
        let data = read_image(input)?;
        Ok(analyze(&data, options)?.findings())
    }

    fn clean(
        &self,
        input: &mut dyn ReadSeek,
        output: &mut dyn Write,
        options: &Options,
    ) -> Result<Vec<Finding>, FormatError> {
        let data = read_image(input)?;
        let analysis = analyze(&data, options)?;
        match &analysis.gain_map {
            None => {
                write_clean(
                    &analysis.parsed,
                    &analysis.primary,
                    Role::Plain,
                    None,
                    output,
                )?;
            }
            Some(gain_map) => {
                let mut gain = Vec::new();
                let packet = (!gain_map.settings.is_empty())
                    .then(|| xmp::gain_map_packet(&gain_map.settings, None));
                write_clean(
                    &gain_map.parsed,
                    &analysis.gain,
                    Role::GainMap,
                    packet.as_deref(),
                    &mut gain,
                )?;
                let packet = gain_map
                    .primary_settings
                    .as_ref()
                    .map(|settings| xmp::gain_map_packet(settings, Some(gain.len())));
                let mut primary = Vec::new();
                let mpf_at = write_clean(
                    &analysis.parsed,
                    &analysis.primary,
                    Role::Primary,
                    packet.as_deref(),
                    &mut primary,
                )?;
                if let Some(at) = mpf_at {
                    let mpf = ultra_hdr::mpf(at, primary.len(), gain.len());
                    if let Some(slot) = primary.get_mut(at..at + mpf.len()) {
                        slot.copy_from_slice(&mpf);
                    }
                }
                output.write_all(&primary)?;
                output.write_all(&gain)?;
            }
        }
        Ok(analysis.findings())
    }
}

/// What a JPEG is in the file being cleaned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Plain,
    /// The main image of an Ultra HDR photo, whose gain map is kept.
    Primary,
    /// The gain map: an image of its own, cleaned the same way, keeping only
    /// its settings.
    GainMap,
}

struct Analysis<'a> {
    parsed: Parsed<'a>,
    primary: Vec<Finding>,
    gain_map: Option<ultra_hdr::GainMap<'a>>,
    /// The gain map's findings, without the "HDR gain map" prefix.
    gain: Vec<Finding>,
    /// The kept gain map, and anything else after the main image.
    after: Vec<Finding>,
}

impl Analysis<'_> {
    fn findings(&self) -> Vec<Finding> {
        let gain = self.gain.iter().map(|f| f.clone().within("HDR gain map"));
        self.primary
            .iter()
            .cloned()
            .chain(gain)
            .chain(self.after.iter().cloned())
            .collect()
    }
}

fn analyze<'a>(data: &'a [u8], options: &Options) -> Result<Analysis<'a>, FormatError> {
    let parsed = parse(data)?;
    let Some(gain_map) = ultra_hdr::find(data, &parsed) else {
        let primary = inspect_segments(&parsed, options, Role::Plain);
        return Ok(Analysis {
            parsed,
            primary,
            gain_map: None,
            gain: Vec::new(),
            after: Vec::new(),
        });
    };
    let primary = inspect_segments(&parsed, options, Role::Primary);
    let gain = inspect_segments(&gain_map.parsed, options, Role::GainMap);
    let mut kept = Finding::new(
        FindingKind::HiddenImage,
        Location::new(["HDR gain map"]),
        Disposition::Keep(KeepReason::PartOfImage),
    )
    .with_value(Value::Bytes(gain_map.range.len() as u64));
    kept.risk = Risk::Low;
    let mut after = vec![kept];
    // Other images and data around the gain map (such as a Motion Photo video).
    let primary_end = data.len() - parsed.trailing.len();
    let rest = [
        data.get(primary_end..gain_map.range.start)
            .unwrap_or_default(),
        data.get(gain_map.range.end..).unwrap_or_default(),
    ];
    let rest_len: usize = rest.iter().map(|r| r.len()).sum();
    if rest
        .iter()
        .any(|r| r.iter().any(|&b| b != 0x00 && b != 0xFF))
    {
        after.push(
            Finding::new(
                FindingKind::HiddenImage,
                Location::new(["MPF"]),
                Disposition::Remove,
            )
            .with_value(Value::Bytes(rest_len as u64)),
        );
    }
    Ok(Analysis {
        parsed,
        primary,
        gain_map: Some(gain_map),
        gain,
        after,
    })
}

#[derive(Debug)]
pub(super) struct Segment<'a> {
    pub(super) marker: u8,
    /// Where the payload starts in the parsed data.
    pub(super) at: usize,
    /// Payload after the 2-byte length.
    pub(super) data: &'a [u8],
    /// Compressed image data following a start-of-scan segment.
    pub(super) scan: &'a [u8],
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
            // `FF 00` is an escaped data byte, never a marker. Writing it back
            // as a segment would make the next reader see image data instead.
            0x00 => return Err(FormatError::malformed("invalid JPEG marker")),
            _ => {
                let length = usize::from(cursor.u16_be()?);
                let payload = length
                    .checked_sub(2)
                    .ok_or_else(|| FormatError::malformed("invalid JPEG segment length"))?;
                let at = data.len() - cursor.remaining().len();
                let data = cursor.take(payload)?;
                let scan = if marker == SOS {
                    scan_data(&mut cursor)?
                } else {
                    &[]
                };
                segments.push(Segment {
                    marker,
                    at,
                    data,
                    scan,
                });
            }
        }
    }
}

/// Compressed data ends at the first 0xFF that isn't byte stuffing (`FF 00`)
/// or a restart marker (`FF D0`–`FF D7`).
fn scan_data<'a>(cursor: &mut Cursor<'a>) -> Result<&'a [u8], FormatError> {
    let end = cursor
        .remaining()
        .windows(2)
        .position(
            |pair| matches!(pair, [0xFF, next] if *next != 0x00 && !(0xD0..=0xD7).contains(next)),
        )
        .ok_or_else(|| FormatError::malformed("JPEG image data is truncated"))?;
    cursor.take(end)
}

fn inspect_segments(parsed: &Parsed<'_>, options: &Options, role: Role) -> Vec<Finding> {
    let ultra = role != Role::Plain;
    let skip: &'static [&'static str] = if ultra { xmp::ULTRA_HDR } else { &[] };
    let mut findings = Vec::new();
    let mut icc_chunks: Vec<(u8, &[u8])> = Vec::new();
    let mut extended_xmp: Vec<(u32, &[u8])> = Vec::new();
    let mut has_mpf = false;
    let mut seen_exif = false;

    for segment in &parsed.segments {
        let data = segment.data;
        let after = |prefix: &[u8]| data.strip_prefix(prefix).unwrap_or_default();
        match segment.marker {
            APP0 if data.starts_with(JFIF) => {
                if let [.., width, height] = data.get(..JFIF_HEADER_LEN).unwrap_or_default()
                    && *width > 0
                    && *height > 0
                {
                    findings.push(
                        Finding::new(
                            FindingKind::Thumbnail,
                            Location::new(["JFIF"]),
                            Disposition::Remove,
                        )
                        .with_value(Value::Bytes((data.len() - JFIF_HEADER_LEN) as u64)),
                    );
                }
            }
            APP0 if data.starts_with(JFXX) => findings.push(
                Finding::new(
                    FindingKind::Thumbnail,
                    Location::new(["JFIF extension"]),
                    Disposition::Remove,
                )
                .with_value(Value::Bytes(data.len() as u64)),
            ),
            APP1 if data.starts_with(EXIF) => {
                let mut exif = exif::inspect(after(EXIF), &Location::new(["EXIF"]));
                // Only the first EXIF block can be rebuilt; any other is
                // dropped, and so is a gain map's.
                if seen_exif || role == Role::GainMap {
                    drop_all(&mut exif);
                } else {
                    apply_options(&mut exif, options);
                }
                seen_exif = true;
                findings.extend(exif);
            }
            APP1 if data.starts_with(XMP) => {
                findings.extend(xmp::inspect_except(
                    after(XMP),
                    &Location::new(["XMP"]),
                    skip,
                ));
            }
            APP1 if data.starts_with(XMP_EXTENSION) => {
                // 32-byte GUID, 4-byte full length, 4-byte offset, then the chunk.
                let mut chunk = Cursor::new(after(XMP_EXTENSION));
                if let (Ok(_guid), Ok(_len), Ok(offset)) =
                    (chunk.skip(32), chunk.u32_be(), chunk.u32_be())
                {
                    extended_xmp.push((offset, chunk.remaining()));
                }
            }
            APP2 if data.starts_with(ICC) => {
                // Sequence number (1-based) and chunk count, then the chunk.
                let mut chunk = Cursor::new(after(ICC));
                if let (Ok(sequence), Ok(_count)) = (chunk.u8(), chunk.u8()) {
                    icc_chunks.push((sequence, chunk.remaining()));
                }
            }
            APP2 if data.starts_with(MPF) => has_mpf = true,
            // The gain map's settings in ISO 21496-1 form: numbers only.
            APP2 if ultra && data.starts_with(ISO_GAIN_MAP) => {}
            APP13 if data.starts_with(PHOTOSHOP) => {
                let mut photoshop =
                    iptc::inspect_photoshop(after(PHOTOSHOP), &Location::new(["Photoshop"]));
                drop_all(&mut photoshop);
                findings.extend(photoshop);
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
        findings.extend(xmp::inspect_except(
            &packet,
            &Location::new(["XMP", "Extended"]),
            skip,
        ));
    }
    if !icc_chunks.is_empty() {
        icc_chunks.sort_by_key(|(sequence, _)| *sequence);
        let profile: Vec<u8> = icc_chunks
            .into_iter()
            .flat_map(|(_, c)| c.iter().copied())
            .collect();
        let mut icc = [icc::inspect(&profile, Location::new(["ICC"]))];
        if role == Role::GainMap {
            drop_all(&mut icc);
        } else {
            apply_options(&mut icc, options);
        }
        findings.extend(icc);
    }
    // What follows the main image is reported with the gain map.
    if role == Role::Primary {
        return findings;
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

/// Counts what's written, to know where the MPF segment lands.
struct Counter<'a> {
    out: &'a mut dyn Write,
    written: usize,
}

impl Write for Counter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.out.write(buf)?;
        self.written += n;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// Writes only what's needed to display the image, plus what the findings
/// say is kept. Everything after the end-of-image marker is dropped. For
/// an Ultra HDR photo, `xmp` replaces the first XMP packet, and the main
/// image gets a blank MPF segment; returns where its `MPF\0` is, to fill in.
fn write_clean(
    parsed: &Parsed<'_>,
    findings: &[Finding],
    role: Role,
    xmp: Option<&[u8]>,
    out: &mut dyn Write,
) -> io::Result<Option<usize>> {
    let out = &mut Counter { out, written: 0 };
    let keep_icc = is_kept(findings, FindingKind::ColorProfile, &Location::new(["ICC"]));
    let orientation = kept_orientation(findings, &Location::new(["EXIF", "Orientation"]));
    let mut exif_written = false;
    let mut xmp = xmp;
    let mut mpf_at = None;

    out.write_all(&[0xFF, SOI])?;
    for segment in &parsed.segments {
        let data = segment.data;
        match segment.marker {
            APP0 if data.starts_with(JFIF) => match data.get(..JFIF_HEADER_LEN) {
                Some(header) => {
                    let mut header = header.to_vec();
                    // No embedded thumbnail: width and height 0.
                    header.truncate(JFIF_HEADER_LEN - 2);
                    header.extend([0, 0]);
                    write_segment(out, APP0, &header)?;
                }
                None => write_segment(out, APP0, data)?,
            },
            APP14 if data.starts_with(ADOBE) => write_segment(out, APP14, data)?,
            APP2 if data.starts_with(ICC) && keep_icc => write_segment(out, APP2, data)?,
            APP1 if data.starts_with(XMP) && xmp.is_some() => {
                if let Some(packet) = xmp.take() {
                    write_segment(out, APP1, &[XMP, packet].concat())?;
                }
            }
            APP2 if role != Role::Plain && data.starts_with(ISO_GAIN_MAP) => {
                write_segment(out, APP2, data)?;
            }
            APP2 if role == Role::Primary && data.starts_with(MPF) && mpf_at.is_none() => {
                mpf_at = Some(out.written + 4);
                write_segment(out, APP2, &[0; ultra_hdr::MPF_LEN])?;
            }
            APP1 if data.starts_with(EXIF) && !exif_written => {
                exif_written = true;
                if let Some(orientation) = orientation {
                    let block = [EXIF, &exif::orientation_only(orientation)].concat();
                    write_segment(out, APP1, &block)?;
                }
            }
            APP0..=APP15 | COM => {}
            marker => {
                write_segment(out, marker, data)?;
                out.write_all(segment.scan)?;
            }
        }
    }
    out.write_all(&[0xFF, EOI])?;
    Ok(mpf_at)
}

fn write_segment(out: &mut dyn Write, marker: u8, payload: &[u8]) -> io::Result<()> {
    let length = u16::try_from(payload.len() + 2)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "JPEG segment too long"))?;
    out.write_all(&[0xFF, marker])?;
    out.write_all(&length.to_be_bytes())?;
    out.write_all(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::KeepReason;

    const SRGB: [[f64; 3]; 3] = [
        [0.4361, 0.2225, 0.0139],
        [0.3851, 0.7169, 0.0971],
        [0.1431, 0.0606, 0.7141],
    ];

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

    fn findings(data: &[u8]) -> Vec<Finding> {
        inspect_segments(&parse(data).unwrap(), &Options::default(), Role::Plain)
    }

    fn kinds(data: &[u8]) -> Vec<FindingKind> {
        findings(data).iter().map(|f| f.kind).collect()
    }

    fn clean(data: &[u8], options: &Options) -> Vec<u8> {
        let mut out = Vec::new();
        Jpeg.clean(&mut io::Cursor::new(data), &mut out, options)
            .unwrap();
        out
    }

    fn icc_segment(profile: &[u8]) -> Vec<u8> {
        segment(APP2, &[ICC, &[1, 1], profile].concat())
    }

    #[test]
    fn scan_data_with_stuffing_and_restarts_is_kept_whole() {
        let data = jpeg(&[segment(APP0, b"JFIF\0\x01\x02")], b"");
        let parsed = parse(&data).unwrap();
        assert_eq!(parsed.segments.len(), 2);
        assert_eq!(
            parsed.segments[1].scan,
            [0x12, 0xFF, 0x00, 0x34, 0xFF, 0xD0, 0x56]
        );
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
                segment(APP0, b"JFIF\0\x01\x02\0\0\x01\0\x01\0\0"),
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
        let profile = icc::tests::profile("sRGB", SRGB, None);
        let (first, second) = profile.split_at(100);
        let chunk = |seq: u8, data: &[u8]| segment(APP2, &[ICC, &[seq, 2], data].concat());
        // Out of order on purpose.
        let data = jpeg(&[chunk(2, second), chunk(1, first)], b"");
        let findings = findings(&data);
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
    fn escaped_data_byte_is_not_a_marker() {
        let mut data = jpeg(&[], b"");
        // After the scan, fill bytes then 00: found by fuzzing the cleaner.
        let eoi = data.len() - 2;
        data.splice(eoi..eoi, [0xFF, 0xFF, 0x00, 0x00, 0x02]);
        assert!(parse(&data).is_err());
    }

    #[test]
    fn segment_length_below_two_is_an_error() {
        assert!(parse(&[0xFF, SOI, 0xFF, COM, 0x00, 0x01]).is_err());
    }

    #[test]
    fn cleaning_keeps_only_what_is_needed_to_display() {
        let tables = segment(0xDB, &[0; 65]);
        let data = jpeg(
            &[
                segment(APP0, b"JFIF\0\x01\x02\0\0\x01\0\x01\0\0"),
                segment(COM, b"made by Jane"),
                segment(0xEC, b"Ducky"),
                segment(APP13, b"Photoshop 3.0\08BIM"),
                tables.clone(),
            ],
            b"motion photo video",
        );
        let expected = jpeg(
            &[segment(APP0, b"JFIF\0\x01\x02\0\0\x01\0\x01\0\0"), tables],
            b"",
        );
        assert_eq!(clean(&data, &Options::default()), expected);
    }

    #[test]
    fn cleaning_removes_the_jfif_thumbnail() {
        let mut jfif = b"JFIF\0\x01\x02\0\0\x01\0\x01\x02\x01".to_vec();
        jfif.extend([0x80; 6]); // 2x1 RGB thumbnail
        let data = jpeg(&[segment(APP0, &jfif)], b"");
        assert_eq!(kinds(&data), [FindingKind::Thumbnail]);

        let cleaned = clean(&data, &Options::default());
        assert!(kinds(&cleaned).is_empty());
        assert_eq!(
            parse(&cleaned).unwrap().segments[0].data,
            b"JFIF\0\x01\x02\0\0\x01\0\x01\0\0"
        );
    }

    #[test]
    fn cleaning_rebuilds_exif_with_only_the_orientation() {
        let rotated = [EXIF, &exif::orientation_only(6)].concat();
        let data = jpeg(&[segment(APP1, &rotated)], b"");
        let cleaned = clean(&data, &Options::default());
        assert_eq!(cleaned, data);

        let stripped = clean(
            &data,
            &Options {
                strip_orientation: true,
                ..Options::default()
            },
        );
        assert_eq!(stripped, jpeg(&[], b""));
    }

    #[test]
    fn upright_images_need_no_exif_at_all() {
        let upright = [EXIF, &exif::orientation_only(1)].concat();
        let data = jpeg(&[segment(APP1, &upright)], b"");
        assert_eq!(clean(&data, &Options::default()), jpeg(&[], b""));
    }

    #[test]
    fn cleaning_keeps_standard_profiles_only_unless_asked() {
        let standard = jpeg(
            &[icc_segment(&icc::tests::profile("sRGB", SRGB, None))],
            b"",
        );
        assert_eq!(clean(&standard, &Options::default()), standard);

        let custom = jpeg(
            &[icc_segment(&icc::tests::profile(
                "Monitor",
                SRGB,
                Some(b"vcgt"),
            ))],
            b"",
        );
        assert_eq!(clean(&custom, &Options::default()), jpeg(&[], b""));
        let keep = Options {
            keep_color_profile: true,
            ..Options::default()
        };
        assert_eq!(clean(&custom, &keep), custom);
        assert_eq!(
            findings_with(&custom, &keep)[0].disposition,
            Disposition::Keep(KeepReason::UserRequested)
        );
    }

    #[test]
    fn nothing_inside_dropped_blocks_is_reported_as_kept() {
        let photoshop_icc = {
            let profile = icc::tests::profile("sRGB", SRGB, None);
            let mut resource = b"Photoshop 3.0\x008BIM\x04\x0f\0\0".to_vec();
            resource.extend((profile.len() as u32).to_be_bytes());
            resource.extend(&profile);
            resource
        };
        let second_exif = [EXIF, &exif::orientation_only(6)].concat();
        let data = jpeg(
            &[
                segment(APP1, &[EXIF, &exif::orientation_only(3)].concat()),
                segment(APP1, &second_exif),
                segment(APP13, &photoshop_icc),
            ],
            b"",
        );
        let kept: Vec<_> = findings(&data)
            .into_iter()
            .filter(|f| matches!(f.disposition, Disposition::Keep(_)))
            .map(|f| f.location.to_string())
            .collect();
        assert_eq!(kept, ["EXIF > Orientation"]);
    }

    fn findings_with(data: &[u8], options: &Options) -> Vec<Finding> {
        inspect_segments(&parse(data).unwrap(), options, Role::Plain)
    }

    /// An Ultra HDR photo: a main image whose XMP has the version, an
    /// author and the directory, then a gain map with its settings and an
    /// editor's name, then `after`.
    fn ultra_hdr(after: &[u8]) -> Vec<u8> {
        let gain_xmp = br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/" xmlns:xmp="http://ns.adobe.com/xap/1.0/" hdrgm:Version="1.0" hdrgm:GainMapMax="2.5" xmp:CreatorTool="Editor 3"/></rdf:RDF></x:xmpmeta>"#;
        let gain = jpeg(
            &[
                segment(APP1, &[XMP, gain_xmp].concat()),
                segment(APP2, &[ISO_GAIN_MAP, &[0, 0, 0, 0, 1, 2, 3, 4]].concat()),
                segment(COM, b"gain map by Jane"),
            ],
            b"",
        );
        let primary_xmp = format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:Container="http://ns.google.com/photos/1.0/container/" xmlns:Item="http://ns.google.com/photos/1.0/container/item/" hdrgm:Version="1.0"><dc:creator><rdf:Seq><rdf:li>Jane Doe</rdf:li></rdf:Seq></dc:creator><Container:Directory><rdf:Seq><rdf:li rdf:parseType="Resource"><Container:Item Item:Semantic="Primary" Item:Mime="image/jpeg"/></rdf:li><rdf:li rdf:parseType="Resource"><Container:Item Item:Semantic="GainMap" Item:Mime="image/jpeg" Item:Length="{}"/></rdf:li></rdf:Seq></Container:Directory></rdf:Description></rdf:RDF></x:xmpmeta>"#,
            gain.len()
        );
        let xmp = segment(APP1, &[XMP, primary_xmp.as_bytes()].concat());
        // The MPF segment's `MPF\0` comes after SOI, the XMP and its own header.
        let at = 2 + xmp.len() + 4;
        let mut primary = jpeg(
            &[
                xmp.clone(),
                segment(APP2, &[0; ultra_hdr::MPF_LEN]),
                segment(APP2, &[ISO_GAIN_MAP, &[0, 0, 0, 0]].concat()),
            ],
            b"",
        );
        let mpf = ultra_hdr::mpf(at, primary.len(), gain.len());
        primary[at..at + mpf.len()].copy_from_slice(&mpf);
        [primary, gain, after.to_vec()].concat()
    }

    fn run(data: &[u8]) -> (Vec<u8>, Vec<Finding>) {
        let options = Options::default();
        let mut out = Vec::new();
        let findings = Jpeg
            .clean(&mut io::Cursor::new(data), &mut out, &options)
            .unwrap();
        let inspected = Jpeg.inspect(&mut io::Cursor::new(data), &options).unwrap();
        assert_eq!(inspected, findings);
        (out, findings)
    }

    #[test]
    fn ultra_hdr_keeps_its_gain_map() {
        let data = ultra_hdr(b"\0\0\0\x18ftypmp42 motion photo video");
        let (out, findings) = run(&data);
        let summary: Vec<_> = findings
            .iter()
            .map(|f| (f.location.to_string(), f.kind, f.disposition))
            .collect();
        assert_eq!(
            summary,
            [
                (
                    "XMP > dc:creator".into(),
                    FindingKind::PersonName,
                    Disposition::Remove
                ),
                (
                    "HDR gain map > XMP > xmp:CreatorTool".into(),
                    FindingKind::Software,
                    Disposition::Remove
                ),
                (
                    "HDR gain map > Comment".into(),
                    FindingKind::Comment,
                    Disposition::Remove
                ),
                (
                    "HDR gain map".into(),
                    FindingKind::HiddenImage,
                    Disposition::Keep(KeepReason::PartOfImage)
                ),
                ("MPF".into(), FindingKind::HiddenImage, Disposition::Remove),
            ]
        );
        for gone in [&b"Jane"[..], b"Editor", b"motion photo"] {
            assert!(!out.windows(gone.len()).any(|w| w == gone));
        }
        // The cleaned file still has its gain map, found through the new MPF.
        let parsed = parse(&out).unwrap();
        let gain_map = ultra_hdr::find(&out, &parsed).expect("gain map");
        assert_eq!(gain_map.range.end, out.len());
        assert_eq!(gain_map.settings.len(), 2);
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains(&format!("Item:Length=\"{}\"", gain_map.range.len())));
        // Nothing left to remove, and a second cleaning changes nothing.
        let (again, findings) = run(&out);
        assert_eq!(again, out);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].disposition,
            Disposition::Keep(KeepReason::PartOfImage)
        );
    }

    #[test]
    fn a_gain_map_that_cant_be_found_goes() {
        let mut data = ultra_hdr(b"");
        // Point the MPF entry past the end of the file.
        let at = data.windows(4).position(|w| w == MPF).unwrap() + 4 + 50 + 16 + 8;
        data[at..at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        let (out, findings) = run(&data);
        assert!(findings.iter().any(|f| f.kind == FindingKind::HiddenImage
            && f.disposition == Disposition::Remove));
        assert!(!out.windows(ISO_GAIN_MAP.len()).any(|w| w == ISO_GAIN_MAP));
        assert!(parse(&out).unwrap().trailing.is_empty());
    }
}
