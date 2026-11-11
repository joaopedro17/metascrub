//! FLAC: `fLaC`, a list of metadata blocks, then the sound as frames. The
//! stream description, seek table and cue sheet are kept; tags (Vorbis
//! comments), pictures, application data and padding go, and so do ID3 or
//! APE tags some programs add before or after. Frames don't give their
//! length, so they're checked through their checksums: every byte of
//! sound must belong to a frame whose CRC-16 is right, or the file is
//! refused, since other data there could hold anything.

use std::io::{self, BufReader, Read, SeekFrom, Write};
use std::sync::LazyLock;

use super::{Tag, id3, read_at, text, trailing_tags, vorbis};
use crate::format::bytes::latin1;
use crate::format::{FormatError, FormatHandler, ReadSeek};
use crate::limits::MAX_AUDIO_TAG_BYTES;
use crate::model::{Disposition, Finding, FindingKind, KeepReason, Location, Value};
use crate::options::Options;

const STREAMINFO: u8 = 0;
const PADDING: u8 = 1;
const APPLICATION: u8 = 2;
const SEEKTABLE: u8 = 3;
const VORBIS_COMMENT: u8 = 4;
const CUESHEET: u8 = 5;
const PICTURE: u8 = 6;
const INVALID: u8 = 127;

const STREAMINFO_LEN: u64 = 34;
/// The longest a frame header can be: sync and codes, a 7-byte frame
/// number, block size and sample rate, CRC-8.
const MAX_HEADER_LEN: usize = 16;
/// The shortest a frame can be: a 6-byte header, one constant subframe of
/// 2 bytes and the CRC-16.
const MIN_FRAME_LEN: u64 = 10;

#[derive(Debug)]
pub(crate) struct Flac;

impl FormatHandler for Flac {
    fn inspect(
        &self,
        input: &mut dyn ReadSeek,
        _options: &Options,
    ) -> Result<Vec<Finding>, FormatError> {
        Ok(scan(input)?.findings)
    }

    fn clean(
        &self,
        input: &mut dyn ReadSeek,
        output: &mut dyn Write,
        _options: &Options,
    ) -> Result<Vec<Finding>, FormatError> {
        let layout = scan(input)?;
        output.write_all(b"fLaC")?;
        let count = layout.kept.len();
        for (i, block) in layout.kept.iter().enumerate() {
            let last = if i + 1 == count { 0x80 } else { 0 };
            let len = u32::try_from(block.len)
                .map_err(|_| FormatError::malformed("a FLAC block is too long"))?
                .to_be_bytes();
            output.write_all(&[block.kind | last])?;
            output.write_all(len.get(1..).unwrap_or_default())?;
            copy(input, output, block.at, block.len)?;
        }
        copy(
            input,
            output,
            layout.sound_start,
            layout.sound_end - layout.sound_start,
        )?;
        Ok(layout.findings)
    }
}

fn copy(
    input: &mut dyn ReadSeek,
    output: &mut dyn Write,
    at: u64,
    len: u64,
) -> Result<(), FormatError> {
    input.seek(SeekFrom::Start(at))?;
    if io::copy(&mut (&mut *input).take(len), output)? != len {
        return Err(FormatError::malformed(
            "the file got shorter while it was cleaned",
        ));
    }
    Ok(())
}

/// A metadata block kept in the output: its type and where its data is.
#[derive(Debug)]
struct Block {
    kind: u8,
    at: u64,
    len: u64,
}

#[derive(Debug)]
struct Layout {
    kept: Vec<Block>,
    sound_start: u64,
    sound_end: u64,
    findings: Vec<Finding>,
}

fn scan(input: &mut dyn ReadSeek) -> Result<Layout, FormatError> {
    let len = input.seek(SeekFrom::End(0))?;
    let mut findings = Vec::new();
    let mut pos = 0;
    while let Some(tag) = id3::v2_at(input, pos, len)? {
        pos = tag.end;
        findings.extend(tag.findings);
    }
    if read_at(input, pos, 4.min(len - pos))? != b"fLaC" {
        return Err(FormatError::malformed("missing the FLAC signature"));
    }
    pos += 4;

    let mut kept = Vec::new();
    let mut first = true;
    loop {
        let header = read_at(input, pos, 4.min(len - pos))?;
        let [flags, a, b, c] = header[..] else {
            return Err(FormatError::malformed("FLAC metadata cut short"));
        };
        let kind = flags & 0x7F;
        let block_len = u64::from(u32::from_be_bytes([0, a, b, c]));
        let at = pos + 4;
        if at + block_len > len {
            return Err(FormatError::malformed(
                "a FLAC block runs past the end of the file",
            ));
        }
        let misplaced = (kind == STREAMINFO) != first;
        if misplaced || (first && block_len != STREAMINFO_LEN) {
            return Err(FormatError::malformed(
                "FLAC must start with exactly one stream description",
            ));
        }
        first = false;
        match kind {
            STREAMINFO | SEEKTABLE => kept.push(Block {
                kind,
                at,
                len: block_len,
            }),
            CUESHEET => {
                kept.push(Block {
                    kind,
                    at,
                    len: block_len,
                });
                findings.push(cue_sheet(input, at, block_len)?);
            }
            INVALID => return Err(FormatError::malformed("an invalid FLAC block type")),
            _ => findings.extend(removed_block(input, kind, at, block_len)?),
        }
        pos = at + block_len;
        if flags & 0x80 != 0 {
            break;
        }
    }

    let mut trailing: Vec<Tag> = Vec::new();
    let sound_end = trailing_tags(input, pos, len, &mut trailing)?;
    walk(input, pos, sound_end)?;
    for tag in trailing.into_iter().rev() {
        findings.extend(tag.findings);
    }
    Ok(Layout {
        kept,
        sound_start: pos,
        sound_end,
        findings,
    })
}

fn removed_block(
    input: &mut dyn ReadSeek,
    kind: u8,
    at: u64,
    len: u64,
) -> Result<Vec<Finding>, FormatError> {
    let finding = |kind: FindingKind, location: Location, value: Option<Value>| {
        let finding = Finding::new(kind, location, Disposition::Remove);
        match value {
            Some(value) => finding.with_value(value),
            None => finding,
        }
    };
    let bytes = Some(Value::Bytes(len));
    Ok(match kind {
        PADDING => {
            // Padding is zeros; anything else in it could be hidden data.
            input.seek(SeekFrom::Start(at))?;
            let mut nonzero = false;
            for byte in BufReader::new(&mut *input).bytes().take(len as usize) {
                nonzero |= byte? != 0;
            }
            if nonzero {
                vec![finding(
                    FindingKind::UnknownMetadata,
                    Location::new(["Padding"]),
                    bytes,
                )]
            } else {
                Vec::new()
            }
        }
        VORBIS_COMMENT if len <= MAX_AUDIO_TAG_BYTES => {
            vorbis::findings("Vorbis comment", &read_at(input, at, len)?)?
        }
        PICTURE => vec![finding(
            FindingKind::CoverArt,
            Location::new(["Picture"]),
            bytes,
        )],
        APPLICATION => {
            let id = latin1(&read_at(input, at, 4.min(len))?);
            vec![finding(
                FindingKind::UnknownMetadata,
                Location::new(["Application".to_owned(), id]),
                bytes,
            )]
        }
        _ => vec![finding(
            FindingKind::UnknownMetadata,
            Location::new([format!("Block type {kind}")]),
            bytes,
        )],
    })
}

/// Kept for CD images; it holds track positions and, at most, the CD's
/// catalog number.
fn cue_sheet(input: &mut dyn ReadSeek, at: u64, len: u64) -> Result<Finding, FormatError> {
    let catalog = read_at(input, at, 128.min(len))?;
    let finding = Finding::new(
        FindingKind::TechnicalInfo,
        Location::new(["Cue sheet"]),
        Disposition::Keep(KeepReason::RequiredToPlay),
    );
    Ok(match text(&latin1(&catalog)) {
        Some(value) => finding.with_value(value),
        None => finding,
    })
}

/// The length of the frame header at the start of `data`, if one is there
/// and its CRC-8 is right.
fn header_len(data: &[u8]) -> Option<usize> {
    let [0xFF, b1, b2, b3, rest @ ..] = data else {
        return None;
    };
    let block_size = b2 >> 4;
    let sample_rate = b2 & 0x0F;
    let channels = b3 >> 4;
    let bits = (b3 >> 1) & 0b111;
    if b1 & 0xFE != 0xF8
        || block_size == 0
        || sample_rate == 15
        || channels > 10
        || bits == 3
        || b3 & 1 != 0
    {
        return None;
    }
    // The frame or sample number, coded like UTF-8 in 1 to 7 bytes.
    let first = *rest.first()?;
    let number_len = match first.leading_ones() {
        0 => 1,
        n @ 2..=7 => n as usize,
        _ => return None,
    };
    let continuation = rest.get(1..number_len)?;
    if continuation.iter().any(|b| b & 0xC0 != 0x80) {
        return None;
    }
    let extra = match block_size {
        6 => 1,
        7 => 2,
        _ => 0,
    } + match sample_rate {
        12 => 1,
        13 | 14 => 2,
        _ => 0,
    };
    let len = 4 + number_len + extra;
    let crc = *data.get(len)?;
    (crc8(data.get(..len)?) == crc).then_some(len + 1)
}

fn crc8(data: &[u8]) -> u8 {
    data.iter().fold(0, |crc, &b| {
        (0..8).fold(crc ^ b, |c, _| {
            if c & 0x80 != 0 {
                (c << 1) ^ 0x07
            } else {
                c << 1
            }
        })
    })
}

/// CRC-16 with polynomial 0x8005, one entry per byte value.
static CRC16: LazyLock<[u16; 256]> = LazyLock::new(|| {
    std::array::from_fn(|i| {
        (0..8).fold((i as u16) << 8, |crc, _| {
            if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            }
        })
    })
});

fn crc16(crc: u16, byte: u8) -> u16 {
    let index = usize::from((crc >> 8) as u8 ^ byte);
    (crc << 8) ^ CRC16.get(index).copied().unwrap_or(0)
}

/// Checks that the sound from `start` to `end` is a chain of frames: a new
/// frame starts only where the previous one's CRC-16 comes out right and a
/// valid header follows, and the last one ends exactly at `end`.
fn walk(input: &mut dyn ReadSeek, start: u64, end: u64) -> Result<(), FormatError> {
    input.seek(SeekFrom::Start(start))?;
    let mut reader = BufReader::with_capacity(1 << 16, (&mut *input).take(end - start));
    let mut buf: Vec<u8> = Vec::with_capacity(1 << 17);
    let mut at = 0usize;
    let mut pos = start;
    let mut frame_start = start;
    let mut crc = 0u16;
    let mut chunk = vec![0u8; 1 << 16];
    loop {
        if buf.len() - at < MAX_HEADER_LEN {
            if at > 0 {
                buf.drain(..at);
                at = 0;
            }
            let read = reader.read(&mut chunk)?;
            buf.extend_from_slice(chunk.get(..read).unwrap_or_default());
            if read > 0 {
                continue;
            }
        }
        let Some(&byte) = buf.get(at) else { break };
        let rest = buf.get(at..).unwrap_or_default();
        if pos == start {
            if header_len(rest).is_none() {
                return Err(FormatError::malformed(
                    "no FLAC frame where the sound should start",
                ));
            }
        } else if byte == 0xFF
            && crc == 0
            && pos >= frame_start + MIN_FRAME_LEN
            && header_len(rest).is_some()
        {
            frame_start = pos;
        }
        crc = crc16(crc, byte);
        at += 1;
        pos += 1;
    }
    if pos == start {
        return Err(FormatError::malformed("a FLAC file without any sound"));
    }
    if crc != 0 {
        return Err(FormatError::Unsupported(
            "data that isn't sound in the FLAC stream, which could hold anything".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(super) mod tests {
    use std::io::Cursor;

    use super::*;

    /// A frame of one constant 8-bit mono sample: header, subframe, CRC-16.
    pub(crate) fn frame(number: u8) -> Vec<u8> {
        let mut frame = vec![0xFF, 0xF8, 0x19, 0x02, number];
        frame.push(crc8(&frame));
        frame.extend_from_slice(&[0x00, 0x2A]);
        let crc = frame.iter().fold(0, |c, &b| crc16(c, b));
        frame.extend_from_slice(&crc.to_be_bytes());
        frame
    }

    fn sound() -> Vec<u8> {
        (0..4).flat_map(frame).collect()
    }

    fn block(kind: u8, last: bool, data: &[u8]) -> Vec<u8> {
        let len = (data.len() as u32).to_be_bytes();
        [&[kind | if last { 0x80 } else { 0 }], &len[1..], data].concat()
    }

    fn streaminfo(last: bool) -> Vec<u8> {
        block(STREAMINFO, last, &[7; 34])
    }

    fn clean(data: &[u8]) -> Result<(Vec<u8>, Vec<Finding>), FormatError> {
        let mut out = Vec::new();
        let findings = Flac.clean(&mut Cursor::new(data), &mut out, &Options::default())?;
        Ok((out, findings))
    }

    #[test]
    fn frames_are_found_through_their_checksums() {
        assert_eq!(header_len(&frame(0)), Some(6));
        let data = sound();
        walk(&mut Cursor::new(&data), 0, data.len() as u64).unwrap();
        // A frame number past 127 takes two bytes, coded like UTF-8.
        let mut long = vec![0xFF, 0xF8, 0x19, 0x02, 0xC2, 0x80];
        long.push(crc8(&long));
        assert_eq!(header_len(&long), Some(7));
    }

    #[test]
    fn tags_pictures_and_padding_go() {
        let comment =
            crate::format::audio::vorbis::tests::block("reference libFLAC", &["ARTIST=Jane Doe"]);
        let data = [
            b"fLaC".as_slice(),
            &streaminfo(false),
            &block(SEEKTABLE, false, &[0; 18]),
            &block(VORBIS_COMMENT, false, &comment),
            &block(PICTURE, false, &[1; 40]),
            &block(APPLICATION, false, b"riffLIST"),
            &block(PADDING, true, &[0; 100]),
            &sound(),
        ]
        .concat();
        let (out, findings) = clean(&data).unwrap();
        let expected = [
            b"fLaC".as_slice(),
            &streaminfo(false),
            &block(SEEKTABLE, true, &[0; 18]),
            &sound(),
        ]
        .concat();
        assert_eq!(out, expected);
        let kinds: Vec<_> = findings.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            [
                FindingKind::Software,
                FindingKind::PersonName,
                FindingKind::CoverArt,
                FindingKind::UnknownMetadata,
            ]
        );
        assert_eq!(clean(&out).unwrap(), (out.clone(), Vec::new()));
    }

    #[test]
    fn id3_tags_around_it_go_too() {
        let id3 = [
            b"ID3\x03\0\0\0\0\0\x0f".as_slice(),
            b"TPE1\0\0\0\x05\0\0\0Jane",
        ]
        .concat();
        let mut v1 = vec![0u8; 128];
        v1[..3].copy_from_slice(b"TAG");
        v1[3..10].copy_from_slice(b"Holiday");
        v1[127] = 0xFF;
        let bare = [b"fLaC".as_slice(), &streaminfo(true), &sound()].concat();
        let data = [id3, bare.clone(), v1].concat();
        let (out, findings) = clean(&data).unwrap();
        assert_eq!(out, bare);
        assert_eq!(findings.len(), 2);
    }

    #[test]
    fn the_cue_sheet_is_kept() {
        let mut cue = vec![0u8; 400];
        cue[..13].copy_from_slice(b"1234567890123");
        let data = [
            b"fLaC".as_slice(),
            &streaminfo(false),
            &block(CUESHEET, true, &cue),
            &sound(),
        ]
        .concat();
        let (out, findings) = clean(&data).unwrap();
        assert_eq!(out, data);
        assert_eq!(
            findings[0].disposition,
            Disposition::Keep(KeepReason::RequiredToPlay)
        );
    }

    #[test]
    fn anything_between_frames_is_refused() {
        let sound = sound();
        let data = [
            b"fLaC".as_slice(),
            &streaminfo(true),
            &sound[..20],
            b"hidden",
            &sound[20..],
        ]
        .concat();
        assert!(matches!(clean(&data), Err(FormatError::Unsupported(_))));
        let padding = [
            b"fLaC".as_slice(),
            &streaminfo(false),
            &block(PADDING, true, b"not zeros"),
            &sound,
        ]
        .concat();
        assert_eq!(
            clean(&padding).unwrap().1[0].kind,
            FindingKind::UnknownMetadata
        );
    }

    #[test]
    fn the_stream_description_must_come_first() {
        let data = [
            b"fLaC".as_slice(),
            &block(SEEKTABLE, true, &[0; 18]),
            &sound(),
        ]
        .concat();
        assert!(matches!(clean(&data), Err(FormatError::Malformed(_))));
    }
}
