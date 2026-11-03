//! The `moov` box of videos and music: header dates, metadata per file and
//! per track, and tracks that record data next to picture and sound (GPS,
//! sensors, timecode), whose samples are zeroed where they are.

use super::boxes::{self, FourCc, Header};
use super::{Plan, metadata, unused, user_data, uuid};
use crate::format::bytes::Cursor;
use crate::format::unpack::utc;
use crate::format::{FormatError, ReadSeek};
use crate::model::{Disposition, Finding, FindingKind, Location, Value};

/// Seconds from 1904, where QuickTime dates start, to 1970.
const SINCE_1904: u64 = 2_082_844_800;

pub(super) fn movie(
    input: &mut dyn ReadSeek,
    moov: &Header,
    plan: &mut Plan,
) -> Result<(), FormatError> {
    let children = boxes::children(input, moov, 0)?;
    let fragmented = children.iter().any(|c| c.kind == *b"mvex");
    let root = Location::new(Vec::<String>::new());
    let mut dates = None;
    let mut number = 0;
    for child in &children {
        match &child.kind {
            b"mvhd" => header_dates(input, child, &mut dates, plan)?,
            b"udta" => user_data(input, child, &Location::new(["User data"]), plan)?,
            b"meta" => metadata(input, child, &Location::new(["Metadata"]), plan)?,
            b"uuid" => uuid(input, child, &root, plan)?,
            b"free" | b"skip" => unused(input, child, &root, plan)?,
            b"trak" => {
                number += 1;
                track(input, child, number, fragmented, &mut dates, plan)?;
            }
            _ => {}
        }
    }
    if let Some(seconds) = dates {
        let seconds = seconds.checked_sub(SINCE_1904).unwrap_or(seconds);
        plan.findings.push(
            Finding::new(
                FindingKind::Timestamp,
                Location::new(["Header dates"]),
                Disposition::Normalize,
            )
            .with_value(Value::Text(utc(seconds))),
        );
    }
    Ok(())
}

/// `mvhd`, `tkhd` and `mdhd` start with a creation and a modification date,
/// 32-bit in version 0 and 64-bit in version 1. They're required, so they're
/// zeroed rather than removed.
fn header_dates(
    input: &mut dyn ReadSeek,
    header: &Header,
    first: &mut Option<u64>,
    plan: &mut Plan,
) -> Result<(), FormatError> {
    let fields = boxes::read_range(input, header.payload, header.len().min(20))?;
    let mut cursor = Cursor::new(&fields);
    let version = cursor.u8()?;
    cursor.skip(3)?;
    let (creation, modification, len) = match version {
        0 => (u64::from(cursor.u32_be()?), u64::from(cursor.u32_be()?), 8),
        1 => (
            u64::from_be_bytes(cursor.array()?),
            u64::from_be_bytes(cursor.array()?),
            16,
        ),
        _ => return Ok(()),
    };
    if creation != 0 || modification != 0 {
        first.get_or_insert(if creation != 0 {
            creation
        } else {
            modification
        });
        plan.zero(header.payload + 4, len);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Content {
    /// Picture, sound or anything else needed to play the file.
    Media,
    /// Recorded data that says where or when: zeroed.
    Data(FindingKind, Disposition, &'static str),
    Text,
    Unknown,
}

fn content(handler: FourCc, format: FourCc) -> Content {
    use FindingKind::{GpsLocation, Timestamp, UnknownMetadata};
    match (&handler, &format) {
        (_, b"gpmd") => Content::Data(GpsLocation, Disposition::Remove, "GoPro telemetry"),
        (_, b"camm") => Content::Data(GpsLocation, Disposition::Remove, "Camera motion"),
        (_, b"djmd" | b"dbgi") => {
            Content::Data(GpsLocation, Disposition::Remove, "DJI flight data")
        }
        (_, b"mebx") => Content::Data(UnknownMetadata, Disposition::Remove, "Timed metadata"),
        (b"tmcd", _) | (_, b"tmcd") => Content::Data(Timestamp, Disposition::Normalize, "Timecode"),
        (b"vide" | b"soun" | b"auxv" | b"pict" | b"hint" | b"odsm" | b"sdsm", _) => Content::Media,
        (b"text" | b"sbtl" | b"subt" | b"clcp", _)
        | (_, b"tx3g" | b"text" | b"wvtt" | b"stpp" | b"c608" | b"c708") => Content::Text,
        _ => Content::Unknown,
    }
}

/// The boxes `trak` reaches through `mdia`, `minf` and `stbl` that matter here.
#[derive(Debug, Default)]
struct Parts {
    handler: FourCc,
    format: FourCc,
    descriptions: Option<Header>,
    references: Option<Header>,
    sizes: Option<Header>,
    chunks: Option<Header>,
    offsets: Option<Header>,
}

fn track(
    input: &mut dyn ReadSeek,
    trak: &Header,
    number: usize,
    fragmented: bool,
    dates: &mut Option<u64>,
    plan: &mut Plan,
) -> Result<(), FormatError> {
    let location = Location::new([format!("Track {number}")]);
    let mut parts = Parts::default();
    for child in boxes::children(input, trak, 0)? {
        match &child.kind {
            b"tkhd" => header_dates(input, &child, dates, plan)?,
            b"udta" => user_data(input, &child, &location.child("User data"), plan)?,
            b"meta" => metadata(input, &child, &location.child("Metadata"), plan)?,
            b"uuid" if child.uuid == Some(super::XMP_UUID) => {
                uuid(input, &child, &location, plan)?;
            }
            b"mdia" => media(input, &child, dates, &mut parts, plan)?,
            _ => {}
        }
    }
    let name = boxes::name(parts.format).trim_end().to_owned();
    match content(parts.handler, parts.format) {
        Content::Media if parts.handler == *b"vide" => {
            if let Some(stsd) = &parts.descriptions {
                encoder_names(input, stsd, &location, plan)?;
            }
        }
        Content::Media => {}
        Content::Text => plan.findings.push(
            Finding::new(FindingKind::TextTrack, location, Disposition::Warn)
                .with_value(Value::Text(name)),
        ),
        Content::Unknown => plan.findings.push(
            Finding::new(FindingKind::UnknownMetadata, location, Disposition::Warn)
                .with_value(Value::Text(name)),
        ),
        Content::Data(kind, disposition, label) => {
            if fragmented {
                return Err(FormatError::Unsupported(format!(
                    "{label} in a video split into fragments"
                )));
            }
            if !self_contained(input, parts.references.as_ref())? {
                return Err(FormatError::Unsupported(format!(
                    "{label} stored in another file"
                )));
            }
            let ranges = samples(input, &parts)?;
            let mut recorded = false;
            for &(at, len) in &ranges {
                if !plan.in_media_data(at, at + len) {
                    return Err(FormatError::malformed("track data outside the media data"));
                }
                recorded = recorded || !boxes::is_zero(input, at, len)?;
            }
            if recorded {
                let total = ranges.iter().map(|(_, len)| len).sum();
                let mut finding = Finding::new(kind, location.child(label), disposition);
                // A timecode's size says nothing; a recording's says how much.
                if kind != FindingKind::Timestamp {
                    finding.value = Some(Value::Bytes(total));
                }
                plan.findings.push(finding);
                for (at, len) in ranges {
                    plan.zero(at, len);
                }
            }
        }
    }
    Ok(())
}

fn media(
    input: &mut dyn ReadSeek,
    mdia: &Header,
    dates: &mut Option<u64>,
    parts: &mut Parts,
    plan: &mut Plan,
) -> Result<(), FormatError> {
    for child in boxes::children(input, mdia, 0)? {
        match &child.kind {
            b"mdhd" => header_dates(input, &child, dates, plan)?,
            // Version and flags, a QuickTime component type, the handler.
            b"hdlr" => parts.handler = field(input, &child, 8)?,
            b"minf" => {
                for child in boxes::children(input, &child, 0)? {
                    match &child.kind {
                        b"dinf" => {
                            parts.references = boxes::children(input, &child, 0)?
                                .into_iter()
                                .find(|c| c.kind == *b"dref");
                        }
                        b"stbl" => {
                            for child in boxes::children(input, &child, 0)? {
                                match &child.kind {
                                    // Version and flags, a count, then the
                                    // first entry's size and format.
                                    b"stsd" => {
                                        parts.format = field(input, &child, 12)?;
                                        parts.descriptions = Some(child);
                                    }
                                    b"stsz" | b"stz2" => parts.sizes = Some(child),
                                    b"stsc" => parts.chunks = Some(child),
                                    b"stco" | b"co64" => parts.offsets = Some(child),
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Video sample descriptions name what made them: a 4-letter vendor
/// (`appl`, `FFmp`) and a 32-byte "compressor name" (`GoPro AVC encoder`).
/// They're only labels, so they're zeroed.
fn encoder_names(
    input: &mut dyn ReadSeek,
    stsd: &Header,
    location: &Location,
    plan: &mut Plan,
) -> Result<(), FormatError> {
    // Six reserved bytes, the data reference, version and revision, then
    // the vendor; after quality, size, resolution, data size and frame
    // count, the name.
    const VENDOR: u64 = 12;
    const NAME: usize = 30;
    const LEN: u64 = 62;
    for entry in boxes::children(input, stsd, 8)? {
        if entry.len() < VENDOR + LEN {
            continue;
        }
        let fields = boxes::read_range(input, entry.payload + VENDOR, LEN)?;
        let (vendor, rest) = fields.split_at(4);
        // A length, then the name. Blank names say nothing.
        let (len, name) = rest
            .get(NAME - 4..)
            .and_then(<[u8]>::split_first)
            .unwrap_or((&0, &[]));
        let name = String::from_utf8_lossy(name.get(..usize::from(*len)).unwrap_or(name));
        let name = name.trim_matches(|c: char| c == '\0' || c.is_whitespace());
        let vendor_set = vendor.iter().any(|&b| b != 0);
        if vendor_set {
            plan.zero(entry.payload + VENDOR, 4);
        }
        if !name.is_empty() {
            plan.zero(entry.payload + VENDOR + NAME as u64, 32);
        }
        let value = if name.is_empty() {
            crate::format::bytes::latin1(vendor)
        } else {
            name.to_owned()
        };
        if vendor_set || !name.is_empty() {
            plan.findings.push(
                Finding::new(
                    FindingKind::Software,
                    location.child("Encoder"),
                    Disposition::Remove,
                )
                .with_value(Value::Text(value)),
            );
        }
    }
    Ok(())
}

/// Four bytes at `at` in the payload; zeros when the box is shorter.
fn field(input: &mut dyn ReadSeek, header: &Header, at: u64) -> Result<FourCc, FormatError> {
    let fields = boxes::read_range(input, header.payload, header.len().min(at + 4))?;
    Ok(fields
        .get(usize::try_from(at).unwrap_or(usize::MAX)..)
        .and_then(|rest| rest.try_into().ok())
        .unwrap_or_default())
}

/// Whether every data reference says "in this file" (flag 1).
fn self_contained(input: &mut dyn ReadSeek, dref: Option<&Header>) -> Result<bool, FormatError> {
    let Some(dref) = dref else {
        return Ok(true);
    };
    for entry in boxes::children(input, dref, 8)? {
        let flags = boxes::read_range(input, entry.payload, entry.len().min(4))?;
        if flags.get(3).is_none_or(|flags| flags & 1 == 0) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Where a track's samples are, from its sample tables: the size of each
/// sample, how many samples each chunk holds, and where each chunk starts.
/// Neighbouring samples are merged into one range.
fn samples(input: &mut dyn ReadSeek, parts: &Parts) -> Result<Vec<(u64, u64)>, FormatError> {
    let missing = || FormatError::malformed("a track without sample tables");
    let sizes = parts.sizes.ok_or_else(missing)?;
    let chunks = parts.chunks.ok_or_else(missing)?;
    let offsets = parts.offsets.ok_or_else(missing)?;
    if sizes.kind == *b"stz2" {
        return Err(FormatError::Unsupported("compact sample sizes".into()));
    }

    let data = boxes::read(input, &sizes)?;
    let mut cursor = Cursor::new(&data);
    cursor.skip(4)?;
    let fixed = u64::from(cursor.u32_be()?);
    let count = u64::from(cursor.u32_be()?);
    let table = if fixed == 0 {
        let len = usize::try_from(count)
            .ok()
            .and_then(|count| count.checked_mul(4))
            .ok_or_else(|| FormatError::malformed("too many samples"))?;
        cursor.take(len)?
    } else {
        &[]
    };
    let size = |sample: u64| -> u64 {
        if fixed != 0 {
            return fixed;
        }
        usize::try_from(sample)
            .ok()
            .and_then(|i| table.get(i * 4..i * 4 + 4))
            .and_then(|bytes| bytes.try_into().ok())
            .map_or(0, |bytes| u64::from(u32::from_be_bytes(bytes)))
    };

    let data = boxes::read(input, &chunks)?;
    let mut cursor = Cursor::new(&data);
    cursor.skip(4)?;
    let mut runs = Vec::new();
    for _ in 0..cursor.u32_be()? {
        let first = u64::from(cursor.u32_be()?);
        let per_chunk = u64::from(cursor.u32_be()?);
        cursor.skip(4)?;
        runs.push((first, per_chunk));
    }

    let data = boxes::read(input, &offsets)?;
    let mut cursor = Cursor::new(&data);
    cursor.skip(4)?;
    let wide = offsets.kind == *b"co64";
    let mut ranges: Vec<(u64, u64)> = Vec::new();
    let mut sample = 0;
    let mut run = 0;
    for chunk in 1..=u64::from(cursor.u32_be()?) {
        let offset = if wide {
            u64::from_be_bytes(cursor.array()?)
        } else {
            u64::from(cursor.u32_be()?)
        };
        while runs.get(run + 1).is_some_and(|&(first, _)| first <= chunk) {
            run += 1;
        }
        let per_chunk = runs.get(run).map_or(0, |&(_, n)| n);
        let n = per_chunk.min(count - sample);
        let len = if fixed != 0 {
            fixed
                .checked_mul(n)
                .ok_or_else(|| FormatError::malformed("too many samples"))?
        } else {
            (sample..sample + n).map(size).sum()
        };
        sample += n;
        if len > 0 {
            let end = offset
                .checked_add(len)
                .ok_or_else(|| FormatError::malformed("track data outside the file"))?;
            match ranges.last_mut() {
                Some((at, previous)) if *at + *previous == offset => *previous += len,
                _ => ranges.push((offset, end - offset)),
            }
        }
        if sample == count {
            break;
        }
    }
    Ok(ranges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_recordings_from_picture_and_sound() {
        let data = |kind, label| Content::Data(kind, Disposition::Remove, label);
        for (handler, format, expected) in [
            (b"vide", b"avc1", Content::Media),
            (b"soun", b"mp4a", Content::Media),
            (
                b"meta",
                b"gpmd",
                data(FindingKind::GpsLocation, "GoPro telemetry"),
            ),
            (
                b"camm",
                b"camm",
                data(FindingKind::GpsLocation, "Camera motion"),
            ),
            (
                b"meta",
                b"djmd",
                data(FindingKind::GpsLocation, "DJI flight data"),
            ),
            (
                b"meta",
                b"mebx",
                data(FindingKind::UnknownMetadata, "Timed metadata"),
            ),
            (
                b"tmcd",
                b"tmcd",
                Content::Data(FindingKind::Timestamp, Disposition::Normalize, "Timecode"),
            ),
            (b"sbtl", b"tx3g", Content::Text),
            (b"text", b"text", Content::Text),
            (b"subt", b"wvtt", Content::Text),
            (b"meta", b"abcd", Content::Unknown),
        ] {
            assert_eq!(
                content(*handler, *format),
                expected,
                "{handler:?} {format:?}"
            );
        }
    }
}
