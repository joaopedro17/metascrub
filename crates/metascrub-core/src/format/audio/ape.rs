//! Tags some players and taggers append after the sound: APEv1/APEv2
//! (named items, ended by a 32-byte footer) and Lyrics3 (song lyrics and a
//! few fields).

use super::{Tag, read_at, text};
use crate::format::bytes::{Cursor, latin1};
use crate::format::{FormatError, ReadSeek};
use crate::limits::MAX_AUDIO_TAG_BYTES;
use crate::model::{Disposition, Finding, FindingKind, Location, Value};

const APE_FOOTER_LEN: u64 = 32;
const HAS_HEADER: u32 = 1 << 31;
const BINARY: u32 = 0b110;

const LYRICS_BEGIN: &[u8] = b"LYRICSBEGIN";
/// Lyrics3 version 1 has no size: its start is searched for, this far back.
const LYRICS_V1_MAX_LEN: u64 = 5100 + 11 + 9;

/// The APE tag that ends at `end`.
pub(super) fn ape_before(
    input: &mut dyn ReadSeek,
    floor: u64,
    end: u64,
) -> Result<Option<Tag>, FormatError> {
    let Some(footer_at) = end.checked_sub(APE_FOOTER_LEN).filter(|&p| p >= floor) else {
        return Ok(None);
    };
    let footer = read_at(input, footer_at, APE_FOOTER_LEN)?;
    let mut cursor = Cursor::new(&footer);
    if cursor.take(8)? != b"APETAGEX" {
        return Ok(None);
    }
    let version = cursor.u32_le()?;
    let size = u64::from(cursor.u32_le()?);
    let count = cursor.u32_le()?;
    let flags = cursor.u32_le()?;
    let header = if flags & HAS_HEADER != 0 {
        APE_FOOTER_LEN
    } else {
        0
    };
    let start = end
        .checked_sub(size)
        .and_then(|p| p.checked_sub(header))
        .filter(|&p| p >= floor && size >= APE_FOOTER_LEN)
        .ok_or_else(|| FormatError::malformed("an APE tag claims more than the file holds"))?;
    let label = format!("APEv{}", version / 1000);
    let items_len = size - APE_FOOTER_LEN;
    let findings = if items_len > MAX_AUDIO_TAG_BYTES {
        vec![block(&label, FindingKind::UnknownMetadata, items_len)]
    } else {
        let items = read_at(input, footer_at - items_len, items_len)?;
        items_findings(&label, &items, count)
    };
    Ok(Some(Tag {
        start,
        end,
        findings,
    }))
}

fn block(label: &str, kind: FindingKind, len: u64) -> Finding {
    Finding::new(kind, Location::new([label]), Disposition::Remove).with_value(Value::Bytes(len))
}

fn items_findings(label: &str, items: &[u8], count: u32) -> Vec<Finding> {
    let mut cursor = Cursor::new(items);
    let mut findings = Vec::new();
    for _ in 0..count {
        let Some((key, value, flags)) = item(&mut cursor) else {
            findings.push(block(
                label,
                FindingKind::UnknownMetadata,
                cursor.remaining().len() as u64,
            ));
            break;
        };
        let value = if flags & BINARY == 0 {
            text(&String::from_utf8_lossy(value))
        } else {
            Some(Value::Bytes(value.len() as u64))
        };
        let finding = Finding::new(
            kind_of(&key),
            Location::new([label.to_owned(), key]),
            Disposition::Remove,
        );
        findings.push(match value {
            Some(value) => finding.with_value(value),
            None => finding,
        });
    }
    if findings.is_empty() {
        findings.push(block(label, FindingKind::TechnicalInfo, items.len() as u64));
    }
    findings
}

fn item<'a>(cursor: &mut Cursor<'a>) -> Option<(String, &'a [u8], u32)> {
    let len = usize::try_from(cursor.u32_le().ok()?).ok()?;
    let flags = cursor.u32_le().ok()?;
    let rest = cursor.remaining();
    let key_len = rest.iter().position(|&b| b == 0)?;
    let key = latin1(rest.get(..key_len)?);
    cursor.skip(key_len + 1).ok()?;
    let value = cursor.take(len).ok()?;
    Some((key, value, flags))
}

fn kind_of(key: &str) -> FindingKind {
    use FindingKind::*;
    let key = key.to_ascii_lowercase();
    if key.starts_with("cover art") {
        return CoverArt;
    }
    if ["replaygain", "mp3gain", "r128"]
        .iter()
        .any(|p| key.starts_with(p))
    {
        return TechnicalInfo;
    }
    if [
        "musicbrainz",
        "acoustid",
        "isrc",
        "barcode",
        "catalog",
        "ean/upc",
        "isbn",
        "lc",
    ]
    .iter()
    .any(|p| key.starts_with(p))
    {
        return DocumentId;
    }
    match key.as_str() {
        "title" | "subtitle" | "album" | "genre" | "lyrics" | "debut album" | "index"
        | "related" | "abstract" | "bibliography" => Description,
        "artist" | "album artist" | "composer" | "conductor" | "performer" | "writer"
        | "copyright" | "lyricist" | "arranger" => PersonName,
        "publisher" | "label" => Organization,
        "comment" => Comment,
        "year" | "record date" | "file date" | "date" => Timestamp,
        "tool name" | "tool version" | "encoder" | "encoded by" => Software,
        "record location" => GpsLocation,
        "file" => DownloadUrl,
        "track" | "disc" | "media" | "language" | "bpm" => TechnicalInfo,
        _ => CustomProperty,
    }
}

/// Lyrics3 (version 1 or 2) that ends at `end`.
pub(super) fn lyrics_before(
    input: &mut dyn ReadSeek,
    floor: u64,
    end: u64,
) -> Result<Option<Tag>, FormatError> {
    let Some(marker_at) = end.checked_sub(9).filter(|&p| p >= floor) else {
        return Ok(None);
    };
    let marker = read_at(input, marker_at, 9)?;
    let start = match marker.as_slice() {
        b"LYRICS200" => {
            let Some(size_at) = marker_at.checked_sub(6).filter(|&p| p >= floor) else {
                return Ok(None);
            };
            let size = read_at(input, size_at, 6)?;
            let Some(size) = std::str::from_utf8(&size)
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
            else {
                return Ok(None);
            };
            size_at.checked_sub(size).filter(|&p| p >= floor)
        }
        b"LYRICSEND" => {
            let from = marker_at.saturating_sub(LYRICS_V1_MAX_LEN).max(floor);
            let area = read_at(input, from, marker_at - from)?;
            area.windows(LYRICS_BEGIN.len())
                .rposition(|w| w == LYRICS_BEGIN)
                .map(|i| from + i as u64)
        }
        _ => return Ok(None),
    };
    let Some(start) = start else {
        return Ok(None);
    };
    if read_at(input, start, LYRICS_BEGIN.len() as u64)? != LYRICS_BEGIN {
        return Ok(None);
    }
    Ok(Some(Tag {
        start,
        end,
        findings: vec![block("Lyrics3", FindingKind::Description, end - start)],
    }))
}

#[cfg(test)]
pub(super) mod tests {
    use std::io;

    use super::*;

    pub(crate) fn ape_tag(items: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (key, value, flags) in items {
            body.extend_from_slice(&(value.len() as u32).to_le_bytes());
            body.extend_from_slice(&flags.to_le_bytes());
            body.extend_from_slice(key.as_bytes());
            body.push(0);
            body.extend_from_slice(value);
        }
        let mut footer = b"APETAGEX".to_vec();
        footer.extend_from_slice(&2000u32.to_le_bytes());
        footer.extend_from_slice(&(body.len() as u32 + 32).to_le_bytes());
        footer.extend_from_slice(&(items.len() as u32).to_le_bytes());
        footer.extend_from_slice(&0u32.to_le_bytes());
        footer.extend_from_slice(&[0; 8]);
        [body, footer].concat()
    }

    #[test]
    fn reads_ape_items() {
        let tag = ape_tag(&[
            ("Artist", b"Jane Doe", 0),
            ("Cover Art (Front)", b"cover.jpg\0\xFF\xD8", 2),
            ("REPLAYGAIN_TRACK_GAIN", b"-6.2 dB", 0),
        ]);
        let data = [b"audio".as_slice(), &tag].concat();
        let found = ape_before(&mut io::Cursor::new(&data), 0, data.len() as u64)
            .unwrap()
            .unwrap();
        assert_eq!(found.start, 5);
        let kinds: Vec<_> = found.findings.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            [
                FindingKind::PersonName,
                FindingKind::CoverArt,
                FindingKind::TechnicalInfo
            ]
        );
        assert_eq!(
            found.findings[0].value,
            Some(Value::Text("Jane Doe".into()))
        );
    }

    #[test]
    fn finds_both_lyrics_versions() {
        let v2 = [
            b"audio".as_slice(),
            b"LYRICSBEGININD0000210",
            b"000021LYRICS200",
        ]
        .concat();
        let tag = lyrics_before(&mut io::Cursor::new(&v2), 0, v2.len() as u64)
            .unwrap()
            .unwrap();
        assert_eq!(tag.start, 5);
        let v1 = [b"audio".as_slice(), b"LYRICSBEGINla la", b"LYRICSEND"].concat();
        let tag = lyrics_before(&mut io::Cursor::new(&v1), 0, v1.len() as u64)
            .unwrap()
            .unwrap();
        assert_eq!(tag.start, 5);
    }

    #[test]
    fn a_footer_claiming_too_much_is_damage() {
        let mut tag = ape_tag(&[("Title", b"x", 0)]);
        let at = tag.len() - 32 + 12;
        tag[at..at + 4].copy_from_slice(&1000u32.to_le_bytes());
        assert!(ape_before(&mut io::Cursor::new(&tag), 0, tag.len() as u64).is_err());
    }
}
