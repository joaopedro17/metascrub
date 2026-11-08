//! MP3 (MPEG audio layers I to III): a run of frames, each with a 4-byte
//! header that gives its length. Tags sit before the first frame (ID3v2)
//! and after the last (APE, Lyrics3, ID3v1, ID3v2 with a footer). Cleaning
//! keeps exactly the frames, after checking that every byte between the
//! tags is a frame: anything else could hide data, so it's refused.

use std::io::{self, BufReader, Read, SeekFrom, Write};

use super::{Tag, ape, id3, text};
use crate::format::{FormatError, FormatHandler, ReadSeek};
use crate::model::{Disposition, Finding, FindingKind, KeepReason, Location};
use crate::options::Options;

/// Zero bytes some taggers leave between the tag and the first frame.
const MAX_PADDING: u64 = 64 * 1024;

#[derive(Debug)]
pub(crate) struct Mp3;

impl FormatHandler for Mp3 {
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
        input.seek(SeekFrom::Start(layout.start))?;
        let copied = io::copy(&mut (&mut *input).take(layout.end - layout.start), output)?;
        if copied != layout.end - layout.start {
            return Err(FormatError::malformed(
                "the file got shorter while it was cleaned",
            ));
        }
        Ok(layout.findings)
    }
}

#[derive(Debug)]
struct Layout {
    /// The frames: everything else goes.
    start: u64,
    end: u64,
    findings: Vec<Finding>,
}

fn scan(input: &mut dyn ReadSeek) -> Result<Layout, FormatError> {
    let len = input.seek(SeekFrom::End(0))?;
    let mut findings = Vec::new();
    let mut start = 0;
    while let Some(tag) = id3::v2_at(input, start, len)? {
        start = tag.end;
        findings.extend(tag.findings);
    }
    start = skip_zeros(input, start, len)?;

    let mut trailing: Vec<Tag> = Vec::new();
    let mut end = len;
    let frames = loop {
        end = trailing_tags(input, start, end, &mut trailing)?;
        let frames = walk(input, start, end)?;
        if frames.end == end {
            break frames;
        }
        // Zeros after the frames were dropped: a tag may end where they began.
        end = frames.end;
    };
    if let Some(info) = info_frame(input, start, frames.first)? {
        findings.push(info);
    }
    for tag in trailing.into_iter().rev() {
        findings.extend(tag.findings);
    }
    Ok(Layout {
        start,
        end: frames.end,
        findings,
    })
}

/// Tags that end at `end`, from the end inward, in whatever order they
/// were added. Returns where the first of them starts.
fn trailing_tags(
    input: &mut dyn ReadSeek,
    floor: u64,
    mut end: u64,
    trailing: &mut Vec<Tag>,
) -> Result<u64, FormatError> {
    loop {
        let tag = match id3::v1_before(input, floor, end)? {
            Some(tag) => Some(tag),
            None => match ape::ape_before(input, floor, end)? {
                Some(tag) => Some(tag),
                None => match ape::lyrics_before(input, floor, end)? {
                    Some(tag) => Some(tag),
                    None => id3::v2_footer_before(input, floor, end)?,
                },
            },
        };
        let Some(tag) = tag else { return Ok(end) };
        end = tag.start;
        trailing.push(tag);
    }
}

fn skip_zeros(input: &mut dyn ReadSeek, start: u64, len: u64) -> Result<u64, FormatError> {
    input.seek(SeekFrom::Start(start))?;
    let mut zeros = 0;
    for byte in BufReader::new(&mut *input)
        .bytes()
        .take(MAX_PADDING as usize)
    {
        if byte? != 0 {
            break;
        }
        zeros += 1;
    }
    Ok((start + zeros).min(len))
}

/// A frame header, read from its 4 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Frame {
    /// 1 for MPEG-1, 2 for MPEG-2, 25 for MPEG-2.5.
    version: u8,
    layer: u8,
    sample_rate: u32,
    mono: bool,
    len: u64,
}

impl Frame {
    pub(super) fn parse(header: [u8; 4]) -> Option<Self> {
        let [a, b, c, d] = header;
        if a != 0xFF || b & 0xE0 != 0xE0 {
            return None;
        }
        let version = match (b >> 3) & 0b11 {
            0b00 => 25,
            0b10 => 2,
            0b11 => 1,
            _ => return None,
        };
        let layer = match (b >> 1) & 0b11 {
            0b01 => 3,
            0b10 => 2,
            0b11 => 1,
            _ => return None,
        };
        let bitrate_index = usize::from(c >> 4);
        let rate_index = usize::from((c >> 2) & 0b11);
        let padding = u64::from((c >> 1) & 1);
        // Index 0 is "free format", whose frames don't give their length.
        if bitrate_index == 0 || bitrate_index == 15 || rate_index == 3 || d & 0b11 == 0b10 {
            return None;
        }
        let row: &[u32; 15] = match (version, layer) {
            (1, 1) => &[
                0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448,
            ],
            (1, 2) => &[
                0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
            ],
            (1, _) => &[
                0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
            ],
            (_, 1) => &[
                0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256,
            ],
            _ => &[0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
        };
        let bitrate = u64::from(*row.get(bitrate_index)?) * 1000;
        let base: u32 = *[44100, 48000, 32000].get(rate_index)?;
        let sample_rate = match version {
            1 => base,
            2 => base / 2,
            _ => base / 4,
        };
        let rate = u64::from(sample_rate);
        let len = match (layer, version) {
            (1, _) => (12 * bitrate / rate + padding) * 4,
            (3, 2 | 25) => 72 * bitrate / rate + padding,
            _ => 144 * bitrate / rate + padding,
        };
        Some(Self {
            version,
            layer,
            sample_rate,
            mono: d >> 6 == 0b11,
            len,
        })
    }

    fn same_stream(&self, other: &Self) -> bool {
        (self.version, self.layer, self.sample_rate)
            == (other.version, other.layer, other.sample_rate)
    }
}

fn read_header(input: &mut impl Read) -> io::Result<Option<[u8; 4]>> {
    let mut header = [0; 4];
    match input.read_exact(&mut header) {
        Ok(()) => Ok(Some(header)),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
        Err(e) => Err(e),
    }
}

/// Whether `count` frames of one stream follow each other from `start`.
pub(super) fn starts_with_frames(
    input: &mut dyn ReadSeek,
    start: u64,
    len: u64,
    count: usize,
) -> Result<bool, FormatError> {
    input.seek(SeekFrom::Start(start))?;
    let mut reader = BufReader::new(&mut *input);
    let mut pos = start;
    let mut first: Option<Frame> = None;
    for _ in 0..count {
        let Some(frame) = read_header(&mut reader)?.and_then(Frame::parse) else {
            return Ok(false);
        };
        if first.is_some_and(|f| !f.same_stream(&frame)) || pos + frame.len > len {
            return Ok(false);
        }
        first.get_or_insert(frame);
        pos += frame.len;
        reader.seek_relative(frame.len as i64 - 4)?;
    }
    Ok(true)
}

#[derive(Debug)]
struct Frames {
    first: Frame,
    /// After the last frame (zero bytes after it are dropped).
    end: u64,
}

/// Checks that frames fill everything from `start` to `end`. A last frame
/// cut short is still sound, so it's kept.
fn walk(input: &mut dyn ReadSeek, start: u64, end: u64) -> Result<Frames, FormatError> {
    input.seek(SeekFrom::Start(start))?;
    let mut reader = BufReader::with_capacity(64 * 1024, &mut *input);
    let mut pos = start;
    let mut first: Option<Frame> = None;
    while pos < end {
        let header = if end - pos >= 4 {
            read_header(&mut reader)?
        } else {
            None
        };
        let frame = header
            .and_then(Frame::parse)
            .filter(|f| first.is_none_or(|first| first.same_stream(f)));
        let Some(frame) = frame else {
            if first.is_some() && only_zeros(&mut reader, header, end - pos)? {
                break;
            }
            return Err(match first {
                None => FormatError::malformed("no MP3 frame where the sound should start"),
                Some(_) => FormatError::Unsupported(
                    "data that isn't sound between the MP3 frames, which could hold anything"
                        .into(),
                ),
            });
        };
        first.get_or_insert(frame);
        if frame.len > end - pos {
            pos = end;
            break;
        }
        pos += frame.len;
        reader.seek_relative(frame.len as i64 - 4)?;
    }
    let first = first.ok_or_else(|| FormatError::malformed("an MP3 file without any sound"))?;
    Ok(Frames { first, end: pos })
}

/// Whether the rest, `left` bytes starting with `header`, is all zeros.
fn only_zeros(
    reader: &mut impl Read,
    header: Option<[u8; 4]>,
    left: u64,
) -> Result<bool, FormatError> {
    if header.is_some_and(|h| h != [0; 4]) {
        return Ok(false);
    }
    let read = if header.is_some() { 4 } else { 0 };
    let mut rest = Vec::new();
    reader.take(left - read).read_to_end(&mut rest)?;
    Ok(rest.iter().all(|&b| b == 0))
}

/// The first frame may be silent and hold the stream's length and seek
/// table instead (Xing or Info, with LAME's details, or VBRI). Players need
/// it to show the length and seek, so it's kept, with the encoder's name.
fn info_frame(
    input: &mut dyn ReadSeek,
    start: u64,
    frame: Frame,
) -> Result<Option<Finding>, FormatError> {
    let side_info = match (frame.version, frame.mono) {
        (1, false) => 32,
        (1, true) | (_, false) => 17,
        (_, true) => 9,
    };
    input.seek(SeekFrom::Start(start))?;
    let mut data = Vec::new();
    (&mut *input).take(frame.len).read_to_end(&mut data)?;
    let at = |offset: usize, len: usize| data.get(offset..offset + len).unwrap_or_default();
    let xing = 4 + side_info;
    let (name, encoder) = match at(xing, 4) {
        b"Xing" | b"Info" => {
            let flags = u32::from_be_bytes(at(xing + 4, 4).try_into().unwrap_or_default());
            // Frames, bytes, table of contents and quality, when present.
            let fields: usize = [(1, 4), (2, 4), (4, 100), (8, 4)]
                .iter()
                .filter(|(bit, _)| flags & bit != 0)
                .map(|(_, len)| len)
                .sum();
            let encoder = String::from_utf8_lossy(at(xing + 8 + fields, 9)).into_owned();
            ("Xing", encoder)
        }
        _ if at(36, 4) == b"VBRI" => ("VBRI", String::new()),
        _ => return Ok(None),
    };
    let finding = Finding::new(
        FindingKind::TechnicalInfo,
        Location::new([name]),
        Disposition::Keep(KeepReason::RequiredToPlay),
    );
    Ok(Some(match text(&encoder) {
        Some(value)
            if encoder
                .chars()
                .all(|c| c.is_ascii_graphic() || c == ' ' || c == '\0') =>
        {
            finding.with_value(value)
        }
        _ => finding,
    }))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// A 128 kbit/s, 44.1 kHz stereo MPEG-1 layer III frame of 417 bytes.
    pub(crate) fn frame() -> Vec<u8> {
        let mut frame = vec![0u8; 417];
        frame[..4].copy_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
        frame
    }

    fn frames(n: usize) -> Vec<u8> {
        frame().repeat(n)
    }

    fn clean(data: &[u8]) -> Result<(Vec<u8>, Vec<Finding>), FormatError> {
        let mut out = Vec::new();
        let findings = Mp3.clean(&mut Cursor::new(data), &mut out, &Options::default())?;
        Ok((out, findings))
    }

    #[test]
    fn reads_frame_lengths() {
        let frame = Frame::parse([0xFF, 0xFB, 0x90, 0x00]).unwrap();
        assert_eq!(
            (frame.version, frame.layer, frame.sample_rate, frame.len),
            (1, 3, 44100, 417)
        );
        let padded = Frame::parse([0xFF, 0xFB, 0x92, 0x00]).unwrap();
        assert_eq!(padded.len, 418);
        // MPEG-2 layer III, 64 kbit/s, 22.05 kHz.
        assert_eq!(Frame::parse([0xFF, 0xF3, 0x80, 0xC0]).unwrap().len, 208);
        // Free format, reserved values and ADTS (layer 0) aren't frames.
        for header in [
            [0xFF, 0xFB, 0x00, 0],
            [0xFF, 0xFB, 0xF0, 0],
            [0xFF, 0xFB, 0x9C, 0],
            [0xFF, 0xF1, 0x50, 0x80],
        ] {
            assert_eq!(Frame::parse(header), None, "{header:x?}");
        }
    }

    #[test]
    fn only_the_frames_are_kept() {
        let mut v1 = vec![0u8; 128];
        v1[..3].copy_from_slice(b"TAG");
        v1[33..41].copy_from_slice(b"Jane Doe");
        v1[127] = 0xFF;
        let id3 = [
            b"ID3\x03\0\0\0\0\0\x0f".as_slice(),
            b"TIT2\0\0\0\x05\0\0\0Song",
        ]
        .concat();
        let sound = frames(5);
        let data = [id3, vec![0; 10], sound.clone(), v1].concat();
        let (out, findings) = clean(&data).unwrap();
        assert_eq!(out, sound);
        let kinds: Vec<_> = findings.iter().map(|f| f.kind).collect();
        assert_eq!(kinds, [FindingKind::Description, FindingKind::PersonName]);
        // Already clean: nothing found, nothing changed.
        let (again, findings) = clean(&out).unwrap();
        assert_eq!((again, findings), (sound, Vec::new()));
    }

    #[test]
    fn anything_between_frames_is_refused() {
        let data = [frames(3), b"hidden".to_vec(), frames(3)].concat();
        assert!(matches!(clean(&data), Err(FormatError::Unsupported(_))));
    }

    #[test]
    fn a_last_frame_cut_short_and_zeros_after_it_are_fine() {
        let cut = [frames(3), frame()[..100].to_vec()].concat();
        assert_eq!(clean(&cut).unwrap().0, cut);
        let zeros = [frames(3), vec![0; 50]].concat();
        assert_eq!(clean(&zeros).unwrap().0, frames(3));
    }

    /// Found by the `audio` fuzz target: a tag hidden behind trailing zeros
    /// was only found when the cleaned file was read again.
    #[test]
    fn fuzz_tag_before_trailing_zeros() {
        let ape = crate::format::audio::ape::tests::ape_tag(&[("Artist", b"Jane Doe", 0)]);
        // The tag fills the end of the last frame, and a zero follows.
        let mut last = frame();
        let at = last.len() - ape.len();
        last[at..].copy_from_slice(&ape);
        let data = [frames(2), last.clone(), vec![0]].concat();
        let (out, findings) = clean(&data).unwrap();
        assert_eq!(out, [frames(2), last[..at].to_vec()].concat());
        assert_eq!(findings[0].kind, FindingKind::PersonName);
        assert_eq!(clean(&out).unwrap(), (out.clone(), Vec::new()));
    }

    #[test]
    fn detection_needs_several_frames_in_a_row() {
        let data = frames(3);
        let len = data.len() as u64;
        assert!(starts_with_frames(&mut Cursor::new(&data), 0, len, 3).unwrap());
        let one = [frame(), vec![1; 900]].concat();
        assert!(!starts_with_frames(&mut Cursor::new(&one), 0, one.len() as u64, 3).unwrap());
    }

    #[test]
    fn the_xing_frame_is_kept_with_its_encoder() {
        let mut info = frame();
        info[36..40].copy_from_slice(b"Info");
        info[40..44].copy_from_slice(&0x0Fu32.to_be_bytes());
        let lame = 36 + 8 + 4 + 4 + 100 + 4;
        info[lame..lame + 9].copy_from_slice(b"LAME3.100");
        let data = [info.clone(), frames(2)].concat();
        let (out, findings) = clean(&data).unwrap();
        assert_eq!(out, data);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].disposition,
            Disposition::Keep(KeepReason::RequiredToPlay)
        );
        assert_eq!(findings[0].value, text("LAME3.100"));
    }
}
