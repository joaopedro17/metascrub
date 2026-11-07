//! PDF documents. The file is always written anew from the objects it
//! still uses, so earlier versions appended by incremental saves, unused
//! objects and data after the end can't survive cleaning.

mod annotations;
mod attachments;

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};

use lopdf::xref::XrefType;
use lopdf::{Dictionary, Document, LoadOptions, Object, ObjectId, StringFormat};
use md5::{Digest, Md5};

use super::{
    Depth, FormatError, FormatHandler, ReadSeek, clean_embedded, drop_all, ensure_clean,
    read_image, xmp,
};
use crate::limits::MAX_PDF_STREAM_BYTES;
use crate::model::{Disposition, Finding, FindingKind, Location, Value};
use crate::options::Options;

/// Inline dictionaries and arrays nest; real files stay far below this.
const MAX_NESTING: usize = 64;

/// Keys removed wherever they appear, with what they hold.
const STRIPPED: &[(&[u8], &str)] = &[
    (b"Metadata", "XMP"),
    (b"PieceInfo", "PieceInfo"),
    (b"Thumb", "Thumb"),
    (b"LastModified", "LastModified"),
    (b"SpiderInfo", "SpiderInfo"),
];

#[derive(Debug)]
pub(super) struct Pdf;

impl FormatHandler for Pdf {
    fn inspect(
        &self,
        input: &mut dyn ReadSeek,
        options: &Options,
    ) -> Result<Vec<Finding>, FormatError> {
        let data = read_image(input)?;
        Ok(analyze_safely(&data, options)?.findings)
    }

    fn clean(
        &self,
        input: &mut dyn ReadSeek,
        output: &mut dyn Write,
        options: &Options,
    ) -> Result<Vec<Finding>, FormatError> {
        let data = read_image(input)?;
        let analysis = analyze_safely(&data, options)?;
        output.write_all(&settled(analysis.output, options)?)?;
        Ok(analysis.findings)
    }

    fn checks_its_output(&self) -> bool {
        true
    }
}

/// The PDF library can write something from a damaged file that it then
/// reads differently, so the output is cleaned again until nothing
/// changes: what's written always reads back as itself, and cleaning it
/// again is a no-op. That last reading doubles as the check that nothing
/// was left, which would otherwise read a large PDF once more.
fn settled(mut output: Vec<u8>, options: &Options) -> Result<Vec<u8>, FormatError> {
    for _ in 0..3 {
        let again = analyze_safely(&output, options)?;
        if again.output == output {
            ensure_clean(&again.findings)?;
            return Ok(output);
        }
        output = again.output;
    }
    Err(FormatError::malformed(
        "the PDF couldn't be rewritten consistently",
    ))
}

#[derive(Debug)]
struct Analysis {
    findings: Vec<Finding>,
    output: Vec<u8>,
}

/// The PDF library may panic on a damaged file; that file fails, the run goes on.
fn analyze_safely(data: &[u8], options: &Options) -> Result<Analysis, FormatError> {
    catch_unwind(AssertUnwindSafe(|| analyze(data, options)))
        .unwrap_or_else(|_| Err(FormatError::malformed("the PDF library couldn't read it")))
}

fn analyze(data: &[u8], options: &Options) -> Result<Analysis, FormatError> {
    let _depth = Depth::enter()?;
    let mut doc = load(data)?;
    refuse_signed(&doc)?;

    let mut findings = file_findings(data);
    // Object and cross-reference streams were unpacked while reading, and
    // the writer leaves them out along with the linearization dictionary.
    // Anything else claiming to be one would silently vanish on save.
    // Object 0 can't exist: it heads the table's list of free entries.
    doc.objects.retain(|(n, _), o| {
        *n != 0 && !matches!(o.type_name(), Ok(b"ObjStm" | b"XRef" | b"Linearized"))
    });
    if doc.catalog().is_err() {
        return Err(FormatError::malformed("PDF without a catalog"));
    }
    // Empty, or the linearization dictionary: not worth reporting as unused.
    let rebuilt: HashSet<ObjectId> = doc
        .objects
        .iter()
        .filter(|(_, o)| o.is_null() || dict_of(o).is_some_and(|d| d.has(b"Linearized")))
        .map(|(id, _)| *id)
        .collect();
    let unused = doc
        .prune_objects()
        .into_iter()
        .filter(|id| !rebuilt.contains(id))
        .count();
    if unused > 0 {
        findings.push(
            Finding::new(
                FindingKind::UnknownMetadata,
                Location::new(["Unused objects"]),
                Disposition::Remove,
            )
            .with_value(Value::Integer(unused as i64)),
        );
    }

    for object in doc.objects.values_mut() {
        finite(object, 0);
    }
    let names = Names::new(&doc);
    info(&mut doc, &mut findings);
    strip_keys(&mut doc, &names, &mut findings);
    annotations::clean(&mut doc, &names, options, &mut findings);
    // What the removed annotations alone used goes before attachments are counted.
    doc.prune_objects();
    attachments::clean(&mut doc, options, &mut findings)?;
    scripts_and_forms(&doc, &mut findings);
    images(&mut doc, options, &mut findings);

    let old_id = string_array(doc.trailer.get(b"ID").ok());
    let root = doc
        .trailer
        .get(b"Root")
        .cloned()
        .map_err(|_| FormatError::malformed("PDF without a catalog"))?;
    doc.trailer = Dictionary::new();
    doc.trailer.set("Root", root);
    doc.reference_table.cross_reference_type = XrefType::CrossReferenceTable;
    doc.prune_objects();
    // The cross-reference table's size follows it.
    doc.max_id = doc.objects.keys().map(|(n, _)| *n).max().unwrap_or(0);
    let (output, id) = save_with_id(&mut doc)?;
    if let Some(old) = old_id
        && old != [id.clone(), id]
    {
        findings.push(
            Finding::new(
                FindingKind::DocumentId,
                Location::new(["Trailer", "ID"]),
                Disposition::Remove,
            )
            .with_value(Value::Text(old.first().map(|b| hex(b)).unwrap_or_default())),
        );
    }
    Ok(Analysis { findings, output })
}

fn load(data: &[u8]) -> Result<Document, FormatError> {
    let options = LoadOptions {
        max_decompressed_size: Some(MAX_PDF_STREAM_BYTES),
        ..LoadOptions::default()
    };
    let doc = Document::load_mem_with_options(data, options).map_err(|e| match e {
        lopdf::Error::Decryption(_) | lopdf::Error::InvalidPassword => FormatError::Encrypted,
        other => FormatError::malformed(format!("PDF: {other}")),
    })?;
    if doc.was_encrypted() || doc.trailer.has(b"Encrypt") {
        return Err(FormatError::Encrypted);
    }
    Ok(doc)
}

/// Rewriting a signed PDF breaks its signature, which would make the
/// document look tampered with.
fn refuse_signed(doc: &Document) -> Result<(), FormatError> {
    let mut signed = doc.catalog().is_ok_and(|c| c.has(b"Perms"));
    for object in doc.objects.values() {
        visit(object, 0, &mut |d| signed |= d.has(b"ByteRange"));
    }
    if signed {
        return Err(FormatError::Unsupported(
            "digitally signed PDFs: cleaning would break the signature".into(),
        ));
    }
    Ok(())
}

/// What only the file's bytes show: incremental saves and data after the end.
fn file_findings(data: &[u8]) -> Vec<Finding> {
    let mut findings = Vec::new();
    let sections = sections(data);
    // A linearized file has a second section for its first page.
    let linearized = data
        .get(..1024)
        .unwrap_or(data)
        .windows(11)
        .any(|w| w == b"/Linearized");
    let updates = sections.saturating_sub(1 + usize::from(linearized));
    if updates > 0 {
        findings.push(
            Finding::new(
                FindingKind::EarlierVersions,
                Location::new(["Incremental updates"]),
                Disposition::Remove,
            )
            .with_value(Value::Integer(updates as i64)),
        );
    }
    if let Some(end) = data.windows(5).rposition(|w| w == b"%%EOF") {
        let after = data.get(end + 5..).unwrap_or_default();
        let trailing = after
            .iter()
            .filter(|b| !matches!(b, b'\r' | b'\n' | b' ' | b'\t' | 0))
            .count();
        if trailing > 0 {
            findings.push(
                Finding::new(
                    FindingKind::TrailingData,
                    Location::new(["After the end of the file"]),
                    Disposition::Remove,
                )
                .with_value(Value::Bytes(after.len() as u64)),
            );
        }
    }
    findings
}

/// How many sections the file has: each incremental save appends one,
/// whose trailer points back to the one before with `/Prev`. Followed from
/// the end of the file, since text anywhere else could look like a trailer.
fn sections(data: &[u8]) -> usize {
    let tail = data.len().saturating_sub(1024);
    let Some(mut offset) = data
        .get(tail..)
        .and_then(|t| rfind(t, b"startxref"))
        .and_then(|i| number_after(data.get(tail + i + 9..)?))
    else {
        return 1;
    };
    let mut seen = HashSet::new();
    while seen.insert(offset) && seen.len() < 1000 {
        let Some(section) = data.get(offset..) else {
            break;
        };
        // A table's trailer ends before `startxref`; a cross-reference
        // stream's dictionary before `stream`.
        let end: &[u8] = if section.starts_with(b"xref") {
            b"startxref"
        } else {
            b"stream"
        };
        let header = section
            .get(..find(section, end).unwrap_or(section.len()))
            .unwrap_or_default();
        match find(header, b"/Prev").and_then(|i| number_after(header.get(i + 5..)?)) {
            Some(previous) => offset = previous,
            None => break,
        }
    }
    seen.len()
}

fn find(data: &[u8], needle: &[u8]) -> Option<usize> {
    data.windows(needle.len()).position(|w| w == needle)
}

fn rfind(data: &[u8], needle: &[u8]) -> Option<usize> {
    data.windows(needle.len()).rposition(|w| w == needle)
}

fn number_after(data: &[u8]) -> Option<usize> {
    let digits: String = data
        .iter()
        .skip_while(|b| b.is_ascii_whitespace())
        .take_while(|b| b.is_ascii_digit())
        .map(|&b| char::from(b))
        .collect();
    digits.parse().ok()
}

/// The document information dictionary: author, title, dates, software.
fn info(doc: &mut Document, findings: &mut Vec<Finding>) {
    let Some(value) = doc.trailer.remove(b"Info") else {
        return;
    };
    let Some(info) = take(doc, value) else {
        return;
    };
    let Some(info) = dict_of(&info) else {
        return;
    };
    for (key, value) in info.iter() {
        let Some(text) = text(value).filter(|t| !t.trim().is_empty()) else {
            continue;
        };
        let key = String::from_utf8_lossy(key).into_owned();
        let (kind, text) = match key.as_str() {
            "Author" => (FindingKind::PersonName, text),
            "Creator" | "Producer" => (FindingKind::Software, text),
            "CreationDate" | "ModDate" => (FindingKind::Timestamp, date(&text)),
            "Title" | "Subject" | "Keywords" => (FindingKind::Description, text),
            "Trapped" => (FindingKind::TechnicalInfo, text),
            _ => (FindingKind::CustomProperty, format!("{key}: {text}")),
        };
        findings.push(
            Finding::new(
                kind,
                Location::new(["Info", key.as_str()]),
                Disposition::Remove,
            )
            .with_value(Value::Text(text)),
        );
    }
}

/// XMP metadata, application data, thumbnails and modification dates,
/// wherever they're attached: the document, pages, images, fonts.
fn strip_keys(doc: &mut Document, names: &Names, findings: &mut Vec<Finding>) {
    let mut removed = Vec::new();
    let ids: Vec<ObjectId> = doc.objects.keys().copied().collect();
    for id in ids {
        if let Some(object) = doc.objects.get_mut(&id) {
            visit_mut(object, 0, &mut |d| {
                for (key, label) in STRIPPED {
                    if let Some(value) = d.remove(key) {
                        removed.push((id, *label, value));
                    }
                }
            });
        }
    }
    for (owner, label, value) in removed {
        let location = Location::new([names.owner(owner).as_str(), label]);
        let Some(object) = take(doc, value) else {
            continue;
        };
        match label {
            "XMP" => {
                let Object::Stream(stream) = &object else {
                    continue;
                };
                let xml = stream
                    .decompressed_content_with_limit(MAX_PDF_STREAM_BYTES)
                    .unwrap_or_else(|_| stream.content.clone());
                let mut found = xmp::inspect(&xml, &location);
                drop_all(&mut found);
                if found.is_empty() {
                    found.push(
                        Finding::new(FindingKind::UnknownMetadata, location, Disposition::Remove)
                            .with_value(Value::Bytes(xml.len() as u64)),
                    );
                }
                findings.extend(found);
            }
            "PieceInfo" => {
                let applications = dict_of(&object)
                    .map(|d| {
                        d.iter()
                            .map(|(k, _)| String::from_utf8_lossy(k).into_owned())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                findings.push(
                    Finding::new(FindingKind::UnknownMetadata, location, Disposition::Remove)
                        .with_value(Value::Text(applications)),
                );
            }
            "Thumb" => {
                let size = match &object {
                    Object::Stream(s) => s.content.len() as u64,
                    _ => 0,
                };
                findings.push(
                    Finding::new(FindingKind::Thumbnail, location, Disposition::Remove)
                        .with_value(Value::Bytes(size)),
                );
            }
            "LastModified" => {
                let finding = Finding::new(FindingKind::Timestamp, location, Disposition::Remove);
                findings.push(match text(&object) {
                    Some(t) => finding.with_value(Value::Text(date(&t))),
                    None => finding,
                });
            }
            _ => findings.push(Finding::new(
                FindingKind::DownloadUrl,
                location,
                Disposition::Remove,
            )),
        }
    }
}

/// Reported only: JavaScript can't be told apart from what the document
/// needs to work, and form values are what someone typed in.
fn scripts_and_forms(doc: &Document, findings: &mut Vec<Finding>) {
    let mut scripts = 0;
    for object in doc.objects.values() {
        visit(object, 0, &mut |d| {
            if d.has(b"JS") || name(d, b"S") == Some(b"JavaScript") {
                scripts += 1;
            }
        });
    }
    if scripts > 0 {
        findings.push(
            Finding::new(
                FindingKind::Macros,
                Location::new(["JavaScript"]),
                Disposition::Warn,
            )
            .with_value(Value::Integer(scripts)),
        );
    }

    let Some(form) = doc
        .catalog()
        .ok()
        .and_then(|c| c.get(b"AcroForm").ok())
        .and_then(|f| dict_of(resolve(doc, f)))
    else {
        return;
    };
    let mut values = Vec::new();
    let mut seen = HashSet::new();
    if let Ok(fields) = form.get(b"Fields") {
        form_values(doc, resolve(doc, fields), "", 0, &mut seen, &mut values);
    }
    if !values.is_empty() {
        findings.push(
            Finding::new(
                FindingKind::FormData,
                Location::new(["Form fields"]),
                Disposition::Warn,
            )
            .with_value(Value::Text(values.join("; "))),
        );
    }
    if form.has(b"XFA") {
        findings.push(Finding::new(
            FindingKind::FormData,
            Location::new(["Form fields", "XFA"]),
            Disposition::Warn,
        ));
    }
}

fn form_values(
    doc: &Document,
    fields: &Object,
    parent: &str,
    depth: usize,
    seen: &mut HashSet<ObjectId>,
    values: &mut Vec<String>,
) {
    let Ok(fields) = fields.as_array() else {
        return;
    };
    if depth > MAX_NESTING {
        return;
    }
    for field in fields {
        if let Object::Reference(id) = field
            && !seen.insert(*id)
        {
            continue;
        }
        let Some(field) = dict_of(resolve(doc, field)) else {
            continue;
        };
        let partial = field.get(b"T").ok().and_then(text).unwrap_or_default();
        let full = match (parent.is_empty(), partial.is_empty()) {
            (true, _) => partial,
            (false, true) => parent.to_owned(),
            (false, false) => format!("{parent}.{partial}"),
        };
        if let Some(value) = field
            .get(b"V")
            .ok()
            .map(|v| resolve(doc, v))
            .and_then(text)
            .filter(|v| !v.trim().is_empty() && v != "Off")
        {
            values.push(format!("{full}: {value}"));
        }
        if let Ok(kids) = field.get(b"Kids") {
            form_values(doc, resolve(doc, kids), &full, depth + 1, seen, values);
        }
    }
}

/// JPEG images keep their EXIF and XMP inside the PDF. PDF viewers ignore
/// the rotation tag, so it goes too.
fn images(doc: &mut Document, options: &Options, findings: &mut Vec<Finding>) {
    let options = Options {
        strip_orientation: true,
        ..*options
    };
    for (id, object) in doc.objects.iter_mut() {
        let Object::Stream(stream) = object else {
            continue;
        };
        let jpeg = name(&stream.dict, b"Subtype") == Some(b"Image")
            && match stream.dict.get(b"Filter") {
                Ok(Object::Name(f)) => f == b"DCTDecode",
                Ok(Object::Array(f)) => {
                    matches!(f.as_slice(), [Object::Name(n)] if n == b"DCTDecode")
                }
                _ => false,
            };
        if !jpeg {
            continue;
        }
        let location = format!("Image {}", id.0);
        match clean_embedded(&stream.content, &options) {
            Ok(Some(cleaned)) => {
                findings.extend(
                    cleaned
                        .findings
                        .into_iter()
                        .map(|f| f.within(location.as_str())),
                );
                if cleaned.data != stream.content {
                    stream.set_content(cleaned.data);
                }
            }
            Ok(None) => {}
            Err(_) => findings.push(Finding::new(
                FindingKind::UnknownMetadata,
                Location::new([location]),
                Disposition::Warn,
            )),
        }
    }
}

/// The ID is a hash of the cleaned file, so it reveals nothing and the
/// same input always gives the same output.
fn save_with_id(doc: &mut Document) -> Result<(Vec<u8>, Vec<u8>), FormatError> {
    let without_id = save(doc)?;
    let id = Md5::digest(&without_id).to_vec();
    let string = Object::String(id.clone(), StringFormat::Hexadecimal);
    doc.trailer
        .set("ID", Object::Array(vec![string.clone(), string]));
    Ok((save(doc)?, id))
}

fn save(doc: &mut Document) -> Result<Vec<u8>, FormatError> {
    let mut out = Vec::new();
    doc.save_to(&mut out)
        .map_err(|e| FormatError::malformed(format!("PDF: {e}")))?;
    Ok(out)
}

/// Pages and the catalog by name, for locations people can follow.
struct Names {
    pages: HashMap<ObjectId, u32>,
    catalog: Option<ObjectId>,
}

impl Names {
    fn new(doc: &Document) -> Self {
        Self {
            pages: doc.get_pages().into_iter().map(|(n, id)| (id, n)).collect(),
            catalog: doc.trailer.get(b"Root").and_then(Object::as_reference).ok(),
        }
    }

    fn owner(&self, id: ObjectId) -> String {
        match self.pages.get(&id) {
            Some(n) => format!("Page {n}"),
            None if self.catalog == Some(id) => "Document".into(),
            None => format!("Object {}", id.0),
        }
    }
}

/// Removes the object `value` refers to and returns it; a direct value
/// is returned as is. Whatever still refers to it now finds `null`. The
/// catalog is only copied: a damaged file may point at it from anywhere.
fn take(doc: &mut Document, value: Object) -> Option<Object> {
    match value {
        Object::Reference(id) if Some(id) == catalog_id(doc) => doc.objects.get(&id).cloned(),
        Object::Reference(id) => doc
            .objects
            .get_mut(&id)
            .map(|o| std::mem::replace(o, Object::Null))
            .filter(|o| !o.is_null()),
        other => Some(other),
    }
}

fn catalog_id(doc: &Document) -> Option<ObjectId> {
    doc.trailer.get(b"Root").and_then(Object::as_reference).ok()
}

fn resolve<'a>(doc: &'a Document, mut object: &'a Object) -> &'a Object {
    for _ in 0..8 {
        match object {
            Object::Reference(id) => match doc.objects.get(id) {
                Some(target) => object = target,
                None => return &Object::Null,
            },
            _ => return object,
        }
    }
    &Object::Null
}

fn dict_of(object: &Object) -> Option<&Dictionary> {
    match object {
        Object::Dictionary(d) => Some(d),
        Object::Stream(s) => Some(&s.dict),
        _ => None,
    }
}

fn name<'a>(dict: &'a Dictionary, key: &[u8]) -> Option<&'a [u8]> {
    dict.get(key).and_then(Object::as_name).ok()
}

fn text(object: &Object) -> Option<String> {
    match object {
        Object::String(..) => lopdf::decode_text_string(object).ok(),
        Object::Name(n) => Some(String::from_utf8_lossy(n).into_owned()),
        Object::Integer(n) => Some(n.to_string()),
        Object::Real(n) => Some(n.to_string()),
        Object::Boolean(b) => Some(b.to_string()),
        _ => None,
    }
}

/// `D:20260314092652+01'00'` → `2026-03-14 09:26:52 +01:00`; anything
/// else is shown as written.
fn date(text: &str) -> String {
    let digits = text.strip_prefix("D:").unwrap_or(text);
    let part = |range: std::ops::Range<usize>| {
        digits
            .get(range)
            .filter(|p| p.bytes().all(|b| b.is_ascii_digit()))
    };
    let (Some(y), Some(mo), Some(d)) = (part(0..4), part(4..6), part(6..8)) else {
        return text.to_owned();
    };
    let time = match (part(8..10), part(10..12), part(12..14)) {
        (Some(h), Some(mi), Some(s)) => format!(" {h}:{mi}:{s}"),
        _ => String::new(),
    };
    let zone = digits
        .get(14..)
        .map(|z| z.replace('\'', ":").trim_end_matches(':').to_owned())
        .filter(|z| !z.is_empty())
        .map(|z| {
            if z == "Z" {
                " UTC".to_owned()
            } else {
                format!(" {z}")
            }
        })
        .unwrap_or_default();
    format!("{y}-{mo}-{d}{time}{zone}")
}

fn string_array(object: Option<&Object>) -> Option<Vec<Vec<u8>>> {
    object?
        .as_array()
        .ok()?
        .iter()
        .map(|o| o.as_str().ok().map(<[u8]>::to_vec))
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Infinite or undefined numbers aren't valid PDF; written out, they
/// couldn't be read back.
fn finite(object: &mut Object, depth: usize) {
    if depth > MAX_NESTING {
        return;
    }
    match object {
        Object::Real(n) if !n.is_finite() => *object = Object::Integer(0),
        Object::Array(items) => items.iter_mut().for_each(|o| finite(o, depth + 1)),
        Object::Dictionary(d) => d.iter_mut().for_each(|(_, o)| finite(o, depth + 1)),
        Object::Stream(s) => s.dict.iter_mut().for_each(|(_, o)| finite(o, depth + 1)),
        _ => {}
    }
}

/// Every dictionary in `object`, including inline ones and stream dictionaries.
fn visit(object: &Object, depth: usize, f: &mut impl FnMut(&Dictionary)) {
    if depth > MAX_NESTING {
        return;
    }
    match object {
        Object::Dictionary(d) => {
            f(d);
            for (_, v) in d.iter() {
                visit(v, depth + 1, f);
            }
        }
        Object::Stream(s) => {
            f(&s.dict);
            for (_, v) in s.dict.iter() {
                visit(v, depth + 1, f);
            }
        }
        Object::Array(a) => {
            for v in a {
                visit(v, depth + 1, f);
            }
        }
        _ => {}
    }
}

/// Like [`visit`], letting `f` change each dictionary before its contents are visited.
fn visit_mut(object: &mut Object, depth: usize, f: &mut impl FnMut(&mut Dictionary)) {
    if depth > MAX_NESTING {
        return;
    }
    let dict = match object {
        Object::Dictionary(d) => d,
        Object::Stream(s) => &mut s.dict,
        Object::Array(a) => {
            for v in a {
                visit_mut(v, depth + 1, f);
            }
            return;
        }
        _ => return,
    };
    f(dict);
    for (_, v) in dict.iter_mut() {
        visit_mut(v, depth + 1, f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::inspect_bytes;
    use crate::model::KeepReason;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/generated/pdf")
                .join(name),
        )
        .unwrap()
    }

    fn clean_with(data: &[u8], options: &Options) -> (Vec<u8>, Vec<Finding>) {
        let mut output = Vec::new();
        let findings = Pdf
            .clean(&mut std::io::Cursor::new(data), &mut output, options)
            .unwrap();
        (output, findings)
    }

    fn contains(data: &[u8], text: &str) -> bool {
        data.windows(text.len()).any(|w| w == text.as_bytes())
    }

    fn kinds(findings: &[Finding]) -> HashSet<FindingKind> {
        findings.iter().map(|f| f.kind).collect()
    }

    /// Cleaned output reads back with nothing left to remove, and cleaning
    /// it again changes nothing.
    fn assert_clean(cleaned: &[u8], options: &Options) {
        let (_, findings) = inspect_bytes(cleaned, options).unwrap();
        let left: Vec<_> = findings
            .iter()
            .filter(|f| !f.disposition.is_acceptable_after_cleaning())
            .collect();
        assert!(left.is_empty(), "{left:?}");
        assert_eq!(clean_with(cleaned, options).0, cleaned);
    }

    #[test]
    fn document_metadata_and_earlier_versions_are_removed() {
        let (cleaned, findings) = clean_with(&fixture("report.pdf"), &Options::default());
        for kind in [
            FindingKind::PersonName,
            FindingKind::Software,
            FindingKind::Timestamp,
            FindingKind::Description,
            FindingKind::CustomProperty,
            FindingKind::DocumentId,
            FindingKind::Thumbnail,
            FindingKind::EarlierVersions,
            FindingKind::GpsLocation,
        ] {
            assert!(kinds(&findings).contains(&kind), "{kind:?} not reported");
        }
        for text in [
            "Jane",
            "Draft budget",
            "Final budget",
            "Acme",
            "Illustrator",
            "Microsoft",
            "xmpmeta",
        ] {
            assert!(!contains(&cleaned, text), "{text} left in the output");
        }
        assert_eq!(cleaned.windows(5).filter(|w| *w == b"%%EOF").count(), 1);
        assert!(contains(&cleaned, "/ID"));
        assert_clean(&cleaned, &Options::default());
    }

    #[test]
    fn comments_are_removed_but_links_stay() {
        let (cleaned, findings) = clean_with(&fixture("comments.pdf"), &Options::default());
        let doc = Document::load_mem(&cleaned).unwrap();
        let page = *doc.get_pages().values().next().unwrap();
        let annots = doc
            .get_dictionary(page)
            .unwrap()
            .get(b"Annots")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(annots.len(), 1);
        let link = doc
            .get_dictionary(annots[0].as_reference().unwrap())
            .unwrap();
        assert_eq!(name(link, b"Subtype"), Some(b"Link".as_slice()));
        assert!(!contains(&cleaned, "Jane") && !contains(&cleaned, "Check the totals"));
        let comments: Vec<_> = findings
            .iter()
            .filter(|f| f.kind == FindingKind::Comment)
            .collect();
        assert_eq!(comments.len(), 2);
        assert_clean(&cleaned, &Options::default());
    }

    #[test]
    fn kept_comments_show_a_neutral_author_and_no_dates() {
        let keep = Options {
            keep_comments: true,
            ..Options::default()
        };
        let (cleaned, findings) = clean_with(&fixture("comments.pdf"), &keep);
        assert!(contains(&cleaned, "Check the totals") && contains(&cleaned, "(Author)"));
        assert!(
            !contains(&cleaned, "Jane")
                && !contains(&cleaned, "John")
                && !contains(&cleaned, "D:2026")
        );
        assert!(findings.iter().any(|f| f.kind == FindingKind::Comment
            && f.disposition == Disposition::Keep(KeepReason::UserRequested)));
        assert_clean(&cleaned, &keep);
    }

    #[test]
    fn attachments_are_removed_or_kept_cleaned() {
        let data = fixture("attachments.pdf");
        let (cleaned, findings) = clean_with(&data, &Options::default());
        assert!(!contains(&cleaned, "Salaries") && !contains(&cleaned, "Contract between"));
        assert!(!contains(&cleaned, "EmbeddedFile"));
        let files: Vec<_> = findings
            .iter()
            .filter(|f| f.kind == FindingKind::EmbeddedFile)
            .filter_map(|f| f.value.clone())
            .collect();
        assert_eq!(files.len(), 3, "{files:?}");
        assert_clean(&cleaned, &Options::default());

        let keep = Options {
            keep_attachments: true,
            ..Options::default()
        };
        let (cleaned, findings) = clean_with(&data, &keep);
        assert!(contains(&cleaned, "Salaries 2026") && contains(&cleaned, "Contract between"));
        assert!(!contains(&cleaned, "Users") && !contains(&cleaned, "D:2026"));
        // The photo inside was cleaned like any photo.
        assert!(findings.iter().any(|f| f.kind == FindingKind::GpsLocation
            && f.location.parts().first().map(String::as_str) == Some("photo.jpg")));
        assert!(findings.iter().any(|f| f.kind == FindingKind::FilePath));
        assert_clean(&cleaned, &keep);
    }

    #[test]
    fn scripts_and_form_values_are_reported() {
        let (cleaned, findings) = clean_with(&fixture("form.pdf"), &Options::default());
        let warned: Vec<_> = findings
            .iter()
            .filter(|f| f.disposition == Disposition::Warn)
            .map(|f| (f.kind, f.value.clone()))
            .collect();
        assert_eq!(
            warned,
            [
                (FindingKind::Macros, Some(Value::Integer(1))),
                (
                    FindingKind::FormData,
                    Some(Value::Text("Name: Jane Doe".into()))
                ),
            ]
        );
        assert_clean(&cleaned, &Options::default());
    }

    /// The `clean` fuzzer found that an attached PDF, kept and stored
    /// uncompressed, made the outer file look like it had earlier versions.
    #[test]
    fn an_attached_pdf_is_not_an_earlier_version() {
        let mut doc = Document::load_mem(&fixture("clean.pdf")).unwrap();
        let mut file = Dictionary::new();
        file.set("Type", Object::Name(b"EmbeddedFile".to_vec()));
        let file = doc.add_object(lopdf::Stream::new(file, fixture("report.pdf")));
        let mut streams = Dictionary::new();
        streams.set("F", file);
        let mut spec = Dictionary::new();
        spec.set("Type", Object::Name(b"Filespec".to_vec()));
        spec.set("F", Object::string_literal("report.pdf"));
        spec.set("EF", streams);
        let spec = doc.add_object(spec);
        let mut tree = Dictionary::new();
        tree.set(
            "Names",
            Object::Array(vec![Object::string_literal("report.pdf"), spec.into()]),
        );
        let mut names = Dictionary::new();
        names.set("EmbeddedFiles", tree);
        doc.catalog_mut().unwrap().set("Names", names);
        let data = save(&mut doc).unwrap();

        let keep = Options {
            keep_attachments: true,
            ..Options::default()
        };
        let (cleaned, findings) = clean_with(&data, &keep);
        // The attachment's own earlier version is reported within it.
        let outer: Vec<_> = findings
            .iter()
            .filter(|f| f.kind == FindingKind::EarlierVersions)
            .map(|f| f.location.to_string())
            .collect();
        assert_eq!(outer, ["report.pdf > Incremental updates"]);
        assert!(
            contains(&cleaned, "startxref\n")
                && cleaned.windows(9).filter(|w| *w == b"startxref").count() > 1
        );
        assert_clean(&cleaned, &keep);
    }

    /// Found by the fuzzer too: a name or string that reads `startxref`.
    #[test]
    fn text_that_looks_like_a_trailer_is_not_an_earlier_version() {
        let mut doc = Document::load_mem(&fixture("clean.pdf")).unwrap();
        let catalog = doc.catalog_mut().unwrap();
        catalog.set(
            "Astartxref",
            Object::string_literal("startxref\n9\n%%EOF /Prev 9"),
        );
        let data = save(&mut doc).unwrap();
        let (cleaned, findings) = clean_with(&data, &Options::default());
        assert!(!kinds(&findings).contains(&FindingKind::EarlierVersions));
        assert_clean(&cleaned, &Options::default());
        assert_eq!(sections(&fixture("report.pdf")), 2);
    }

    /// Found by the fuzzer: the writer leaves out anything typed as file
    /// structure, so a page claiming to be one must not count as kept.
    #[test]
    fn objects_the_writer_drops_are_dropped_first() {
        let mut doc = Document::load_mem(&fixture("comments.pdf")).unwrap();
        let page = *doc.get_pages().values().next().unwrap();
        if let Ok(Object::Dictionary(page)) = doc.get_object_mut(page) {
            page.set("Type", Object::Name(b"XRef".to_vec()));
        }
        let data = save(&mut doc).unwrap();
        let (cleaned, _) = clean_with(&data, &Options::default());
        assert_clean(&cleaned, &Options::default());
    }

    /// Found by the fuzzer: an object numbered 0 is written but can't be
    /// read back, so a catalog numbered 0 means there is none.
    #[test]
    fn a_catalog_that_cant_exist_is_an_error() {
        let text = String::from_utf8_lossy(&fixture("clean.pdf"))
            .replace("/Root 1 0 R", "/Root 0 0 R")
            .replace("\n1 0 obj", "\n0 0 obj");
        assert!(matches!(
            analyze_safely(text.as_bytes(), &Options::default()),
            Err(FormatError::Malformed(_))
        ));
    }

    /// Found by the fuzzer: metadata pointing at the catalog must not take
    /// the document's root with it.
    #[test]
    fn the_catalog_survives_metadata_pointing_at_it() {
        let mut doc = Document::load_mem(&fixture("comments.pdf")).unwrap();
        let root = doc.trailer.get(b"Root").unwrap().clone();
        doc.trailer.set("Info", root.clone());
        doc.catalog_mut().unwrap().set("Metadata", root);
        let data = save(&mut doc).unwrap();
        let (cleaned, _) = clean_with(&data, &Options::default());
        assert_clean(&cleaned, &Options::default());
    }

    /// Found by the fuzzer: the library wrote an infinite number as `inf`,
    /// which it can't read back, losing the page tree.
    #[test]
    fn infinite_numbers_are_written_as_zero() {
        let huge = format!("/UserUnit {}.0 /MediaBox", "9".repeat(60));
        let data = String::from_utf8_lossy(&fixture("clean.pdf")).replacen("/MediaBox", &huge, 1);
        let (cleaned, _) = clean_with(data.as_bytes(), &Options::default());
        assert!(!contains(&cleaned, "inf") && contains(&cleaned, "/UserUnit 0"));
        assert_eq!(Document::load_mem(&cleaned).unwrap().get_pages().len(), 1);
        assert_clean(&cleaned, &Options::default());
    }

    #[test]
    fn encrypted_pdfs_are_skipped() {
        assert!(matches!(
            analyze_safely(&fixture("encrypted.pdf"), &Options::default()),
            Err(FormatError::Encrypted)
        ));
    }

    #[test]
    fn signed_pdfs_are_refused() {
        let mut doc = Document::load_mem(&fixture("clean.pdf")).unwrap();
        let mut signature = Dictionary::new();
        signature.set("Type", Object::Name(b"Sig".to_vec()));
        signature.set(
            "ByteRange",
            Object::Array(vec![0.into(), 10.into(), 20.into(), 30.into()]),
        );
        doc.add_object(signature);
        let data = save(&mut doc).unwrap();
        assert!(matches!(
            analyze_safely(&data, &Options::default()),
            Err(FormatError::Unsupported(_))
        ));
    }

    #[test]
    fn clean_documents_only_gain_an_id() {
        let (cleaned, findings) = clean_with(&fixture("clean.pdf"), &Options::default());
        assert!(findings.is_empty(), "{findings:?}");
        assert_clean(&cleaned, &Options::default());
    }

    #[test]
    fn damaged_files_are_errors() {
        for data in [
            &b"%PDF-1.7\n"[..],
            b"%PDF-1.7\n1 0 obj << /Type /Catalog >>",
        ] {
            assert!(matches!(
                analyze_safely(data, &Options::default()),
                Err(FormatError::Malformed(_))
            ));
        }
    }

    #[test]
    fn formats_dates_for_people() {
        assert_eq!(
            date("D:20260314092652+01'00'"),
            "2026-03-14 09:26:52 +01:00"
        );
        assert_eq!(date("D:20260314093000Z"), "2026-03-14 09:30:00 UTC");
        assert_eq!(date("D:2026"), "D:2026");
        assert_eq!(date("yesterday"), "yesterday");
    }
}
