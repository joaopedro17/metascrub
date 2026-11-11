//! ID3 tags: ID3v2 (versions 2.2 to 2.4) before the sound, or after it
//! with a footer, and the fixed-size ID3v1 and "TAG+" blocks at the very
//! end. Cleaning removes them whole, so their content is only read to say
//! what they hold.

use std::io::SeekFrom;

use super::{Tag, read_at, text};
use crate::format::bytes::latin1;
use crate::format::{FormatError, ReadSeek};
use crate::limits::MAX_AUDIO_TAG_BYTES;
use crate::model::{Disposition, Finding, FindingKind, Location, Value};

const HEADER_LEN: u64 = 10;
const V1_LEN: u64 = 128;
const ENHANCED_LEN: u64 = 227;

const UNSYNCHRONISED: u8 = 0x80;
const EXTENDED_HEADER: u8 = 0x40;
const FOOTER: u8 = 0x10;

#[derive(Debug, Clone, Copy)]
struct Header {
    major: u8,
    flags: u8,
    /// Of the frames and padding, without header and footer.
    size: u64,
}

impl Header {
    fn parse(bytes: &[u8], magic: &[u8; 3]) -> Option<Self> {
        let (start, rest) = bytes.split_at_checked(3)?;
        let [major, revision, flags, size @ ..] = rest else {
            return None;
        };
        let size: [u8; 4] = size.get(..4)?.try_into().ok()?;
        if start != magic || !(2..=4).contains(major) || *revision == 0xFF {
            return None;
        }
        Some(Self {
            major: *major,
            flags: *flags,
            size: syncsafe(size)?,
        })
    }

    fn total_len(self) -> u64 {
        let footer = if self.major == 4 && self.flags & FOOTER != 0 {
            HEADER_LEN
        } else {
            0
        };
        HEADER_LEN + self.size + footer
    }

    fn label(self) -> String {
        format!("ID3v2.{}", self.major)
    }
}

/// Four 7-bit bytes: the top bit of each must be clear.
fn syncsafe(bytes: [u8; 4]) -> Option<u64> {
    bytes.iter().try_fold(0u64, |size, &b| {
        (b < 0x80).then_some((size << 7) | u64::from(b))
    })
}

fn read_header(
    input: &mut dyn ReadSeek,
    pos: u64,
    len: u64,
    magic: &[u8; 3],
) -> Result<Option<Header>, FormatError> {
    if pos.saturating_add(HEADER_LEN) > len {
        return Ok(None);
    }
    input.seek(SeekFrom::Start(pos))?;
    let mut bytes = [0; HEADER_LEN as usize];
    input.read_exact(&mut bytes)?;
    Ok(Header::parse(&bytes, magic))
}

/// Where the ID3v2 tag at `pos` ends, if one starts there and fits.
pub(super) fn v2_end(
    input: &mut dyn ReadSeek,
    pos: u64,
    len: u64,
) -> Result<Option<u64>, FormatError> {
    Ok(read_header(input, pos, len, b"ID3")?
        .map(|header| pos + header.total_len())
        .filter(|&end| end <= len))
}

/// The ID3v2 tag at `pos`.
pub(super) fn v2_at(
    input: &mut dyn ReadSeek,
    pos: u64,
    len: u64,
) -> Result<Option<Tag>, FormatError> {
    let Some(header) = read_header(input, pos, len, b"ID3")? else {
        return Ok(None);
    };
    let end = pos + header.total_len();
    if end > len {
        return Err(FormatError::malformed(
            "an ID3 tag runs past the end of the file",
        ));
    }
    Ok(Some(Tag {
        start: pos,
        end,
        findings: v2_findings(input, pos, header)?,
    }))
}

/// An ID3v2 tag that ends at `end` with a footer (only version 2.4 has one).
pub(super) fn v2_footer_before(
    input: &mut dyn ReadSeek,
    floor: u64,
    end: u64,
) -> Result<Option<Tag>, FormatError> {
    let Some(footer_at) = end.checked_sub(HEADER_LEN).filter(|&p| p >= floor) else {
        return Ok(None);
    };
    let Some(footer) = read_header(input, footer_at, end, b"3DI")? else {
        return Ok(None);
    };
    let Some(start) = end.checked_sub(footer.total_len()).filter(|&p| p >= floor) else {
        return Ok(None);
    };
    match v2_at(input, start, end)? {
        Some(tag) if tag.end == end => Ok(Some(tag)),
        _ => Ok(None),
    }
}

fn whole(label: String, size: u64) -> Finding {
    Finding::new(
        FindingKind::UnknownMetadata,
        Location::new([label]),
        Disposition::Remove,
    )
    .with_value(Value::Bytes(size))
}

fn v2_findings(
    input: &mut dyn ReadSeek,
    pos: u64,
    header: Header,
) -> Result<Vec<Finding>, FormatError> {
    let label = header.label();
    // Version 2.2 uses this flag for compression, which no one implemented.
    let compressed = header.major == 2 && header.flags & EXTENDED_HEADER != 0;
    if header.size > MAX_AUDIO_TAG_BYTES || compressed {
        return Ok(vec![whole(label, header.size)]);
    }
    let mut body = read_at(input, pos + HEADER_LEN, header.size)?;
    if header.flags & UNSYNCHRONISED != 0 && header.major < 4 {
        body = resynchronise(&body);
    }
    let mut at = 0usize;
    if header.flags & EXTENDED_HEADER != 0 {
        let size = body
            .get(..4)
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .map(|b| match header.major {
                3 => u64::from(u32::from_be_bytes(b)) + 4,
                _ => syncsafe(b).unwrap_or(u64::MAX),
            });
        match size
            .and_then(|s| usize::try_from(s).ok())
            .filter(|&s| s <= body.len())
        {
            Some(size) => at = size,
            None => return Ok(vec![whole(label, header.size)]),
        }
    }
    let mut findings = Vec::new();
    let id_len = if header.major == 2 { 3 } else { 4 };
    let header_len = if header.major == 2 { 6 } else { 10 };
    while let Some(frame) = body.get(at..at + header_len) {
        let id = frame.get(..id_len).unwrap_or_default();
        if id.first() == Some(&0) {
            break; // Padding.
        }
        let size = frame
            .get(id_len..id_len + if header.major == 2 { 3 } else { 4 })
            .unwrap_or_default();
        let size = match header.major {
            2 => size.iter().fold(0usize, |s, &b| (s << 8) | usize::from(b)),
            3 => size.iter().fold(0usize, |s, &b| (s << 8) | usize::from(b)),
            _ => <[u8; 4]>::try_from(size)
                .ok()
                .and_then(syncsafe)
                .and_then(|s| usize::try_from(s).ok())
                .unwrap_or(usize::MAX),
        };
        let data = at
            .checked_add(header_len)
            .and_then(|start| Some((start, start.checked_add(size)?)))
            .and_then(|(start, end)| body.get(start..end));
        let valid_id = id
            .iter()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
        let (Some(data), true) = (data, valid_id) else {
            // Damaged or not frames at all: reported as one block.
            let rest = body.len().saturating_sub(at);
            findings.push(whole(label.clone(), rest as u64));
            break;
        };
        let flags = frame.get(8..10).unwrap_or_default();
        findings.push(frame_finding(&label, header.major, id, flags, data));
        at += header_len + size;
    }
    if findings.is_empty() {
        findings.push(
            Finding::new(
                FindingKind::TechnicalInfo,
                Location::new([label]),
                Disposition::Remove,
            )
            .with_value(Value::Bytes(header.size)),
        );
    }
    Ok(findings)
}

/// Undoes "unsynchronisation": a zero byte inserted after every 0xFF.
fn resynchronise(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut previous = 0u8;
    for &b in data {
        if !(previous == 0xFF && b == 0) {
            out.push(b);
        }
        previous = b;
    }
    out
}

fn frame_finding(label: &str, major: u8, raw_id: &[u8], flags: &[u8], data: &[u8]) -> Finding {
    let raw = latin1(raw_id);
    let id = if major == 2 {
        v22_name(&raw)
    } else {
        raw.as_str()
    };
    let location = Location::new([label.to_owned(), raw.clone()]);
    let format_flags = flags.get(1).copied().unwrap_or(0);
    // Compressed or encrypted frames can't be read; their size still counts.
    let unreadable = match major {
        3 => format_flags & 0xC0 != 0,
        4 => format_flags & 0x0C != 0,
        _ => false,
    };
    let mut payload = data.to_vec();
    if major == 4 && format_flags & 0x02 != 0 {
        payload = resynchronise(&payload);
    }
    if major == 4 && format_flags & 0x01 != 0 {
        payload = payload.get(4..).unwrap_or_default().to_vec();
    }
    let (kind, value) = if unreadable {
        (kind_of(id), Some(Value::Bytes(data.len() as u64)))
    } else {
        describe(id, &payload)
    };
    let finding = Finding::new(kind, location, Disposition::Remove);
    match value {
        Some(value) => finding.with_value(value),
        None => finding,
    }
}

fn describe(id: &str, data: &[u8]) -> (FindingKind, Option<Value>) {
    let bytes = Some(Value::Bytes(data.len() as u64));
    let Some((&encoding, rest)) = data.split_first() else {
        return (kind_of(id), None);
    };
    match id {
        "TXXX" => {
            let (name, value) = split_terminated(encoding, rest);
            let name = decode(encoding, name);
            let value = decode(encoding, value);
            (user_kind(&name), text(&format!("{name}: {value}")))
        }
        "WXXX" => {
            let (_, url) = split_terminated(encoding, rest);
            (FindingKind::CustomProperty, text(&latin1(url)))
        }
        "COMM" | "USLT" => {
            let after_language = rest.get(3..).unwrap_or_default();
            let (_, value) = split_terminated(encoding, after_language);
            (kind_of(id), text(&decode(encoding, value)))
        }
        _ if id.starts_with('T') => (kind_of(id), text(&decode(encoding, rest))),
        // URL frames have no encoding byte: all of it is the address.
        _ if id.starts_with('W') => (kind_of(id), text(&latin1(data))),
        _ => (kind_of(id), bytes),
    }
}

/// `TXXX` frames are named by whoever wrote them.
fn user_kind(name: &str) -> FindingKind {
    let name = name.to_ascii_lowercase();
    if ["replaygain", "mp3gain", "r128", "itunnorm", "itunsmpb"]
        .iter()
        .any(|p| name.starts_with(p))
    {
        FindingKind::TechnicalInfo
    } else if [
        "musicbrainz",
        "acoustid",
        "discogs",
        "isrc",
        "barcode",
        "upc",
        "catalog",
        "asin",
    ]
    .iter()
    .any(|p| name.contains(p))
    {
        FindingKind::DocumentId
    } else {
        FindingKind::CustomProperty
    }
}

fn kind_of(id: &str) -> FindingKind {
    use FindingKind::*;
    match id {
        "TIT1" | "TIT2" | "TIT3" | "TALB" | "TCON" | "TOAL" | "TSST" | "TMOO" | "TDES" | "TKWD"
        | "TCAT" | "TSOA" | "TSOT" | "TOFN" | "USLT" | "SYLT" | "CHAP" | "CTOC" | "WOAF"
        | "WOAR" | "WOAS" | "WORS" | "WPAY" | "WPUB" | "WCOM" | "WCOP" | "WFED" | "COMR"
        | "USER" | "LINK" => Description,
        "TPE1" | "TPE2" | "TPE3" | "TPE4" | "TCOM" | "TEXT" | "TOLY" | "TOPE" | "TENC" | "TMCL"
        | "TIPL" | "IPLS" | "TOWN" | "TCOP" | "TPRO" | "TSOP" | "TSO2" | "TSOC" | "POPM"
        | "OWNE" => PersonName,
        "TPUB" | "TRSN" | "TRSO" => Organization,
        "COMM" => Comment,
        "TYER" | "TDAT" | "TIME" | "TRDA" | "TDRC" | "TDRL" | "TDOR" | "TORY" | "TDEN" | "TDTG" => {
            Timestamp
        }
        "TSSE" => Software,
        "UFID" | "PRIV" | "MCDI" | "TSRC" | "GRID" | "TGID" => DocumentId,
        "APIC" => CoverArt,
        "GEOB" => EmbeddedFile,
        "TXXX" | "WXXX" => CustomProperty,
        "TRCK" | "TPOS" | "TBPM" | "TKEY" | "TLAN" | "TLEN" | "TMED" | "TFLT" | "TDLY" | "TSIZ"
        | "TCMP" | "PCST" | "RVA2" | "RVAD" | "EQU2" | "EQUA" | "RVRB" | "ETCO" | "MLLT"
        | "SYTC" | "RBUF" | "AENC" | "POSS" | "ASPI" | "SEEK" | "SIGN" | "ENCR" | "PCNT" => {
            TechnicalInfo
        }
        _ => UnknownMetadata,
    }
}

/// Version 2.2's three-letter names, as their later four-letter equivalents.
fn v22_name(id: &str) -> &str {
    match id {
        "TT1" => "TIT1",
        "TT2" => "TIT2",
        "TT3" => "TIT3",
        "TP1" => "TPE1",
        "TP2" => "TPE2",
        "TP3" => "TPE3",
        "TP4" => "TPE4",
        "TCM" => "TCOM",
        "TXT" => "TEXT",
        "TLA" => "TLAN",
        "TCO" => "TCON",
        "TAL" => "TALB",
        "TPA" => "TPOS",
        "TRK" => "TRCK",
        "TRC" => "TSRC",
        "TYE" => "TYER",
        "TDA" => "TDAT",
        "TIM" => "TIME",
        "TRD" => "TRDA",
        "TMT" => "TMED",
        "TFT" => "TFLT",
        "TBP" => "TBPM",
        "TCR" => "TCOP",
        "TPB" => "TPUB",
        "TEN" => "TENC",
        "TSS" => "TSSE",
        "TOF" => "TOFN",
        "TLE" => "TLEN",
        "TSI" => "TSIZ",
        "TDY" => "TDLY",
        "TKE" => "TKEY",
        "TOT" => "TOAL",
        "TOA" => "TOPE",
        "TOL" => "TOLY",
        "TOR" => "TORY",
        "TXX" => "TXXX",
        "WAF" => "WOAF",
        "WAR" => "WOAR",
        "WAS" => "WOAS",
        "WCM" => "WCOM",
        "WCP" => "WCOP",
        "WPB" => "WPUB",
        "WXX" => "WXXX",
        "IPL" => "IPLS",
        "ULT" => "USLT",
        "COM" => "COMM",
        "PIC" => "APIC",
        "GEO" => "GEOB",
        "UFI" => "UFID",
        "CNT" => "PCNT",
        "POP" => "POPM",
        "RVA" => "RVAD",
        "EQU" => "EQUA",
        "REV" => "RVRB",
        "SLT" => "SYLT",
        "STC" => "SYTC",
        "ETC" => "ETCO",
        "MLL" => "MLLT",
        "BUF" => "RBUF",
        "CRA" => "AENC",
        "LNK" => "LINK",
        "MCI" => "MCDI",
        other => other,
    }
}

/// Splits after a string ended by NUL: one byte in Latin-1 and UTF-8, two
/// (aligned) in UTF-16. Without a terminator, all of it is the string.
fn split_terminated(encoding: u8, data: &[u8]) -> (&[u8], &[u8]) {
    let wide = matches!(encoding, 1 | 2);
    let end = if wide {
        data.chunks(2).position(|c| c == [0, 0]).map(|i| i * 2)
    } else {
        data.iter().position(|&b| b == 0)
    };
    match end {
        Some(end) => {
            let skip = if wide { 2 } else { 1 };
            (
                data.get(..end).unwrap_or_default(),
                data.get(end + skip..).unwrap_or_default(),
            )
        }
        None => (data, &[]),
    }
}

/// Several values in one frame are separated by NULs; they're joined with "; ".
fn decode(encoding: u8, data: &[u8]) -> String {
    let text = match encoding {
        1 | 2 => {
            let mut little = encoding == 1;
            let mut data = data;
            if let Some(rest) = data.strip_prefix(&[0xFF, 0xFE]) {
                little = true;
                data = rest;
            } else if let Some(rest) = data.strip_prefix(&[0xFE, 0xFF]) {
                little = false;
                data = rest;
            }
            let units: Vec<u16> = data
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&pair| {
                    if little {
                        u16::from_le_bytes(pair)
                    } else {
                        u16::from_be_bytes(pair)
                    }
                })
                .collect();
            String::from_utf16_lossy(&units).replace('\u{FEFF}', "")
        }
        3 => String::from_utf8_lossy(data).into_owned(),
        _ => latin1(data),
    };
    text.split('\0')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

/// ID3v1 at the end, with the "TAG+" block that may precede it.
pub(super) fn v1_before(
    input: &mut dyn ReadSeek,
    floor: u64,
    end: u64,
) -> Result<Option<Tag>, FormatError> {
    let Some(start) = end.checked_sub(V1_LEN).filter(|&p| p >= floor) else {
        return Ok(None);
    };
    let block = read_at(input, start, V1_LEN)?;
    if block.get(..3) != Some(b"TAG") {
        return Ok(None);
    }
    let mut tag = Tag {
        start,
        end,
        findings: v1_findings(&block),
    };
    if let Some(enhanced) = start.checked_sub(ENHANCED_LEN).filter(|&p| p >= floor) {
        let block = read_at(input, enhanced, ENHANCED_LEN)?;
        if block.get(..4) == Some(b"TAG+") {
            let mut findings = enhanced_findings(&block);
            findings.append(&mut tag.findings);
            tag = Tag {
                start: enhanced,
                end,
                findings,
            };
        }
    }
    Ok(Some(tag))
}

fn field(block: &[u8], start: usize, len: usize) -> Option<Value> {
    text(&latin1(block.get(start..start + len).unwrap_or_default()))
}

fn v1_findings(block: &[u8]) -> Vec<Finding> {
    let location = |name: &str| Location::new(["ID3v1", name]);
    let mut findings = Vec::new();
    let mut add = |kind, name: &str, value: Option<Value>| {
        if let Some(value) = value {
            findings
                .push(Finding::new(kind, location(name), Disposition::Remove).with_value(value));
        }
    };
    add(FindingKind::Description, "Title", field(block, 3, 30));
    add(FindingKind::PersonName, "Artist", field(block, 33, 30));
    add(FindingKind::Description, "Album", field(block, 63, 30));
    add(FindingKind::Timestamp, "Year", field(block, 93, 4));
    // ID3v1.1 keeps a track number in the last two bytes of the comment.
    let track = block.get(125) == Some(&0) && block.get(126).is_some_and(|&t| t != 0);
    let comment_len = if track { 28 } else { 30 };
    add(
        FindingKind::Comment,
        "Comment",
        field(block, 97, comment_len),
    );
    if track {
        add(
            FindingKind::TechnicalInfo,
            "Track",
            block.get(126).map(|&t| Value::Integer(t.into())),
        );
    }
    if let Some(&genre) = block.get(127).filter(|&&g| g != 0xFF) {
        add(
            FindingKind::TechnicalInfo,
            "Genre",
            Some(Value::Integer(genre.into())),
        );
    }
    if findings.is_empty() {
        findings.push(Finding::new(
            FindingKind::TechnicalInfo,
            Location::new(["ID3v1"]),
            Disposition::Remove,
        ));
    }
    findings
}

fn enhanced_findings(block: &[u8]) -> Vec<Finding> {
    let location = |name: &str| Location::new(["TAG+", name]);
    let mut findings = Vec::new();
    for (kind, name, start, len) in [
        (FindingKind::Description, "Title", 4, 60),
        (FindingKind::PersonName, "Artist", 64, 60),
        (FindingKind::Description, "Album", 124, 60),
        (FindingKind::Description, "Genre", 185, 30),
        (FindingKind::TechnicalInfo, "Start", 215, 6),
        (FindingKind::TechnicalInfo, "End", 221, 6),
    ] {
        if let Some(value) = field(block, start, len) {
            findings
                .push(Finding::new(kind, location(name), Disposition::Remove).with_value(value));
        }
    }
    if findings.is_empty() {
        findings.push(Finding::new(
            FindingKind::TechnicalInfo,
            Location::new(["TAG+"]),
            Disposition::Remove,
        ));
    }
    findings
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn frame(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(data);
        out
    }

    fn tag(major: u8, frames: &[u8]) -> Vec<u8> {
        let size = frames.len() as u32;
        let syncsafe = [
            (size >> 21) as u8 & 0x7F,
            (size >> 14) as u8 & 0x7F,
            (size >> 7) as u8 & 0x7F,
            size as u8 & 0x7F,
        ];
        [b"ID3".as_slice(), &[major, 0, 0], &syncsafe, frames].concat()
    }

    #[test]
    fn reads_what_each_frame_holds() {
        let frames = [
            frame(b"TIT2", b"\x03Holiday"),
            frame(b"TPE1", b"\x01\xFF\xFEJ\0a\0n\0e\0"),
            frame(b"COMM", b"\x00engshort\0Nice day"),
            frame(b"TXXX", b"\x00REPLAYGAIN_TRACK_GAIN\0-6.2 dB"),
            frame(b"APIC", b"\x00image/jpeg\0\x03\0\xFF\xD8\xFF\xD9"),
            frame(b"PRIV", b"AmazonID\x00123"),
        ]
        .concat();
        let data = [tag(3, &frames), vec![0; 20]].concat();
        let tag = v2_at(&mut Cursor::new(&data), 0, data.len() as u64)
            .unwrap()
            .unwrap();
        assert_eq!(tag.end, 10 + frames.len() as u64);
        let found: Vec<_> = tag
            .findings
            .iter()
            .map(|f| (f.kind, f.value.clone()))
            .collect();
        assert_eq!(
            found,
            [
                (
                    FindingKind::Description,
                    Some(Value::Text("Holiday".into()))
                ),
                (FindingKind::PersonName, Some(Value::Text("Jane".into()))),
                (FindingKind::Comment, Some(Value::Text("Nice day".into()))),
                (
                    FindingKind::TechnicalInfo,
                    Some(Value::Text("REPLAYGAIN_TRACK_GAIN: -6.2 dB".into()))
                ),
                (FindingKind::CoverArt, Some(Value::Bytes(18))),
                (FindingKind::DocumentId, Some(Value::Bytes(12))),
            ]
        );
    }

    #[test]
    fn a_tag_ending_with_a_footer_is_found_from_the_end() {
        let mut tag = tag(4, &frame(b"TIT2", b"\x00Song"));
        tag[5] = FOOTER;
        let mut footer = tag[..10].to_vec();
        footer[..3].copy_from_slice(b"3DI");
        let data = [b"audio".as_slice(), &tag, &footer].concat();
        let found = v2_footer_before(&mut Cursor::new(&data), 0, data.len() as u64)
            .unwrap()
            .unwrap();
        assert_eq!((found.start, found.end), (5, data.len() as u64));
    }

    #[test]
    fn reads_id3v1_and_the_enhanced_block() {
        let mut v1 = vec![0u8; 128];
        v1[..3].copy_from_slice(b"TAG");
        v1[3..10].copy_from_slice(b"Holiday");
        v1[33..41].copy_from_slice(b"Jane Doe");
        v1[93..97].copy_from_slice(b"2026");
        v1[126] = 7;
        v1[127] = 0xFF;
        let mut plus = vec![0u8; 227];
        plus[..4].copy_from_slice(b"TAG+");
        plus[64..72].copy_from_slice(b"Jane Doe");
        let data = [b"audio".as_slice(), &plus, &v1].concat();
        let tag = v1_before(&mut Cursor::new(&data), 0, data.len() as u64)
            .unwrap()
            .unwrap();
        assert_eq!(tag.start, 5);
        let kinds: Vec<_> = tag.findings.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            [
                FindingKind::PersonName,
                FindingKind::Description,
                FindingKind::PersonName,
                FindingKind::Timestamp,
                FindingKind::TechnicalInfo,
            ]
        );
    }

    #[test]
    fn damaged_frames_are_reported_as_one_block() {
        let data = tag(3, b"TIT2\0\0\x10\0\0\0abc");
        let tag = v2_at(&mut Cursor::new(&data), 0, data.len() as u64)
            .unwrap()
            .unwrap();
        assert_eq!(tag.findings.len(), 1);
        assert_eq!(tag.findings[0].kind, FindingKind::UnknownMetadata);
    }

    #[test]
    fn unsynchronisation_is_undone() {
        assert_eq!(
            resynchronise(&[0xFF, 0x00, 0xE0, 0xFF, 0x00, 0x00]),
            [0xFF, 0xE0, 0xFF, 0x00]
        );
    }
}
