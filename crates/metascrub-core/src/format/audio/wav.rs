//! WAV: a RIFF file of chunks, each a four-letter name, a little-endian
//! size and its data, padded to an even length. The format (`fmt `), the
//! sound (`data`) and what players and samplers need (`fact`, cue points,
//! loops, instrument settings) are kept, without the sampler's model and
//! SMPTE time; tags (`LIST INFO`), cue labels,
//! broadcast metadata (`bext`), iXML, XMP, ID3 and any other chunk go.
//! The sound is copied byte for byte. Files over 4 GB (RF64) and object
//! audio (ADM, as Dolby Atmos masters carry) are refused: one can't be read
//! as plain RIFF, the other can't play without the chunks that describe it.

use std::io::{self, BufReader, Read, SeekFrom, Write};

use super::{Tag, id3, read_at, text, trailing_tags};
use crate::format::bytes::{Cursor, latin1};
use crate::format::{FormatError, FormatHandler, ReadSeek, xmp};
use crate::limits::MAX_AUDIO_TAG_BYTES;
use crate::model::{Disposition, Finding, FindingKind, KeepReason, Location, Value};
use crate::options::Options;

/// Chunks kept as they are.
const KEPT: &[&[u8; 4]] = &[b"fmt ", b"fact", b"data", b"cue ", b"smpl", b"inst"];

#[derive(Debug)]
pub(crate) struct Wav;

impl FormatHandler for Wav {
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
        let size = 4 + layout
            .kept
            .iter()
            .map(|c| 8 + c.len + (c.len & 1))
            .sum::<u64>();
        let size =
            u32::try_from(size).map_err(|_| FormatError::malformed("a WAV file over 4 GB"))?;
        output.write_all(b"RIFF")?;
        output.write_all(&size.to_le_bytes())?;
        output.write_all(b"WAVE")?;
        for chunk in &layout.kept {
            output.write_all(&chunk.id)?;
            // Fits: it came from a 32-bit size.
            output.write_all(&(chunk.len as u32).to_le_bytes())?;
            if let Some(data) = &chunk.replaced {
                output.write_all(data)?;
            } else {
                input.seek(SeekFrom::Start(chunk.at))?;
                if io::copy(&mut (&mut *input).take(chunk.len), output)? != chunk.len {
                    return Err(FormatError::malformed(
                        "the file got shorter while it was cleaned",
                    ));
                }
            }
            if chunk.len & 1 == 1 {
                output.write_all(&[0])?;
            }
        }
        Ok(layout.findings)
    }
}

#[derive(Debug)]
struct Chunk {
    id: [u8; 4],
    at: u64,
    len: u64,
    /// What to write instead of the original data, when some is blanked.
    replaced: Option<Vec<u8>>,
}

#[derive(Debug)]
struct Layout {
    kept: Vec<Chunk>,
    findings: Vec<Finding>,
}

fn scan(input: &mut dyn ReadSeek) -> Result<Layout, FormatError> {
    let len = input.seek(SeekFrom::End(0))?;
    let header = read_at(input, 0, 12.min(len))?;
    let mut cursor = Cursor::new(&header);
    let magic = cursor.array::<4>()?;
    let size = u64::from(cursor.u32_le()?);
    if matches!(&magic, b"RF64" | b"BW64") {
        return Err(FormatError::Unsupported(
            "WAV files over 4 GB (RF64)".into(),
        ));
    }
    if &magic != b"RIFF" || cursor.take(4)? != b"WAVE" {
        return Err(FormatError::malformed("missing the WAV signature"));
    }
    let riff_end = 8 + size;
    if riff_end > len {
        return Err(FormatError::malformed("the WAV file is cut short"));
    }

    let mut kept = Vec::new();
    let mut findings = Vec::new();
    let mut has_format = false;
    let mut has_sound = false;
    let mut pos = 12;
    while pos < riff_end {
        if riff_end - pos < 8 {
            return Err(FormatError::malformed("a WAV chunk cut short"));
        }
        let head = read_at(input, pos, 8)?;
        let mut cursor = Cursor::new(&head);
        let id = cursor.array::<4>()?;
        let chunk_len = u64::from(cursor.u32_le()?);
        let at = pos + 8;
        if at + chunk_len > riff_end {
            return Err(FormatError::malformed(
                "a WAV chunk runs past the end of the file",
            ));
        }
        match &id {
            b"fmt " if has_format => {
                return Err(FormatError::malformed("two WAV format chunks"));
            }
            b"fmt " => has_format = true,
            b"data" if !has_format || has_sound => {
                return Err(FormatError::malformed(
                    "WAV sound without one format before it",
                ));
            }
            b"data" => has_sound = true,
            // Compressed sound's length in samples.
            b"fact" => {}
            b"cue " | b"smpl" | b"inst" => findings.push(
                Finding::new(
                    FindingKind::TechnicalInfo,
                    Location::new([latin1(&id).trim_end()]),
                    Disposition::Keep(KeepReason::RequiredToPlay),
                )
                .with_value(Value::Bytes(chunk_len)),
            ),
            b"chna" | b"axml" | b"bxml" | b"sxml" => {
                return Err(FormatError::Unsupported(
                    "object audio (ADM, as in Dolby Atmos masters)".into(),
                ));
            }
            _ => findings.extend(removed(input, id, at, chunk_len)?),
        }
        if KEPT.contains(&&id) {
            let replaced = if &id == b"smpl" {
                sampler(input, at, chunk_len, &mut findings)?
            } else {
                None
            };
            kept.push(Chunk {
                id,
                at,
                len: chunk_len,
                replaced,
            });
        }
        // Writers sometimes leave out the pad byte of a last odd chunk.
        pos = (at + chunk_len + (chunk_len & 1)).min(riff_end);
    }
    if !has_sound {
        return Err(FormatError::malformed("a WAV file without sound"));
    }

    let mut trailing: Vec<Tag> = Vec::new();
    let tags_start = trailing_tags(input, riff_end, len, &mut trailing)?;
    if !is_zero(input, riff_end, tags_start - riff_end)? {
        findings.push(
            Finding::new(
                FindingKind::TrailingData,
                Location::new(["After the end of the file"]),
                Disposition::Remove,
            )
            .with_value(Value::Bytes(tags_start - riff_end)),
        );
    }
    for tag in trailing.into_iter().rev() {
        findings.extend(tag.findings);
    }
    Ok(Layout { kept, findings })
}

/// A sampler's settings: loops and tuning, needed to play it, but also
/// the maker and model of the sampler and a SMPTE time, which are blanked.
fn sampler(
    input: &mut dyn ReadSeek,
    at: u64,
    len: u64,
    findings: &mut Vec<Finding>,
) -> Result<Option<Vec<u8>>, FormatError> {
    if len > MAX_AUDIO_TAG_BYTES {
        return Err(FormatError::TooLarge {
            limit: MAX_AUDIO_TAG_BYTES,
        });
    }
    let mut data = read_at(input, at, len)?;
    let mut changed = false;
    let mut blank =
        |range: std::ops::Range<usize>, kind, name: &str, findings: &mut Vec<Finding>| {
            let Some(field) = data.get_mut(range) else {
                return;
            };
            if field.iter().all(|&b| b == 0) {
                return;
            }
            let value = field
                .chunks(4)
                .map(|n| u32::from_le_bytes(n.try_into().unwrap_or_default()).to_string())
                .collect::<Vec<_>>()
                .join(", ");
            field.fill(0);
            changed = true;
            findings.push(
                Finding::new(kind, Location::new(["smpl", name]), Disposition::Normalize)
                    .with_value(Value::Text(value)),
            );
        };
    blank(
        0..8,
        FindingKind::DeviceModel,
        "Manufacturer and product",
        findings,
    );
    blank(20..28, FindingKind::Timestamp, "SMPTE offset", findings);
    Ok(changed.then_some(data))
}

fn is_zero(input: &mut dyn ReadSeek, at: u64, len: u64) -> Result<bool, FormatError> {
    input.seek(SeekFrom::Start(at))?;
    for byte in BufReader::new((&mut *input).take(len)).bytes() {
        if byte? != 0 {
            return Ok(false);
        }
    }
    Ok(true)
}

fn finding(kind: FindingKind, location: Location, value: Option<Value>) -> Finding {
    let finding = Finding::new(kind, location, Disposition::Remove);
    match value {
        Some(value) => finding.with_value(value),
        None => finding,
    }
}

/// What a chunk cleaning removes held.
fn removed(
    input: &mut dyn ReadSeek,
    id: [u8; 4],
    at: u64,
    len: u64,
) -> Result<Vec<Finding>, FormatError> {
    let name = latin1(&id).trim_end().to_owned();
    let whole = |kind| {
        vec![finding(
            kind,
            Location::new([name.as_str()]),
            Some(Value::Bytes(len)),
        )]
    };
    match &id {
        b"JUNK" | b"junk" | b"PAD " | b"pad " | b"FLLR" => {
            // Padding is zeros; anything else in it could be hidden data.
            return Ok(if is_zero(input, at, len)? {
                Vec::new()
            } else {
                whole(FindingKind::UnknownMetadata)
            });
        }
        b"PEAK" | b"levl" | b"acid" => return Ok(whole(FindingKind::TechnicalInfo)),
        b"id3 " | b"ID3 " => {
            return match id3::v2_at(input, at, at + len) {
                Ok(Some(tag)) => Ok(tag.findings),
                Err(FormatError::Io(error)) => Err(FormatError::Io(error)),
                Ok(None) | Err(_) => Ok(whole(FindingKind::UnknownMetadata)),
            };
        }
        _ => {}
    }
    if len > MAX_AUDIO_TAG_BYTES {
        return Ok(whole(FindingKind::UnknownMetadata));
    }
    let data = read_at(input, at, len)?;
    let found = match &id {
        b"LIST" => match data.get(..4) {
            Some(b"INFO") => info(data.get(4..).unwrap_or_default()),
            Some(b"adtl") => cue_labels(data.get(4..).unwrap_or_default()),
            Some(b"wavl") => {
                return Err(FormatError::Unsupported(
                    "WAV sound split into a wave list".into(),
                ));
            }
            _ => None,
        },
        b"bext" => Some(broadcast(&data)),
        b"_PMX" => Some(xmp::inspect(&data, &Location::new(["XMP"]))),
        _ => None,
    };
    Ok(match found {
        Some(found) if !found.is_empty() => found,
        // A chunk the parsers can't read, or one with nothing in it.
        Some(_) => whole(FindingKind::TechnicalInfo),
        None if &id == b"LIST" => {
            let kind = data.get(..4).map(latin1).unwrap_or_default();
            vec![finding(
                FindingKind::UnknownMetadata,
                Location::new(["LIST".to_owned(), kind]),
                Some(Value::Bytes(len)),
            )]
        }
        None => whole(FindingKind::UnknownMetadata),
    })
}

/// `LIST INFO`: four-letter fields of text, `INAM` (title), `IART`
/// (artist)… `None` when it can't be read.
fn info(data: &[u8]) -> Option<Vec<Finding>> {
    let mut findings = Vec::new();
    for (id, value) in subchunks(data)? {
        let name = latin1(&id);
        let finding = finding(
            info_kind(&name),
            Location::new(["LIST INFO".to_owned(), name]),
            text(&String::from_utf8_lossy(value)),
        );
        findings.push(finding);
    }
    Some(findings)
}

fn info_kind(id: &str) -> FindingKind {
    use FindingKind::*;
    match id {
        "INAM" | "IPRD" | "IGNR" | "IKEY" | "ISBJ" | "IARL" | "ISRF" | "IMED" => Description,
        "IART" | "IENG" | "ICOP" | "ITCH" | "ISRC" | "ICMS" => PersonName,
        "ICMT" => Comment,
        "ICRD" | "IDIT" | "ISMP" => Timestamp,
        "ISFT" => Software,
        "ITRK" | "IPRT" | "ILNG" | "IDPI" | "ILGT" | "ICRP" | "IDIM" | "IPLT" | "ISHP" => {
            TechnicalInfo
        }
        _ => CustomProperty,
    }
}

/// `LIST adtl`: names and notes given to cue points.
fn cue_labels(data: &[u8]) -> Option<Vec<Finding>> {
    let mut findings = Vec::new();
    for (id, value) in subchunks(data)? {
        // A cue point number, and for `ltxt` 16 more bytes of settings.
        let skip = if &id == b"ltxt" { 20 } else { 4 };
        let kind = if &id == b"note" {
            FindingKind::Comment
        } else {
            FindingKind::Description
        };
        let value = value.get(skip..).unwrap_or_default();
        findings.push(finding(
            kind,
            Location::new(["LIST adtl".to_owned(), latin1(&id)]),
            text(&String::from_utf8_lossy(value)),
        ));
    }
    Some(findings)
}

/// The chunks inside a `LIST`, each padded to an even length.
fn subchunks(data: &[u8]) -> Option<Vec<([u8; 4], &[u8])>> {
    let mut cursor = Cursor::new(data);
    let mut chunks = Vec::new();
    while !cursor.is_empty() {
        let id = cursor.array::<4>().ok()?;
        let len = cursor.u32_le().ok()? as usize;
        chunks.push((id, cursor.take(len).ok()?));
        if len % 2 == 1 && !cursor.is_empty() {
            cursor.skip(1).ok()?;
        }
    }
    Some(chunks)
}

/// Broadcast WAV's `bext`: fixed-size fields, then free text.
fn broadcast(data: &[u8]) -> Vec<Finding> {
    let field = |start: usize, end: usize| {
        data.get(start..end.min(data.len()))
            .and_then(|b| text(&String::from_utf8_lossy(b)))
    };
    let location = |name: &str| Location::new(["bext", name]);
    let mut findings = Vec::new();
    for (start, end, name, kind) in [
        (0, 256, "Description", FindingKind::Description),
        (256, 288, "Originator", FindingKind::PersonName),
        (288, 320, "OriginatorReference", FindingKind::DocumentId),
    ] {
        if let Some(value) = field(start, end) {
            findings.push(finding(kind, location(name), Some(value)));
        }
    }
    let date = [field(320, 330), field(330, 338)]
        .into_iter()
        .flatten()
        .filter_map(|v| match v {
            Value::Text(t) => Some(t),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    if !date.is_empty() {
        findings.push(finding(
            FindingKind::Timestamp,
            location("OriginationDate"),
            Some(Value::Text(date)),
        ));
    }
    let samples = data
        .get(338..346)
        .and_then(|b| b.try_into().ok())
        .map_or(0, u64::from_le_bytes);
    if samples != 0 {
        // Samples since midnight: the time of day the recording started.
        findings.push(finding(
            FindingKind::Timestamp,
            location("TimeReference"),
            Some(Value::Integer(i64::try_from(samples).unwrap_or(i64::MAX))),
        ));
    }
    if let Some(umid) = data.get(348..412).filter(|u| u.iter().any(|&b| b != 0)) {
        let hex: String = umid.iter().map(|b| format!("{b:02x}")).collect();
        findings.push(finding(
            FindingKind::DocumentId,
            location("UMID"),
            Some(Value::Text(hex.trim_end_matches('0').to_owned())),
        ));
    }
    if data
        .get(412..422)
        .is_some_and(|l| l.iter().any(|&b| b != 0))
    {
        findings.push(finding(
            FindingKind::TechnicalInfo,
            location("Loudness"),
            None,
        ));
    }
    if let Some(history) = data
        .get(602..)
        .and_then(|b| text(&String::from_utf8_lossy(b)))
    {
        findings.push(finding(
            FindingKind::Software,
            location("CodingHistory"),
            Some(history),
        ));
    }
    findings
}

#[cfg(test)]
mod tests {
    use std::io::Cursor as IoCursor;

    use super::*;

    const FORMAT: &[u8] = b"\x01\x00\x01\x00\x44\xac\x00\x00\x88\x58\x01\x00\x02\x00\x10\x00";

    fn chunk(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
        if data.len() % 2 == 1 {
            out.push(0);
        }
        out
    }

    fn wav(chunks: &[Vec<u8>]) -> Vec<u8> {
        let body = chunks.concat();
        [
            b"RIFF".as_slice(),
            &(body.len() as u32 + 4).to_le_bytes(),
            b"WAVE",
            &body,
        ]
        .concat()
    }

    fn list(kind: &[u8; 4], items: &[Vec<u8>]) -> Vec<u8> {
        chunk(b"LIST", &[kind.as_slice(), &items.concat()].concat())
    }

    fn clean(data: &[u8]) -> Result<(Vec<u8>, Vec<Finding>), FormatError> {
        let mut out = Vec::new();
        let findings = Wav.clean(&mut IoCursor::new(data), &mut out, &Options::default())?;
        Ok((out, findings))
    }

    fn sound() -> Vec<u8> {
        chunk(b"data", &[1, 2, 3, 4, 5, 6, 7])
    }

    #[test]
    fn tags_go_and_the_sound_stays() {
        let mut bext = vec![0u8; 602];
        bext[..9].copy_from_slice(b"Interview");
        bext[256..264].copy_from_slice(b"Jane Doe");
        bext[320..330].copy_from_slice(b"2026-03-14");
        bext[338] = 7;
        bext.extend_from_slice(b"A=PCM,F=48000\r\n");
        let data = wav(&[
            chunk(b"fmt ", FORMAT),
            chunk(b"bext", &bext),
            list(
                b"INFO",
                &[chunk(b"IART", b"Jane Doe\0"), chunk(b"ISFT", b"Lavf60\0")],
            ),
            chunk(b"JUNK", &[0; 28]),
            sound(),
            chunk(b"_PMX", b"<x:xmpmeta xmlns:x='adobe:ns:meta/'/>"),
            chunk(b"zzzz", b"anything"),
        ]);
        let (out, findings) = clean(&data).unwrap();
        assert_eq!(out, wav(&[chunk(b"fmt ", FORMAT), sound()]));
        let kinds: Vec<_> = findings.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            [
                FindingKind::Description,
                FindingKind::PersonName,
                FindingKind::Timestamp,
                FindingKind::Timestamp,
                FindingKind::Software,
                FindingKind::PersonName,
                FindingKind::Software,
                FindingKind::TechnicalInfo,
                FindingKind::UnknownMetadata,
            ]
        );
        assert_eq!(clean(&out).unwrap(), (out.clone(), Vec::new()));
    }

    #[test]
    fn cue_points_and_loops_are_kept_but_their_labels_go() {
        let cue = chunk(b"cue ", &[1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]);
        let smpl = chunk(b"smpl", &[0; 36]);
        let labels = list(b"adtl", &[chunk(b"labl", b"\x01\0\0\0Chorus\0")]);
        let fact = chunk(b"fact", &[7, 0, 0, 0]);
        let data = wav(&[
            chunk(b"fmt ", FORMAT),
            fact.clone(),
            cue.clone(),
            sound(),
            smpl.clone(),
            labels,
        ]);
        let (out, findings) = clean(&data).unwrap();
        assert_eq!(
            out,
            wav(&[chunk(b"fmt ", FORMAT), fact, cue, sound(), smpl])
        );
        assert_eq!(
            findings[0].disposition,
            Disposition::Keep(KeepReason::RequiredToPlay)
        );
        assert_eq!(findings[2].value, Some(Value::Text("Chorus".into())));
    }

    #[test]
    fn the_sampler_model_and_time_are_blanked() {
        let mut smpl = vec![0u8; 36];
        smpl[0] = 0x47; // Akai
        smpl[4] = 0x5E;
        smpl[24] = 9;
        let data = wav(&[chunk(b"fmt ", FORMAT), chunk(b"smpl", &smpl), sound()]);
        let (out, findings) = clean(&data).unwrap();
        assert_eq!(
            out,
            wav(&[chunk(b"fmt ", FORMAT), chunk(b"smpl", &[0; 36]), sound()])
        );
        let kinds: Vec<_> = findings.iter().map(|f| (f.kind, f.disposition)).collect();
        assert_eq!(
            kinds,
            [
                (
                    FindingKind::TechnicalInfo,
                    Disposition::Keep(KeepReason::RequiredToPlay)
                ),
                (FindingKind::DeviceModel, Disposition::Normalize),
                (FindingKind::Timestamp, Disposition::Normalize),
            ]
        );
        assert_eq!(clean(&out).unwrap().1.len(), 1);
    }

    #[test]
    fn data_after_the_file_goes() {
        let bare = wav(&[chunk(b"fmt ", FORMAT), sound()]);
        let mut v1 = vec![0u8; 128];
        v1[..3].copy_from_slice(b"TAG");
        v1[33..41].copy_from_slice(b"Jane Doe");
        let data = [bare.as_slice(), b"appended", &v1].concat();
        let (out, findings) = clean(&data).unwrap();
        assert_eq!(out, bare);
        assert_eq!(findings[0].kind, FindingKind::TrailingData);
        assert_eq!(findings[1].kind, FindingKind::PersonName);
    }

    #[test]
    fn what_cant_be_cleaned_safely_is_refused() {
        let rf64 = [
            b"RF64\xff\xff\xff\xffWAVE".as_slice(),
            &chunk(b"ds64", &[0; 28]),
        ]
        .concat();
        assert!(matches!(clean(&rf64), Err(FormatError::Unsupported(_))));
        let adm = wav(&[chunk(b"fmt ", FORMAT), chunk(b"chna", &[0; 4]), sound()]);
        assert!(matches!(clean(&adm), Err(FormatError::Unsupported(_))));
        let wavl = wav(&[chunk(b"fmt ", FORMAT), list(b"wavl", &[sound()])]);
        assert!(matches!(clean(&wavl), Err(FormatError::Unsupported(_))));
        let mut cut = wav(&[chunk(b"fmt ", FORMAT), sound()]);
        cut.truncate(cut.len() - 2);
        assert!(matches!(clean(&cut), Err(FormatError::Malformed(_))));
        let no_format = wav(&[sound()]);
        assert!(matches!(clean(&no_format), Err(FormatError::Malformed(_))));
    }
}
