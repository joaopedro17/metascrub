//! Music and sound: MP3, FLAC, Ogg Vorbis and Opus. Tags sit before or after the sound and are
//! removed whole; the sound itself is copied byte for byte, so it plays
//! exactly as before.

mod ape;
mod flac;
mod id3;
mod mp3;
mod ogg;
mod vorbis;

use std::io::{Read, SeekFrom};

pub(super) use flac::Flac;
pub(super) use mp3::Mp3;
pub(super) use ogg::{Ogg, is_supported as is_ogg};

use super::{FormatError, ReadSeek};
use crate::limits::MAX_AUDIO_TAG_BYTES;
use crate::model::{Finding, FormatId, Value};

/// A tag found in the file, removed whole by cleaning.
#[derive(Debug)]
struct Tag {
    start: u64,
    end: u64,
    findings: Vec<Finding>,
}

/// Longest text value kept for a finding.
const MAX_VALUE_CHARS: usize = 1000;

/// Detects audio that has no fixed signature at the very start: an ID3 tag
/// or bare MP3 frames. `None` if it's neither.
pub(super) fn detect(input: &mut dyn ReadSeek) -> Result<Option<FormatId>, FormatError> {
    let len = input.seek(SeekFrom::End(0))?;
    let mut start = 0;
    let mut tagged = false;
    while let Some(end) = id3::v2_end(input, start, len)? {
        start = end;
        tagged = true;
    }
    if tagged && read_at(input, start, 4.min(len - start))? == b"fLaC" {
        return Ok(Some(FormatId::Flac));
    }
    // Two frames after a tag, three without: a few bytes of bare frame
    // header can match other files by chance.
    let frames = if tagged { 2 } else { mp3::UNTAGGED_FRAMES };
    Ok(mp3::starts_with_frames(input, start, len, frames)?.then_some(FormatId::Mp3))
}

/// Reads `len` bytes at `pos`, up to the limit for tags held in memory.
fn read_at(input: &mut dyn ReadSeek, pos: u64, len: u64) -> Result<Vec<u8>, FormatError> {
    if len > MAX_AUDIO_TAG_BYTES {
        return Err(FormatError::TooLarge {
            limit: MAX_AUDIO_TAG_BYTES,
        });
    }
    input.seek(SeekFrom::Start(pos))?;
    let mut data = Vec::new();
    (&mut *input).take(len).read_to_end(&mut data)?;
    if data.len() as u64 != len {
        return Err(FormatError::malformed(
            "a tag runs past the end of the file",
        ));
    }
    Ok(data)
}

/// A text value, cut to a sensible length, with NUL padding dropped.
fn text(value: &str) -> Option<Value> {
    let value = value.trim_matches(|c: char| c == '\0' || c.is_whitespace());
    (!value.is_empty()).then(|| Value::Text(value.chars().take(MAX_VALUE_CHARS).collect()))
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
