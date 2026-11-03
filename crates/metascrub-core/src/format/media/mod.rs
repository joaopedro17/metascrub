//! MP4, QuickTime, M4A, HEIC and AVIF files share one box structure.
//! Cleaning never moves a byte: metadata boxes become `free` space filled
//! with zeros, dates and the samples of data tracks are zeroed, and photo
//! metadata is overwritten at its own size. Every offset in the file, such
//! as where each video frame starts, stays valid.

mod boxes;
mod items;
mod movie;
mod tags;

use std::io::{self, Read, SeekFrom, Write};
use std::ops::Range;

use boxes::{FourCc, Header};

use super::{FormatError, FormatHandler, ReadSeek, xmp};
use crate::limits::MAX_MEDIA_BOX_BYTES;
use crate::model::{Disposition, Finding, FindingKind, FormatId, Location, Value};
use crate::options::Options;

/// The `uuid` of an XMP box.
const XMP_UUID: [u8; 16] = [
    0xBE, 0x7A, 0xCF, 0xCB, 0x97, 0xA9, 0x42, 0xE8, 0x9C, 0x71, 0x99, 0x94, 0x91, 0xE3, 0xAF, 0xAC,
];

/// `ftyp` is short; anything longer than this isn't one worth reading.
const MAX_FILE_TYPE_BYTES: u64 = 4096;

#[derive(Debug)]
pub(super) struct Media;

impl FormatHandler for Media {
    fn inspect(
        &self,
        input: &mut dyn ReadSeek,
        options: &Options,
    ) -> Result<Vec<Finding>, FormatError> {
        process(input, None, options)
    }

    fn clean(
        &self,
        input: &mut dyn ReadSeek,
        output: &mut dyn Write,
        options: &Options,
    ) -> Result<Vec<Finding>, FormatError> {
        process(input, Some(output), options)
    }
}

/// The format a file's `ftyp` brands name. Formats built on the same
/// structure that MetaScrub doesn't clean (3GP, camera raw files) are `None`.
pub(super) fn identify(major: FourCc, compatible: &[FourCc]) -> Option<FormatId> {
    let of = |brand: &FourCc| {
        Some(match brand {
            b"avif" | b"avis" => FormatId::Avif,
            b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx" | b"heif" | b"mif1"
            | b"mif2" | b"msf1" => FormatId::Heic,
            b"qt  " => FormatId::Mov,
            b"M4A " | b"M4B " | b"M4P " => FormatId::M4a,
            b"isom" | b"iso2" | b"iso3" | b"iso4" | b"iso5" | b"iso6" | b"iso7" | b"iso8"
            | b"iso9" | b"mp41" | b"mp42" | b"mp71" | b"avc1" | b"M4V " | b"M4VH" | b"M4VP"
            | b"dash" | b"XAVC" | b"MSNV" | b"f4v " | b"mmp4" => FormatId::Mp4,
            _ => return None,
        })
    };
    let avif = || compatible.iter().any(|b| matches!(b, b"avif" | b"avis"));
    match of(&major) {
        // The generic image brands: AVIF says so among the others.
        Some(FormatId::Heic) if matches!(&major, b"mif1" | b"mif2" | b"msf1") && avif() => {
            Some(FormatId::Avif)
        }
        Some(format) => Some(format),
        None if major.starts_with(b"3g") || major == *b"crx " => None,
        None => compatible.iter().find_map(of),
    }
}

/// Reads the brands in the `ftyp` box at the start of `input`.
pub(super) fn detect(input: &mut dyn ReadSeek) -> Result<Option<FormatId>, FormatError> {
    input.seek(SeekFrom::Start(0))?;
    let mut data = Vec::new();
    (&mut *input)
        .take(MAX_FILE_TYPE_BYTES)
        .read_to_end(&mut data)?;
    let size = data
        .first_chunk::<4>()
        .map_or(0, |size| u32::from_be_bytes(*size) as usize);
    let brands = data.get(8..size.min(data.len())).unwrap_or_default();
    let Some((major, rest)) = brands.split_first_chunk::<4>() else {
        return Ok(None);
    };
    // After the major brand comes its version, then the compatible brands.
    let compatible: Vec<FourCc> = rest
        .get(4..)
        .unwrap_or_default()
        .as_chunks::<4>()
        .0
        .to_vec();
    Ok(identify(*major, &compatible))
}

#[derive(Debug)]
enum Fill {
    Bytes(Vec<u8>),
    Zeros(u64),
}

/// What cleaning reports and the bytes it changes. Nothing is written
/// until the whole file has been read.
#[derive(Debug, Default)]
struct Plan {
    findings: Vec<Finding>,
    patches: Vec<(u64, Fill)>,
    media_data: Vec<Range<u64>>,
    /// Where the cleaned file ends: before data that follows the last box.
    end: u64,
}

impl Plan {
    fn overwrite(&mut self, at: u64, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            self.patches.push((at, Fill::Bytes(bytes)));
        }
    }

    fn zero(&mut self, at: u64, len: u64) {
        if len > 0 {
            self.patches.push((at, Fill::Zeros(len)));
        }
    }

    /// Turns a box into free space: its type becomes `free`, the rest zeros.
    fn free(&mut self, header: &Header) {
        self.overwrite(header.start + 4, b"free".to_vec());
        self.zero(header.body, header.end - header.body);
    }

    /// Whether `start..end` lies within one `mdat` box, where pictures,
    /// sound and data tracks are, and never the boxes that describe them.
    fn in_media_data(&self, start: u64, end: u64) -> bool {
        self.media_data
            .iter()
            .any(|range| start >= range.start && end <= range.end)
    }

    fn write(
        mut self,
        input: &mut dyn ReadSeek,
        output: &mut dyn Write,
    ) -> Result<Vec<Finding>, FormatError> {
        self.patches.sort_by_key(|(at, _)| *at);
        input.seek(SeekFrom::Start(0))?;
        let mut at = 0;
        for (start, fill) in self.patches {
            let len = match &fill {
                Fill::Bytes(bytes) => bytes.len() as u64,
                Fill::Zeros(len) => *len,
            };
            if start < at || start + len > self.end {
                return Err(FormatError::malformed("metadata in overlapping places"));
            }
            copy(input, output, start - at)?;
            match fill {
                Fill::Bytes(bytes) => output.write_all(&bytes)?,
                Fill::Zeros(len) => {
                    io::copy(&mut io::repeat(0).take(len), output)?;
                }
            }
            input.seek(SeekFrom::Start(start + len))?;
            at = start + len;
        }
        copy(input, output, self.end - at)?;
        Ok(self.findings)
    }
}

fn copy(input: &mut dyn ReadSeek, output: &mut dyn Write, len: u64) -> Result<(), FormatError> {
    if io::copy(&mut (&mut *input).take(len), output)? != len {
        return Err(FormatError::malformed("the file ends early"));
    }
    Ok(())
}

fn process(
    input: &mut dyn ReadSeek,
    output: Option<&mut dyn Write>,
    options: &Options,
) -> Result<Vec<Finding>, FormatError> {
    let len = input.seek(SeekFrom::End(0))?;
    let (top, end) = boxes::list(input, 0, len, true)?;
    let mut plan = Plan {
        media_data: top
            .iter()
            .filter(|b| b.kind == *b"mdat")
            .map(|b| b.payload..b.end)
            .collect(),
        end: len,
        ..Plan::default()
    };
    let root = Location::new(Vec::<String>::new());
    for header in &top {
        match &header.kind {
            b"moov" => movie::movie(input, header, &mut plan)?,
            b"meta" if items::is_image(input, header)? => {
                items::image(input, header, &mut plan, options)?;
            }
            b"meta" => metadata(input, header, &Location::new(["Metadata"]), &mut plan)?,
            b"udta" => user_data(input, header, &Location::new(["User data"]), &mut plan)?,
            b"uuid" => uuid(input, header, &root, &mut plan)?,
            b"free" | b"skip" => unused(input, header, &root, &mut plan)?,
            _ => {}
        }
    }
    if end < len {
        plan.findings.push(
            Finding::new(
                FindingKind::TrailingData,
                Location::new(["End of file"]),
                Disposition::Remove,
            )
            .with_value(Value::Bytes(len - end)),
        );
        plan.end = end;
    }
    match output {
        Some(output) => plan.write(input, output),
        None => Ok(plan.findings),
    }
}

/// Reports a metadata box and turns it into free space.
fn blank(
    input: &mut dyn ReadSeek,
    header: &Header,
    location: &Location,
    plan: &mut Plan,
    read: fn(&[u8], &Location) -> Vec<Finding>,
) -> Result<(), FormatError> {
    let findings = if header.len() > MAX_MEDIA_BOX_BYTES {
        vec![
            Finding::new(
                FindingKind::UnknownMetadata,
                location.clone(),
                Disposition::Remove,
            )
            .with_value(Value::Bytes(header.len())),
        ]
    } else {
        read(&boxes::read(input, header)?, location)
    };
    plan.findings.extend(findings);
    plan.free(header);
    Ok(())
}

fn user_data(
    input: &mut dyn ReadSeek,
    header: &Header,
    location: &Location,
    plan: &mut Plan,
) -> Result<(), FormatError> {
    blank(input, header, location, plan, tags::user_data)
}

fn metadata(
    input: &mut dyn ReadSeek,
    header: &Header,
    location: &Location,
    plan: &mut Plan,
) -> Result<(), FormatError> {
    blank(input, header, location, plan, tags::metadata)
}

/// Boxes a program defines for itself, named by a 16-byte ID. Only XMP's
/// is readable.
fn uuid(
    input: &mut dyn ReadSeek,
    header: &Header,
    location: &Location,
    plan: &mut Plan,
) -> Result<(), FormatError> {
    if header.uuid == Some(XMP_UUID) {
        return blank(input, header, &location.child("XMP"), plan, xmp::inspect);
    }
    blank(
        input,
        header,
        &location.child("uuid"),
        plan,
        |data, location| {
            vec![
                Finding::new(
                    FindingKind::UnknownMetadata,
                    location.clone(),
                    Disposition::Remove,
                )
                .with_value(Value::Bytes(data.len() as u64)),
            ]
        },
    )
}

/// Free space should be empty; programs sometimes leave old data in it.
fn unused(
    input: &mut dyn ReadSeek,
    header: &Header,
    location: &Location,
    plan: &mut Plan,
) -> Result<(), FormatError> {
    let len = header.end - header.body;
    if !boxes::is_zero(input, header.body, len)? {
        plan.findings.push(
            Finding::new(
                FindingKind::UnknownMetadata,
                location.child("Unused space"),
                Disposition::Remove,
            )
            .with_value(Value::Bytes(len)),
        );
        plan.zero(header.body, len);
    }
    Ok(())
}

#[cfg(test)]
pub(super) mod tests {
    use std::io::Cursor;

    use super::boxes::tests::{full, make};
    use super::tags::tests::apple_metadata;
    use super::*;

    /// 2026-03-14 09:26:52 UTC, in QuickTime's seconds since 1904.
    const DATE: u32 = 3_856_325_212;

    pub(crate) fn ftyp(major: &[u8; 4], compatible: &[&[u8; 4]]) -> Vec<u8> {
        let mut payload = major.to_vec();
        payload.extend([0; 4]);
        for brand in compatible {
            payload.extend(*brand);
        }
        make(b"ftyp", &payload)
    }

    fn dated(kind: &[u8; 4], rest: usize) -> Vec<u8> {
        let mut payload = [DATE.to_be_bytes(), DATE.to_be_bytes()].concat();
        payload.resize(8 + rest, 0);
        full(kind, 0, 0, &payload)
    }

    fn words(numbers: &[u32]) -> Vec<u8> {
        numbers.iter().flat_map(|n| n.to_be_bytes()).collect()
    }

    /// A track with one sample per chunk, at the given offsets and sizes.
    fn track(handler: &[u8; 4], format: &[u8; 4], samples: &[(u32, u32)]) -> Vec<u8> {
        track_in(handler, format, samples, true)
    }

    fn track_in(
        handler: &[u8; 4],
        format: &[u8; 4],
        samples: &[(u32, u32)],
        this_file: bool,
    ) -> Vec<u8> {
        let count = u32::try_from(samples.len()).unwrap();
        let hdlr = full(b"hdlr", 0, 0, &[&[0; 4], &handler[..], &[0; 13]].concat());
        let entry = make(format, &[0; 78]);
        let stsd = full(b"stsd", 0, 0, &[&words(&[1])[..], &entry].concat());
        let sizes: Vec<u32> = samples.iter().map(|&(_, size)| size).collect();
        let offsets: Vec<u32> = samples.iter().map(|&(offset, _)| offset).collect();
        let stbl = make(
            b"stbl",
            &[
                stsd,
                full(b"stsz", 0, 0, &[words(&[0, count]), words(&sizes)].concat()),
                full(b"stsc", 0, 0, &words(&[1, 1, 1, 1])),
                full(b"stco", 0, 0, &[words(&[count]), words(&offsets)].concat()),
            ]
            .concat(),
        );
        let url = full(b"url ", 0, u32::from(this_file), b"");
        let dref = full(b"dref", 0, 0, &[&words(&[1])[..], &url].concat());
        let minf = make(b"minf", &[make(b"dinf", &dref), stbl].concat());
        let mdia = make(b"mdia", &[dated(b"mdhd", 12), hdlr, minf].concat());
        make(b"trak", &[dated(b"tkhd", 72), mdia].concat())
    }

    fn moov(children: &[Vec<u8>]) -> Vec<u8> {
        make(b"moov", &[dated(b"mvhd", 88), children.concat()].concat())
    }

    /// `ftyp`, `mdat` holding `media`, then what `rest` builds given where
    /// `media` starts in the file.
    fn file(media: &[u8], rest: impl Fn(u32) -> Vec<u8>) -> Vec<u8> {
        let ftyp = ftyp(b"isom", &[b"isom", b"mp41"]);
        let start = u32::try_from(ftyp.len() + 8).unwrap();
        [ftyp, make(b"mdat", media), rest(start)].concat()
    }

    pub(crate) fn inspect(data: &[u8]) -> Result<Vec<Finding>, FormatError> {
        Media.inspect(&mut Cursor::new(data), &Options::default())
    }

    pub(crate) fn clean_with(data: &[u8], options: &Options) -> (Vec<u8>, Vec<Finding>) {
        let mut out = Vec::new();
        let findings = Media
            .clean(&mut Cursor::new(data), &mut out, options)
            .unwrap();
        (out, findings)
    }

    pub(crate) fn clean(data: &[u8]) -> (Vec<u8>, Vec<Finding>) {
        clean_with(data, &Options::default())
    }

    fn position(data: &[u8], part: &[u8]) -> usize {
        data.windows(part.len()).position(|w| w == part).unwrap()
    }

    fn top_kinds(data: &[u8]) -> Vec<String> {
        let len = data.len() as u64;
        boxes::list(&mut Cursor::new(data), 0, len, true)
            .unwrap()
            .0
            .into_iter()
            .map(|b| boxes::name(b.kind))
            .collect()
    }

    #[test]
    fn identifies_formats_by_brand() {
        assert_eq!(identify(*b"qt  ", &[]), Some(FormatId::Mov));
        assert_eq!(identify(*b"M4A ", &[]), Some(FormatId::M4a));
        assert_eq!(identify(*b"heic", &[]), Some(FormatId::Heic));
        assert_eq!(identify(*b"mif1", &[*b"heic"]), Some(FormatId::Heic));
        assert_eq!(identify(*b"mif1", &[*b"avif"]), Some(FormatId::Avif));
        assert_eq!(identify(*b"abcd", &[*b"mp42"]), Some(FormatId::Mp4));
        assert_eq!(identify(*b"3gp4", &[*b"isom"]), None);
        assert_eq!(identify(*b"crx ", &[*b"isom"]), None);
        assert_eq!(identify(*b"abcd", &[]), None);
        let avif = ftyp(b"mif1", &[b"mif1", b"avif"]);
        assert_eq!(
            detect(&mut Cursor::new(&avif)).unwrap(),
            Some(FormatId::Avif)
        );
        assert_eq!(
            crate::format::sniff(&avif),
            Some(FormatId::Heic),
            "the first bytes alone can't tell"
        );
    }

    #[test]
    fn telemetry_is_zeroed_and_the_rest_stays() {
        let media = b"VIDEOFRAME-1VIDEOFRAME-2GPS5 lat lon fixGPS5 lat lon fix";
        let data = file(media, |start| {
            moov(&[
                track(b"vide", b"avc1", &[(start, 12), (start + 12, 12)]),
                track(b"meta", b"gpmd", &[(start + 24, 16), (start + 40, 16)]),
            ])
        });
        let (cleaned, findings) = clean(&data);
        assert_eq!(
            findings,
            inspect(&data).unwrap(),
            "inspect says what clean does"
        );
        let gps = findings
            .iter()
            .find(|f| f.kind == FindingKind::GpsLocation)
            .unwrap();
        assert_eq!(gps.location.to_string(), "Track 2 > GoPro telemetry");
        assert_eq!(gps.value, Some(Value::Bytes(32)));
        let dates = findings
            .iter()
            .find(|f| f.kind == FindingKind::Timestamp)
            .unwrap();
        assert_eq!(dates.disposition, Disposition::Normalize);
        assert_eq!(dates.value, Some(Value::Text("2026-03-14 09:26:52".into())));

        assert_eq!(cleaned.len(), data.len());
        let at = position(&data, media);
        assert_eq!(&cleaned[at..at + 24], b"VIDEOFRAME-1VIDEOFRAME-2");
        assert!(cleaned[at + 24..at + 56].iter().all(|&b| b == 0));
        assert_eq!(inspect(&cleaned).unwrap(), []);
        assert_eq!(clean(&cleaned).0, cleaned, "cleaning twice changes nothing");
    }

    #[test]
    fn subtitles_and_unknown_data_are_reported_but_kept() {
        let media = b"Hello thereabcd";
        let data = file(media, |start| {
            moov(&[
                track(b"sbtl", b"tx3g", &[(start, 11)]),
                track(b"meta", b"abcd", &[(start + 11, 4)]),
            ])
        });
        let (cleaned, findings) = clean(&data);
        let warnings: Vec<_> = findings
            .iter()
            .filter(|f| f.disposition == Disposition::Warn)
            .map(|f| (f.kind, f.location.to_string()))
            .collect();
        assert_eq!(
            warnings,
            [
                (FindingKind::TextTrack, "Track 1".to_owned()),
                (FindingKind::UnknownMetadata, "Track 2".to_owned()),
            ]
        );
        let at = position(&data, media);
        assert_eq!(&cleaned[at..at + media.len()], media);
    }

    #[test]
    fn data_tracks_that_cant_be_zeroed_safely_are_refused() {
        let gps = |samples: &[(u32, u32)], this_file: bool| {
            track_in(b"meta", b"gpmd", samples, this_file)
        };
        let fragmented = file(b"GPS5", |start| {
            moov(&[gps(&[(start, 4)], true), make(b"mvex", b"")])
        });
        let elsewhere = file(b"GPS5", |start| moov(&[gps(&[(start, 4)], false)]));
        let outside = file(b"GPS5", |_| moov(&[gps(&[(0, 4)], true)]));
        let overflowing = file(b"GPS5", |start| moov(&[gps(&[(start, 5)], true)]));
        for (name, data) in [
            ("fragmented", fragmented),
            ("elsewhere", elsewhere),
            ("outside", outside),
            ("overflowing", overflowing),
        ] {
            assert!(inspect(&data).is_err(), "{name}");
        }
    }

    #[test]
    fn metadata_boxes_become_free_space() {
        let mut xyz = vec![0, 18, 0x15, 0xC7];
        xyz.extend(b"+48.8584+002.2945/");
        let udta = make(b"udta", &make(b"\xA9xyz", &xyz));
        let meta = make(
            b"meta",
            &apple_metadata(&[("com.apple.quicktime.make", "Fictional")]),
        );
        let mut xmp = XMP_UUID.to_vec();
        xmp.extend(b"<x:xmpmeta xmlns:x='adobe:ns:meta/'/>");
        let data = [
            file(b"frames", |_| moov(&[udta.clone(), meta.clone()])),
            make(b"uuid", &xmp),
            make(b"uuid", &[[7; 16].as_slice(), b"vendor data"].concat()),
            make(b"free", b"old metadata"),
            make(b"free", &[0; 8]),
            b"trailing".to_vec(),
        ]
        .concat();
        let (cleaned, findings) = clean(&data);
        let found: Vec<_> = findings
            .iter()
            .map(|f| (f.kind, f.location.to_string()))
            .collect();
        for expected in [
            (FindingKind::GpsLocation, "User data > ©xyz"),
            (
                FindingKind::DeviceModel,
                "Metadata > com.apple.quicktime.make",
            ),
            (FindingKind::UnknownMetadata, "uuid"),
            (FindingKind::UnknownMetadata, "Unused space"),
            (FindingKind::TrailingData, "End of file"),
        ] {
            assert!(
                found.contains(&(expected.0, expected.1.to_owned())),
                "{expected:?} in {found:?}"
            );
        }
        assert_eq!(cleaned.len(), data.len() - b"trailing".len());
        assert_eq!(
            top_kinds(&cleaned),
            ["ftyp", "mdat", "moov", "free", "free", "free", "free"]
        );
        for secret in [
            &b"+48.8584"[..],
            b"Fictional",
            b"vendor data",
            b"old metadata",
        ] {
            assert!(
                !cleaned.windows(secret.len()).any(|w| w == secret),
                "{}",
                String::from_utf8_lossy(secret)
            );
        }
        assert_eq!(inspect(&cleaned).unwrap(), []);
        assert_eq!(clean(&cleaned).0, cleaned);
    }

    #[test]
    fn damaged_files_are_errors() {
        let data = file(b"frames", |_| moov(&[]));
        assert!(inspect(&data[..data.len() - 1]).is_err());
        let mut overlapping = data.clone();
        let at = position(&overlapping, b"mdat") - 4;
        overlapping[at + 3] = 0xFF;
        assert!(inspect(&overlapping).is_err());
    }
}
