//! HEIC and AVIF photos: the picture, its previews and its Exif and XMP are
//! "items" listed in the top-level `meta` box, their bytes usually in
//! `mdat`. Exif and XMP are overwritten where they are, at the same size;
//! everything else is part of the picture and stays.

use std::collections::BTreeMap;

use super::Plan;
use super::boxes::{self, FourCc, Header};
use crate::format::bytes::{Cursor, split_nul};
use crate::format::{FormatError, ReadSeek, apply_options, exif, kept_orientation, xmp};
use crate::model::{Disposition, Finding, FindingKind, KeepReason, Location, Risk, Value};
use crate::options::Options;

/// The Exif item's own header: where the TIFF data starts, then the
/// `Exif\0\0` prefix iPhones write.
const EXIF_PREFIX: &[u8] = b"\0\0\0\x06Exif\0\0";
/// A TIFF structure with an empty list of tags.
const EMPTY_TIFF: &[u8] = b"MM\0\x2a\0\0\0\x08\0\0\0\0\0\0";

/// Whether this `meta` box describes a picture, rather than holding tags.
/// QuickTime's `meta` has no version and flags, so its first box comes
/// straight away.
pub(super) fn is_image(input: &mut dyn ReadSeek, meta: &Header) -> Result<bool, FormatError> {
    let head = boxes::read_range(input, meta.payload, meta.len().min(8))?;
    if head.get(4..8) == Some(b"hdlr") {
        return Ok(false);
    }
    let children = boxes::children(input, meta, 4)?;
    let Some(hdlr) = children.iter().find(|c| c.kind == *b"hdlr") else {
        return Ok(false);
    };
    let fields = boxes::read_range(input, hdlr.payload, hdlr.len().min(12))?;
    Ok(fields.get(8..12) == Some(b"pict"))
}

#[derive(Debug)]
struct Item {
    id: u32,
    kind: FourCc,
    content_type: String,
}

#[derive(Debug)]
struct Place {
    method: u16,
    data_reference: u16,
    /// Offsets as written: from the file's start, or from `idat`'s.
    extents: Vec<(u64, u64)>,
}

pub(super) fn image(
    input: &mut dyn ReadSeek,
    meta: &Header,
    plan: &mut Plan,
    options: &Options,
) -> Result<(), FormatError> {
    let data = boxes::read(input, meta)?;
    let children = boxes::parse(&data, 4)?;
    let mut items = Vec::new();
    let mut places = BTreeMap::new();
    let mut references = Vec::new();
    let mut properties = Vec::new();
    let mut associations = BTreeMap::new();
    let mut idat = None;
    for (header, payload) in &children {
        match &header.kind {
            b"iinf" => items = item_infos(payload)?,
            b"iloc" => places = item_places(payload)?,
            b"iref" => references = item_references(payload)?,
            b"iprp" => {
                for (header, payload) in boxes::parse(payload, 0)? {
                    match &header.kind {
                        b"ipco" => properties = boxes::parse(payload, 0)?,
                        b"ipma" => associations = item_properties(payload)?,
                        _ => {}
                    }
                }
            }
            b"idat" => idat = Some(header.shifted(meta.payload)),
            _ => {}
        }
    }

    let mut findings = Vec::new();
    for item in &items {
        let place = || {
            places
                .get(&item.id)
                .ok_or_else(|| FormatError::malformed("an item with no location"))
        };
        match &item.kind {
            b"Exif" => {
                let extents = extents(place()?, idat.as_ref(), plan)?;
                let data = read_extents(input, &extents)?;
                let location = Location::new(["EXIF"]);
                let tiff = Cursor::new(&data)
                    .u32_be()
                    .ok()
                    .and_then(|offset| data.get(usize::try_from(offset).ok()?.checked_add(4)?..))
                    .unwrap_or_default();
                let mut exif = exif::inspect(tiff, &location);
                apply_options(&mut exif, options);
                let orientation = kept_orientation(&exif, &location.child("Orientation"));
                let tiff = orientation.map_or_else(|| EMPTY_TIFF.to_vec(), exif::orientation_only);
                let content = [EXIF_PREFIX, &tiff].concat();
                overwrite(plan, &extents, &data, content, "Exif")?;
                findings.extend(exif);
            }
            b"mime" if is_xmp(&item.content_type) => {
                let extents = extents(place()?, idat.as_ref(), plan)?;
                let data = read_extents(input, &extents)?;
                findings.extend(xmp::inspect(&data, &Location::new(["XMP"])));
                let content = xmp::empty_packet(data.len())
                    .ok_or_else(|| FormatError::malformed("the XMP item is too small"))?;
                overwrite(plan, &extents, &data, content, "XMP")?;
            }
            b"mime" | b"uri " => findings.push(
                Finding::new(
                    FindingKind::UnknownMetadata,
                    Location::new([boxes::name(item.kind).trim_end()]),
                    Disposition::Warn,
                )
                .with_value(Value::Text(item.content_type.clone())),
            ),
            _ => {}
        }
    }

    let size = |id: u32| {
        places
            .get(&id)
            .map(|place| Value::Bytes(place.extents.iter().map(|(_, len)| len).sum()))
    };
    for (kind, from) in &references {
        let (finding_kind, name) = match kind {
            b"thmb" => (FindingKind::HiddenImage, "Thumbnail"),
            b"auxl" => {
                let aux_type = associations
                    .get(from)
                    .into_iter()
                    .flatten()
                    .filter_map(|&index: &usize| properties.get(index.checked_sub(1)?))
                    .find(|(header, _)| header.kind == *b"auxC")
                    .and_then(|(_, payload)| split_nul(payload.get(4..)?))
                    .map(|(urn, _)| String::from_utf8_lossy(urn).into_owned())
                    .unwrap_or_default();
                match auxiliary(&aux_type) {
                    Some(name) => (FindingKind::HiddenImage, name),
                    None => continue,
                }
            }
            _ => continue,
        };
        let mut finding = Finding::new(
            finding_kind,
            Location::new([name]),
            Disposition::Keep(KeepReason::PartOfImage),
        );
        finding.risk = Risk::Low;
        finding.value = size(*from);
        findings.push(finding);
    }
    plan.findings.extend(findings);
    Ok(())
}

fn is_xmp(content_type: &str) -> bool {
    let content_type = content_type.to_ascii_lowercase();
    content_type.contains("rdf+xml") || content_type.contains("xmp")
}

/// What an auxiliary image is for; `None` for transparency, which is part
/// of how the picture looks. HEVC numbers them: 1 is alpha, 2 is depth.
fn auxiliary(urn: &str) -> Option<&'static str> {
    let urn = urn.to_ascii_lowercase();
    Some(if urn.contains("alpha") || urn.ends_with("auxid:1") {
        return None;
    } else if urn.contains("depth") || urn.contains("disparity") || urn.ends_with("auxid:2") {
        "Depth map"
    } else if urn.contains("gainmap") {
        "HDR gain map"
    } else if urn.contains("matte") {
        "Portrait matte"
    } else {
        "Auxiliary image"
    })
}

/// File offsets of an item's bytes, which must lie in `mdat` or `idat`:
/// anywhere else they would overlap the boxes that describe the file.
fn extents(
    place: &Place,
    idat: Option<&Header>,
    plan: &Plan,
) -> Result<Vec<(u64, u64)>, FormatError> {
    if place.data_reference != 0 || place.method > 1 {
        return Err(FormatError::Unsupported(
            "photo metadata stored in another file or another item".into(),
        ));
    }
    place
        .extents
        .iter()
        .map(|&(offset, len)| {
            let (base, inside) = match (place.method, idat) {
                (0, _) => (0, None),
                (_, Some(idat)) => (idat.payload, Some(idat.payload..idat.end)),
                (_, None) => return Err(FormatError::malformed("an item in a missing idat")),
            };
            let start = base
                .checked_add(offset)
                .ok_or_else(|| FormatError::malformed("an item outside the file"))?;
            let end = start
                .checked_add(len)
                .filter(|_| len > 0)
                .ok_or_else(|| FormatError::malformed("an item outside the file"))?;
            let fits = match inside {
                Some(range) => start >= range.start && end <= range.end,
                None => plan.in_media_data(start, end),
            };
            if fits {
                Ok((start, len))
            } else {
                Err(FormatError::malformed("an item outside the media data"))
            }
        })
        .collect()
}

fn read_extents(input: &mut dyn ReadSeek, extents: &[(u64, u64)]) -> Result<Vec<u8>, FormatError> {
    let mut data = Vec::new();
    for &(at, len) in extents {
        if data.len() as u64 + len > crate::limits::MAX_MEDIA_BOX_BYTES {
            return Err(FormatError::TooLarge {
                limit: crate::limits::MAX_MEDIA_BOX_BYTES,
            });
        }
        data.extend(boxes::read_range(input, at, len)?);
    }
    Ok(data)
}

/// Writes `content`, padded with zeros to the item's size, over its extents.
fn overwrite(
    plan: &mut Plan,
    extents: &[(u64, u64)],
    old: &[u8],
    mut content: Vec<u8>,
    what: &str,
) -> Result<(), FormatError> {
    if content.len() > old.len() {
        return Err(FormatError::malformed(format!(
            "the {what} item is too small to rewrite"
        )));
    }
    content.resize(old.len(), 0);
    if content == old {
        return Ok(());
    }
    let mut rest = content.as_slice();
    for &(at, len) in extents {
        let (part, after) =
            rest.split_at(usize::try_from(len).unwrap_or(rest.len()).min(rest.len()));
        plan.overwrite(at, part.to_vec());
        rest = after;
    }
    Ok(())
}

fn item_infos(data: &[u8]) -> Result<Vec<Item>, FormatError> {
    let version = data.first().copied().unwrap_or_default();
    let skip = if version == 0 { 6 } else { 8 };
    let mut items = Vec::new();
    for (header, payload) in boxes::parse(data, skip)? {
        if header.kind != *b"infe" {
            continue;
        }
        let mut cursor = Cursor::new(payload);
        let version = cursor.u8()?;
        cursor.skip(3)?;
        if version < 2 {
            continue;
        }
        let id = if version == 2 {
            u32::from(cursor.u16_be()?)
        } else {
            cursor.u32_be()?
        };
        cursor.skip(2)?;
        let kind = cursor.array()?;
        let rest = cursor.remaining();
        let content_type = split_nul(rest)
            .and_then(|(_, after)| split_nul(after))
            .map(|(content_type, _)| String::from_utf8_lossy(content_type).into_owned())
            .unwrap_or_default();
        items.push(Item {
            id,
            kind,
            content_type,
        });
    }
    Ok(items)
}

fn sized(cursor: &mut Cursor<'_>, size: u8) -> Result<u64, FormatError> {
    match size {
        0 => Ok(0),
        4 => Ok(u64::from(cursor.u32_be()?)),
        8 => Ok(u64::from_be_bytes(cursor.array()?)),
        _ => Err(FormatError::malformed("invalid field size in iloc")),
    }
}

fn item_places(data: &[u8]) -> Result<BTreeMap<u32, Place>, FormatError> {
    let mut cursor = Cursor::new(data);
    let version = cursor.u8()?;
    cursor.skip(3)?;
    let [sizes, more] = cursor.array()?;
    let (offset_size, length_size) = (sizes >> 4, sizes & 0x0F);
    let base_size = more >> 4;
    let index_size = if matches!(version, 1 | 2) {
        more & 0x0F
    } else {
        0
    };
    let count = if version < 2 {
        u32::from(cursor.u16_be()?)
    } else {
        cursor.u32_be()?
    };
    let mut places = BTreeMap::new();
    for _ in 0..count {
        let id = if version < 2 {
            u32::from(cursor.u16_be()?)
        } else {
            cursor.u32_be()?
        };
        let method = if matches!(version, 1 | 2) {
            cursor.u16_be()? & 0x0F
        } else {
            0
        };
        let data_reference = cursor.u16_be()?;
        let base = sized(&mut cursor, base_size)?;
        let mut extents = Vec::new();
        for _ in 0..cursor.u16_be()? {
            sized(&mut cursor, index_size)?;
            let offset = sized(&mut cursor, offset_size)?;
            let len = sized(&mut cursor, length_size)?;
            let offset = base
                .checked_add(offset)
                .ok_or_else(|| FormatError::malformed("an item outside the file"))?;
            extents.push((offset, len));
        }
        places.insert(
            id,
            Place {
                method,
                data_reference,
                extents,
            },
        );
    }
    Ok(places)
}

/// `(reference type, item it's from)`, such as a thumbnail's `thmb`.
fn item_references(data: &[u8]) -> Result<Vec<(FourCc, u32)>, FormatError> {
    let version = data.first().copied().unwrap_or_default();
    let mut references = Vec::new();
    for (header, payload) in boxes::parse(data, 4)? {
        let mut cursor = Cursor::new(payload);
        let from = if version == 0 {
            u32::from(cursor.u16_be()?)
        } else {
            cursor.u32_be()?
        };
        references.push((header.kind, from));
    }
    Ok(references)
}

/// Item → indexes of its properties in `ipco`, starting at 1.
fn item_properties(data: &[u8]) -> Result<BTreeMap<u32, Vec<usize>>, FormatError> {
    let mut cursor = Cursor::new(data);
    let version = cursor.u8()?;
    let [_, _, flags] = cursor.array::<3>()?;
    let wide = flags & 1 == 1;
    let mut associations = BTreeMap::new();
    for _ in 0..cursor.u32_be()? {
        let id = if version < 1 {
            u32::from(cursor.u16_be()?)
        } else {
            cursor.u32_be()?
        };
        let mut indexes = Vec::new();
        for _ in 0..cursor.u8()? {
            indexes.push(if wide {
                usize::from(cursor.u16_be()? & 0x7FFF)
            } else {
                usize::from(cursor.u8()? & 0x7F)
            });
        }
        associations.insert(id, indexes);
    }
    Ok(associations)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::super::boxes::tests::{full, make};
    use super::super::tests::{clean, clean_with, ftyp, inspect};
    use super::*;

    const PICTURE: &[u8] = b"PICTURE!";
    const XMP: &[u8] = br#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:creator><rdf:Seq><rdf:li>Jane Doe</rdf:li></rdf:Seq></dc:creator></rdf:Description></rdf:RDF></x:xmpmeta>                                                                                                    <?xpacket end="w"?>"#;

    /// The Exif item of a photo: an offset, `Exif\0\0`, then camera.jpg's
    /// TIFF data (GPS, serial number, orientation 6, a thumbnail).
    fn exif_item() -> Vec<u8> {
        let jpeg = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/generated/jpeg/camera.jpg"),
        )
        .unwrap();
        let at = jpeg.windows(6).position(|w| w == b"Exif\0\0").unwrap();
        let len = usize::from(u16::from_be_bytes([jpeg[at - 2], jpeg[at - 1]])) - 2;
        [&[0, 0, 0, 6][..], &jpeg[at..at + len]].concat()
    }

    fn infe(id: u16, kind: &[u8; 4], content_type: &str) -> Vec<u8> {
        let mut payload = id.to_be_bytes().to_vec();
        payload.extend([0, 0]);
        payload.extend(kind);
        payload.push(0);
        if kind == b"mime" {
            payload.extend(content_type.as_bytes());
            payload.push(0);
        }
        full(b"infe", 2, 0, &payload)
    }

    struct Entry<'a> {
        id: u16,
        kind: &'a [u8; 4],
        content_type: &'a str,
        data: &'a [u8],
        in_idat: bool,
    }

    fn entry<'a>(id: u16, kind: &'a [u8; 4], data: &'a [u8]) -> Entry<'a> {
        Entry {
            id,
            kind,
            content_type: "",
            data,
            in_idat: false,
        }
    }

    /// A photo whose items' bytes follow one another in `mdat`, or `idat`.
    fn photo(entries: &[Entry<'_>], outside: bool) -> Vec<u8> {
        let build = |mdat_start: u32| {
            let mut infos = (entries.len() as u16).to_be_bytes().to_vec();
            let mut places = vec![0x44, 0x00];
            places.extend((entries.len() as u16).to_be_bytes());
            let (mut in_mdat, mut in_idat) = (0u32, 0u32);
            let mut mdat = Vec::new();
            let mut idat = Vec::new();
            for entry in entries {
                infos.extend(infe(entry.id, entry.kind, entry.content_type));
                let len = entry.data.len() as u32;
                let (method, offset) = if entry.in_idat {
                    idat.extend(entry.data);
                    in_idat += len;
                    (1u16, in_idat - len)
                } else if outside {
                    (0, 0)
                } else {
                    mdat.extend(entry.data);
                    in_mdat += len;
                    (0, mdat_start + in_mdat - len)
                };
                places.extend(entry.id.to_be_bytes());
                places.extend(method.to_be_bytes());
                places.extend([0, 0, 0, 1]);
                places.extend(offset.to_be_bytes());
                places.extend(len.to_be_bytes());
            }
            let mut references = Vec::new();
            for (kind, from) in [(b"thmb", 4u16), (b"auxl", 5), (b"auxl", 6)] {
                let mut payload = from.to_be_bytes().to_vec();
                payload.extend([0, 1, 0, 1]);
                references.extend(make(kind, &payload));
            }
            let properties = make(
                b"ipco",
                &[
                    full(b"auxC", 0, 0, b"urn:mpeg:hevc:2015:auxid:2\0"),
                    full(b"auxC", 0, 0, b"urn:mpeg:hevc:2015:auxid:1\0"),
                ]
                .concat(),
            );
            let associations = full(b"ipma", 0, 0, &[0, 0, 0, 2, 0, 5, 1, 1, 0, 6, 1, 2]);
            let meta = full(
                b"meta",
                0,
                0,
                &[
                    full(b"hdlr", 0, 0, b"\0\0\0\0pict\0\0\0\0\0\0\0\0\0\0\0\0\0"),
                    full(b"pitm", 0, 0, &[0, 1]),
                    full(b"iinf", 0, 0, &infos),
                    full(b"iloc", 1, 0, &places),
                    full(b"iref", 0, 0, &references),
                    make(b"iprp", &[properties, associations].concat()),
                    make(b"idat", &idat),
                ]
                .concat(),
            );
            [
                ftyp(b"heic", &[b"mif1", b"heic"]),
                meta,
                make(b"mdat", &mdat),
            ]
            .concat()
        };
        // The boxes before the data have the same size whatever the offsets.
        let draft = build(0);
        build(u32::try_from(draft.len()).unwrap() - in_mdat_len(entries, outside))
    }

    fn in_mdat_len(entries: &[Entry<'_>], outside: bool) -> u32 {
        if outside {
            return 0;
        }
        entries
            .iter()
            .filter(|e| !e.in_idat)
            .map(|e| e.data.len() as u32)
            .sum()
    }

    fn standard(exif: &[u8]) -> Vec<u8> {
        photo(
            &[
                entry(1, b"hvc1", PICTURE),
                entry(2, b"Exif", exif),
                Entry {
                    content_type: "application/rdf+xml",
                    ..entry(3, b"mime", XMP)
                },
                entry(4, b"hvc1", b"THUMB"),
                entry(5, b"hvc1", b"DEPTH"),
                entry(6, b"hvc1", b"ALPHA"),
            ],
            false,
        )
    }

    fn kinds(findings: &[Finding]) -> Vec<(FindingKind, String)> {
        findings
            .iter()
            .map(|f| (f.kind, f.location.to_string()))
            .collect()
    }

    #[test]
    fn exif_and_xmp_are_overwritten_and_the_picture_stays() {
        let exif = exif_item();
        let data = standard(&exif);
        let (cleaned, findings) = clean(&data);
        assert_eq!(
            findings,
            inspect(&data).unwrap(),
            "inspect says what clean does"
        );
        let found = kinds(&findings);
        for expected in [
            (FindingKind::GpsLocation, "EXIF > GPS"),
            (FindingKind::DeviceSerial, "EXIF > BodySerialNumber"),
            (FindingKind::PersonName, "XMP > dc:creator"),
            (FindingKind::HiddenImage, "Thumbnail"),
            (FindingKind::HiddenImage, "Depth map"),
        ] {
            assert!(
                found.contains(&(expected.0, expected.1.to_owned())),
                "{expected:?} in {found:?}"
            );
        }
        assert_eq!(
            found
                .iter()
                .filter(|(k, _)| *k == FindingKind::HiddenImage)
                .count(),
            2
        );
        let depth = findings
            .iter()
            .find(|f| f.location.to_string() == "Depth map")
            .unwrap();
        assert_eq!(depth.risk, Risk::Low);
        assert_eq!(
            depth.disposition,
            Disposition::Keep(KeepReason::PartOfImage)
        );

        assert_eq!(cleaned.len(), data.len());
        for part in [PICTURE, b"THUMB", b"DEPTH", b"ALPHA"] {
            assert!(cleaned.windows(part.len()).any(|w| w == part));
        }
        let at = data.windows(exif.len()).position(|w| w == exif).unwrap();
        let rebuilt = &cleaned[at..at + exif.len()];
        let expected = [EXIF_PREFIX, &exif::orientation_only(6)].concat();
        assert_eq!(&rebuilt[..expected.len()], expected);
        assert!(rebuilt[expected.len()..].iter().all(|&b| b == 0));
        let at = data.windows(XMP.len()).position(|w| w == XMP).unwrap();
        assert_eq!(
            cleaned[at..at + XMP.len()],
            xmp::empty_packet(XMP.len()).unwrap()
        );

        let left: Vec<_> = inspect(&cleaned)
            .unwrap()
            .into_iter()
            .filter(|f| !f.disposition.is_acceptable_after_cleaning())
            .collect();
        assert_eq!(left, []);
        assert_eq!(clean(&cleaned).0, cleaned, "cleaning twice changes nothing");
    }

    #[test]
    fn without_orientation_the_exif_item_is_empty() {
        let data = standard(&exif_item());
        let options = Options {
            strip_orientation: true,
            ..Options::default()
        };
        let (cleaned, _) = clean_with(&data, &options);
        let found = kinds(&inspect(&cleaned).unwrap());
        assert_eq!(
            found,
            [
                (FindingKind::HiddenImage, "Thumbnail".to_owned()),
                (FindingKind::HiddenImage, "Depth map".to_owned()),
            ]
        );
    }

    #[test]
    fn items_in_idat_are_overwritten_too() {
        let exif = exif_item();
        let data = photo(
            &[
                entry(1, b"hvc1", PICTURE),
                Entry {
                    in_idat: true,
                    ..entry(2, b"Exif", &exif)
                },
            ],
            false,
        );
        let (cleaned, findings) = clean(&data);
        assert!(findings.iter().any(|f| f.kind == FindingKind::GpsLocation));
        assert!(
            !inspect(&cleaned)
                .unwrap()
                .iter()
                .any(|f| f.kind == FindingKind::GpsLocation)
        );
    }

    #[test]
    fn items_outside_the_media_data_are_refused() {
        let data = photo(&[entry(2, b"Exif", &exif_item())], true);
        assert!(inspect(&data).is_err());
    }
}
