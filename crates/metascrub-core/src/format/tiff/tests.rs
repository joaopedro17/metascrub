use std::io::Cursor;

use super::*;
use crate::format::icc::tests::profile;

/// A tag's value, laid out by [`build`].
#[derive(Clone)]
pub(crate) enum V {
    Short(u16),
    Long(u32),
    Ascii(&'static str),
    /// Any type and its little- or big-endian bytes.
    Raw(u16, Vec<u8>),
    /// Picture data: becomes StripOffsets, with StripByteCounts added.
    Strip(Vec<u8>),
    /// A pointer to another directory (EXIF, GPS).
    Ifd(Vec<(u16, V)>),
    /// SubIFDs.
    Ifds(Vec<Vec<(u16, V)>>),
    /// Bytes no tag points to (the tag number is ignored).
    Unreferenced(Vec<u8>),
}

pub(crate) struct Builder {
    pub big: bool,
    pub little: bool,
    out: Vec<u8>,
}

impl Builder {
    fn layout(&self) -> Layout {
        Layout {
            little: self.little,
            big: self.big,
        }
    }

    fn n16(&self, n: u16) -> Vec<u8> {
        if self.little {
            n.to_le_bytes()
        } else {
            n.to_be_bytes()
        }
        .to_vec()
    }

    fn n32(&self, n: u32) -> Vec<u8> {
        if self.little {
            n.to_le_bytes()
        } else {
            n.to_be_bytes()
        }
        .to_vec()
    }

    fn align(&mut self) {
        if self.out.len() % 2 == 1 {
            self.out.push(0);
        }
    }

    fn place(&mut self, data: &[u8]) -> u64 {
        self.align();
        let at = self.out.len() as u64;
        self.out.extend_from_slice(data);
        at
    }

    /// Writes the values, then the directory; returns where it is.
    fn ifd(&mut self, entries: &[(u16, V)], next: u64) -> u64 {
        let mut flat: Vec<(u16, u16, u64, Vec<u8>)> = Vec::new();
        let pointer = |b: &Self, at: u64| -> (u16, Vec<u8>) {
            if b.big {
                (16, b.layout().offset_bytes(at))
            } else {
                (4, b.n32(at as u32))
            }
        };
        for (tag, value) in entries {
            match value {
                V::Short(n) => flat.push((*tag, 3, 1, self.n16(*n))),
                V::Long(n) => flat.push((*tag, 4, 1, self.n32(*n))),
                V::Ascii(s) => {
                    let mut b = s.as_bytes().to_vec();
                    b.push(0);
                    flat.push((*tag, 2, b.len() as u64, b));
                }
                V::Raw(kind, b) => {
                    let size = match kind {
                        3 | 8 => 2,
                        4 | 9 | 11 | 13 => 4,
                        5 | 10 | 12 | 16 | 17 | 18 => 8,
                        _ => 1,
                    };
                    flat.push((*tag, *kind, (b.len() / size) as u64, b.clone()));
                }
                V::Strip(data) => {
                    let at = self.place(data);
                    let (kind, b) = pointer(self, at);
                    flat.push((tags::STRIP_OFFSETS, kind, 1, b));
                    let (kind, b) = pointer(self, data.len() as u64);
                    flat.push((tags::STRIP_BYTE_COUNTS, kind, 1, b));
                }
                V::Ifd(child) => {
                    let at = self.ifd(child, 0);
                    let (kind, b) = pointer(self, at);
                    flat.push((*tag, kind, 1, b));
                }
                V::Unreferenced(data) => {
                    self.place(data);
                }
                V::Ifds(children) => {
                    let mut b = Vec::new();
                    let mut kind = 4;
                    for child in children {
                        let at = self.ifd(child, 0);
                        let p = pointer(self, at);
                        kind = p.0;
                        b.extend(p.1);
                    }
                    flat.push((*tag, kind, children.len() as u64, b));
                }
            }
        }
        flat.sort_by_key(|e| e.0);
        let inline = if self.big { 8 } else { 4 };
        let mut values = Vec::new();
        for (_, _, _, b) in &mut flat {
            if b.len() > inline {
                let at = self.place(b);
                values.push(Some(at));
            } else {
                values.push(None);
            }
        }
        self.align();
        let at = self.out.len() as u64;
        let layout = self.layout();
        let mut table = layout.count_bytes(flat.len() as u64);
        for ((tag, kind, count, b), placed) in flat.iter().zip(values) {
            table.extend(self.n16(*tag));
            table.extend(self.n16(*kind));
            if self.big {
                table.extend(layout.offset_bytes(*count));
            } else {
                table.extend(self.n32(*count as u32));
            }
            let mut value = match placed {
                Some(at) => layout.offset_bytes(at),
                None => b.clone(),
            };
            value.resize(inline, 0);
            table.extend(value);
        }
        table.extend(layout.offset_bytes(next));
        self.out.extend(table);
        at
    }
}

/// A TIFF with `pages`, little-endian unless `big_endian`, BigTIFF if `big`.
pub(crate) fn build(pages: &[Vec<(u16, V)>], little: bool, big: bool) -> Vec<u8> {
    let mut b = Builder {
        big,
        little,
        out: Vec::new(),
    };
    b.out.extend(if little { b"II" } else { b"MM" });
    if big {
        b.out.extend(b.n16(43));
        b.out.extend(b.n16(8));
        b.out.extend(b.n16(0));
        b.out.extend([0; 8]);
    } else {
        b.out.extend(b.n16(42));
        b.out.extend([0; 4]);
    }
    let mut next = 0;
    for page in pages.iter().rev() {
        next = b.ifd(page, next);
    }
    let first = b.layout().offset_bytes(next);
    let at = if big { 8 } else { 4 };
    b.out[at..at + first.len()].copy_from_slice(&first);
    b.out
}

pub(crate) const PIXELS: [u8; 12] = [0, 40, 80, 120, 160, 200, 240, 30, 70, 110, 150, 190];

/// A 4×3 gray picture's tags.
pub(crate) fn gray() -> Vec<(u16, V)> {
    vec![
        (256, V::Short(4)),
        (257, V::Short(3)),
        (258, V::Short(8)),
        (259, V::Short(1)),
        (262, V::Short(1)),
        (273, V::Strip(PIXELS.to_vec())),
        (277, V::Short(1)),
        (278, V::Short(3)),
        (
            282,
            V::Raw(5, [72u32.to_le_bytes(), 1u32.to_le_bytes()].concat()),
        ),
        (
            283,
            V::Raw(5, [72u32.to_le_bytes(), 1u32.to_le_bytes()].concat()),
        ),
        (296, V::Short(2)),
    ]
}

fn with(mut tags: Vec<(u16, V)>, more: &[(u16, V)]) -> Vec<(u16, V)> {
    tags.extend_from_slice(more);
    tags
}

fn run(data: &[u8], options: &Options) -> Result<(Vec<u8>, Vec<Finding>), FormatError> {
    let mut out = Vec::new();
    let findings = Tiff.clean(&mut Cursor::new(data), &mut out, options)?;
    let inspected = Tiff.inspect(&mut Cursor::new(data), options)?;
    assert_eq!(inspected, findings, "inspect and clean disagree");
    Ok((out, findings))
}

fn clean(data: &[u8]) -> (Vec<u8>, Vec<Finding>) {
    run(data, &Options::default()).unwrap()
}

/// Cleaning `data` leaves nothing to remove, and a second cleaning
/// changes nothing.
fn assert_clean(out: &[u8]) -> Vec<Finding> {
    let (again, findings) = clean(out);
    assert_eq!(again, out, "cleaning twice changed the output");
    assert!(
        findings
            .iter()
            .all(|f| f.disposition.is_acceptable_after_cleaning()),
        "{findings:?}"
    );
    findings
}

fn kinds(findings: &[Finding]) -> Vec<FindingKind> {
    findings.iter().map(|f| f.kind).collect()
}

/// The tags of every directory of the cleaned file, as the `exif` crate reads them.
fn tags_of(data: &[u8]) -> Vec<u16> {
    let parsed = exif::Reader::new().read_raw(data.to_vec()).unwrap();
    parsed.fields().map(|f| f.tag.number()).collect()
}

fn gps() -> Vec<(u16, V)> {
    let dms = |d: u32, m: u32| -> Vec<u8> {
        [d, 1, m, 1, 0, 1]
            .iter()
            .flat_map(|n| n.to_le_bytes())
            .collect()
    };
    vec![
        (1, V::Ascii("N")),
        (2, V::Raw(5, dms(38, 42))),
        (3, V::Ascii("W")),
        (4, V::Raw(5, dms(9, 8))),
    ]
}

#[test]
fn metadata_goes_and_pixels_stay() {
    let exif = vec![
        (0x9003, V::Ascii("2026:03:14 09:26:52")),
        (0xA431, V::Ascii("SN 0042")),
        (0x927C, V::Raw(7, b"Nikon\0 private stuff".to_vec())),
        (0xA005, V::Ifd(vec![(1, V::Ascii("R98"))])),
    ];
    let mut data = build(
        &[with(
            gray(),
            &[
                (271, V::Ascii("Scanner Co")),
                (272, V::Ascii("Model 9")),
                (305, V::Ascii("ScanTool 2.1")),
                (306, V::Ascii("2026:03:14 09:26:52")),
                (315, V::Ascii("Jane Doe")),
                (
                    700,
                    V::Raw(1, br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"/>"#.to_vec()),
                ),
                (34665, V::Ifd(exif)),
                (34853, V::Ifd(gps())),
            ],
        )],
        true,
        false,
    );
    data.extend(b"appended");
    let (out, findings) = clean(&data);
    let found = kinds(&findings);
    for kind in [
        FindingKind::DeviceModel,
        FindingKind::Software,
        FindingKind::Timestamp,
        FindingKind::PersonName,
        FindingKind::DeviceSerial,
        FindingKind::UnknownMetadata,
        FindingKind::GpsLocation,
        FindingKind::TrailingData,
    ] {
        assert!(found.contains(&kind), "{kind:?} in {found:?}");
    }
    let gps = findings
        .iter()
        .find(|f| f.kind == FindingKind::GpsLocation)
        .unwrap();
    assert_eq!(gps.location.to_string(), "TIFF > GPS");
    assert!(matches!(gps.value, Some(Value::Coordinates { .. })));
    assert_eq!(
        tags_of(&out),
        [256, 257, 258, 259, 262, 273, 277, 278, 279, 282, 283, 296]
    );
    let at = out.windows(PIXELS.len()).position(|w| w == PIXELS).unwrap();
    assert_eq!(&data[at..at + PIXELS.len()], PIXELS, "the pixels moved");
    assert!(!out.windows(8).any(|w| w == b"Jane Doe"));
    assert!(!out.windows(5).any(|w| w == b"Nikon"));
    assert!(out.len() < data.len());
    assert!(assert_clean(&out).is_empty());
}

#[test]
fn every_byte_order_and_bigtiff() {
    for (little, big) in [(true, false), (false, false), (true, true), (false, true)] {
        let data = build(
            &[with(gray(), &[(305, V::Ascii("ScanTool 2.1"))])],
            little,
            big,
        );
        let (out, findings) = clean(&data);
        assert_eq!(kinds(&findings), [FindingKind::Software], "{little} {big}");
        assert!(out.windows(PIXELS.len()).any(|w| w == PIXELS));
        assert!(assert_clean(&out).is_empty());
    }
}

#[test]
fn every_page_is_cleaned() {
    let data = build(
        &[
            with(gray(), &[(315, V::Ascii("Jane Doe"))]),
            with(
                gray(),
                &[
                    (315, V::Ascii("John Roe")),
                    (297, V::Raw(3, vec![1, 0, 2, 0])),
                ],
            ),
        ],
        true,
        false,
    );
    let (out, findings) = clean(&data);
    let places: Vec<_> = findings.iter().map(|f| f.location.to_string()).collect();
    assert_eq!(places, ["TIFF > Artist", "TIFF > Page 2 > Artist"]);
    assert_eq!(
        out.windows(PIXELS.len()).filter(|w| *w == PIXELS).count(),
        2
    );
    assert!(assert_clean(&out).is_empty());
}

#[test]
fn rotation_is_kept_unless_asked() {
    let data = build(&[with(gray(), &[(274, V::Short(6))])], true, false);
    let (out, findings) = clean(&data);
    assert_eq!(
        findings[0].disposition,
        Disposition::Keep(KeepReason::RequiredToRender)
    );
    assert!(tags_of(&out).contains(&274));
    let strip = Options {
        strip_orientation: true,
        ..Options::default()
    };
    let (out, _) = run(&data, &strip).unwrap();
    assert!(!tags_of(&out).contains(&274));
}

#[test]
fn color_profiles_follow_the_usual_rule() {
    let calibrated = profile(
        "EIZO calibrated",
        [[0.5, 0.25, 0.01], [0.3, 0.6, 0.1], [0.15, 0.06, 0.7]],
        None,
    );
    let data = build(
        &[with(gray(), &[(34675, V::Raw(7, calibrated))])],
        true,
        false,
    );
    let (out, findings) = clean(&data);
    assert_eq!(findings[0].kind, FindingKind::ColorProfile);
    assert_eq!(findings[0].disposition, Disposition::Remove);
    assert!(!tags_of(&out).contains(&34675));
    let keep = Options {
        keep_color_profile: true,
        ..Options::default()
    };
    let (out, _) = run(&data, &keep).unwrap();
    assert!(tags_of(&out).contains(&34675));
}

#[test]
fn geotiff_placement_goes_unless_kept() {
    let tiepoint: Vec<u8> = [0.0f64, 0.0, 0.0, -9.14, 38.71, 0.0]
        .iter()
        .flat_map(|n| n.to_le_bytes())
        .collect();
    let data = build(
        &[with(gray(), &[(33922, V::Raw(12, tiepoint))])],
        true,
        false,
    );
    let (out, findings) = clean(&data);
    assert_eq!(kinds(&findings), [FindingKind::GpsLocation]);
    assert_eq!(findings[0].location.to_string(), "TIFF > GeoTIFF");
    assert!(!tags_of(&out).contains(&33922));
    let keep = Options {
        keep_location: true,
        ..Options::default()
    };
    let (out, findings) = run(&data, &keep).unwrap();
    assert_eq!(
        findings[0].disposition,
        Disposition::Keep(KeepReason::UserRequested)
    );
    assert!(tags_of(&out).contains(&33922));
}

#[test]
fn photoshop_layers_go_and_the_picture_stays() {
    let data = build(
        &[with(
            gray(),
            &[(
                37724,
                V::Raw(7, b"Adobe Photoshop Document Data Block\0layers".to_vec()),
            )],
        )],
        true,
        false,
    );
    let (out, findings) = clean(&data);
    assert_eq!(kinds(&findings), [FindingKind::HiddenImage]);
    assert_eq!(findings[0].disposition, Disposition::Remove);
    assert!(!out.windows(6).any(|w| w == b"layers"));
}

fn dng(extra_raw: &[(u16, V)]) -> Vec<u8> {
    let raw = with(
        vec![
            (254, V::Long(0)),
            (256, V::Short(4)),
            (257, V::Short(3)),
            (258, V::Short(8)),
            (259, V::Short(1)),
            (262, V::Short(32803)),
            (273, V::Strip(PIXELS.to_vec())),
            (277, V::Short(1)),
            (278, V::Short(3)),
            (33421, V::Raw(3, vec![2, 0, 2, 0])),
            (33422, V::Raw(1, vec![0, 1, 1, 2])),
            (50717, V::Short(255)),
        ],
        extra_raw,
    );
    let mut preview = gray();
    preview.retain(|(tag, _)| *tag != 273);
    preview.push((273, V::Strip(vec![9; 12])));
    build(
        &[with(
            preview,
            &[
                (254, V::Long(1)),
                (271, V::Ascii("Canon")),
                (330, V::Ifds(vec![raw])),
                (50706, V::Raw(1, vec![1, 6, 0, 0])),
                (50708, V::Ascii("Canon EOS R5")),
                (50735, V::Ascii("0123456789")),
                (50740, V::Raw(1, b"Adobe\0MakN maker notes".to_vec())),
                (50827, V::Ascii("IMG_0042.CR3")),
                (34665, V::Ifd(vec![(0xA435, V::Ascii("LENS-77"))])),
            ],
        )],
        true,
        false,
    )
}

#[test]
fn dng_keeps_what_raw_editors_need() {
    let data = dng(&[]);
    let mut input = Cursor::new(&data);
    assert!(is_dng(&mut input).unwrap());
    let (out, findings) = clean(&data);
    let summary: Vec<_> = findings
        .iter()
        .map(|f| (f.location.to_string(), f.kind, f.disposition))
        .collect();
    assert_eq!(
        summary,
        [
            (
                "DNG > Preview".into(),
                FindingKind::HiddenImage,
                Disposition::Keep(KeepReason::PartOfImage)
            ),
            (
                "DNG > EXIF > LensSerialNumber".into(),
                FindingKind::DeviceSerial,
                Disposition::Remove
            ),
            (
                "DNG > UniqueCameraModel".into(),
                FindingKind::DeviceModel,
                Disposition::Keep(KeepReason::RequiredToRender)
            ),
            (
                "DNG > CameraSerialNumber".into(),
                FindingKind::DeviceSerial,
                Disposition::Remove
            ),
            (
                "DNG > DNGPrivateData".into(),
                FindingKind::UnknownMetadata,
                Disposition::Remove
            ),
            (
                "DNG > OriginalRawFileName".into(),
                FindingKind::FileName,
                Disposition::Remove
            ),
            (
                "DNG > Make".into(),
                FindingKind::DeviceModel,
                Disposition::Remove
            ),
        ]
    );
    assert!(out.windows(PIXELS.len()).any(|w| w == PIXELS));
    assert!(out.windows(12).any(|w| w == b"Canon EOS R5"));
    assert!(!out.windows(10).any(|w| w == b"0123456789"));
    assert_eq!(assert_clean(&out).len(), 2);
}

#[test]
fn other_raw_photos_are_refused() {
    // A color filter array without DNGVersion: NEF, ARW, PEF…
    let mut raw = gray();
    raw.retain(|(tag, _)| *tag != 262);
    raw.push((262, V::Short(32803)));
    let data = build(&[raw], true, false);
    assert!(matches!(
        run(&data, &Options::default()),
        Err(FormatError::Unsupported(_))
    ));
    let mut cr2 = build(&[gray()], true, false);
    cr2[8..10].copy_from_slice(b"CR");
    assert!(matches!(
        run(&cr2, &Options::default()),
        Err(FormatError::Unsupported(_))
    ));
}

#[test]
fn unknown_compressions_are_refused() {
    let mut tags = gray();
    tags.retain(|(tag, _)| *tag != 259);
    // Old-style JPEG, whose stream can carry its own metadata elsewhere.
    tags.push((259, V::Short(6)));
    let data = build(&[tags], true, false);
    assert!(matches!(
        run(&data, &Options::default()),
        Err(FormatError::Unsupported(_))
    ));
}

#[test]
fn hidden_bytes_are_zeroed() {
    let data = build(
        &[with(gray(), &[(0, V::Unreferenced(b"Jane".to_vec()))])],
        true,
        false,
    );
    let at = data.windows(4).position(|w| w == b"Jane").unwrap();
    let (out, findings) = clean(&data);
    assert_eq!(kinds(&findings), [FindingKind::UnknownMetadata]);
    assert_eq!(findings[0].location.to_string(), "TIFF > Unused space");
    assert_eq!(&out[at..at + 4], [0; 4]);
    assert_eq!(out.len(), data.len());
}

#[test]
fn jpeg_strips_lose_their_comments() {
    let mut stream = vec![0xFF, 0xD8, 0xFF, 0xFE, 0, 10];
    stream.extend(b"Jane Doe");
    stream.extend([0xFF, 0xDA, 0, 2, 1, 2, 3, 0xFF, 0xD9]);
    let mut tags = gray();
    tags.retain(|(tag, _)| *tag != 259 && *tag != 273);
    tags.extend([(259, V::Short(7)), (273, V::Strip(stream.clone()))]);
    let data = build(&[tags], true, false);
    let (out, findings) = clean(&data);
    assert_eq!(kinds(&findings), [FindingKind::Comment]);
    assert!(!out.windows(8).any(|w| w == b"Jane Doe"));
    assert_eq!(out.len(), data.len());
    assert!(assert_clean(&out).is_empty());
}

#[test]
fn damaged_files_are_errors() {
    let data = build(&[gray()], true, false);
    for len in [0, 3, 7, 9, data.len() - 1] {
        assert!(
            run(&data[..len], &Options::default()).is_err(),
            "cut at {len}"
        );
    }
    // The first directory points to itself as the next one.
    let mut looping = data.clone();
    let first = u32::from_le_bytes(looping[4..8].try_into().unwrap()) as usize;
    let entries = u16::from_le_bytes(looping[first..first + 2].try_into().unwrap()) as usize;
    let next = first + 2 + entries * 12;
    looping[next..next + 4].copy_from_slice(&(first as u32).to_le_bytes());
    assert!(run(&looping, &Options::default()).is_err());
}
