//! Office Open XML documents (DOCX, XLSX, PPTX): a ZIP package of parts,
//! mostly XML, linked by relationships. Cleaning follows the relationships
//! from the package root and keeps only the parts that make up the
//! document; everything else is removed.

mod comments;
mod package;
mod props;
mod xml;

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Write};

use super::archive::{self, Entry, zip_error};
use super::{FormatError, FormatHandler, ReadSeek, clean_bytes, read_image};
use crate::model::{Disposition, Finding, FindingKind, FormatId, Location, Value};
use crate::options::Options;
use package::{CONTENT_TYPES, ContentTypes, PACKAGE_RELS, Relationship};
use xml::Element;

const W: &[&str] = &[
    "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
    "http://purl.oclc.org/ooxml/wordprocessingml/main",
];
const W14: &[&str] = &["http://schemas.microsoft.com/office/word/2010/wordml"];
const W15: &[&str] = &["http://schemas.microsoft.com/office/word/2012/wordml"];
const R: &[&str] = &[
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships",
    "http://purl.oclc.org/ooxml/officeDocument/relationships",
];
const MC: &[&str] = &["http://schemas.openxmlformats.org/markup-compatibility/2006"];
const X15AC: &[&str] = &["http://schemas.microsoft.com/office/spreadsheetml/2010/11/ac"];
const XR: &[&str] = &["http://schemas.microsoft.com/office/spreadsheetml/2014/revision"];
const P: &[&str] = &[
    "http://schemas.openxmlformats.org/presentationml/2006/main",
    "http://purl.oclc.org/ooxml/presentationml/main",
];
const P14: &[&str] = &["http://schemas.microsoft.com/office/powerpoint/2010/main"];
const S: &[&str] = &[
    "http://schemas.openxmlformats.org/spreadsheetml/2006/main",
    "http://purl.oclc.org/ooxml/spreadsheetml/main",
];
const P188: &[&str] = &["http://schemas.microsoft.com/office/powerpoint/2018/8/main"];

/// Word elements that record tracked changes.
const TRACKED_CHANGES: &[&str] = &[
    "ins",
    "del",
    "moveFrom",
    "moveTo",
    "rPrChange",
    "pPrChange",
    "sectPrChange",
    "tblPrChange",
    "trPrChange",
    "tcPrChange",
    "tblGridChange",
    "tblPrExChange",
    "numberingChange",
    "cellIns",
    "cellDel",
    "cellMerge",
];

/// `[Content_Types].xml` and `_rels/.rels` are tiny in real documents.
const MAX_SMALL_PART_BYTES: u64 = 1024 * 1024;

#[derive(Debug)]
pub(super) struct Office;

impl FormatHandler for Office {
    fn inspect(
        &self,
        input: &mut dyn ReadSeek,
        options: &Options,
    ) -> Result<Vec<Finding>, FormatError> {
        let data = read_image(input)?;
        Ok(analyze(&data, options)?.findings)
    }

    fn clean(
        &self,
        input: &mut dyn ReadSeek,
        output: &mut dyn Write,
        options: &Options,
    ) -> Result<Vec<Finding>, FormatError> {
        let data = read_image(input)?;
        let analysis = analyze(&data, options)?;
        archive::write(&analysis.entries, output)?;
        Ok(analysis.findings)
    }
}

/// Tells Word, Excel and PowerPoint files apart from other ZIP files by the
/// content type of the main part the package points to, the same part
/// cleaning starts from. Reads only the ZIP directory and two small parts.
pub(super) fn identify(input: &mut dyn ReadSeek) -> Result<FormatId, FormatError> {
    let mut zip = zip::ZipArchive::new(input).map_err(zip_error)?;
    let (Some(types), Some(relationships)) = (
        small_entry(&mut zip, CONTENT_TYPES)?,
        small_entry(&mut zip, PACKAGE_RELS)?,
    ) else {
        return Ok(FormatId::Zip);
    };
    let types = ContentTypes::parse(&types)?;
    let main = package::parse_relationships(&relationships)?
        .into_iter()
        .filter(|r| !r.external && r.short_kind() == "officeDocument")
        .find_map(|r| package::resolve("", &r.target));
    let Some(main) = main.filter(|m| {
        zip.file_names()
            .any(|n| n.is_ok_and(|n| n.eq_ignore_ascii_case(m)))
    }) else {
        return Ok(FormatId::Zip);
    };
    let content_type = types.of(&main).unwrap_or_default().to_ascii_lowercase();
    // Binary workbooks (.xlsb) end in `.main` without `+xml`.
    if !content_type.ends_with(".main+xml") {
        return Ok(FormatId::Zip);
    }
    let any = |names: &[&str]| names.iter().any(|n| content_type.contains(n));
    Ok(if any(&["wordprocessingml", "ms-word."]) {
        FormatId::Docx
    } else if any(&["spreadsheetml", "ms-excel."]) {
        FormatId::Xlsx
    } else if any(&["presentationml", "ms-powerpoint."]) {
        FormatId::Pptx
    } else {
        FormatId::Zip
    })
}

/// An entry found by name regardless of case, if it's small enough to be
/// a package part rather than anything else.
fn small_entry<R: Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
) -> Result<Option<Vec<u8>>, FormatError> {
    let Some(index) = zip
        .file_names()
        .position(|n| n.is_ok_and(|n| n.eq_ignore_ascii_case(name)))
    else {
        return Ok(None);
    };
    let file = zip.by_index(index).map_err(zip_error)?;
    let mut data = Vec::new();
    file.take(MAX_SMALL_PART_BYTES).read_to_end(&mut data)?;
    Ok(Some(data))
}

/// What happens to the part a relationship points to, by relationship type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Keep,
    Image,
    Remove(Removed),
    /// Depends on the format and on `--keep comments`.
    Comments,
    Refuse(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Removed {
    Core,
    App,
    Custom,
    Thumbnail,
    CustomXml,
    PrinterSettings,
    People,
    SensitivityLabels,
    Comments,
    /// Only reachable from another removed part.
    Dependent,
}

fn role(kind: &str) -> Role {
    match kind {
        "officeDocument"
        | "styles"
        | "stylesWithEffects"
        | "settings"
        | "webSettings"
        | "fontTable"
        | "theme"
        | "themeOverride"
        | "numbering"
        | "footnotes"
        | "endnotes"
        | "header"
        | "footer"
        | "glossaryDocument"
        | "hyperlink"
        | "font"
        | "worksheet"
        | "chartsheet"
        | "dialogsheet"
        | "sharedStrings"
        | "calcChain"
        | "drawing"
        | "vmlDrawing"
        | "chart"
        | "chartUserShapes"
        | "chartStyle"
        | "chartColorStyle"
        | "diagramData"
        | "diagramLayout"
        | "diagramQuickStyle"
        | "diagramColors"
        | "diagramDrawing"
        | "table"
        | "tableSingleCells"
        | "pivotTable"
        | "pivotCacheDefinition"
        | "pivotCacheRecords"
        | "queryTable"
        | "connections"
        | "sheetMetadata"
        | "volatileDependencies"
        | "slicer"
        | "slicerCache"
        | "timeline"
        | "timelineCache"
        | "ctrlProp"
        | "namedSheetView"
        | "richStyles"
        | "rdRichValue"
        | "rdRichValueStructure"
        | "rdRichValueTypes"
        | "rdArray"
        | "rdSupportingPropertyBag"
        | "rdSupportingPropertyBagStructure"
        | "rdRichValueWebImage"
        | "FeaturePropertyBag"
        | "slide"
        | "slideLayout"
        | "slideMaster"
        | "notesSlide"
        | "notesMaster"
        | "handoutMaster"
        | "presProps"
        | "viewProps"
        | "tableStyles"
        | "tags"
        | "slideUpdateInfo"
        | "media"
        | "video"
        | "audio" => Role::Keep,
        "image" | "hdphoto" => Role::Image,
        "core-properties" => Role::Remove(Removed::Core),
        "extended-properties" => Role::Remove(Removed::App),
        "custom-properties" => Role::Remove(Removed::Custom),
        "thumbnail" => Role::Remove(Removed::Thumbnail),
        "customXml" | "customXmlProps" => Role::Remove(Removed::CustomXml),
        "printerSettings" => Role::Remove(Removed::PrinterSettings),
        "people" => Role::Remove(Removed::People),
        "classificationlabels" => Role::Remove(Removed::SensitivityLabels),
        "comments" | "commentsExtended" | "commentsIds" | "commentsExtensible" => Role::Comments,
        "threadedComment" | "person" | "commentAuthors" | "authors" => Role::Comments,
        "revisionHeaders" | "revisionLog" | "usernames" => Role::Refuse("tracked changes"),
        "vbaProject" | "wordVbaData" | "keyMapCustomizations" | "vbaProjectSignature" => {
            Role::Refuse("macros")
        }
        "oleObject"
        | "package"
        | "control"
        | "activeXControl"
        | "activeXControlBinary"
        | "aFChunk"
        | "subDocument" => Role::Refuse("embedded files"),
        "externalLink" | "externalLinkPath" | "externalLinkLong" => {
            Role::Refuse("links to other workbooks")
        }
        _ => Role::Refuse("unrecognized parts"),
    }
}

#[derive(Debug)]
struct Analysis {
    findings: Vec<Finding>,
    entries: Vec<Entry>,
}

/// The parts reached by following relationships from the package root.
#[derive(Debug, Default)]
struct Walk {
    /// Parts to keep (lower-case names).
    kept: HashSet<String>,
    images: HashSet<String>,
    removed: Vec<(String, Removed)>,
    /// The relationships each kept part (and the package, `""`) keeps.
    relationships: HashMap<String, Vec<Relationship>>,
    /// Relationship IDs each kept part lost, so references to them can go.
    removed_ids: HashMap<String, HashSet<String>>,
    findings: Vec<Finding>,
}

struct Package<'a> {
    entries: &'a [Entry],
    index: HashMap<String, usize>,
}

impl<'a> Package<'a> {
    fn new(entries: &'a [Entry]) -> Self {
        let index = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.name.to_lowercase(), i))
            .collect();
        Self { entries, index }
    }

    fn get(&self, name: &str) -> Option<&'a Entry> {
        self.index
            .get(&name.to_lowercase())
            .and_then(|&i| self.entries.get(i))
    }

    fn relationships(&self, source: &str) -> Result<Vec<Relationship>, FormatError> {
        match self.get(&package::relationships_part(source)) {
            Some(entry) => package::parse_relationships(&entry.data),
            None => Ok(Vec::new()),
        }
    }
}

fn analyze(data: &[u8], options: &Options) -> Result<Analysis, FormatError> {
    let archive = archive::read(data)?;
    let package = Package::new(&archive.entries);
    let mut findings = Vec::new();
    if !archive.comment.is_empty() {
        findings.push(
            Finding::new(
                FindingKind::Comment,
                Location::new(["ZIP"]),
                Disposition::Remove,
            )
            .with_value(Value::Text(
                String::from_utf8_lossy(&archive.comment).into_owned(),
            )),
        );
    }
    if let Some(date) = archive.latest_date {
        findings.push(
            Finding::new(
                FindingKind::Timestamp,
                Location::new(["ZIP", "File dates"]),
                Disposition::Remove,
            )
            .with_value(Value::Text(date.to_string())),
        );
    }
    let mut types = ContentTypes::parse(
        &package
            .get(CONTENT_TYPES)
            .ok_or_else(|| FormatError::malformed("missing [Content_Types].xml"))?
            .data,
    )?;

    let walk = walk(&package, &types, options)?;
    findings.extend(walk.findings.iter().cloned());
    for (name, removed) in &walk.removed {
        if let Some(entry) = package.get(name) {
            findings.extend(removed_findings(&entry.name, &entry.data, *removed));
        }
    }

    let mut entries = Vec::new();
    for entry in &archive.entries {
        let name = entry.name.to_lowercase();
        if name == CONTENT_TYPES.to_lowercase() || name.ends_with(".rels") {
            continue;
        }
        if !walk.kept.contains(&name) {
            if !walk.removed.iter().any(|(removed, _)| removed == &name) {
                findings.push(
                    Finding::new(
                        FindingKind::UnknownMetadata,
                        Location::new([entry.name.as_str()]),
                        Disposition::Remove,
                    )
                    .with_value(Value::Bytes(entry.data.len() as u64)),
                );
            }
            continue;
        }
        let data = if walk.images.contains(&name) {
            match clean_bytes(&entry.data, options)? {
                Some(cleaned) => {
                    findings.extend(
                        cleaned
                            .findings
                            .into_iter()
                            .map(|f| f.within(entry.name.as_str())),
                    );
                    cleaned.data
                }
                None => entry.data.clone(),
            }
        } else {
            let content_type = types
                .of(&entry.name)
                .unwrap_or_default()
                .to_ascii_lowercase();
            let removed_ids = walk.removed_ids.get(&name);
            match rewrite(entry, &content_type, removed_ids, options, &mut findings)? {
                Some(data) => data,
                None => entry.data.clone(),
            }
        };
        entries.push(Entry {
            name: entry.name.clone(),
            data,
        });
    }

    // Relationship parts of the kept parts, after the parts themselves.
    for entry in &archive.entries {
        let name = entry.name.to_lowercase();
        let source = walk
            .relationships
            .keys()
            .find(|source| package::relationships_part(source).to_lowercase() == name);
        if let Some(source) = source
            && !source.is_empty()
            && let Some(relationships) = walk.relationships.get(source)
            && !relationships.is_empty()
        {
            entries.push(Entry {
                name: entry.name.clone(),
                data: package::relationships_xml(relationships),
            });
        }
    }

    types.retain_parts(|part| walk.kept.contains(&part.to_lowercase()));
    let mut output = vec![
        Entry {
            name: CONTENT_TYPES.to_owned(),
            data: types.to_xml(),
        },
        Entry {
            name: PACKAGE_RELS.to_owned(),
            data: package::relationships_xml(
                walk.relationships
                    .get("")
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
            ),
        },
    ];
    output.extend(entries);
    Ok(Analysis {
        findings,
        entries: output,
    })
}

/// Follows the relationships from the package root, deciding for each part
/// whether it stays. Refuses documents with parts not handled yet.
fn walk(
    package: &Package<'_>,
    types: &ContentTypes,
    options: &Options,
) -> Result<Walk, FormatError> {
    let mut walk = Walk::default();
    let mut queue = VecDeque::from([String::new()]);
    while let Some(source) = queue.pop_front() {
        let mut kept_relationships = Vec::new();
        let relationships = package.relationships(&source)?;
        let drops_notes = !options.keep_comments
            && relationships.iter().any(|r| {
                !r.external
                    && r.short_kind() == "comments"
                    && package::resolve(&source, &r.target)
                        .and_then(|t| types.of(&t))
                        .is_some_and(|ct| {
                            ct.to_ascii_lowercase().contains("spreadsheetml.comments")
                        })
            });
        for relationship in relationships {
            let kind = relationship.short_kind();
            if relationship.external {
                match kind {
                    "hyperlink" => kept_relationships.push(relationship),
                    "attachedTemplate" => {
                        walk.findings.push(
                            Finding::new(
                                FindingKind::FilePath,
                                Location::new([
                                    package::relationships_part(&source).as_str(),
                                    "attachedTemplate",
                                ]),
                                Disposition::Remove,
                            )
                            .with_value(Value::Text(relationship.target.clone())),
                        );
                        removed_id(&mut walk, &source, &relationship.id);
                    }
                    _ => return Err(refuse("links to other files")),
                }
                continue;
            }
            let Some(target) = package::resolve(&source, &relationship.target) else {
                kept_relationships.push(relationship);
                continue;
            };
            let target_name = target.to_lowercase();
            let role = match role(kind) {
                Role::Comments => comment_role(types.of(&target), options),
                Role::Keep if kind == "vmlDrawing" && drops_notes => {
                    notes_layer_role(package.get(&target).map(|e| e.data.as_slice()))
                }
                other => other,
            };
            match role {
                Role::Keep | Role::Image => {
                    if role == Role::Image {
                        walk.images.insert(target_name.clone());
                    }
                    kept_relationships.push(relationship);
                    walk.removed.retain(|(name, _)| name != &target_name);
                    if package.get(&target).is_some() && walk.kept.insert(target_name) {
                        queue.push_back(target);
                    }
                }
                Role::Remove(removed) => {
                    removed_id(&mut walk, &source, &relationship.id);
                    remove(&mut walk, package, &target, removed)?;
                }
                Role::Comments => return Err(refuse("comments")),
                Role::Refuse(what) => {
                    return Err(if what == "unrecognized parts" {
                        FormatError::Unsupported(format!("Office part type {}", relationship.kind))
                    } else {
                        refuse(what)
                    });
                }
            }
        }
        walk.relationships
            .insert(source.to_lowercase(), kept_relationships);
    }
    Ok(walk)
}

/// Comment parts are recognized by content type: Word, Excel and
/// PowerPoint all call their relationship `comments`.
fn comment_role(content_type: Option<&str>, options: &Options) -> Role {
    match (
        content_type.is_some_and(comments::is_comment_part),
        options.keep_comments,
    ) {
        (false, _) => Role::Refuse("comments"),
        (true, true) => Role::Keep,
        (true, false) => Role::Remove(Removed::Comments),
    }
}

/// Excel shows notes through a drawing layer. When the notes go, a layer
/// holding only notes goes too; one with form controls as well can't be
/// split safely.
fn notes_layer_role(data: Option<&[u8]>) -> Role {
    match data.and_then(comments::vml_shapes) {
        Some(shapes) if shapes.notes == 0 => Role::Keep,
        Some(shapes) if shapes.others == 0 => Role::Remove(Removed::Dependent),
        _ => Role::Refuse("notes next to form controls"),
    }
}

fn refuse(what: &str) -> FormatError {
    FormatError::Unsupported(format!("Office documents with {what} aren't supported yet"))
}

fn removed_id(walk: &mut Walk, source: &str, id: &str) {
    walk.removed_ids
        .entry(source.to_lowercase())
        .or_default()
        .insert(id.to_owned());
}

/// Removes `part` and whatever only it points to.
fn remove(
    walk: &mut Walk,
    package: &Package<'_>,
    part: &str,
    removed: Removed,
) -> Result<(), FormatError> {
    let name = part.to_lowercase();
    if walk.kept.contains(&name) || walk.removed.iter().any(|(n, _)| n == &name) {
        return Ok(());
    }
    if package.get(part).is_none() {
        return Ok(());
    }
    walk.removed.push((name, removed));
    for relationship in package.relationships(part)? {
        if !relationship.external
            && let Some(target) = package::resolve(part, &relationship.target)
        {
            remove(walk, package, &target, Removed::Dependent)?;
        }
    }
    Ok(())
}

fn removed_findings(name: &str, data: &[u8], removed: Removed) -> Vec<Finding> {
    let location = Location::new([name]);
    let whole = |kind| {
        vec![
            Finding::new(kind, location.clone(), Disposition::Remove)
                .with_value(Value::Bytes(data.len() as u64)),
        ]
    };
    match removed {
        Removed::Core => props::core(data, &location),
        Removed::App => props::app(data, &location),
        Removed::Custom => props::custom(data, &location),
        Removed::People => props::people(data, &location),
        Removed::Comments => comments::findings(data, &location),
        Removed::Thumbnail => whole(FindingKind::Thumbnail),
        Removed::CustomXml | Removed::SensitivityLabels => whole(FindingKind::CustomProperty),
        Removed::PrinterSettings => whole(FindingKind::DeviceModel),
        Removed::Dependent => Vec::new(),
    }
}

/// Removes IDs, paths and similar details from a Word, Excel or PowerPoint
/// XML part. `None` when nothing needed to change.
fn rewrite(
    entry: &Entry,
    content_type: &str,
    removed_ids: Option<&HashSet<String>>,
    options: &Options,
    findings: &mut Vec<Finding>,
) -> Result<Option<Vec<u8>>, FormatError> {
    let word = content_type.contains("wordprocessingml");
    let excel = content_type.contains("spreadsheetml");
    let powerpoint = content_type.contains("presentationml");
    let comment_part = comments::is_comment_part(content_type);
    if !(word || excel || powerpoint || comment_part) || !content_type.ends_with("xml") {
        return Ok(None);
    }
    let mut nodes = xml::parse(&entry.data)?;
    let location = Location::new([entry.name.as_str()]);
    let mut found = Vec::new();
    let mut changed = false;

    if word {
        let mut tracked = false;
        xml::for_each_element(&mut nodes, &mut |e| {
            tracked |= TRACKED_CHANGES.iter().any(|name| e.is(W, name));
        });
        if tracked {
            return Err(refuse("tracked changes"));
        }
    }

    if comment_part && options.keep_comments {
        changed |= comments::anonymize(&mut nodes, &location, &mut found);
    }

    let points_to_removed = |e: &Element| {
        removed_ids.is_some_and(|ids| e.attribute(R, "id").is_some_and(|id| ids.contains(id)))
    };
    let removed = xml::remove_elements(&mut nodes, &mut |e: &Element| {
        // Without the comments, their markers point nowhere: in Word's
        // text, Excel's link to the notes layer, PowerPoint's link to the
        // slide's modern comments.
        (excel && e.is(S, "legacyDrawing") && points_to_removed(e))
            || (powerpoint
                && !options.keep_comments
                && e.is(P, "ext")
                && e.find(P188, "commentRel").is_some())
            || (word
                && !options.keep_comments
                && (e.is(W, "commentRangeStart")
                    || e.is(W, "commentRangeEnd")
                    || e.is(W, "commentReference")))
            || (word
                && (e.is(W, "rsids")
                    || e.is(W, "attachedTemplate")
                    || e.is(W, "docVars")
                    || e.is(W14, "docId")
                    || e.is(W15, "docId")))
            || (excel
                && ((e.is(MC, "AlternateContent") && e.find(X15AC, "absPath").is_some())
                    || e.is(XR, "revisionPtr")))
            || (powerpoint && e.is(P, "ext") && e.find(P14, "creationId").is_some())
    });
    for e in &removed {
        changed = true;
        let finding = |kind, name: &str, value| {
            Finding::new(kind, location.child(name), Disposition::Remove).with_value(value)
        };
        let count = |e: &Element| Value::Integer(e.elements().count() as i64);
        let attribute = |e: &Element, ns, name| {
            Value::Text(e.attribute(ns, name).unwrap_or_default().to_owned())
        };
        found.push(match () {
            _ if e.is(W, "rsids") => finding(FindingKind::DocumentId, "rsids", count(e)),
            _ if e.is(W, "docVars") => finding(FindingKind::CustomProperty, "docVars", count(e)),
            _ if e.is(W14, "docId") => {
                finding(FindingKind::DocumentId, "docId", attribute(e, W14, "val"))
            }
            _ if e.is(W15, "docId") => {
                finding(FindingKind::DocumentId, "docId", attribute(e, W15, "val"))
            }
            _ if e.is(MC, "AlternateContent") => {
                let path = e
                    .find(X15AC, "absPath")
                    .and_then(|p| p.attribute(&[""], "url"))
                    .unwrap_or_default();
                finding(
                    FindingKind::FilePath,
                    "absPath",
                    Value::Text(path.to_owned()),
                )
            }
            _ if e.is(XR, "revisionPtr") => finding(
                FindingKind::DocumentId,
                "revisionPtr",
                attribute(e, &[""], "documentId"),
            ),
            _ if e.is(P, "ext") => {
                let id = e
                    .find(P14, "creationId")
                    .and_then(|c| c.attribute(&[""], "val"))
                    .unwrap_or_default();
                finding(
                    FindingKind::DocumentId,
                    "creationId",
                    Value::Text(id.to_owned()),
                )
            }
            // `attachedTemplate` was reported with its relationship, comment
            // markers and links with the comments.
            _ => continue,
        });
    }

    let mut removed_attributes = 0;
    let mut revision_ids = 0;
    xml::for_each_element(&mut nodes, &mut |e| {
        if let Some(ids) = removed_ids {
            removed_attributes +=
                e.remove_attributes(|ns, _, value| R.contains(&ns) && ids.contains(value));
        }
        if word {
            revision_ids +=
                e.remove_attributes(|ns, local, _| W.contains(&ns) && local.starts_with("rsid"));
        }
    });
    changed |= removed_attributes + revision_ids > 0;
    if revision_ids > 0 {
        found.push(
            Finding::new(
                FindingKind::DocumentId,
                location.child("rsid"),
                Disposition::Remove,
            )
            .with_value(Value::Integer(revision_ids as i64)),
        );
    }

    findings.extend(found);
    Ok(if changed {
        Some(xml::write(&nodes)?)
    } else {
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::archive::tests::zip_of;
    use crate::format::inspect_bytes;
    use crate::model::KeepReason;
    use comments::W16CEX;
    use std::io::Cursor;

    const RELS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
    const DOC_RELS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    const WORD_MAIN: &str =
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml";

    fn relationships(rels: &[(&str, &str, &str)]) -> String {
        let mut out = format!(r#"<Relationships xmlns="{RELS}">"#);
        for (id, kind, target) in rels {
            let (kind, mode) = match kind.strip_suffix('!') {
                Some(kind) => (kind, r#" TargetMode="External""#),
                None => (*kind, ""),
            };
            let kind = if kind.contains('/') {
                kind.to_owned()
            } else {
                format!("{DOC_RELS}/{kind}")
            };
            out.push_str(&format!(
                r#"<Relationship Id="{id}" Type="{kind}" Target="{target}"{mode}/>"#
            ));
        }
        out + "</Relationships>"
    }

    fn content_types(main: &str) -> String {
        format!(
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="jpeg" ContentType="image/jpeg"/><Override PartName="/word/document.xml" ContentType="{WORD_MAIN}"/><Override PartName="/word/settings.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/><Override PartName="/xl/workbook.xml" ContentType="{main}"/></Types>"#
        )
    }

    fn camera_jpeg() -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/generated/jpeg/camera.jpg"),
        )
        .unwrap()
    }

    /// A Word document with one of every kind of metadata this part handles.
    fn docx(document: &str, extra_rels: &[(&str, &str, &str)]) -> Vec<u8> {
        let ct = content_types(WORD_MAIN);
        let package_rels = relationships(&[
            ("rId1", "officeDocument", "word/document.xml"),
            (
                "rId2",
                "http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties",
                "docProps/core.xml",
            ),
            ("rId3", "extended-properties", "docProps/app.xml"),
            (
                "rId4",
                "http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail",
                "docProps/thumbnail.jpeg",
            ),
        ]);
        let mut rels = vec![
            ("rId1", "settings", "settings.xml"),
            (
                "rId2",
                "http://schemas.microsoft.com/office/2011/relationships/people",
                "people.xml",
            ),
            ("rId3", "customXml", "../customXml/item1.xml"),
            ("rId4", "image", "media/image1.jpeg"),
            ("rId5", "hyperlink!", "https://example.com/"),
        ];
        rels.extend_from_slice(extra_rels);
        let document_rels = relationships(&rels);
        let settings_rels = relationships(&[(
            "rId1",
            "attachedTemplate!",
            r"file:///C:\Users\jane\Templates\Report.dotx",
        )]);
        let item_rels = relationships(&[("rId1", "customXmlProps", "itemProps1.xml")]);
        let w = W[0];
        let settings = format!(
            r#"<w:settings xmlns:w="{w}" xmlns:r="{DOC_RELS}" xmlns:w15="{}"><w:attachedTemplate r:id="rId1"/><w:rsids><w:rsidRoot w:val="00A1B2C3"/><w:rsid w:val="00A1B2C3"/></w:rsids><w15:docId w15:val="{{1234}}"/><w:zoom w:percent="100"/></w:settings>"#,
            W15[0]
        );
        let people = format!(
            r#"<w15:people xmlns:w15="{}"><w15:person w15:author="Jane Doe"/></w15:people>"#,
            W15[0]
        );
        let jpeg = camera_jpeg();
        zip_of(&[
            ("[Content_Types].xml", ct.as_bytes()),
            ("_rels/.rels", package_rels.as_bytes()),
            ("word/document.xml", document.as_bytes()),
            ("word/_rels/document.xml.rels", document_rels.as_bytes()),
            ("word/settings.xml", settings.as_bytes()),
            ("word/_rels/settings.xml.rels", settings_rels.as_bytes()),
            ("word/people.xml", people.as_bytes()),
            ("word/media/image1.jpeg", &jpeg),
            ("customXml/item1.xml", b"<client>Acme</client>"),
            ("customXml/_rels/item1.xml.rels", item_rels.as_bytes()),
            ("customXml/itemProps1.xml", b"<ds:datastoreItem/>"),
            ("docProps/core.xml", br#"<cp:coreProperties xmlns:cp="urn:cp" xmlns:dc="urn:dc"><dc:creator>Jane Doe</dc:creator></cp:coreProperties>"#),
            ("docProps/app.xml", b"<Properties><Company>Fictional Ltd</Company></Properties>"),
            ("docProps/thumbnail.jpeg", &jpeg),
            ("leftover.bin", b"private"),
        ])
    }

    fn document(body: &str) -> String {
        format!(
            r#"<w:document xmlns:w="{}" xmlns:r="{DOC_RELS}"><w:body><w:p w:rsidR="00A1B2C3" w:rsidRDefault="00A1B2C3"><w:r><w:t>Hello</w:t></w:r></w:p>{body}</w:body></w:document>"#,
            W[0]
        )
    }

    fn clean(data: &[u8]) -> Result<(Vec<u8>, Vec<Finding>), FormatError> {
        let mut out = Vec::new();
        let findings = Office.clean(&mut Cursor::new(data), &mut out, &Options::default())?;
        Ok((out, findings))
    }

    fn names(data: &[u8]) -> Vec<String> {
        archive::read(data)
            .unwrap()
            .entries
            .into_iter()
            .map(|e| e.name)
            .collect()
    }

    fn entry(data: &[u8], name: &str) -> String {
        let archive = archive::read(data).unwrap();
        let entry = archive.entries.iter().find(|e| e.name == name).unwrap();
        String::from_utf8(entry.data.clone()).unwrap()
    }

    #[test]
    fn identifies_office_documents_by_their_main_part() {
        let data = docx(&document(""), &[]);
        assert_eq!(identify(&mut Cursor::new(&data)).unwrap(), FormatId::Docx);
        let types = content_types(
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml",
        );
        let workbook = zip_of(&[
            ("[Content_Types].xml", types.as_bytes()),
            (
                "_rels/.rels",
                relationships(&[("rId1", "officeDocument", "xl/workbook.xml")]).as_bytes(),
            ),
            ("xl/workbook.xml", b"<workbook/>"),
        ]);
        assert_eq!(
            identify(&mut Cursor::new(&workbook)).unwrap(),
            FormatId::Xlsx
        );
        // A Word content type that isn't the main part's doesn't count.
        let decoy = zip_of(&[
            ("[Content_Types].xml", content_types(WORD_MAIN).as_bytes()),
            (
                "_rels/.rels",
                relationships(&[("rId1", "officeDocument", "word/missing.xml")]).as_bytes(),
            ),
        ]);
        assert_eq!(identify(&mut Cursor::new(&decoy)).unwrap(), FormatId::Zip);
        let plain = zip_of(&[("notes.txt", b"hello")]);
        assert_eq!(identify(&mut Cursor::new(&plain)).unwrap(), FormatId::Zip);
    }

    #[test]
    fn cleaning_keeps_the_document_and_removes_metadata_parts() {
        let (cleaned, findings) = clean(&docx(&document(""), &[])).unwrap();
        assert_eq!(
            names(&cleaned),
            [
                "[Content_Types].xml",
                "_rels/.rels",
                "word/document.xml",
                "word/settings.xml",
                "word/media/image1.jpeg",
                "word/_rels/document.xml.rels",
            ]
        );
        let kinds: HashSet<_> = findings.iter().map(|f| f.kind).collect();
        for kind in [
            FindingKind::Timestamp,
            FindingKind::PersonName,
            FindingKind::Organization,
            FindingKind::Thumbnail,
            FindingKind::CustomProperty,
            FindingKind::FilePath,
            FindingKind::DocumentId,
            FindingKind::GpsLocation,
            FindingKind::UnknownMetadata,
        ] {
            assert!(kinds.contains(&kind), "{kind:?} not reported: {findings:?}");
        }

        let rels = entry(&cleaned, "word/_rels/document.xml.rels");
        assert!(rels.contains("hyperlink") && rels.contains("media/image1.jpeg"));
        assert!(!rels.contains("people") && !rels.contains("customXml"));
        assert!(!entry(&cleaned, "_rels/.rels").contains("docProps"));
        assert!(!entry(&cleaned, "[Content_Types].xml").contains("core.xml"));
        assert!(!entry(&cleaned, "word/document.xml").contains("rsid"));
        let settings = entry(&cleaned, "word/settings.xml");
        assert!(!settings.contains("rsid") && !settings.contains("Template"));
        assert!(!settings.contains("docId") && settings.contains("w:zoom"));
    }

    #[test]
    fn cleaned_documents_verify_and_clean_to_the_same_bytes() {
        let (cleaned, _) = clean(&docx(&document(""), &[])).unwrap();
        let (_, findings) = inspect_bytes(&cleaned, &Options::default()).unwrap();
        let left: Vec<_> = findings
            .iter()
            .filter(|f| !f.disposition.is_acceptable_after_cleaning())
            .collect();
        assert!(left.is_empty(), "{left:?}");
        assert_eq!(clean(&cleaned).unwrap().0, cleaned);
    }

    #[test]
    fn features_not_handled_yet_are_refused() {
        let unsupported = |data: Vec<u8>| matches!(clean(&data), Err(FormatError::Unsupported(_)));
        let tracked = document(
            r#"<w:p><w:del w:author="Jane"><w:r><w:delText>secret</w:delText></w:r></w:del></w:p>"#,
        );
        assert!(unsupported(docx(&tracked, &[])));
        assert!(unsupported(docx(
            &document(""),
            &[("rId9", "comments", "comments.xml")]
        )));
        assert!(unsupported(docx(
            &document(""),
            &[("rId9", "vbaProject", "vbaProject.bin")]
        )));
        assert!(unsupported(docx(
            &document(""),
            &[("rId9", "image!", r"file:///C:\Users\jane\photo.jpg")]
        )));
        assert!(unsupported(docx(
            &document(""),
            &[("rId9", "http://example.com/new-thing", "x.xml")]
        )));
    }

    #[test]
    fn workbook_paths_and_printer_settings_are_removed() {
        let workbook = format!(
            r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:mc="{}" xmlns:x15ac="{}" xmlns:xr="{}"><mc:AlternateContent><mc:Choice Requires="x15"><x15ac:absPath url="C:\Users\jane\Documents\"/></mc:Choice></mc:AlternateContent><xr:revisionPtr documentId="13_ncr:1_{{ABC}}"/><sheets/></workbook>"#,
            MC[0], X15AC[0], XR[0]
        );
        let sheet = format!(
            r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="{DOC_RELS}"><pageSetup orientation="portrait" r:id="rId1"/></worksheet>"#
        );
        let types = r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#;
        let data = zip_of(&[
            ("[Content_Types].xml", types.as_bytes()),
            (
                "_rels/.rels",
                relationships(&[("rId1", "officeDocument", "xl/workbook.xml")]).as_bytes(),
            ),
            ("xl/workbook.xml", workbook.as_bytes()),
            (
                "xl/_rels/workbook.xml.rels",
                relationships(&[("rId1", "worksheet", "worksheets/sheet1.xml")]).as_bytes(),
            ),
            ("xl/worksheets/sheet1.xml", sheet.as_bytes()),
            (
                "xl/worksheets/_rels/sheet1.xml.rels",
                relationships(&[(
                    "rId1",
                    "printerSettings",
                    "../printerSettings/printerSettings1.bin",
                )])
                .as_bytes(),
            ),
            (
                "xl/printerSettings/printerSettings1.bin",
                b"\\\\CORP-PRN01\\HP LaserJet",
            ),
        ]);
        let (cleaned, findings) = clean(&data).unwrap();
        let workbook = entry(&cleaned, "xl/workbook.xml");
        assert!(!workbook.contains("absPath") && !workbook.contains("revisionPtr"));
        assert!(workbook.contains("<sheets/>"));
        let sheet = entry(&cleaned, "xl/worksheets/sheet1.xml");
        assert!(
            sheet.contains(r#"<pageSetup orientation="portrait"/>"#),
            "{sheet}"
        );
        assert!(
            !names(&cleaned)
                .iter()
                .any(|n| n.contains("printerSettings"))
        );
        let path = findings
            .iter()
            .find(|f| f.kind == FindingKind::FilePath)
            .unwrap();
        assert_eq!(
            path.value,
            Some(Value::Text(r"C:\Users\jane\Documents\".into()))
        );
    }

    #[test]
    fn slide_creation_ids_are_removed() {
        let slide = format!(
            r#"<p:sld xmlns:p="{}"><p:extLst><p:ext uri="{{BB962C8B}}"><p14:creationId xmlns:p14="{}" val="2731249867"/></p:ext></p:extLst></p:sld>"#,
            P[0], P14[0]
        );
        let types = r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/><Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/></Types>"#;
        let data = zip_of(&[
            ("[Content_Types].xml", types.as_bytes()),
            (
                "_rels/.rels",
                relationships(&[("rId1", "officeDocument", "ppt/presentation.xml")]).as_bytes(),
            ),
            (
                "ppt/presentation.xml",
                format!(r#"<p:presentation xmlns:p="{}"/>"#, P[0]).as_bytes(),
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                relationships(&[("rId1", "slide", "slides/slide1.xml")]).as_bytes(),
            ),
            ("ppt/slides/slide1.xml", slide.as_bytes()),
        ]);
        let (cleaned, findings) = clean(&data).unwrap();
        assert!(entry(&cleaned, "ppt/slides/slide1.xml").contains("<p:extLst></p:extLst>"));
        let id = findings
            .iter()
            .find(|f| f.kind == FindingKind::DocumentId)
            .unwrap();
        assert_eq!(id.value, Some(Value::Text("2731249867".into())));
    }

    /// A minimal Word document with one comment, its markers and its
    /// extended parts.
    fn commented_docx() -> Vec<u8> {
        let w = W[0];
        let types = format!(
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="{WORD_MAIN}"/><Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/><Override PartName="/word/commentsExtensible.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.commentsExtensible+xml"/></Types>"#
        );
        let document = format!(
            r#"<w:document xmlns:w="{w}"><w:body><w:p><w:commentRangeStart w:id="0"/><w:r><w:t>Draft</w:t></w:r><w:commentRangeEnd w:id="0"/><w:r><w:commentReference w:id="0"/></w:r></w:p></w:body></w:document>"#
        );
        let comments = format!(
            r#"<w:comments xmlns:w="{w}"><w:comment w:id="0" w:author="Jane Doe" w:date="2026-03-14T09:30:00Z" w:initials="JD"><w:p><w:r><w:t>Check with Acme.</w:t></w:r></w:p></w:comment></w:comments>"#
        );
        let extensible = format!(
            r#"<w16cex:commentsExtensible xmlns:w16cex="{}"><w16cex:commentExtensible w16cex:durableId="1A2B" w16cex:dateUtc="2026-03-14T12:30:00Z"/></w16cex:commentsExtensible>"#,
            W16CEX[0]
        );
        zip_of(&[
            ("[Content_Types].xml", types.as_bytes()),
            ("_rels/.rels", relationships(&[("rId1", "officeDocument", "word/document.xml")]).as_bytes()),
            ("word/document.xml", document.as_bytes()),
            (
                "word/_rels/document.xml.rels",
                relationships(&[
                    ("rId1", "comments", "comments.xml"),
                    ("rId2", "http://schemas.microsoft.com/office/2018/08/relationships/commentsExtensible", "commentsExtensible.xml"),
                ])
                .as_bytes(),
            ),
            ("word/comments.xml", comments.as_bytes()),
            ("word/commentsExtensible.xml", extensible.as_bytes()),
        ])
    }

    fn clean_with(data: &[u8], options: &Options) -> (Vec<u8>, Vec<Finding>) {
        let mut out = Vec::new();
        let findings = Office
            .clean(&mut Cursor::new(data), &mut out, options)
            .unwrap();
        (out, findings)
    }

    #[test]
    fn word_comments_are_removed_with_their_markers() {
        let (cleaned, findings) = clean_with(&commented_docx(), &Options::default());
        assert_eq!(
            names(&cleaned),
            ["[Content_Types].xml", "_rels/.rels", "word/document.xml"]
        );
        let document = entry(&cleaned, "word/document.xml");
        assert!(!document.contains("comment"), "{document}");
        assert!(document.contains("Draft"));
        let values: Vec<_> = findings
            .iter()
            .filter(|f| {
                f.location
                    .parts()
                    .first()
                    .is_some_and(|p| p == "word/comments.xml")
            })
            .map(|f| (f.kind, f.value.clone()))
            .collect();
        assert_eq!(
            values,
            [
                (
                    FindingKind::Comment,
                    Some(Value::Text("Check with Acme.".into()))
                ),
                (
                    FindingKind::PersonName,
                    Some(Value::Text("Jane Doe".into()))
                ),
                (FindingKind::PersonName, Some(Value::Text("JD".into()))),
                (
                    FindingKind::Timestamp,
                    Some(Value::Text("2026-03-14T09:30:00Z".into()))
                ),
            ]
        );
    }

    #[test]
    fn kept_comments_lose_their_author_initials_and_dates() {
        let keep = Options {
            keep_comments: true,
            ..Options::default()
        };
        let (cleaned, findings) = clean_with(&commented_docx(), &keep);
        let comments = entry(&cleaned, "word/comments.xml");
        assert!(comments.contains(r#"w:author="Author""#), "{comments}");
        assert!(
            !comments.contains("Jane") && !comments.contains("JD") && !comments.contains("w:date")
        );
        assert!(comments.contains("Check with Acme."));
        assert!(!entry(&cleaned, "word/commentsExtensible.xml").contains("dateUtc"));
        assert!(entry(&cleaned, "word/document.xml").contains("commentReference"));
        let comment = findings
            .iter()
            .find(|f| f.kind == FindingKind::Comment)
            .unwrap();
        assert_eq!(
            comment.disposition,
            Disposition::Keep(KeepReason::UserRequested)
        );
        assert!(
            findings
                .iter()
                .any(|f| f.disposition == Disposition::Normalize)
        );

        // Already anonymized: nothing left to change.
        let (again, findings) = clean_with(&cleaned, &keep);
        assert_eq!(again, cleaned);
        assert!(
            findings
                .iter()
                .all(|f| f.disposition.is_acceptable_after_cleaning())
        );
    }

    /// A workbook with a note, a threaded comment and its author, and the
    /// drawing layer that shows the note. `layer` holds the shape types.
    fn commented_xlsx(layer: &[&str]) -> Vec<u8> {
        let types = r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Default Extension="vml" ContentType="application/vnd.openxmlformats-officedocument.vmlDrawing"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/comments1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.comments+xml"/><Override PartName="/xl/threadedComments/threadedComment1.xml" ContentType="application/vnd.ms-excel.threadedcomments+xml"/><Override PartName="/xl/persons/person.xml" ContentType="application/vnd.ms-excel.person+xml"/></Types>"#;
        let shapes: String = layer
            .iter()
            .map(|t| format!(r#"<v:shape><x:ClientData ObjectType="{t}"/></v:shape>"#))
            .collect();
        let vml = format!(
            r#"<xml xmlns:v="urn:schemas-microsoft-com:vml" xmlns:x="urn:schemas-microsoft-com:office:excel">{shapes}</xml>"#
        );
        let sheet = format!(
            r#"<worksheet xmlns="{}" xmlns:r="{DOC_RELS}"><sheetData/><legacyDrawing r:id="rId2"/></worksheet>"#,
            S[0]
        );
        let tc = "http://schemas.microsoft.com/office/spreadsheetml/2018/threadedcomments";
        let threaded = format!(
            r#"<ThreadedComments xmlns="{tc}"><threadedComment ref="B2" dT="2026-03-14T09:30:00.00" personId="{{P1}}" id="{{C1}}"><text>Is this final?</text></threadedComment></ThreadedComments>"#
        );
        let persons = format!(
            r#"<personList xmlns="{tc}"><person displayName="Jane Doe" id="{{P1}}" userId="jane.doe@example.com" providerId="AD"/></personList>"#
        );
        let notes = format!(
            r#"<comments xmlns="{}"><authors><author>Jane Doe</author></authors><commentList><comment ref="A1" authorId="0"><text><r><t>Jane Doe:</t></r><r><t>Check.</t></r></text></comment></commentList></comments>"#,
            S[0]
        );
        let ms = "http://schemas.microsoft.com/office/2017/10/relationships";
        zip_of(&[
            ("[Content_Types].xml", types.as_bytes()),
            (
                "_rels/.rels",
                relationships(&[("rId1", "officeDocument", "xl/workbook.xml")]).as_bytes(),
            ),
            (
                "xl/workbook.xml",
                format!(r#"<workbook xmlns="{}"/>"#, S[0]).as_bytes(),
            ),
            (
                "xl/_rels/workbook.xml.rels",
                relationships(&[
                    ("rId1", "worksheet", "worksheets/sheet1.xml"),
                    ("rId2", &format!("{ms}/person"), "persons/person.xml"),
                ])
                .as_bytes(),
            ),
            ("xl/worksheets/sheet1.xml", sheet.as_bytes()),
            (
                "xl/worksheets/_rels/sheet1.xml.rels",
                relationships(&[
                    ("rId1", "comments", "../comments1.xml"),
                    ("rId2", "vmlDrawing", "../drawings/vmlDrawing1.vml"),
                    (
                        "rId3",
                        &format!("{ms}/threadedComment"),
                        "../threadedComments/threadedComment1.xml",
                    ),
                ])
                .as_bytes(),
            ),
            ("xl/comments1.xml", notes.as_bytes()),
            ("xl/drawings/vmlDrawing1.vml", vml.as_bytes()),
            (
                "xl/threadedComments/threadedComment1.xml",
                threaded.as_bytes(),
            ),
            ("xl/persons/person.xml", persons.as_bytes()),
        ])
    }

    #[test]
    fn excel_notes_and_threaded_comments_are_removed() {
        let (cleaned, findings) = clean_with(&commented_xlsx(&["Note"]), &Options::default());
        assert_eq!(
            names(&cleaned),
            [
                "[Content_Types].xml",
                "_rels/.rels",
                "xl/workbook.xml",
                "xl/worksheets/sheet1.xml",
                "xl/_rels/workbook.xml.rels",
            ]
        );
        assert!(!entry(&cleaned, "xl/worksheets/sheet1.xml").contains("legacyDrawing"));
        let comments: Vec<_> = findings
            .iter()
            .filter(|f| f.kind == FindingKind::Comment)
            .collect();
        assert_eq!(comments.len(), 2);
        assert!(
            findings
                .iter()
                .any(|f| f.value == Some(Value::Text("jane.doe@example.com".into())))
        );
    }

    #[test]
    fn excel_comments_kept_lose_names_and_accounts() {
        let keep = Options {
            keep_comments: true,
            ..Options::default()
        };
        let (cleaned, _) = clean_with(&commented_xlsx(&["Note"]), &keep);
        assert!(entry(&cleaned, "xl/worksheets/sheet1.xml").contains("legacyDrawing"));
        for part in [
            "xl/comments1.xml",
            "xl/persons/person.xml",
            "xl/threadedComments/threadedComment1.xml",
        ] {
            let xml = entry(&cleaned, part);
            assert!(
                !xml.contains("Jane") && !xml.contains("jane") && !xml.contains("2026"),
                "{xml}"
            );
        }
        assert_eq!(clean_with(&cleaned, &keep).0, cleaned);
    }

    #[test]
    fn notes_sharing_a_layer_with_form_controls_are_refused() {
        let mixed = commented_xlsx(&["Note", "Checkbox"]);
        assert!(matches!(clean(&mixed), Err(FormatError::Unsupported(_))));
        let keep = Options {
            keep_comments: true,
            ..Options::default()
        };
        let mut out = Vec::new();
        assert!(
            Office
                .clean(&mut Cursor::new(&mixed), &mut out, &keep)
                .is_ok()
        );
    }

    #[test]
    fn powerpoint_modern_comments_are_removed_with_their_link() {
        let (p, p188) = (P[0], P188[0]);
        let types = r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/><Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/><Override PartName="/ppt/comments/modernComment_100_1.xml" ContentType="application/vnd.ms-powerpoint.comments+xml"/><Override PartName="/ppt/authors.xml" ContentType="application/vnd.ms-powerpoint.authors+xml"/></Types>"#;
        let slide = format!(
            r#"<p:sld xmlns:p="{p}" xmlns:r="{DOC_RELS}"><p:cSld/><p:extLst><p:ext uri="{{6950BFC3-D8DA-4A85-94F7-54DA5524770B}}"><p188:commentRel xmlns:p188="{p188}" r:id="rId1"/></p:ext></p:extLst></p:sld>"#
        );
        let comments = format!(
            r#"<p188:cmLst xmlns:p188="{p188}" xmlns:a="urn:a"><p188:cm id="{{C1}}" authorId="{{A1}}" created="2026-03-14T09:30:00.000"><p188:txBody><a:p><a:r><a:t>Shorter title?</a:t></a:r></a:p></p188:txBody></p188:cm></p188:cmLst>"#
        );
        let authors = format!(
            r#"<p188:authorLst xmlns:p188="{p188}"><p188:author id="{{A1}}" name="Jane Doe" initials="JD" userId="jane.doe@example.com" providerId="AD"/></p188:authorLst>"#
        );
        let ms = "http://schemas.microsoft.com/office/2018/10/relationships";
        let data = zip_of(&[
            ("[Content_Types].xml", types.as_bytes()),
            (
                "_rels/.rels",
                relationships(&[("rId1", "officeDocument", "ppt/presentation.xml")]).as_bytes(),
            ),
            (
                "ppt/presentation.xml",
                format!(r#"<p:presentation xmlns:p="{p}"/>"#).as_bytes(),
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                relationships(&[
                    ("rId1", "slide", "slides/slide1.xml"),
                    ("rId2", &format!("{ms}/authors"), "authors.xml"),
                ])
                .as_bytes(),
            ),
            ("ppt/slides/slide1.xml", slide.as_bytes()),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                relationships(&[(
                    "rId1",
                    &format!("{ms}/comments"),
                    "../comments/modernComment_100_1.xml",
                )])
                .as_bytes(),
            ),
            ("ppt/comments/modernComment_100_1.xml", comments.as_bytes()),
            ("ppt/authors.xml", authors.as_bytes()),
        ]);
        let (cleaned, findings) = clean_with(&data, &Options::default());
        assert!(
            !names(&cleaned)
                .iter()
                .any(|n| n.contains("omment") || n.contains("authors"))
        );
        let slide = entry(&cleaned, "ppt/slides/slide1.xml");
        assert!(slide.contains("<p:extLst></p:extLst>"), "{slide}");
        assert!(
            findings
                .iter()
                .any(|f| f.value == Some(Value::Text("Shorter title?".into())))
        );

        let keep = Options {
            keep_comments: true,
            ..Options::default()
        };
        let (cleaned, _) = clean_with(&data, &keep);
        assert!(entry(&cleaned, "ppt/slides/slide1.xml").contains("commentRel"));
        assert!(!entry(&cleaned, "ppt/authors.xml").contains("jane"));
    }

    #[test]
    fn missing_content_types_is_an_error() {
        let data = zip_of(&[("word/document.xml", b"<w:document/>")]);
        assert!(matches!(clean(&data), Err(FormatError::Malformed(_))));
    }
}
