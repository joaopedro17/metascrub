//! TIFF and DNG. Every directory (pages, sub-images such as DNG's raw data
//! and previews) is cleaned with an allowlist: tags a picture needs stay,
//! everything else goes, along with EXIF, GPS, XMP, IPTC and Photoshop
//! data. Nothing moves: directories shrink in place, and every byte no
//! kept tag points to is zeroed, so the strips and tiles stay where they are.

mod ifd;
mod streams;
mod tags;
#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::io::{self, Read, SeekFrom, Write};
use std::ops::Range;

use exif::{Context, In, Tag};
use ifd::{Entry, Layout};
use tags::Role;

use super::{
    FormatError, FormatHandler, ReadSeek, apply_options, drop_all, exif as exif_fields, icc, iptc,
    xmp,
};
use crate::limits::MAX_MEDIA_BOX_BYTES;
use crate::model::{Disposition, Finding, FindingKind, KeepReason, Location, Risk, Value};
use crate::options::Options;

type Reader = fn(&[u8], &Location) -> Vec<Finding>;

/// Directories in one file, all pages and sub-images together.
const MAX_IFDS: usize = 65_536;
/// How deep sub-images may nest (DNG uses one level).
const MAX_DEPTH: usize = 4;
/// Values shown in reports are read up to this size.
const MAX_SHOWN: u64 = 1024 * 1024;

#[derive(Debug)]
pub(super) struct Tiff;

impl FormatHandler for Tiff {
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

/// Whether the TIFF is a DNG raw photo: its first directory says which
/// DNG version it follows.
pub(super) fn is_dng(input: &mut dyn ReadSeek) -> Result<bool, FormatError> {
    let len = input.seek(SeekFrom::End(0))?;
    let (layout, first) = ifd::header(input)?;
    let ifd = ifd::read_ifd(input, layout, first, len)?;
    Ok(ifd.entries.iter().any(|e| e.tag == tags::DNG_VERSION))
}

struct Plan<'a> {
    layout: Layout,
    len: u64,
    dng: bool,
    options: &'a Options,
    findings: Vec<Finding>,
    /// Bytes kept tags point to: values, strips and tiles.
    values: Vec<Range<u64>>,
    /// The header and directories, which nothing else may overlap.
    directories: Vec<Range<u64>>,
    /// Everything kept, merged, once the walk is done.
    kept: Vec<Range<u64>>,
    /// Bytes removed tags pointed to.
    removed: Vec<Range<u64>>,
    /// Directories rewritten without their removed entries.
    rewrites: Vec<(u64, Vec<u8>)>,
    /// Metadata inside JPEG strips, zeroed at the same size.
    zeros: Vec<Range<u64>>,
    seen: HashSet<u64>,
    /// EXIF and GPS directories, which pages may share.
    seen_metadata: HashSet<u64>,
}

impl Plan<'_> {
    fn visit(&mut self, at: u64) -> Result<(), FormatError> {
        if !self.seen.insert(at) {
            return Err(FormatError::malformed("TIFF directories that loop"));
        }
        self.check_count()
    }

    fn check_count(&self) -> Result<(), FormatError> {
        if self.seen.len() + self.seen_metadata.len() > MAX_IFDS {
            return Err(FormatError::Unsupported(
                "a TIFF with too many directories".into(),
            ));
        }
        Ok(())
    }

    /// Keeps the value of `entry` when it's outside the entry.
    fn keep(&mut self, entry: &Entry) -> Result<(), FormatError> {
        match entry.range() {
            Some(range) if range.end > self.len => {
                Err(FormatError::malformed("a TIFF value outside the file"))
            }
            Some(range) => {
                self.values.push(range);
                Ok(())
            }
            None if entry.len.is_none() => Err(FormatError::malformed(
                "a TIFF tag of an unknown type that pictures need",
            )),
            None => Ok(()),
        }
    }

    fn remove(&mut self, entry: &Entry) {
        if let Some(range) = entry.range() {
            self.removed.push(range.start..range.end.min(self.len));
        }
    }

    fn bytes(
        &self,
        input: &mut dyn ReadSeek,
        entry: &Entry,
    ) -> Result<Option<Vec<u8>>, FormatError> {
        ifd::bytes(input, entry, self.len, MAX_SHOWN)
    }

    fn numbers(&self, input: &mut dyn ReadSeek, entry: &Entry) -> Result<Vec<u64>, FormatError> {
        ifd::numbers(input, self.layout, entry, self.len, MAX_MEDIA_BOX_BYTES)
    }

    fn field(
        &self,
        input: &mut dyn ReadSeek,
        entry: &Entry,
        context: Context,
    ) -> Result<Option<exif::Field>, FormatError> {
        // A removed value outside the file has nothing to show.
        let data = match self.bytes(input, entry) {
            Ok(Some(data)) => data,
            Ok(None) | Err(FormatError::Malformed(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        Ok(Some(exif::Field {
            tag: Tag(context, entry.tag),
            ifd_num: In::PRIMARY,
            value: ifd::exif_value(self.layout, entry, &data),
        }))
    }
}

fn process(
    input: &mut dyn ReadSeek,
    output: Option<&mut dyn Write>,
    options: &Options,
) -> Result<Vec<Finding>, FormatError> {
    let len = input.seek(SeekFrom::End(0))?;
    let (layout, first) = ifd::header(input)?;
    input.seek(SeekFrom::Start(8))?;
    let mut canon = [0u8; 2];
    if !layout.big && input.read_exact(&mut canon).is_ok() && canon == *b"CR" {
        return Err(FormatError::Unsupported(
            "a camera raw photo other than DNG (Canon CR2)".into(),
        ));
    }
    if first == 0 {
        return Err(FormatError::malformed("a TIFF without pictures"));
    }
    let dng = is_dng(input)?;
    let root = Location::new([if dng { "DNG" } else { "TIFF" }]);
    let mut plan = Plan {
        layout,
        len,
        dng,
        options,
        findings: Vec::new(),
        values: Vec::new(),
        directories: vec![Range {
            start: 0,
            end: layout.header_len(),
        }],
        kept: Vec::new(),
        removed: Vec::new(),
        rewrites: Vec::new(),
        zeros: Vec::new(),
        seen: HashSet::new(),
        seen_metadata: HashSet::new(),
    };
    let mut at = first;
    let mut page = 1;
    while at != 0 {
        let location = if page == 1 {
            root.clone()
        } else {
            root.child(format!("Page {page}"))
        };
        at = picture(input, &mut plan, at, &location, 0)?;
        page += 1;
    }
    let end = finish(input, &mut plan, &root)?;
    match output {
        Some(output) => write(input, output, plan, end),
        None => Ok(plan.findings),
    }
}

/// Cleans the directory of one picture (a page or a sub-image) and the
/// sub-images it lists. Returns where the next one in its chain is.
fn picture(
    input: &mut dyn ReadSeek,
    plan: &mut Plan,
    at: u64,
    location: &Location,
    depth: usize,
) -> Result<u64, FormatError> {
    plan.visit(at)?;
    let directory = ifd::read_ifd(input, plan.layout, at, plan.len)?;
    let find = |tag: u16| directory.entries.iter().find(|e| e.tag == tag);
    let first_number = |input: &mut dyn ReadSeek, plan: &Plan, tag: u16, default: u64| {
        find(tag).map_or(Ok(default), |entry| {
            plan.numbers(input, entry)
                .map(|n| n.first().copied().unwrap_or(default))
        })
    };
    let compression = first_number(input, plan, tags::COMPRESSION, 1)?;
    if !tags::known_compression(compression) {
        return Err(FormatError::Unsupported(format!(
            "TIFF compression {compression}"
        )));
    }
    if let Some(entry) = find(tags::PHOTOMETRIC) {
        let photometric = plan.numbers(input, entry)?.first().copied().unwrap_or(0);
        if !tags::known_photometric(photometric, plan.dng) {
            return Err(FormatError::Unsupported(
                if matches!(photometric, 32803 | 34892) {
                    "a camera raw photo other than DNG".into()
                } else {
                    format!("TIFF color model {photometric}")
                },
            ));
        }
    }
    let subfile = first_number(input, plan, tags::NEW_SUBFILE_TYPE, 0)?;

    let mut findings = Vec::new();
    let mut others = Vec::new();
    let mut kept: Vec<&Entry> = Vec::new();
    let mut sub_images = Vec::new();
    let mut placement = None;
    for entry in &directory.entries {
        let keep = match tags::role(entry.tag) {
            Role::Structure | Role::PictureData => true,
            Role::SubImages => {
                sub_images.extend(plan.numbers(input, entry)?);
                true
            }
            Role::Exif => {
                let mut fields = Vec::new();
                metadata(input, plan, entry, Context::Exif, &mut fields)?;
                let fields: Vec<_> = fields.iter().collect();
                findings.extend(exif_fields::field_findings(
                    &fields,
                    &location.child("EXIF"),
                ));
                false
            }
            Role::Gps | Role::Interoperability => {
                let mut fields = Vec::new();
                let context = if entry.tag == 34853 {
                    Context::Gps
                } else {
                    Context::Interop
                };
                metadata(input, plan, entry, context, &mut fields)?;
                if context == Context::Gps {
                    let fields: Vec<_> = fields.iter().collect();
                    findings.extend(exif_fields::field_findings(&fields, location));
                }
                false
            }
            Role::Orientation => {
                let Some(field) = plan.field(input, entry, Context::Tiff)? else {
                    return Err(FormatError::malformed(
                        "a TIFF orientation of an unknown type",
                    ));
                };
                let mut finding = [exif_fields::field_finding(
                    &field,
                    "",
                    FindingKind::Orientation,
                    location,
                )];
                apply_options(&mut finding, plan.options);
                let keep = matches!(finding[0].disposition, Disposition::Keep(_));
                findings.extend(finding);
                keep
            }
            Role::ColorProfile => {
                let mut finding = match plan.bytes(input, entry)? {
                    Some(data) => [icc::inspect(&data, location.child("Color profile"))],
                    None => [Finding::new(
                        FindingKind::ColorProfile,
                        location.child("Color profile"),
                        Disposition::Remove,
                    )],
                };
                apply_options(&mut finding, plan.options);
                let keep = matches!(finding[0].disposition, Disposition::Keep(_));
                findings.extend(finding);
                keep
            }
            Role::Xmp | Role::Iptc | Role::Photoshop => {
                let (name, read): (&str, Reader) = match tags::role(entry.tag) {
                    Role::Xmp => ("XMP", xmp::inspect),
                    Role::Iptc => ("IPTC", iptc::inspect_iptc),
                    _ => ("Photoshop", iptc::inspect_photoshop),
                };
                let mut found = match plan.bytes(input, entry) {
                    Ok(Some(data)) => read(&data, &location.child(name)),
                    _ => vec![unknown(location.child(name), entry)],
                };
                drop_all(&mut found);
                findings.extend(found);
                false
            }
            Role::Layers => {
                findings.push(sized(
                    FindingKind::HiddenImage,
                    location.child("Photoshop layers"),
                    entry,
                ));
                false
            }
            Role::Placement => {
                let disposition = if plan.options.keep_location {
                    Disposition::Keep(KeepReason::UserRequested)
                } else {
                    Disposition::Remove
                };
                placement = Some(disposition);
                matches!(disposition, Disposition::Keep(_))
            }
            Role::CameraModel if plan.dng => {
                let mut finding = named(
                    input,
                    plan,
                    entry,
                    "UniqueCameraModel",
                    FindingKind::DeviceModel,
                    location,
                )?;
                finding.disposition = Disposition::Keep(KeepReason::RequiredToRender);
                findings.push(finding);
                true
            }
            Role::CameraModel => {
                findings.push(named(
                    input,
                    plan,
                    entry,
                    "UniqueCameraModel",
                    FindingKind::DeviceModel,
                    location,
                )?);
                false
            }
            Role::Named(name, kind) => {
                findings.push(named(input, plan, entry, name, kind, location)?);
                false
            }
            Role::Other => {
                // EXIF tags sometimes sit in the main directory (TIFF/EP).
                let context = if exif_fields::classify(Tag(Context::Tiff, entry.tag)).is_none()
                    && Tag(Context::Exif, entry.tag).description().is_some()
                {
                    Context::Exif
                } else {
                    Context::Tiff
                };
                match plan.field(input, entry, context)? {
                    Some(field) => others.push(field),
                    None => findings.push(unknown(location.clone(), entry)),
                }
                false
            }
        };
        if keep {
            plan.keep(entry)?;
            kept.push(entry);
        } else {
            plan.remove(entry);
        }
    }
    if let Some(disposition) = placement {
        let mut finding = Finding::new(
            FindingKind::GpsLocation,
            location.child("GeoTIFF"),
            disposition,
        );
        if let Some(value) = map_coordinates(input, plan, &directory.entries)? {
            finding = finding.with_value(value);
        }
        findings.push(finding);
    }
    let others: Vec<_> = others.iter().collect();
    findings.extend(exif_fields::field_findings(&others, location));

    let data_len = picture_data(
        input,
        plan,
        &directory.entries,
        compression,
        location,
        &mut findings,
    )?;
    if subfile & 1 == 1 {
        let mut preview = Finding::new(
            FindingKind::HiddenImage,
            location.child("Preview"),
            Disposition::Keep(KeepReason::PartOfImage),
        )
        .with_value(Value::Bytes(data_len));
        preview.risk = Risk::Low;
        findings.insert(0, preview);
    }
    plan.findings.extend(findings);

    // The directory, rewritten with only what's kept.
    let layout = plan.layout;
    let size = layout.ifd_len(kept.len() as u64);
    let mut rewritten = layout.count_bytes(kept.len() as u64);
    for entry in &kept {
        rewritten.extend_from_slice(&entry.raw);
    }
    rewritten.extend(layout.offset_bytes(directory.next));
    let full = layout.ifd_len(directory.entries.len() as u64);
    plan.directories.push(at..at + size);
    plan.removed.push(at + size..at + full);
    plan.rewrites.push((at, rewritten));

    for (n, mut child) in sub_images.into_iter().enumerate() {
        if depth >= MAX_DEPTH {
            return Err(FormatError::Unsupported(
                "TIFF sub-images nested too deep".into(),
            ));
        }
        let child_location = location.child(format!("Sub-image {}", n + 1));
        while child != 0 {
            child = picture(input, plan, child, &child_location, depth + 1)?;
        }
    }
    Ok(directory.next)
}

/// Keeps the strips or tiles of a picture, checking JPEG streams for
/// metadata of their own. Returns their total size.
fn picture_data(
    input: &mut dyn ReadSeek,
    plan: &mut Plan,
    entries: &[Entry],
    compression: u64,
    location: &Location,
    findings: &mut Vec<Finding>,
) -> Result<u64, FormatError> {
    let find = |tag: u16| entries.iter().find(|e| e.tag == tag);
    let (offsets, sizes) = match (find(tags::STRIP_OFFSETS), find(tags::TILE_OFFSETS)) {
        (Some(offsets), _) => (offsets, find(tags::STRIP_BYTE_COUNTS)),
        (None, Some(offsets)) => (offsets, find(tags::TILE_BYTE_COUNTS)),
        (None, None) => return Ok(0),
    };
    let sizes = sizes.ok_or_else(|| FormatError::malformed("TIFF strips without their sizes"))?;
    let offsets = plan.numbers(input, offsets)?;
    let sizes = plan.numbers(input, sizes)?;
    if offsets.len() != sizes.len() {
        return Err(FormatError::malformed("TIFF strips without their sizes"));
    }
    let mut total = 0u64;
    let mut found = Vec::new();
    for (&at, &len) in offsets.iter().zip(&sizes) {
        // Empty tiles of a sparse picture.
        if len == 0 {
            continue;
        }
        if at.checked_add(len).is_none_or(|end| end > plan.len) {
            return Err(FormatError::malformed("TIFF pictures cut short"));
        }
        total = total.saturating_add(len);
        plan.values.push(at..at + len);
        if tags::is_jpeg(compression) {
            let (more, zeros) = streams::jpeg(input, at..at + len, location)?;
            for finding in more {
                if !found.contains(&finding) {
                    found.push(finding);
                }
            }
            plan.zeros.extend(zeros);
        } else if compression == 52546 && !streams::is_bare_jpeg_xl(input, at, len)? {
            return Err(FormatError::Unsupported(
                "JPEG XL pictures in a container inside a TIFF".into(),
            ));
        }
    }
    if tags::is_jpeg(compression)
        && let Some(range) = find(tags::JPEG_TABLES).and_then(Entry::range)
    {
        let (more, zeros) = streams::jpeg(input, range, location)?;
        found.extend(
            more.into_iter()
                .filter(|f| !found.contains(f))
                .collect::<Vec<_>>(),
        );
        plan.zeros.extend(zeros);
    }
    findings.extend(found);
    Ok(total)
}

/// Walks an EXIF, GPS or interoperability directory, which is removed whole.
fn metadata(
    input: &mut dyn ReadSeek,
    plan: &mut Plan,
    pointer: &Entry,
    context: Context,
    fields: &mut Vec<exif::Field>,
) -> Result<(), FormatError> {
    let at = plan.numbers(input, pointer)?.first().copied().unwrap_or(0);
    if at == 0 || !plan.seen_metadata.insert(at) {
        return Ok(());
    }
    plan.check_count()?;
    let directory = ifd::read_ifd(input, plan.layout, at, plan.len)?;
    plan.removed
        .push(at..at + plan.layout.ifd_len(directory.entries.len() as u64));
    for entry in &directory.entries {
        plan.remove(entry);
        match (context, entry.tag) {
            // Pointers to more directories: the interoperability one holds
            // nothing about anyone.
            (Context::Exif, 40965) => {
                metadata(input, plan, entry, Context::Interop, &mut Vec::new())?;
            }
            (Context::Exif, 34853) => metadata(input, plan, entry, Context::Gps, fields)?,
            _ => {
                if let Some(field) = plan.field(input, entry, context)? {
                    fields.push(field);
                }
            }
        }
    }
    Ok(())
}

fn named(
    input: &mut dyn ReadSeek,
    plan: &Plan,
    entry: &Entry,
    name: &str,
    kind: FindingKind,
    location: &Location,
) -> Result<Finding, FormatError> {
    // Binary values are shown by their size.
    let text = !matches!(entry.kind, 1 | 7) && kind != FindingKind::EmbeddedFile;
    Ok(match plan.field(input, entry, Context::Tiff)? {
        Some(field) if text => exif_fields::field_finding(&field, name, kind, location),
        _ => sized(kind, location.child(name), entry),
    })
}

/// Where a GeoTIFF's first tie point lies, when the map is in latitude
/// and longitude (projected maps would need the projection's maths).
fn map_coordinates(
    input: &mut dyn ReadSeek,
    plan: &Plan,
    entries: &[Entry],
) -> Result<Option<Value>, FormatError> {
    let find = |tag: u16| entries.iter().find(|e| e.tag == tag);
    let (Some(keys), Some(tiepoint)) = (find(34735), find(33922)) else {
        return Ok(None);
    };
    let keys = plan.numbers(input, keys).unwrap_or_default();
    // A 4-number header, then 4 numbers per key: id, where, count, value.
    let geographic = keys
        .get(4..)
        .unwrap_or_default()
        .as_chunks::<4>()
        .0
        .iter()
        .any(|&[id, place, _, value]| id == 1024 && place == 0 && value == 2);
    let Some(exif::Value::Double(point)) = plan
        .field(input, tiepoint, Context::Tiff)?
        .map(|field| field.value)
    else {
        return Ok(None);
    };
    Ok(match point.as_slice() {
        [_, _, _, longitude, latitude, ..]
            if geographic && latitude.abs() <= 90.0 && longitude.abs() <= 180.0 =>
        {
            Some(Value::Coordinates {
                latitude: *latitude,
                longitude: *longitude,
            })
        }
        _ => None,
    })
}

fn sized(kind: FindingKind, location: Location, entry: &Entry) -> Finding {
    let finding = Finding::new(kind, location, Disposition::Remove);
    match entry.len {
        Some(len) => finding.with_value(Value::Bytes(len)),
        None => finding,
    }
}

fn unknown(location: Location, entry: &Entry) -> Finding {
    sized(FindingKind::UnknownMetadata, location, entry)
}

/// Merges ranges that overlap or touch.
fn merge(mut ranges: Vec<Range<u64>>) -> Vec<Range<u64>> {
    ranges.retain(|r| r.start < r.end);
    ranges.sort_by_key(|r| r.start);
    let mut merged: Vec<Range<u64>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

/// The parts of `from` that none of `ranges` (merged) cover.
fn outside(from: Range<u64>, ranges: &[Range<u64>]) -> Vec<Range<u64>> {
    let mut gaps = Vec::new();
    let mut at = from.start;
    for range in ranges {
        if range.end <= at {
            continue;
        }
        if range.start >= from.end {
            break;
        }
        if range.start > at {
            gaps.push(at..range.start);
        }
        at = range.end;
    }
    if at < from.end {
        gaps.push(at..from.end);
    }
    gaps
}

fn overlaps(range: &Range<u64>, merged: &[Range<u64>]) -> bool {
    let i = merged.partition_point(|r| r.end <= range.start);
    merged.get(i).is_some_and(|r| r.start < range.end)
}

/// Checks the layout, reports bytes nothing points to, and returns where
/// the cleaned file ends.
fn finish(input: &mut dyn ReadSeek, plan: &mut Plan, root: &Location) -> Result<u64, FormatError> {
    let directories = merge(plan.directories.clone());
    let total: u64 = plan.directories.iter().map(|r| r.end - r.start).sum();
    if total != directories.iter().map(|r| r.end - r.start).sum::<u64>() {
        return Err(FormatError::malformed("TIFF directories that overlap"));
    }
    let values = merge(std::mem::take(&mut plan.values));
    if directories.iter().any(|d| overlaps(d, &values)) {
        return Err(FormatError::malformed("a TIFF directory inside a picture"));
    }
    let kept = merge([values, directories].concat());
    for range in &plan.removed {
        if range.start < range.end && overlaps(range, &kept) {
            return Err(FormatError::Unsupported(
                "TIFF metadata stored inside what the picture needs".into(),
            ));
        }
    }
    let end = kept.last().map_or(0, |r| r.end);
    let removed = merge(plan.removed.clone());
    let mut unused = 0;
    let mut after = 0;
    for gap in outside(0..plan.len, &kept) {
        for piece in outside(gap, &removed) {
            if !is_zero(input, piece.clone())? {
                if piece.start >= end {
                    after += piece.end - piece.start;
                } else {
                    unused += piece.end - piece.start;
                }
            }
        }
    }
    if unused > 0 {
        plan.findings.push(
            Finding::new(
                FindingKind::UnknownMetadata,
                root.child("Unused space"),
                Disposition::Remove,
            )
            .with_value(Value::Bytes(unused)),
        );
    }
    if after > 0 {
        plan.findings.push(
            Finding::new(
                FindingKind::TrailingData,
                Location::new(["After end of image"]),
                Disposition::Remove,
            )
            .with_value(Value::Bytes(after)),
        );
    }
    plan.kept = kept;
    Ok(end)
}

fn is_zero(input: &mut dyn ReadSeek, range: Range<u64>) -> Result<bool, FormatError> {
    input.seek(SeekFrom::Start(range.start))?;
    let mut left = range.end - range.start;
    let mut buf = vec![0u8; 64 * 1024];
    while left > 0 {
        let n = left.min(buf.len() as u64) as usize;
        let chunk = buf.get_mut(..n).unwrap_or_default();
        input.read_exact(chunk)?;
        if chunk.iter().any(|&b| b != 0) {
            return Ok(false);
        }
        left -= n as u64;
    }
    Ok(true)
}

enum Fill {
    Bytes(Vec<u8>),
    Zeros(u64),
}

/// Copies the file up to `end`, with the directories rewritten, and zeros
/// wherever no kept tag points.
fn write(
    input: &mut dyn ReadSeek,
    output: &mut dyn Write,
    plan: Plan,
    end: u64,
) -> Result<Vec<Finding>, FormatError> {
    let mut patches: Vec<(u64, Fill)> = plan
        .rewrites
        .into_iter()
        .map(|(at, bytes)| (at, Fill::Bytes(bytes)))
        .collect();
    for gap in outside(0..end, &plan.kept) {
        patches.push((gap.start, Fill::Zeros(gap.end - gap.start)));
    }
    for range in merge(plan.zeros) {
        patches.push((range.start, Fill::Zeros(range.end - range.start)));
    }
    patches.sort_by_key(|(at, _)| *at);
    input.seek(SeekFrom::Start(0))?;
    let mut at = 0;
    for (start, fill) in patches {
        let len = match &fill {
            Fill::Bytes(bytes) => bytes.len() as u64,
            Fill::Zeros(len) => *len,
        };
        if start < at || start + len > end {
            return Err(FormatError::malformed(
                "TIFF metadata in overlapping places",
            ));
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
    copy(input, output, end - at)?;
    Ok(plan.findings)
}

fn copy(input: &mut dyn ReadSeek, output: &mut dyn Write, len: u64) -> Result<(), FormatError> {
    if io::copy(&mut (&mut *input).take(len), output)? != len {
        return Err(FormatError::malformed("the file ends early"));
    }
    Ok(())
}
