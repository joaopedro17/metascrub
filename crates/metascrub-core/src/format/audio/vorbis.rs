//! Vorbis comments: the tags of FLAC, Ogg Vorbis and Opus. A vendor string
//! (the encoder) and a list of `NAME=value` fields, all little-endian
//! length-prefixed.

use super::text;
use crate::format::FormatError;
use crate::format::bytes::Cursor;
use crate::model::{Disposition, Finding, FindingKind, Location, Value};

/// What a comment block holds. `label` names the block in locations.
pub(super) fn findings(label: &str, data: &[u8]) -> Result<Vec<Finding>, FormatError> {
    let mut cursor = Cursor::new(data);
    let mut findings = Vec::new();
    let vendor_len = cursor.u32_le()? as usize;
    let vendor = String::from_utf8_lossy(cursor.take(vendor_len)?).into_owned();
    if let Some(value) = text(&vendor) {
        findings.push(
            Finding::new(
                FindingKind::Software,
                Location::new([label, "vendor"]),
                Disposition::Remove,
            )
            .with_value(value),
        );
    }
    let count = cursor.u32_le()?;
    for _ in 0..count {
        let len = cursor.u32_le()? as usize;
        let field = cursor.take(len)?;
        let (name, value) = match field.iter().position(|&b| b == b'=') {
            Some(at) => (
                field.get(..at).unwrap_or_default(),
                field.get(at + 1..).unwrap_or_default(),
            ),
            None => (field, &[][..]),
        };
        let name = String::from_utf8_lossy(name).to_ascii_uppercase();
        let kind = kind_of(&name);
        let value = if kind == FindingKind::CoverArt {
            Some(Value::Bytes(value.len() as u64))
        } else {
            text(&String::from_utf8_lossy(value))
        };
        let finding = Finding::new(
            kind,
            Location::new([label.to_owned(), name]),
            Disposition::Remove,
        );
        findings.push(match value {
            Some(value) => finding.with_value(value),
            None => finding,
        });
    }
    if findings.is_empty() {
        findings.push(
            Finding::new(
                FindingKind::TechnicalInfo,
                Location::new([label]),
                Disposition::Remove,
            )
            .with_value(Value::Bytes(data.len() as u64)),
        );
    }
    Ok(findings)
}

fn kind_of(name: &str) -> FindingKind {
    use FindingKind::*;
    if ["REPLAYGAIN_", "R128_", "MP3GAIN_", "ITUNNORM", "ITUNSMPB"]
        .iter()
        .any(|p| name.starts_with(p))
    {
        return TechnicalInfo;
    }
    if ["MUSICBRAINZ_", "ACOUSTID", "DISCOGS_"]
        .iter()
        .any(|p| name.starts_with(p))
    {
        return DocumentId;
    }
    match name {
        "TITLE" | "ALBUM" | "GENRE" | "DESCRIPTION" | "LYRICS" | "UNSYNCEDLYRICS" | "SUBTITLE"
        | "VERSION" | "GROUPING" | "MOOD" | "WORK" | "MOVEMENTNAME" | "TITLESORT" | "ALBUMSORT" => {
            Description
        }
        "ARTIST" | "ALBUMARTIST" | "ALBUM ARTIST" | "PERFORMER" | "COMPOSER" | "CONDUCTOR"
        | "LYRICIST" | "ARRANGER" | "ENGINEER" | "PRODUCER" | "REMIXER" | "DJMIXER" | "MIXER"
        | "WRITER" | "COPYRIGHT" | "ENCODEDBY" | "ENCODED-BY" | "AUTHOR" | "CONTACT"
        | "ARTISTSORT" | "ALBUMARTISTSORT" | "COMPOSERSORT" | "LICENSE" => PersonName,
        "ORGANIZATION" | "LABEL" | "PUBLISHER" => Organization,
        "COMMENT" => Comment,
        "DATE" | "YEAR" | "ORIGINALDATE" | "ORIGINALYEAR" | "RELEASEDATE" | "RELEASETIME" => {
            Timestamp
        }
        "ENCODER" | "ENCODING" | "ENCODERSETTINGS" | "ENCODER_OPTIONS" | "TOOL" => Software,
        "LOCATION" => GpsLocation,
        "METADATA_BLOCK_PICTURE" | "COVERART" | "COVERARTMIME" => CoverArt,
        "ISRC" | "BARCODE" | "UPC" | "EAN" | "CATALOGNUMBER" | "ASIN" | "DISCID" => DocumentId,
        "TRACKNUMBER" | "TRACKTOTAL" | "TOTALTRACKS" | "DISCNUMBER" | "DISCTOTAL"
        | "TOTALDISCS" | "BPM" | "LANGUAGE" | "MEDIA" | "COMPILATION" | "KEY" => TechnicalInfo,
        _ => CustomProperty,
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    pub(crate) fn block(vendor: &str, fields: &[&str]) -> Vec<u8> {
        let mut out = (vendor.len() as u32).to_le_bytes().to_vec();
        out.extend_from_slice(vendor.as_bytes());
        out.extend_from_slice(&(fields.len() as u32).to_le_bytes());
        for field in fields {
            out.extend_from_slice(&(field.len() as u32).to_le_bytes());
            out.extend_from_slice(field.as_bytes());
        }
        out
    }

    #[test]
    fn reads_each_field() {
        let data = block(
            "reference libFLAC 1.4.3",
            &[
                "TITLE=Holiday",
                "artist=Jane Doe",
                "REPLAYGAIN_TRACK_GAIN=-6.2 dB",
                "X=y",
            ],
        );
        let found: Vec<_> = findings("Vorbis comment", &data)
            .unwrap()
            .into_iter()
            .map(|f| (f.kind, f.location.to_string()))
            .collect();
        assert_eq!(
            found,
            [
                (FindingKind::Software, "Vorbis comment > vendor".to_owned()),
                (
                    FindingKind::Description,
                    "Vorbis comment > TITLE".to_owned()
                ),
                (
                    FindingKind::PersonName,
                    "Vorbis comment > ARTIST".to_owned()
                ),
                (
                    FindingKind::TechnicalInfo,
                    "Vorbis comment > REPLAYGAIN_TRACK_GAIN".to_owned()
                ),
                (FindingKind::CustomProperty, "Vorbis comment > X".to_owned()),
            ]
        );
    }

    #[test]
    fn a_count_beyond_the_data_is_damage() {
        let mut data = block("v", &["A=b"]);
        data[5] = 9;
        assert!(findings("Vorbis comment", &data).is_err());
    }
}
