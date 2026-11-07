//! What the metadata boxes of videos and music say: QuickTime user data
//! (`udta`), iTunes-style tags and Apple's keyed metadata (`meta`), XMP.
//! Cleaning never reads these back; it blanks them out whole.

use super::boxes::{self, FourCc, Header};
use crate::format::bytes::Cursor;
use crate::format::xmp;
use crate::model::{Disposition, Finding, FindingKind, Location, Value};

/// `com.apple.quicktime.location.ISO6709`, `©xyz`: `+37.3317-122.0302+012.3/`.
pub(super) fn iso6709(text: &str) -> Option<Value> {
    let text = text.trim().trim_end_matches('/');
    let split = |s: &str| {
        s.char_indices()
            .skip(1)
            .find(|(_, c)| matches!(c, '+' | '-'))
    };
    let (lon_at, _) = split(text)?;
    let (latitude, rest) = text.split_at(lon_at);
    let longitude = split(rest).map_or(rest, |(alt_at, _)| rest.split_at(alt_at).0);
    let latitude: f64 = latitude.parse().ok()?;
    let longitude: f64 = longitude.parse().ok()?;
    (latitude.abs() <= 90.0 && longitude.abs() <= 180.0).then_some(Value::Coordinates {
        latitude,
        longitude,
    })
}

#[derive(Debug)]
struct Tags {
    location: Location,
    findings: Vec<Finding>,
    technical: i64,
    unknown: i64,
}

impl Tags {
    fn new(location: &Location) -> Self {
        Self {
            location: location.clone(),
            findings: Vec::new(),
            technical: 0,
            unknown: 0,
        }
    }

    fn add(&mut self, kind: Option<FindingKind>, name: &str, value: Option<Value>) {
        match kind {
            Some(FindingKind::TechnicalInfo) => self.technical += 1,
            Some(kind) => {
                let value = match (kind, value) {
                    (FindingKind::GpsLocation, Some(Value::Text(text))) => {
                        iso6709(&text).or(Some(Value::Text(text)))
                    }
                    (_, value) => value,
                };
                let mut finding =
                    Finding::new(kind, self.location.child(name), Disposition::Remove);
                finding.value = value.filter(|v| *v != Value::Text(String::new()));
                self.findings.push(finding);
            }
            None => self.unknown += 1,
        }
    }

    fn finish(mut self) -> Vec<Finding> {
        for (kind, count) in [
            (FindingKind::TechnicalInfo, self.technical),
            (FindingKind::UnknownMetadata, self.unknown),
        ] {
            if count > 0 {
                self.findings.push(
                    Finding::new(kind, self.location.clone(), Disposition::Remove)
                        .with_value(Value::Integer(count)),
                );
            }
        }
        self.findings
    }
}

fn boxes_in(data: &[u8], skip: usize) -> Option<Vec<(Header, &[u8])>> {
    boxes::parse(data, skip).ok()
}

fn unreadable(data: &[u8], location: &Location) -> Vec<Finding> {
    vec![
        Finding::new(
            FindingKind::UnknownMetadata,
            location.clone(),
            Disposition::Remove,
        )
        .with_value(Value::Bytes(data.len() as u64)),
    ]
}

/// The payload of a `udta` box.
pub(super) fn user_data(data: &[u8], location: &Location) -> Vec<Finding> {
    let Some(children) = boxes_in(data, 0) else {
        return unreadable(data, location);
    };
    let mut tags = Tags::new(location);
    for (header, payload) in children {
        let name = boxes::name(header.kind);
        match &header.kind {
            b"meta" => tags.findings.extend(metadata(payload, location)),
            b"XMP_" => tags
                .findings
                .extend(xmp::inspect(payload, &location.child("XMP"))),
            b"loci" => tags.add(
                Some(FindingKind::GpsLocation),
                &name,
                loci(payload).or_else(|| third_party_text(payload)),
            ),
            [0xA9, ..] => tags.add(kind_of(header.kind), &name, quicktime_text(payload)),
            b"auth" | b"perf" | b"cprt" | b"titl" | b"dscp" | b"gnre" | b"albm" | b"kywd"
            | b"clsf" | b"yrrc" => {
                tags.add(kind_of(header.kind), &name, third_party_text(payload));
            }
            b"FIRM" | b"CAME" | b"MUID" | b"GUMI" => {
                let value = text(payload.split(|&b| b == 0).next().unwrap_or_default());
                tags.add(kind_of(header.kind), &name, Some(Value::Text(value)));
            }
            _ => tags.add(kind_of(header.kind), &name, None),
        }
    }
    tags.finish()
}

/// The payload of a `meta` box: QuickTime's starts with its boxes, the
/// ISO one with a version and flags.
pub(super) fn metadata(data: &[u8], location: &Location) -> Vec<Finding> {
    let skip = if data.get(4..8) == Some(b"hdlr") {
        0
    } else {
        4
    };
    let Some(children) = boxes_in(data, skip) else {
        return unreadable(data, location);
    };
    let keys = children
        .iter()
        .find(|(header, _)| header.kind == *b"keys")
        .map(|(_, payload)| keys(payload))
        .unwrap_or_default();
    let mut tags = Tags::new(location);
    for (header, payload) in &children {
        match &header.kind {
            b"ilst" => {
                let Some(items) = boxes_in(payload, 0) else {
                    tags.findings.extend(unreadable(payload, location));
                    continue;
                };
                for (item, content) in items {
                    let index = u32::from_be_bytes(item.kind) as usize;
                    let key = index.checked_sub(1).and_then(|i| keys.get(i));
                    let (kind, name) = match key {
                        Some(key) => (key_kind(key), key.clone()),
                        None if item.kind == *b"----" => {
                            let name = freeform_name(content).unwrap_or_default();
                            (key_kind(&name), name)
                        }
                        None => (kind_of(item.kind), boxes::name(item.kind)),
                    };
                    tags.add(kind, &name, item_value(content));
                }
            }
            b"xml " | b"XMP_" => {
                let xml = if header.kind == *b"xml " {
                    payload.get(4..).unwrap_or_default()
                } else {
                    payload
                };
                tags.findings
                    .extend(xmp::inspect(xml, &location.child("XMP")));
            }
            _ => {}
        }
    }
    tags.finish()
}

fn keys(data: &[u8]) -> Vec<String> {
    let mut cursor = Cursor::new(data);
    let mut keys = Vec::new();
    let Ok(count) = cursor.skip(4).and_then(|()| cursor.u32_be()) else {
        return keys;
    };
    for _ in 0..count {
        let Ok(size) = cursor.u32_be() else { break };
        let Some(Ok(key)) = (size as usize)
            .checked_sub(8)
            .map(|len| cursor.skip(4).and_then(|()| cursor.take(len)))
        else {
            break;
        };
        keys.push(text(key));
    }
    keys
}

/// A `----` item names itself in a `name` box.
fn freeform_name(content: &[u8]) -> Option<String> {
    boxes_in(content, 0)?
        .into_iter()
        .find(|(header, _)| header.kind == *b"name")
        .map(|(_, payload)| text(payload.get(4..).unwrap_or_default()))
}

/// The first `data` box of an item: a type, a locale, then the value.
fn item_value(content: &[u8]) -> Option<Value> {
    let (_, data) = boxes_in(content, 0)?
        .into_iter()
        .find(|(header, _)| header.kind == *b"data")?;
    let mut cursor = Cursor::new(data);
    let kind = cursor.u32_be().ok()? & 0x00FF_FFFF;
    let value = cursor.skip(4).ok().map(|()| cursor.remaining())?;
    Some(match kind {
        1 => Value::Text(text(value)),
        2 => Value::Text(utf16_be(value)),
        21 | 22 if value.len() <= 8 && !value.is_empty() => {
            let mut bytes = [0; 8];
            let start = 8 - value.len();
            bytes.get_mut(start..)?.copy_from_slice(value);
            if kind == 21 && value.first().is_some_and(|b| b & 0x80 != 0) {
                bytes.get_mut(..start)?.fill(0xFF);
            }
            Value::Integer(i64::from_be_bytes(bytes))
        }
        _ => Value::Bytes(value.len() as u64),
    })
}

/// QuickTime's own text: a 16-bit length, a language, the text.
fn quicktime_text(data: &[u8]) -> Option<Value> {
    if data.get(4..8).is_some_and(|kind| kind == b"data") {
        return item_value(data);
    }
    let mut cursor = Cursor::new(data);
    let len = cursor.u16_be().ok()?;
    cursor.skip(2).ok()?;
    let bytes = cursor
        .take(usize::from(len))
        .unwrap_or_else(|_| cursor.remaining());
    Some(Value::Text(text(bytes)))
}

/// 3GPP's: version and flags, a language, the text up to a NUL.
fn third_party_text(data: &[u8]) -> Option<Value> {
    let bytes = data.get(6..)?;
    Some(Value::Text(if bytes.starts_with(&[0xFE, 0xFF]) {
        utf16_be(bytes.get(2..)?)
    } else {
        text(bytes.split(|&b| b == 0).next().unwrap_or_default())
    }))
}

/// 3GPP location: a name, a role, then fixed-point longitude and latitude.
fn loci(data: &[u8]) -> Option<Value> {
    let rest = data.get(6..)?;
    let after_name = if rest.starts_with(&[0xFE, 0xFF]) {
        let end = rest
            .as_chunks::<2>()
            .0
            .iter()
            .position(|pair| *pair == [0, 0])?;
        rest.get(end * 2 + 2..)?
    } else {
        let end = rest.iter().position(|&b| b == 0)?;
        rest.get(end + 1..)?
    };
    let mut cursor = Cursor::new(after_name);
    cursor.skip(1).ok()?;
    let fixed = |n: u32| f64::from(n.cast_signed()) / 65536.0;
    let longitude = fixed(cursor.u32_be().ok()?);
    let latitude = fixed(cursor.u32_be().ok()?);
    (latitude.abs() <= 90.0 && longitude.abs() <= 180.0).then_some(Value::Coordinates {
        latitude,
        longitude,
    })
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_matches(|c: char| c == '\0' || c.is_whitespace())
        .to_owned()
}

fn utf16_be(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| u16::from_be_bytes(pair))
        .collect();
    String::from_utf16_lossy(&units)
        .trim_matches(|c: char| c == '\0' || c.is_whitespace())
        .to_owned()
}

/// Tags named by four letters, in `udta` and in iTunes-style lists.
fn kind_of(kind: FourCc) -> Option<FindingKind> {
    use FindingKind::*;
    Some(match &kind {
        b"\xA9xyz" | b"loci" => GpsLocation,
        b"\xA9mak" | b"\xA9mod" => DeviceModel,
        b"\xA9swr" | b"\xA9too" | b"\xA9enc" | b"\xA9fmt" | b"FIRM" => Software,
        b"\xA9day" | b"purd" | b"yrrc" => Timestamp,
        b"\xA9ART" | b"\xA9aut" | b"\xA9wrt" | b"\xA9prf" | b"\xA9dir" | b"\xA9prd"
        | b"\xA9PRD" | b"\xA9com" | b"\xA9cpy" | b"\xA9own" | b"\xA9src" | b"aART" | b"soar"
        | b"soaa" | b"apID" | b"ownr" | b"cprt" | b"auth" | b"perf" => PersonName,
        b"\xA9cmt" => Comment,
        b"\xA9nam" | b"\xA9alb" | b"\xA9des" | b"\xA9inf" | b"\xA9key" | b"\xA9gen"
        | b"\xA9grp" | b"\xA9lyr" | b"\xA9dis" | b"desc" | b"ldes" | b"tvsh" | b"sonm"
        | b"soal" | b"keyw" | b"catg" | b"titl" | b"dscp" | b"gnre" | b"albm" | b"kywd"
        | b"clsf" | b"chpl" => Description,
        b"covr" => CoverArt,
        b"cnID" | b"atID" | b"plID" | b"geID" | b"sfID" | b"cmID" | b"xid " | b"MUID" | b"GUMI" => {
            DocumentId
        }
        b"CAME" => DeviceSerial,
        b"name" | b"hnti" | b"hinf" | b"trkn" | b"disk" | b"tmpo" | b"cpil" | b"pgap" | b"stik"
        | b"rtng" | b"hdvd" => TechnicalInfo,
        _ => return None,
    })
}

/// Apple's keyed metadata (`com.apple.quicktime.make`) and the names of
/// iTunes `----` items (`iTunSMPB`).
fn key_kind(key: &str) -> Option<FindingKind> {
    use FindingKind::*;
    // Every Apple key has "quicktime" in it, which isn't a time.
    let key = key.to_ascii_lowercase().replace("quicktime", "");
    let has = |words: &[&str]| words.iter().any(|w| key.contains(w));
    Some(if has(&["location", "iso6709", "gps"]) {
        GpsLocation
    } else if has(&["make", "model", "camera", "lens"]) {
        DeviceModel
    } else if has(&["software", "encoder", "version", "tool"]) {
        Software
    } else if has(&["date", "time"]) && !has(&["still-image-time"]) {
        Timestamp
    } else if has(&["identifier", "signature", "uuid"]) {
        DocumentId
    } else if has(&[
        "author",
        "artist",
        "director",
        "producer",
        "publisher",
        "copyright",
        "owner",
        "performer",
        "composer",
    ]) {
        PersonName
    } else if has(&["comment"]) {
        Comment
    } else if has(&[
        "title",
        "description",
        "keyword",
        "information",
        "album",
        "genre",
        "displayname",
    ]) {
        Description
    } else if has(&[
        "itun",
        "fps",
        "rate",
        "intent",
        "orientation",
        "still-image-time",
    ]) {
        TechnicalInfo
    } else {
        return None;
    })
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::boxes::tests::{full, make};
    use super::*;

    fn kinds(findings: &[Finding]) -> Vec<FindingKind> {
        findings.iter().map(|f| f.kind).collect()
    }

    /// An Apple keyed metadata box, as iPhones write in `moov/meta`.
    pub(crate) fn apple_metadata(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut keys = Vec::new();
        keys.extend(u32::try_from(entries.len()).unwrap().to_be_bytes());
        let mut items = Vec::new();
        for (index, (key, value)) in entries.iter().enumerate() {
            keys.extend(make(b"mdta", key.as_bytes()));
            let data = [&[0, 0, 0, 1, 0, 0, 0, 0][..], value.as_bytes()].concat();
            let index = u32::try_from(index + 1).unwrap().to_be_bytes();
            items.extend(make(&index, &make(b"data", &data)));
        }
        let hdlr = full(b"hdlr", 0, 0, b"\0\0\0\0mdta\0\0\0\0\0\0\0\0\0\0\0\0\0");
        let keys = full(b"keys", 0, 0, &keys);
        [hdlr, keys, make(b"ilst", &items)].concat()
    }

    #[test]
    fn reads_iso6709_positions() {
        assert_eq!(
            iso6709("+37.3317-122.0302+012.345/"),
            Some(Value::Coordinates {
                latitude: 37.3317,
                longitude: -122.0302
            })
        );
        assert_eq!(
            iso6709("-33.8688+151.2093/"),
            Some(Value::Coordinates {
                latitude: -33.8688,
                longitude: 151.2093
            })
        );
        assert_eq!(iso6709("+3720.00-12201.81/"), None);
        assert_eq!(iso6709("nowhere"), None);
    }

    #[test]
    fn reads_apple_keyed_metadata() {
        let data = apple_metadata(&[
            ("com.apple.quicktime.location.ISO6709", "+37.3317-122.0302/"),
            ("com.apple.quicktime.make", "Apple"),
            ("com.apple.quicktime.model", "iPhone 15"),
            ("com.apple.quicktime.software", "18.1"),
            (
                "com.apple.quicktime.creationdate",
                "2026-03-14T09:26:52+0100",
            ),
            ("com.apple.quicktime.content.identifier", "4B1C…"),
            ("com.apple.quicktime.full-frame-rate-playback-intent", "1"),
            ("com.example.secret", "x"),
        ]);
        let findings = metadata(&data, &Location::new(["Metadata"]));
        assert_eq!(
            kinds(&findings),
            [
                FindingKind::GpsLocation,
                FindingKind::DeviceModel,
                FindingKind::DeviceModel,
                FindingKind::Software,
                FindingKind::Timestamp,
                FindingKind::DocumentId,
                FindingKind::TechnicalInfo,
                FindingKind::UnknownMetadata,
            ]
        );
        assert_eq!(
            findings[0].value,
            Some(Value::Coordinates {
                latitude: 37.3317,
                longitude: -122.0302
            })
        );
        assert_eq!(
            findings[2].location.to_string(),
            "Metadata > com.apple.quicktime.model"
        );
        assert_eq!(findings[2].value, Some(Value::Text("iPhone 15".into())));
    }

    #[test]
    fn reads_quicktime_and_3gpp_user_data() {
        let qt = |text: &str| {
            let len = u16::try_from(text.len()).unwrap().to_be_bytes();
            [&len[..], &[0x15, 0xC7], text.as_bytes()].concat()
        };
        let mut loci = vec![0; 6];
        loci.extend(b"Home\0\0");
        loci.extend(((-122.03 * 65536.0) as i32).to_be_bytes());
        loci.extend(((37.33 * 65536.0) as i32).to_be_bytes());
        loci.extend(0i32.to_be_bytes());
        let data = [
            make(b"\xA9xyz", &qt("+37.3317-122.0302/")),
            make(b"\xA9too", &qt("Lavf61.7.100")),
            make(b"auth", b"\0\0\0\0\x15\xc7Jane Doe\0"),
            make(b"loci", &loci),
            make(b"CAME", b"C3441326184592\0"),
            make(b"SETT", b"\x01\x02"),
        ]
        .concat();
        let findings = user_data(&data, &Location::new(["User data"]));
        assert_eq!(
            kinds(&findings),
            [
                FindingKind::GpsLocation,
                FindingKind::Software,
                FindingKind::PersonName,
                FindingKind::GpsLocation,
                FindingKind::DeviceSerial,
                FindingKind::UnknownMetadata,
            ]
        );
        assert_eq!(findings[2].value, Some(Value::Text("Jane Doe".into())));
        assert_eq!(findings[1].location.to_string(), "User data > ©too");
        let Some(Value::Coordinates { latitude, .. }) = findings[3].value else {
            panic!("{:?}", findings[3]);
        };
        assert!((latitude - 37.33).abs() < 0.001);
    }

    #[test]
    fn reads_itunes_tags() {
        let item = |kind: &[u8; 4], data_type: u8, value: &[u8]| {
            let data = [&[0, 0, 0, data_type, 0, 0, 0, 0][..], value].concat();
            make(kind, &make(b"data", &data))
        };
        let freeform = make(
            b"----",
            &[
                full(b"mean", 0, 0, b"com.apple.iTunes"),
                full(b"name", 0, 0, b"iTunSMPB"),
                make(b"data", b"\0\0\0\x01\0\0\0\0 00000000"),
            ]
            .concat(),
        );
        let ilst = make(
            b"ilst",
            &[
                item(b"\xA9nam", 1, b"Holiday"),
                item(b"\xA9ART", 1, b"Jane Doe"),
                item(b"apID", 1, b"jane@example.com"),
                item(b"covr", 13, &[0xFF, 0xD8, 0xFF, 0xD9]),
                freeform,
            ]
            .concat(),
        );
        let hdlr = full(b"hdlr", 0, 0, b"\0\0\0\0mdirappl\0\0\0\0\0\0\0\0\0");
        let meta = full(b"meta", 0, 0, &[hdlr, ilst].concat());
        let findings = user_data(&meta, &Location::new(["User data"]));
        assert_eq!(
            kinds(&findings),
            [
                FindingKind::Description,
                FindingKind::PersonName,
                FindingKind::PersonName,
                FindingKind::CoverArt,
                FindingKind::TechnicalInfo,
            ]
        );
        assert_eq!(findings[3].value, Some(Value::Bytes(4)));
    }

    #[test]
    fn damaged_metadata_is_still_reported() {
        let findings = user_data(b"\0\0\0\x50abcd", &Location::new(["User data"]));
        assert_eq!(kinds(&findings), [FindingKind::UnknownMetadata]);
    }
}
