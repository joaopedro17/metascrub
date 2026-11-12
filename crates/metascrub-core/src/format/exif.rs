//! EXIF (a TIFF structure) found in JPEG APP1, PNG `eXIf` and other containers.

use exif::{Context, Field, In, Tag};

use crate::model::{Disposition, Finding, FindingKind, KeepReason, Location, Value};

/// `data` is the TIFF structure itself, starting with `II` or `MM`.
pub(super) fn inspect(data: &[u8], location: &Location) -> Vec<Finding> {
    let parsed = match exif::Reader::new().read_raw(data.to_vec()) {
        Ok(parsed) => parsed,
        Err(_) => {
            return vec![
                Finding::new(
                    FindingKind::UnknownMetadata,
                    location.clone(),
                    Disposition::Remove,
                )
                .with_value(Value::Bytes(data.len() as u64)),
            ];
        }
    };

    let mut findings = Vec::new();
    let mut thumbnail_len = None;
    let mut has_thumbnail = false;
    let mut fields = Vec::new();
    for field in parsed.fields() {
        if field.ifd_num == In::THUMBNAIL {
            has_thumbnail = true;
            if field.tag == Tag::JPEGInterchangeFormatLength {
                thumbnail_len = field.value.get_uint(0);
            }
        } else {
            fields.push(field);
        }
    }
    findings.extend(field_findings(&fields, location));
    // The thumbnail goes before the counts, which are last and at `location` itself.
    let counts = findings
        .iter()
        .rev()
        .take_while(|f| f.location == *location)
        .count();
    let at = findings.len() - counts;
    if has_thumbnail {
        let mut finding = Finding::new(
            FindingKind::Thumbnail,
            location.child("Thumbnail"),
            Disposition::Remove,
        );
        if let Some(length) = thumbnail_len {
            finding = finding.with_value(Value::Bytes(length.into()));
        }
        findings.insert(at, finding);
    }
    findings
}

/// Reports fields that are all removed, except a rotation that's kept: one
/// finding per field worth naming, GPS fields together, and counts for the
/// rest.
pub(super) fn field_findings(fields: &[&Field], location: &Location) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut gps_fields = Vec::new();
    let mut technical = 0;
    let mut unknown = 0;

    for &field in fields {
        if field.tag.context() == Context::Gps {
            gps_fields.push(field);
            continue;
        }
        match classify(field.tag) {
            Some((name, kind)) => findings.push(field_finding(field, name, kind, location)),
            None if is_structural(field.tag) => {}
            None if field.tag.description().is_some() => technical += 1,
            None => unknown += 1,
        }
    }

    if !gps_fields.is_empty() {
        let mut finding = Finding::new(
            FindingKind::GpsLocation,
            location.child("GPS"),
            Disposition::Remove,
        );
        if let Some(value) = coordinates(&gps_fields) {
            finding = finding.with_value(value);
        }
        findings.push(finding);
    }
    for (kind, count) in [
        (FindingKind::TechnicalInfo, technical),
        (FindingKind::UnknownMetadata, unknown),
    ] {
        if count > 0 {
            findings.push(
                Finding::new(kind, location.clone(), Disposition::Remove)
                    .with_value(Value::Integer(count)),
            );
        }
    }
    findings
}

pub(super) fn classify(tag: Tag) -> Option<(&'static str, FindingKind)> {
    use FindingKind::*;
    let kind = match tag {
        Tag::Artist | Tag::Copyright | Tag::CameraOwnerName => PersonName,
        Tag::BodySerialNumber | Tag::LensSerialNumber => DeviceSerial,
        Tag::ImageUniqueID => DocumentId,
        Tag::Make | Tag::Model | Tag::LensMake | Tag::LensModel | Tag::LensSpecification => {
            DeviceModel
        }
        Tag::Software => Software,
        Tag::DateTime
        | Tag::DateTimeOriginal
        | Tag::DateTimeDigitized
        | Tag::OffsetTime
        | Tag::OffsetTimeOriginal
        | Tag::OffsetTimeDigitized
        | Tag::SubSecTime
        | Tag::SubSecTimeOriginal
        | Tag::SubSecTimeDigitized => Timestamp,
        Tag::ImageDescription => Description,
        Tag::UserComment => Comment,
        Tag::MakerNote => UnknownMetadata,
        Tag::Orientation => Orientation,
        // Tags the exif crate doesn't name.
        Tag(Context::Tiff, number) => {
            return match number {
                0x000B => Some(("ProcessingSoftware", Software)),
                0x013C => Some(("HostComputer", DeviceModel)),
                0x9C9B => Some(("XPTitle", Description)),
                0x9C9C => Some(("XPComment", Comment)),
                0x9C9D => Some(("XPAuthor", PersonName)),
                0x9C9E => Some(("XPKeywords", Description)),
                0x9C9F => Some(("XPSubject", Description)),
                _ => None,
            };
        }
        _ => return None,
    };
    Some(("", kind))
}

/// A TIFF structure holding only the Orientation tag: what cleaning keeps
/// when an image would otherwise display rotated.
pub(super) fn orientation_only(orientation: u16) -> Vec<u8> {
    let mut tiff = b"MM\0\x2a\0\0\0\x08".to_vec();
    tiff.extend(1u16.to_be_bytes()); // one entry
    tiff.extend(0x0112u16.to_be_bytes()); // Orientation
    tiff.extend(3u16.to_be_bytes()); // SHORT
    tiff.extend(1u32.to_be_bytes()); // count
    tiff.extend(orientation.to_be_bytes());
    tiff.extend([0, 0]); // value padded to 4 bytes
    tiff.extend(0u32.to_be_bytes()); // no next IFD
    tiff
}

/// Pointers and layout fields that hold no information about the user.
fn is_structural(tag: Tag) -> bool {
    matches!(
        tag,
        Tag::ExifIFDPointer | Tag::GPSInfoIFDPointer | Tag::InteropIFDPointer
    )
}

pub(super) fn field_finding(
    field: &Field,
    name: &str,
    kind: FindingKind,
    location: &Location,
) -> Finding {
    let name = if name.is_empty() {
        field.tag.to_string()
    } else {
        name.to_owned()
    };
    let disposition = match kind {
        // 2–8 rotate or mirror; 1 is upright and anything else is invalid.
        FindingKind::Orientation if matches!(field.value.get_uint(0), Some(2..=8)) => {
            Disposition::Keep(KeepReason::RequiredToRender)
        }
        _ => Disposition::Remove,
    };
    Finding::new(kind, location.child(name), disposition).with_value(value_of(field))
}

fn value_of(field: &Field) -> Value {
    match &field.value {
        exif::Value::Ascii(strings) => Value::Text(
            strings
                .iter()
                .map(|s| String::from_utf8_lossy(s).trim().to_owned())
                .collect::<Vec<_>>()
                .join(", "),
        ),
        exif::Value::Undefined(bytes, _) if field.tag == Tag::UserComment => {
            Value::Text(user_comment(bytes))
        }
        exif::Value::Undefined(bytes, _) => Value::Bytes(bytes.len() as u64),
        // Windows XP* tags: UCS-2 little-endian text stored as bytes.
        exif::Value::Byte(bytes) if field.tag.context() == Context::Tiff => {
            Value::Text(utf16_le(bytes))
        }
        exif::Value::Short(_) | exif::Value::Long(_) => match field.value.get_uint(0) {
            Some(n) => Value::Integer(n.into()),
            None => Value::Text(field.display_value().to_string()),
        },
        _ => Value::Text(field.display_value().to_string()),
    }
}

/// UserComment starts with an 8-byte character code.
fn user_comment(bytes: &[u8]) -> String {
    let (code, text) = bytes.split_at(bytes.len().min(8));
    let text = if code.starts_with(b"UNICODE") {
        utf16_le(text)
    } else {
        String::from_utf8_lossy(text).into_owned()
    };
    text.trim_matches(|c: char| c == '\0' || c.is_whitespace())
        .to_owned()
}

fn utf16_le(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| u16::from_le_bytes(pair))
        .collect();
    String::from_utf16_lossy(&units)
        .trim_end_matches('\0')
        .to_owned()
}

fn coordinates(fields: &[&Field]) -> Option<Value> {
    let find = |tag: Tag| fields.iter().find(|f| f.tag == tag).map(|f| &f.value);
    let degrees = |tag: Tag, negative_ref: u8, ref_tag: Tag| -> Option<f64> {
        let exif::Value::Rational(parts) = find(tag)? else {
            return None;
        };
        let [d, m, s] = parts.as_slice() else {
            return None;
        };
        let value = d.to_f64() + m.to_f64() / 60.0 + s.to_f64() / 3600.0;
        if !value.is_finite() {
            return None;
        }
        let negative = matches!(find(ref_tag), Some(exif::Value::Ascii(r))
            if r.first().and_then(|s| s.first()) == Some(&negative_ref));
        Some(if negative { -value } else { value })
    };
    Some(Value::Coordinates {
        latitude: degrees(Tag::GPSLatitude, b'S', Tag::GPSLatitudeRef)?,
        longitude: degrees(Tag::GPSLongitude, b'W', Tag::GPSLongitudeRef)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orientation_only_block_contains_nothing_else() {
        let findings = inspect(&orientation_only(6), &Location::new(["EXIF"]));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::Orientation);
        assert_eq!(findings[0].value, Some(Value::Integer(6)));
        assert_eq!(
            findings[0].disposition,
            Disposition::Keep(KeepReason::RequiredToRender)
        );
    }

    #[test]
    fn only_valid_rotations_are_kept() {
        for (orientation, kept) in [
            (1, false),
            (2, true),
            (6, true),
            (8, true),
            (0, false),
            (9, false),
        ] {
            let findings = inspect(&orientation_only(orientation), &Location::new(["EXIF"]));
            assert_eq!(
                findings[0].disposition == Disposition::Keep(KeepReason::RequiredToRender),
                kept,
                "orientation {orientation}"
            );
        }
    }

    #[test]
    fn unreadable_exif_is_still_reported() {
        let findings = inspect(b"MM\0\x2a garbage", &Location::new(["EXIF"]));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::UnknownMetadata);
        assert_eq!(findings[0].disposition, Disposition::Remove);
    }

    #[test]
    fn decodes_user_comment_encodings() {
        assert_eq!(user_comment(b"ASCII\0\0\0hello\0"), "hello");
        assert_eq!(user_comment(b"UNICODE\0h\0i\0"), "hi");
        assert_eq!(user_comment(b"short"), "");
    }

    #[test]
    fn classifies_windows_xp_tags_by_number() {
        assert_eq!(
            classify(Tag(Context::Tiff, 0x9C9D)),
            Some(("XPAuthor", FindingKind::PersonName))
        );
        assert_eq!(classify(Tag::Make), Some(("", FindingKind::DeviceModel)));
        assert_eq!(classify(Tag::ExposureTime), None);
    }
}
