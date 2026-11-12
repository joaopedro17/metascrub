//! Matroska (`.mkv`, `.mka`) and WebM. Cleaning never moves a byte:
//! metadata elements become `Void` elements of the same size, and required
//! text (the muxing and writing apps, chapter titles) is blanked with zeros.
//! Clusters (the pictures and sound) and cues are copied as they are.

mod ebml;
#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::io::{SeekFrom, Write};

use ebml::Header;

use super::patch::{self, Fill};
use super::{FormatError, FormatHandler, ReadSeek};
use crate::model::{
    Disposition, Finding, FindingKind, FormatId, KeepReason, Location, Risk, Value,
};
use crate::options::Options;

const EBML: u32 = 0x1A45_DFA3;
const DOC_TYPE: u32 = 0x4282;
const SEGMENT: u32 = 0x1853_8067;
const SEEK_HEAD: u32 = 0x114D_9B74;
const SEEK: u32 = 0x4DBB;
const SEEK_POSITION: u32 = 0x53AC;
const INFO: u32 = 0x1549_A966;
const TRACKS: u32 = 0x1654_AE6B;
const TRACK_ENTRY: u32 = 0xAE;
const CLUSTER: u32 = 0x1F43_B675;
const CUES: u32 = 0x1C53_BB6B;
const CHAPTERS: u32 = 0x1043_A770;
const ATTACHMENTS: u32 = 0x1941_A469;
const ATTACHED_FILE: u32 = 0x61A7;
const TAGS: u32 = 0x1254_C367;
const VOID: u32 = 0xEC;
const CRC32: u32 = 0xBF;

/// Metadata is read up to this size to report it.
const MAX_SHOWN: u64 = 1024 * 1024;
/// Metadata elements (tags, chapters) are walked up to this size.
const MAX_METADATA: u64 = 64 * 1024 * 1024;

#[derive(Debug)]
pub(super) struct Matroska;

impl FormatHandler for Matroska {
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

/// Matroska or WebM, from the `DocType` in the EBML header at the start of
/// `head`; `None` for other EBML documents.
pub(super) fn identify(head: &[u8]) -> Option<FormatId> {
    let (id, id_len) = ebml::id(head)?;
    if id != EBML {
        return None;
    }
    let (size, size_len) = ebml::size(head.get(id_len..)?)?;
    let start = id_len + size_len;
    let body = head.get(start..start.checked_add(usize::try_from(size).ok()?)?)?;
    let mut at = 0;
    while at < body.len() {
        let (id, id_len) = ebml::id(body.get(at..)?)?;
        let (size, size_len) = ebml::size(body.get(at + id_len..)?)?;
        let data = at + id_len + size_len;
        let end = data.checked_add(usize::try_from(size).ok()?)?;
        if id == DOC_TYPE {
            return match ebml::text(body.get(data..end)?).as_str() {
                "webm" => Some(FormatId::Webm),
                "matroska" => Some(FormatId::Mkv),
                _ => None,
            };
        }
        at = end;
    }
    None
}

#[derive(Default)]
struct Plan {
    findings: Vec<Finding>,
    patches: Vec<(u64, Fill)>,
    /// Where elements that became `Void` started.
    voided: HashSet<u64>,
    /// Each `Seek` entry, the element it points to, and its index's checksum.
    seeks: Vec<(Header, u64, Option<Header>)>,
}

impl Plan {
    fn void(&mut self, element: &Header) {
        let Some(end) = element.end else { return };
        let len = end - element.start;
        let header = ebml::void_header(len);
        let rest = len - header.len() as u64;
        self.patches.push((element.start, Fill::Bytes(header)));
        if rest > 0 {
            self.patches.push((end - rest, Fill::Zeros(rest)));
        }
        self.voided.insert(element.start);
    }

    /// Blanks a string element's data, keeping the element.
    fn blank(&mut self, element: &Header) {
        if let Some(len) = element.len().filter(|&len| len > 0) {
            self.patches.push((element.data, Fill::Zeros(len)));
        }
    }

    /// After a master element's children were cleaned: its checksum, if it
    /// has one and something inside changed, can't stay.
    fn checksum(&mut self, children: &[Header], patches_before: usize) {
        if self.patches.len() > patches_before
            && let Some(crc) = children.iter().find(|c| c.id == CRC32)
        {
            self.void(crc);
        }
    }
}

fn process(
    input: &mut dyn ReadSeek,
    output: Option<&mut dyn Write>,
    _options: &Options,
) -> Result<Vec<Finding>, FormatError> {
    let len = input.seek(SeekFrom::End(0))?;
    let first = ebml::header(input, 0, len)?;
    if first.id != EBML {
        return Err(FormatError::malformed("missing the EBML header"));
    }
    doc_type(input, &first)?;
    let mut plan = Plan::default();
    let mut at = first
        .end
        .ok_or_else(|| FormatError::malformed("an EBML header of unknown size"))?;
    let mut end = at;
    let mut segments = 0;
    while at < len {
        let Ok(element) = ebml::header(input, at, len) else {
            break;
        };
        match element.id {
            SEGMENT => {
                segments += 1;
                at = segment(input, &mut plan, &element, len)?;
                end = at;
            }
            // Another document after the first (some live streams).
            EBML if element.end.is_some() => {
                doc_type(input, &element)?;
                at = element.end.unwrap_or(len);
                end = at;
            }
            VOID if element.end.is_some() => {
                at = element.end.unwrap_or(len);
                end = at;
            }
            _ => break,
        }
    }
    if segments == 0 {
        return Err(FormatError::malformed("a Matroska file without a segment"));
    }
    if end < len {
        plan.findings.push(
            Finding::new(
                FindingKind::TrailingData,
                Location::new(["After end of video"]),
                Disposition::Remove,
            )
            .with_value(Value::Bytes(len - end)),
        );
    }
    match output {
        Some(output) => {
            patch::write(input, output, plan.patches, end)?;
            Ok(plan.findings)
        }
        None => Ok(plan.findings),
    }
}

fn doc_type(input: &mut dyn ReadSeek, header: &Header) -> Result<(), FormatError> {
    for child in ebml::children(input, header)? {
        if child.id == DOC_TYPE {
            let data = ebml::data(input, &child, 64)?.unwrap_or_default();
            let doc_type = ebml::text(&data);
            return match doc_type.as_str() {
                "matroska" | "webm" => Ok(()),
                other => Err(FormatError::Unsupported(format!(
                    "EBML documents of type {other:?}"
                ))),
            };
        }
    }
    // Matroska is the default document type.
    Ok(())
}

/// Whether an element ID belongs directly in a segment: one of these ends
/// a cluster of unknown size.
fn is_top_level(id: u32) -> bool {
    matches!(
        id,
        SEEK_HEAD | INFO | TRACKS | CLUSTER | CUES | CHAPTERS | ATTACHMENTS | TAGS | EBML | SEGMENT
    )
}

/// Cleans a segment; returns where it ends.
fn segment(
    input: &mut dyn ReadSeek,
    plan: &mut Plan,
    segment: &Header,
    len: u64,
) -> Result<u64, FormatError> {
    let end = segment.end.unwrap_or(len);
    let mut at = segment.data;
    let mut unknown = 0;
    while at < end {
        let element = ebml::header(input, at, end)?;
        // A live stream may start over with a new header.
        if segment.end.is_none() && matches!(element.id, EBML | SEGMENT) {
            return Ok(at);
        }
        let element_end = match (element.end, element.id) {
            (Some(end), _) => end,
            (None, CLUSTER) => cluster_end(input, &element, end)?,
            _ => {
                return Err(FormatError::Unsupported(
                    "a Matroska element of unknown size other than a cluster".into(),
                ));
            }
        };
        match element.id {
            SEEK_HEAD => seek_head(input, plan, &element, segment.data)?,
            INFO => info(input, plan, &element)?,
            TRACKS => tracks(input, plan, &element)?,
            CHAPTERS => chapters(input, plan, &element)?,
            ATTACHMENTS => attachments(input, plan, &element)?,
            TAGS => tags(input, plan, &element)?,
            CLUSTER | CUES | VOID | CRC32 => {}
            _ => {
                unknown += 1;
                plan.void(&element);
            }
        }
        at = element_end;
    }
    if unknown > 0 {
        plan.findings.push(
            Finding::new(
                FindingKind::UnknownMetadata,
                Location::new(["Segment"]),
                Disposition::Remove,
            )
            .with_value(Value::Integer(unknown)),
        );
    }
    // Index entries that pointed to removed elements would lead players to
    // a `Void`.
    for (seek, target, crc) in std::mem::take(&mut plan.seeks) {
        if plan.voided.contains(&target) && !plan.voided.contains(&seek.start) {
            plan.void(&seek);
            if let Some(crc) = crc.filter(|crc| !plan.voided.contains(&crc.start)) {
                plan.void(&crc);
            }
        }
    }
    Ok(end)
}

/// Where a cluster of unknown size ends: at the next element that belongs
/// to the segment, or the segment's end.
fn cluster_end(input: &mut dyn ReadSeek, cluster: &Header, limit: u64) -> Result<u64, FormatError> {
    let mut at = cluster.data;
    while at < limit {
        let child = ebml::header(input, at, limit)?;
        if is_top_level(child.id) {
            return Ok(at);
        }
        at = child
            .end
            .ok_or_else(|| FormatError::malformed("a cluster's block of unknown size"))?;
    }
    Ok(limit)
}

fn seek_head(
    input: &mut dyn ReadSeek,
    plan: &mut Plan,
    head: &Header,
    segment_data: u64,
) -> Result<(), FormatError> {
    let children = ebml::children(input, head)?;
    let crc = children.iter().find(|c| c.id == CRC32).copied();
    for seek in children.iter().filter(|c| c.id == SEEK) {
        for field in ebml::children(input, seek)? {
            if field.id == SEEK_POSITION {
                let data = ebml::data(input, &field, 8)?.unwrap_or_default();
                if let Some(target) = segment_data.checked_add(ebml::uint(&data)) {
                    plan.seeks.push((*seek, target, crc));
                }
            }
        }
    }
    Ok(())
}

fn string(input: &mut dyn ReadSeek, element: &Header) -> Result<Option<String>, FormatError> {
    Ok(ebml::data(input, element, MAX_SHOWN)?.map(|data| ebml::text(&data)))
}

fn removed(kind: FindingKind, location: Location, value: Value) -> Finding {
    Finding::new(kind, location, Disposition::Remove).with_value(value)
}

/// Reports and voids a text element unless it's empty.
fn void_text(
    input: &mut dyn ReadSeek,
    plan: &mut Plan,
    element: &Header,
    kind: FindingKind,
    location: Location,
) -> Result<(), FormatError> {
    match string(input, element)? {
        Some(text) if text.is_empty() => {}
        Some(text) => plan
            .findings
            .push(removed(kind, location, Value::Text(text))),
        None => plan.findings.push(removed(
            kind,
            location,
            Value::Bytes(element.len().unwrap_or(0)),
        )),
    }
    plan.void(element);
    Ok(())
}

/// Matroska dates count nanoseconds from 2001-01-01.
fn date(nanoseconds: i64) -> String {
    let seconds = nanoseconds.div_euclid(1_000_000_000) + 978_307_200;
    let days = seconds.div_euclid(86_400);
    let time = seconds.rem_euclid(86_400);
    // Days since 1970-01-01 to a calendar date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time / 3600,
        time % 3600 / 60,
        time % 60
    )
}

fn info(input: &mut dyn ReadSeek, plan: &mut Plan, info: &Header) -> Result<(), FormatError> {
    let location = Location::new(["Segment info"]);
    let children = ebml::children(input, info)?;
    let before = plan.patches.len();
    let mut unknown = 0;
    for child in &children {
        match child.id {
            // Title.
            0x7BA9 => void_text(
                input,
                plan,
                child,
                FindingKind::Description,
                location.child("Title"),
            )?,
            // MuxingApp and WritingApp: required, so blanked instead.
            0x4D80 | 0x5741 => {
                let name = if child.id == 0x4D80 {
                    "MuxingApp"
                } else {
                    "WritingApp"
                };
                if let Some(text) = string(input, child)?.filter(|t| !t.is_empty()) {
                    plan.findings.push(
                        Finding::new(
                            FindingKind::Software,
                            location.child(name),
                            Disposition::Normalize,
                        )
                        .with_value(Value::Text(text)),
                    );
                    plan.blank(child);
                }
            }
            // DateUTC.
            0x4461 => {
                let data = ebml::data(input, child, 8)?.unwrap_or_default();
                let value = data
                    .as_slice()
                    .try_into()
                    .map(|b| Value::Text(date(i64::from_be_bytes(b))))
                    .unwrap_or(Value::Bytes(data.len() as u64));
                plan.findings.push(removed(
                    FindingKind::Timestamp,
                    location.child("DateUTC"),
                    value,
                ));
                plan.void(child);
            }
            // SegmentUID, PrevUID, NextUID, SegmentFamily.
            0x73A4 | 0x3CB923 | 0x3EB923 | 0x4444 => {
                plan.findings.push(removed(
                    FindingKind::DocumentId,
                    location.child("SegmentUID"),
                    Value::Bytes(child.len().unwrap_or(0)),
                ));
                plan.void(child);
            }
            // SegmentFilename, PrevFilename, NextFilename.
            0x7384 | 0x3C83AB | 0x3E83BB => {
                void_text(
                    input,
                    plan,
                    child,
                    FindingKind::FileName,
                    location.child("SegmentFilename"),
                )?;
            }
            // TimestampScale, Duration, ChapterTranslate.
            0x2AD7B1 | 0x4489 | 0x6924 | VOID | CRC32 => {}
            _ => {
                unknown += 1;
                plan.void(child);
            }
        }
    }
    if unknown > 0 {
        plan.findings.push(removed(
            FindingKind::UnknownMetadata,
            location,
            Value::Integer(unknown),
        ));
    }
    plan.checksum(&children, before);
    Ok(())
}

fn tracks(input: &mut dyn ReadSeek, plan: &mut Plan, tracks: &Header) -> Result<(), FormatError> {
    let children = ebml::children(input, tracks)?;
    let before = plan.patches.len();
    for (n, entry) in children.iter().filter(|c| c.id == TRACK_ENTRY).enumerate() {
        let location = Location::new(["Tracks".to_owned(), format!("Track {}", n + 1)]);
        let fields = ebml::children(input, entry)?;
        let entry_before = plan.patches.len();
        let mut subtitles = false;
        let mut codec = String::new();
        for field in &fields {
            match field.id {
                // Name.
                0x536E => void_text(
                    input,
                    plan,
                    field,
                    FindingKind::Description,
                    location.child("Name"),
                )?,
                // CodecName.
                0x258688 => void_text(
                    input,
                    plan,
                    field,
                    FindingKind::Software,
                    location.child("CodecName"),
                )?,
                // CodecInfoURL, CodecDownloadURL (old versions).
                0x3B4040 | 0x26B240 => {
                    void_text(
                        input,
                        plan,
                        field,
                        FindingKind::TechnicalInfo,
                        location.child("CodecURL"),
                    )?;
                }
                // TrackType: 17 is subtitles.
                0x83 => {
                    subtitles = ebml::uint(&ebml::data(input, field, 8)?.unwrap_or_default()) == 17;
                }
                // CodecID.
                0x86 => codec = string(input, field)?.unwrap_or_default(),
                _ => {}
            }
        }
        if subtitles {
            plan.findings.push(
                Finding::new(FindingKind::TextTrack, location, Disposition::Warn)
                    .with_value(Value::Text(codec)),
            );
        }
        plan.checksum(&fields, entry_before);
    }
    plan.checksum(&children, before);
    Ok(())
}

/// Blanks every chapter and edition title, keeping the chapter marks.
fn chapters(
    input: &mut dyn ReadSeek,
    plan: &mut Plan,
    chapters: &Header,
) -> Result<(), FormatError> {
    if chapters.len().unwrap_or(u64::MAX) > MAX_METADATA {
        return Err(FormatError::Unsupported(
            "chapters too large to check".into(),
        ));
    }
    let mut titles = Vec::new();
    walk_chapters(input, plan, chapters, &mut titles, 0)?;
    if !titles.is_empty() {
        plan.findings.push(removed(
            FindingKind::Description,
            Location::new(["Chapters", "Titles"]),
            Value::Text(titles.join("; ")),
        ));
    }
    Ok(())
}

fn walk_chapters(
    input: &mut dyn ReadSeek,
    plan: &mut Plan,
    master: &Header,
    titles: &mut Vec<String>,
    depth: usize,
) -> Result<(), FormatError> {
    if depth > 32 {
        return Err(FormatError::malformed("chapters nested too deep"));
    }
    let children = ebml::children(input, master)?;
    let before = plan.patches.len();
    for child in &children {
        match child.id {
            // EditionEntry, ChapterAtom, ChapterDisplay, EditionDisplay.
            0x45B9 | 0xB6 | 0x80 | 0x4520 => walk_chapters(input, plan, child, titles, depth + 1)?,
            // ChapString, EditionString.
            0x85 | 0x4521 => match string(input, child)? {
                Some(text) if text.is_empty() => {}
                Some(text) => {
                    titles.push(text);
                    plan.blank(child);
                }
                None => {
                    titles.push(format!("({} bytes)", child.len().unwrap_or(0)));
                    plan.blank(child);
                }
            },
            _ => {}
        }
    }
    plan.checksum(&children, before);
    Ok(())
}

/// Fonts that styled subtitles need, by media type or file name.
fn is_font(media_type: &str, name: &str) -> bool {
    let media_type = media_type.to_ascii_lowercase();
    let name = name.to_ascii_lowercase();
    media_type.starts_with("font/")
        || matches!(
            media_type.as_str(),
            "application/x-truetype-font"
                | "application/vnd.ms-opentype"
                | "application/font-sfnt"
                | "application/x-font-ttf"
                | "application/x-font-otf"
                | "application/x-font"
                | "application/font-woff"
        )
        || [".ttf", ".otf", ".ttc", ".woff", ".woff2"]
            .iter()
            .any(|ext| name.ends_with(ext))
}

fn attachments(
    input: &mut dyn ReadSeek,
    plan: &mut Plan,
    attachments: &Header,
) -> Result<(), FormatError> {
    let children = ebml::children(input, attachments)?;
    let files: Vec<&Header> = children.iter().filter(|c| c.id == ATTACHED_FILE).collect();
    let mut kept = 0;
    let mut actions = Vec::new();
    for (n, file) in files.iter().enumerate() {
        let location = Location::new(["Attachments".to_owned(), format!("Attachment {}", n + 1)]);
        let fields = ebml::children(input, file)?;
        let mut name = String::new();
        let mut media_type = String::new();
        let mut size = 0;
        let mut description = None;
        for field in &fields {
            match field.id {
                0x466E => name = string(input, field)?.unwrap_or_default(),
                0x4660 => media_type = string(input, field)?.unwrap_or_default(),
                0x465C => size = field.len().unwrap_or(0),
                0x467E => description = Some(*field),
                _ => {}
            }
        }
        if is_font(&media_type, &name) {
            kept += 1;
            let mut finding = Finding::new(
                FindingKind::EmbeddedFile,
                location.clone(),
                Disposition::Keep(KeepReason::RequiredToRender),
            )
            .with_value(Value::Text(name));
            finding.risk = Risk::Low;
            plan.findings.push(finding);
            if let Some(description) = description {
                actions.push((description, Some(location.child("Description"))));
            }
        } else {
            let kind = if media_type.starts_with("image/") {
                FindingKind::CoverArt
            } else {
                FindingKind::EmbeddedFile
            };
            plan.findings
                .push(removed(kind, location, Value::Bytes(size)));
            actions.push((**file, None));
        }
    }
    if kept == 0 && !files.is_empty() {
        plan.void(attachments);
        return Ok(());
    }
    let before = plan.patches.len();
    for (element, description) in actions {
        match description {
            Some(location) => void_text(input, plan, &element, FindingKind::Description, location)?,
            None => plan.void(&element),
        }
    }
    plan.checksum(&children, before);
    Ok(())
}

/// What a tag's name says it holds.
fn tag_kind(name: &str) -> FindingKind {
    use FindingKind::*;
    let name = name.to_ascii_uppercase();
    match name.as_str() {
        "TITLE" | "SUBTITLE" | "DESCRIPTION" | "SYNOPSIS" | "SUMMARY" | "KEYWORDS" | "LYRICS"
        | "SUBJECT" => Description,
        "COMMENT" | "COMMENTS" => Comment,
        "ENCODER"
        | "ENCODER_SETTINGS"
        | "_STATISTICS_WRITING_APP"
        | "HANDLER_NAME"
        | "VENDOR_ID" => Software,
        "DURATION"
        | "BPS"
        | "NUMBER_OF_FRAMES"
        | "NUMBER_OF_BYTES"
        | "_STATISTICS_TAGS"
        | "LANGUAGE"
        | "BPM"
        | "FPS"
        | "ORIGINAL_MEDIA_TYPE" => TechnicalInfo,
        name if name.contains("LOCATION") || name.contains("GPS") => GpsLocation,
        name if name.starts_with("DATE") || name.contains("_DATE") || name.contains("TIME") => {
            Timestamp
        }
        "ARTIST" | "LEAD_PERFORMER" | "ACCOMPANIMENT" | "COMPOSER" | "ARRANGER" | "LYRICIST"
        | "CONDUCTOR" | "DIRECTOR" | "PRODUCER" | "EDITED_BY" | "WRITTEN_BY" | "AUTHOR"
        | "COPYRIGHT" | "PUBLISHER" | "ACTOR" | "ENCODED_BY" | "PRODUCTION_STUDIO"
        | "EXECUTIVE_PRODUCER" | "COPRODUCER" | "DISTRIBUTED_BY" | "MASTERED_BY" | "MIXED_BY"
        | "REMIXED_BY" | "SOUND_ENGINEER" | "CINEMATOGRAPHER" | "PURCHASE_OWNER" => PersonName,
        _ => UnknownMetadata,
    }
}

/// Reports every tag, then removes them all.
fn tags(input: &mut dyn ReadSeek, plan: &mut Plan, tags: &Header) -> Result<(), FormatError> {
    if tags.len().unwrap_or(u64::MAX) <= MAX_METADATA {
        let mut found: Vec<Finding> = Vec::new();
        let mut technical = 0;
        let mut unknown = 0;
        for tag in ebml::children(input, tags)?
            .iter()
            .filter(|c| c.id == 0x7373)
        {
            for simple in ebml::children(input, tag)?
                .iter()
                .filter(|c| c.id == 0x67C8)
            {
                simple_tag(input, simple, &mut found, &mut technical, &mut unknown, 0)?;
            }
        }
        plan.findings.extend(found);
        for (kind, count) in [
            (FindingKind::TechnicalInfo, technical),
            (FindingKind::UnknownMetadata, unknown),
        ] {
            if count > 0 {
                plan.findings.push(removed(
                    kind,
                    Location::new(["Tags"]),
                    Value::Integer(count),
                ));
            }
        }
    } else {
        plan.findings.push(removed(
            FindingKind::UnknownMetadata,
            Location::new(["Tags"]),
            Value::Bytes(tags.len().unwrap_or(0)),
        ));
    }
    plan.void(tags);
    Ok(())
}

fn simple_tag(
    input: &mut dyn ReadSeek,
    simple: &Header,
    found: &mut Vec<Finding>,
    technical: &mut i64,
    unknown: &mut i64,
    depth: usize,
) -> Result<(), FormatError> {
    if depth > 32 {
        return Err(FormatError::malformed("tags nested too deep"));
    }
    let mut name = String::new();
    let mut value = None;
    let mut nested = Vec::new();
    for field in ebml::children(input, simple)? {
        match field.id {
            0x45A3 => name = string(input, &field)?.unwrap_or_default(),
            0x4487 => value = string(input, &field)?.map(Value::Text),
            0x4485 => value = Some(Value::Bytes(field.len().unwrap_or(0))),
            0x67C8 => nested.push(field),
            _ => {}
        }
    }
    match tag_kind(&name) {
        FindingKind::TechnicalInfo => *technical += 1,
        FindingKind::UnknownMetadata => *unknown += 1,
        kind => {
            let location = Location::new(["Tags".to_owned(), name.to_ascii_uppercase()]);
            let value = value.unwrap_or(Value::Bytes(0));
            match found.iter_mut().find(|f| f.location == location) {
                Some(Finding {
                    value: Some(Value::Text(existing)),
                    ..
                }) => {
                    if let Value::Text(more) = value
                        && !existing.split("; ").any(|v| v == more)
                    {
                        existing.push_str("; ");
                        existing.push_str(&more);
                    }
                }
                Some(_) => {}
                None => found.push(removed(kind, location, value)),
            }
        }
    }
    for child in nested {
        simple_tag(input, &child, found, technical, unknown, depth + 1)?;
    }
    Ok(())
}
