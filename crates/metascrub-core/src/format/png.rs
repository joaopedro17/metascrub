//! PNG: a signature followed by length-prefixed, CRC-checked chunks.

use super::bytes::{Cursor, latin1, split_nul};
use super::{FormatError, FormatHandler, ReadSeek, exif, icc, inflate, iptc, read_image, xmp};
use crate::model::{Disposition, Finding, FindingKind, Location, Value};

pub(super) const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// Chunks needed to display the image correctly (requirements §5.1).
/// `cICP`, `mDCV` and `cLLI` describe HDR and wide-gamut color.
const RENDERING_CHUNKS: &[&[u8; 4]] = &[
    b"IHDR", b"PLTE", b"IDAT", b"IEND", b"tRNS", b"gAMA", b"cHRM", b"sRGB", b"pHYs", b"acTL",
    b"fcTL", b"fdAT", b"cICP", b"mDCV", b"cLLI",
];

/// Standard but unnecessary for display; removed without being a privacy risk.
const TECHNICAL_CHUNKS: &[&[u8; 4]] = &[b"bKGD", b"sBIT", b"hIST", b"sPLT"];

#[derive(Debug)]
pub(super) struct Png;

impl FormatHandler for Png {
    fn inspect(&self, input: &mut dyn ReadSeek) -> Result<Vec<Finding>, FormatError> {
        let data = read_image(input)?;
        let parsed = parse(&data)?;
        let mut findings = Vec::new();
        let mut technical = 0;
        for chunk in &parsed.chunks {
            if RENDERING_CHUNKS.contains(&&chunk.kind) {
                continue;
            }
            if TECHNICAL_CHUNKS.contains(&&chunk.kind) {
                technical += 1;
                continue;
            }
            findings.extend(inspect_chunk(chunk));
        }
        if technical > 0 {
            findings.push(
                Finding::new(
                    FindingKind::TechnicalInfo,
                    Location::new(["PNG"]),
                    Disposition::Remove,
                )
                .with_value(Value::Integer(technical)),
            );
        }
        if !parsed.trailing.is_empty() {
            findings.push(
                Finding::new(
                    FindingKind::TrailingData,
                    Location::new(["After end of image"]),
                    Disposition::Remove,
                )
                .with_value(Value::Bytes(parsed.trailing.len() as u64)),
            );
        }
        Ok(findings)
    }
}

#[derive(Debug)]
pub(super) struct Chunk<'a> {
    pub(super) kind: [u8; 4],
    pub(super) data: &'a [u8],
}

#[derive(Debug)]
pub(super) struct Parsed<'a> {
    pub(super) chunks: Vec<Chunk<'a>>,
    pub(super) trailing: &'a [u8],
}

pub(super) fn parse(data: &[u8]) -> Result<Parsed<'_>, FormatError> {
    let mut cursor = Cursor::new(data);
    if cursor.take(SIGNATURE.len())? != SIGNATURE {
        return Err(FormatError::malformed("missing PNG signature"));
    }
    let mut chunks = Vec::new();
    loop {
        let length = cursor.u32_be()? as usize;
        let kind_and_data = cursor.take(length.saturating_add(4))?;
        let crc = cursor.u32_be()?;
        if crc32(kind_and_data) != crc {
            return Err(FormatError::malformed("PNG chunk checksum mismatch"));
        }
        let (kind, data) = kind_and_data.split_at(4);
        let kind: [u8; 4] = kind
            .try_into()
            .map_err(|_| FormatError::malformed("short PNG chunk"))?;
        chunks.push(Chunk { kind, data });
        if &kind == b"IEND" {
            return Ok(Parsed {
                chunks,
                trailing: cursor.remaining(),
            });
        }
    }
}

fn inspect_chunk(chunk: &Chunk<'_>) -> Vec<Finding> {
    let name = latin1(&chunk.kind);
    let location = Location::new(["PNG", name.as_str()]);
    let unknown = || {
        vec![
            Finding::new(
                FindingKind::UnknownMetadata,
                location.clone(),
                Disposition::Remove,
            )
            .with_value(Value::Bytes(chunk.data.len() as u64)),
        ]
    };
    let result = match &chunk.kind {
        b"tEXt" => text_chunk(chunk.data, &location),
        b"zTXt" => compressed_text_chunk(chunk.data, &location),
        b"iTXt" => international_text_chunk(chunk.data, &location),
        b"eXIf" => Ok(exif::inspect(chunk.data, &location)),
        b"iCCP" => icc_chunk(chunk.data, &location),
        b"tIME" => time_chunk(chunk.data, &location),
        _ => return unknown(),
    };
    result.unwrap_or_else(|_| unknown())
}

fn text_chunk(data: &[u8], location: &Location) -> Result<Vec<Finding>, FormatError> {
    let (keyword, text) = split_nul(data).ok_or_else(|| FormatError::malformed("tEXt"))?;
    Ok(text_findings(&latin1(keyword), &latin1(text), location))
}

fn compressed_text_chunk(data: &[u8], location: &Location) -> Result<Vec<Finding>, FormatError> {
    let (keyword, rest) = split_nul(data).ok_or_else(|| FormatError::malformed("zTXt"))?;
    let compressed = rest.get(1..).unwrap_or_default();
    Ok(text_findings(
        &latin1(keyword),
        &latin1(&inflate(compressed)?),
        location,
    ))
}

fn international_text_chunk(data: &[u8], location: &Location) -> Result<Vec<Finding>, FormatError> {
    let malformed = || FormatError::malformed("iTXt");
    let (keyword, rest) = split_nul(data).ok_or_else(malformed)?;
    let mut flags = Cursor::new(rest);
    let compressed = flags.u8()? == 1;
    flags.skip(1)?;
    let (_language, rest) = split_nul(flags.remaining()).ok_or_else(malformed)?;
    let (_translated_keyword, text) = split_nul(rest).ok_or_else(malformed)?;
    let text = if compressed {
        inflate(text)?
    } else {
        text.to_vec()
    };
    Ok(text_findings(
        &String::from_utf8_lossy(keyword),
        &String::from_utf8_lossy(&text),
        location,
    ))
}

fn text_findings(keyword: &str, text: &str, location: &Location) -> Vec<Finding> {
    let location = location.child(keyword);
    if keyword == "XML:com.adobe.xmp" {
        return xmp::inspect(text.as_bytes(), &location);
    }
    if let Some(profile) = keyword.strip_prefix("Raw profile type ") {
        return raw_profile(profile, text, &location);
    }
    let kind = match keyword.to_ascii_lowercase().as_str() {
        "author" | "copyright" => FindingKind::PersonName,
        "title" | "description" | "disclaimer" | "warning" => FindingKind::Description,
        "comment" => FindingKind::Comment,
        "software" => FindingKind::Software,
        "source" => FindingKind::DeviceModel,
        "creation time" => FindingKind::Timestamp,
        k if k.starts_with("date:") => FindingKind::Timestamp,
        k if k.starts_with("exif:gps") => FindingKind::GpsLocation,
        _ => FindingKind::UnknownMetadata,
    };
    vec![
        Finding::new(kind, location, Disposition::Remove)
            .with_value(Value::Text(text.trim().to_owned())),
    ]
}

/// ImageMagick stores whole metadata blocks as hex text:
/// `\n<type>\n<length>\n<hex lines>`.
fn raw_profile(profile: &str, text: &str, location: &Location) -> Vec<Finding> {
    let mut tokens = text.split_whitespace();
    let _type = tokens.next();
    let _length = tokens.next();
    let hex: String = tokens.collect();
    let bytes = decode_hex(&hex);
    match (profile, bytes) {
        ("exif" | "APP1", Some(bytes)) => {
            let tiff = bytes.strip_prefix(b"Exif\0\0").unwrap_or(&bytes);
            exif::inspect(tiff, location)
        }
        ("xmp", Some(bytes)) => xmp::inspect(&bytes, location),
        ("iptc", Some(bytes)) if bytes.starts_with(b"8BIM") => {
            iptc::inspect_photoshop(&bytes, location)
        }
        ("iptc", Some(bytes)) => iptc::inspect_iptc(&bytes, location),
        ("icc" | "icm", Some(bytes)) => vec![icc::inspect(&bytes, location.clone())],
        _ => vec![
            Finding::new(
                FindingKind::UnknownMetadata,
                location.clone(),
                Disposition::Remove,
            )
            .with_value(Value::Bytes(text.len() as u64)),
        ],
    }
}

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    hex.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect()
}

fn icc_chunk(data: &[u8], location: &Location) -> Result<Vec<Finding>, FormatError> {
    let (name, rest) = split_nul(data).ok_or_else(|| FormatError::malformed("iCCP"))?;
    let profile = inflate(rest.get(1..).unwrap_or_default())?;
    let mut finding = icc::inspect(&profile, location.clone());
    if finding.value.is_none() {
        finding = finding.with_value(Value::Text(latin1(name)));
    }
    Ok(vec![finding])
}

fn time_chunk(data: &[u8], location: &Location) -> Result<Vec<Finding>, FormatError> {
    let mut c = Cursor::new(data);
    let year = c.u16_be()?;
    let [month, day, hour, minute, second] = c.array::<5>()?;
    let text = format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC");
    Ok(vec![
        Finding::new(
            FindingKind::Timestamp,
            location.clone(),
            Disposition::Remove,
        )
        .with_value(Value::Text(text)),
    ])
}

// Evaluated at compile time, where an out-of-range index is a build error.
#[allow(clippy::indexing_slicing)]
const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
};

/// CRC-32 as used by PNG (and ZIP, gzip).
pub(crate) fn crc32(data: &[u8]) -> u32 {
    !data.iter().fold(!0u32, |crc, &byte| {
        let index = usize::from((crc as u8) ^ byte);
        CRC_TABLE.get(index).copied().unwrap_or_default() ^ (crc >> 8)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        let mut body = kind.to_vec();
        body.extend(data);
        out.extend(&body);
        out.extend(crc32(&body).to_be_bytes());
        out
    }

    fn png(chunks: &[Vec<u8>], trailing: &[u8]) -> Vec<u8> {
        let mut out = SIGNATURE.to_vec();
        out.extend(chunk(b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 0, 0, 0, 0]));
        for c in chunks {
            out.extend(c);
        }
        out.extend(chunk(b"IDAT", &[0x78, 0x9C, 0x63, 0, 0, 0, 1, 0, 1]));
        out.extend(chunk(b"IEND", &[]));
        out.extend(trailing);
        out
    }

    fn inspect(data: &[u8]) -> Vec<Finding> {
        Png.inspect(&mut std::io::Cursor::new(data)).unwrap()
    }

    #[test]
    fn crc_matches_known_value() {
        // CRC of an empty IEND chunk, present at the end of every PNG.
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
    }

    #[test]
    fn rendering_chunks_are_not_findings() {
        assert!(inspect(&png(&[chunk(b"gAMA", &[0, 0, 0xB1, 0x8F])], b"")).is_empty());
    }

    #[test]
    fn reads_text_chunks() {
        use flate2::{Compression, write::ZlibEncoder};
        use std::io::Write;
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(b"secret note").unwrap();
        let mut ztxt = b"Comment\0\0".to_vec();
        ztxt.extend(encoder.finish().unwrap());

        let findings = inspect(&png(
            &[
                chunk(b"tEXt", b"Author\0Jane Doe"),
                chunk(b"zTXt", &ztxt),
                chunk(b"iTXt", b"Title\0\0\0en\0\0Holiday"),
            ],
            b"",
        ));
        let summary: Vec<_> = findings
            .iter()
            .map(|f| (f.kind, f.location.to_string(), f.value.clone()))
            .collect();
        assert_eq!(
            summary,
            [
                (
                    FindingKind::PersonName,
                    "PNG > tEXt > Author".into(),
                    Some(Value::Text("Jane Doe".into()))
                ),
                (
                    FindingKind::Comment,
                    "PNG > zTXt > Comment".into(),
                    Some(Value::Text("secret note".into()))
                ),
                (
                    FindingKind::Description,
                    "PNG > iTXt > Title".into(),
                    Some(Value::Text("Holiday".into()))
                ),
            ]
        );
    }

    #[test]
    fn reads_modification_time() {
        let findings = inspect(&png(
            &[chunk(b"tIME", &[0x07, 0xEA, 11, 2, 14, 35, 12])],
            b"",
        ));
        assert_eq!(
            findings[0].value,
            Some(Value::Text("2026-11-02 14:35:12 UTC".into()))
        );
    }

    #[test]
    fn unknown_chunks_and_trailing_data_are_reported() {
        let findings = inspect(&png(&[chunk(b"prVt", b"vendor data")], b"extra"));
        let kinds: Vec<_> = findings.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            [FindingKind::UnknownMetadata, FindingKind::TrailingData]
        );
    }

    #[test]
    fn decodes_imagemagick_raw_profiles() {
        let findings = text_findings(
            "Raw profile type iptc",
            "\niptc\n      11\n1c0250000644616e69656c\n",
            &Location::new(["PNG"]),
        );
        assert_eq!(findings[0].kind, FindingKind::PersonName);
        assert_eq!(findings[0].value, Some(Value::Text("Daniel".into())));
    }

    #[test]
    fn bad_checksum_is_an_error() {
        let mut data = png(&[], b"");
        let last = data.len() - 1;
        data[last] ^= 0xFF;
        assert!(parse(&data).is_err());
    }

    #[test]
    fn truncated_files_are_errors_not_panics() {
        let data = png(&[chunk(b"tEXt", b"Author\0Jane")], b"");
        for len in 0..data.len() {
            assert!(parse(&data[..len]).is_err(), "length {len}");
        }
    }
}
