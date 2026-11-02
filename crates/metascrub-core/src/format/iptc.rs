//! Photoshop image resources (JPEG APP13) and the IPTC records they carry.

use super::FormatError;
use super::bytes::Cursor;
use super::{exif, icc, xmp};
use crate::model::{Disposition, Finding, FindingKind, Location, Value};

/// `data` starts at the first `8BIM` resource (after `Photoshop 3.0\0`).
pub(super) fn inspect_photoshop(data: &[u8], location: &Location) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut technical = 0;
    let mut cursor = Cursor::new(data);
    while !cursor.is_empty() {
        let Ok((id, resource)) = next_resource(&mut cursor) else {
            findings.push(
                Finding::new(
                    FindingKind::UnknownMetadata,
                    location.clone(),
                    Disposition::Remove,
                )
                .with_value(Value::Bytes(cursor.remaining().len() as u64)),
            );
            break;
        };
        match id {
            0x0404 => findings.extend(inspect_iptc(resource, &location.child("IPTC"))),
            0x0409 | 0x040C => findings.push(
                Finding::new(
                    FindingKind::Thumbnail,
                    location.child("Thumbnail"),
                    Disposition::Remove,
                )
                .with_value(Value::Bytes(resource.len() as u64)),
            ),
            0x040F => findings.push(icc::inspect(resource, location.child("ICC"))),
            0x041E => findings.push(
                Finding::new(
                    FindingKind::DownloadUrl,
                    location.child("URL list"),
                    Disposition::Remove,
                )
                .with_value(Value::Bytes(resource.len() as u64)),
            ),
            0x0422 => findings.extend(exif::inspect(resource, &location.child("EXIF"))),
            0x0424 => findings.extend(xmp::inspect(resource, &location.child("XMP"))),
            _ => technical += 1,
        }
    }
    if technical > 0 {
        findings.push(
            Finding::new(
                FindingKind::TechnicalInfo,
                location.clone(),
                Disposition::Remove,
            )
            .with_value(Value::Integer(technical)),
        );
    }
    findings
}

fn next_resource<'a>(cursor: &mut Cursor<'a>) -> Result<(u16, &'a [u8]), FormatError> {
    if cursor.array::<4>()? != *b"8BIM" {
        return Err(FormatError::malformed("expected an 8BIM resource"));
    }
    let id = cursor.u16_be()?;
    // Pascal string name, padded so length byte + name is even.
    let name_len = usize::from(cursor.u8()?);
    cursor.skip(name_len + (name_len + 1) % 2)?;
    let size = cursor.u32_be()? as usize;
    let data = cursor.take(size)?;
    cursor.skip(size % 2).ok();
    Ok((id, data))
}

/// IPTC-IIM records: `0x1C`, record number, dataset number, 16-bit size, data.
pub(super) fn inspect_iptc(data: &[u8], location: &Location) -> Vec<Finding> {
    let mut findings: Vec<Finding> = Vec::new();
    let mut technical = 0;
    let mut cursor = Cursor::new(data);
    while let Ok(0x1C) = cursor.u8() {
        let Ok(record) = cursor.u8() else { break };
        let Ok(dataset) = cursor.u8() else { break };
        let Ok(size) = cursor.u16_be() else { break };
        // Sizes over 32 KiB use an extended format no real file needs for text.
        if size & 0x8000 != 0 {
            break;
        }
        let Ok(value) = cursor.take(usize::from(size)) else {
            break;
        };
        let Some((name, kind)) = dataset_kind(record, dataset) else {
            technical += 1;
            continue;
        };
        let text = String::from_utf8_lossy(value).trim().to_owned();
        let location = location.child(name);
        // Repeated datasets (keywords, several bylines) become one finding.
        match findings.iter_mut().find(|f| f.location == location) {
            Some(Finding {
                value: Some(Value::Text(existing)),
                ..
            }) => {
                existing.push_str(", ");
                existing.push_str(&text);
            }
            _ => findings.push(
                Finding::new(kind, location, Disposition::Remove).with_value(Value::Text(text)),
            ),
        }
    }
    if technical > 0 {
        findings.push(
            Finding::new(
                FindingKind::TechnicalInfo,
                location.clone(),
                Disposition::Remove,
            )
            .with_value(Value::Integer(technical)),
        );
    }
    findings
}

fn dataset_kind(record: u8, dataset: u8) -> Option<(&'static str, FindingKind)> {
    use FindingKind::*;
    if record != 2 {
        return None;
    }
    Some(match dataset {
        5 => ("ObjectName", Description),
        25 => ("Keywords", Description),
        40 => ("SpecialInstructions", Description),
        55 => ("DateCreated", Timestamp),
        60 => ("TimeCreated", Timestamp),
        62 => ("DigitalCreationDate", Timestamp),
        63 => ("DigitalCreationTime", Timestamp),
        65 => ("OriginatingProgram", Software),
        70 => ("ProgramVersion", Software),
        80 => ("By-line", PersonName),
        85 => ("By-lineTitle", PersonName),
        90 => ("City", GpsLocation),
        92 => ("Sub-location", GpsLocation),
        95 => ("Province-State", GpsLocation),
        100 => ("CountryCode", GpsLocation),
        101 => ("Country", GpsLocation),
        103 => ("OriginalTransmissionReference", DocumentId),
        105 => ("Headline", Description),
        110 => ("Credit", PersonName),
        115 => ("Source", Description),
        116 => ("CopyrightNotice", PersonName),
        118 => ("Contact", PersonName),
        120 => ("Caption-Abstract", Description),
        122 => ("Writer-Editor", PersonName),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(dataset: u8, text: &str) -> Vec<u8> {
        let mut out = vec![0x1C, 2, dataset];
        out.extend((text.len() as u16).to_be_bytes());
        out.extend(text.as_bytes());
        out
    }

    #[test]
    fn merges_repeated_keywords() {
        let mut data = record(25, "beach");
        data.extend(record(25, "holiday"));
        data.extend(record(80, "Jane Doe"));
        data.extend(record(0, "\0\x04"));
        let findings = inspect_iptc(&data, &Location::new(["IPTC"]));
        assert_eq!(findings.len(), 3);
        assert_eq!(
            findings[0].value,
            Some(Value::Text("beach, holiday".into()))
        );
        assert_eq!(findings[1].kind, FindingKind::PersonName);
        assert_eq!(findings[2].kind, FindingKind::TechnicalInfo);
    }

    #[test]
    fn reads_iptc_inside_photoshop_resources() {
        let iptc = record(90, "Paris");
        let mut data = b"8BIM\x04\x04\0\0".to_vec();
        data.extend((iptc.len() as u32).to_be_bytes());
        data.extend(&iptc);
        if iptc.len() % 2 == 1 {
            data.push(0);
        }
        data.extend(b"8BIM\x04\x0c\0\0\0\0\0\x02ab");
        let findings = inspect_photoshop(&data, &Location::new(["Photoshop"]));
        let kinds: Vec<_> = findings.iter().map(|f| f.kind).collect();
        assert_eq!(kinds, [FindingKind::GpsLocation, FindingKind::Thumbnail]);
        assert_eq!(findings[0].location.to_string(), "Photoshop > IPTC > City");
    }

    #[test]
    fn truncated_resources_are_reported() {
        let findings = inspect_photoshop(b"8BIM\x04\x04\0\0\0\0\0\xff", &Location::new(["P"]));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::UnknownMetadata);
    }
}
