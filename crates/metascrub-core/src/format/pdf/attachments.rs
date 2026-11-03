//! Files attached to a PDF. Removed by default; with `--keep attachments`
//! they stay, cleaned with the same rules when MetaScrub can read them.

use std::collections::BTreeMap;

use lopdf::{Document, Object, ObjectId};

use super::{dict_of, name, resolve, text, visit_mut};
use crate::format::{FormatError, clean_embedded};
use crate::limits::MAX_PDF_STREAM_BYTES;
use crate::model::{Disposition, Finding, FindingKind, KeepReason, Location, Value};
use crate::options::Options;

/// The name a file specification gives its file.
pub(super) fn file_name(doc: &Document, spec: Option<&Object>) -> Option<String> {
    let spec = dict_of(resolve(doc, spec?))?;
    [b"UF".as_slice(), b"F"]
        .iter()
        .find_map(|key| spec.get(key).ok().and_then(text))
        .filter(|n| !n.trim().is_empty())
}

pub(super) fn clean(
    doc: &mut Document,
    options: &Options,
    findings: &mut Vec<Finding>,
) -> Result<(), FormatError> {
    // Embedded file streams, by object, with the name their specification gives.
    let mut files: BTreeMap<ObjectId, String> = BTreeMap::new();
    let mut paths = Vec::new();
    let keep = options.keep_attachments;
    let ids: Vec<ObjectId> = doc.objects.keys().copied().collect();
    for id in ids {
        if let Some(object) = doc.objects.get_mut(&id) {
            visit_mut(object, 0, &mut |d| {
                if !keep {
                    d.remove(b"AF");
                    if let Ok(Object::Dictionary(names)) = d.get_mut(b"Names") {
                        names.remove(b"EmbeddedFiles");
                    }
                }
                let Ok(Object::Dictionary(streams)) = d.get(b"EF") else {
                    return;
                };
                let streams: Vec<ObjectId> = streams
                    .iter()
                    .filter_map(|(_, s)| s.as_reference().ok())
                    .collect();
                let mut file = [b"UF".as_slice(), b"F"]
                    .iter()
                    .find_map(|key| d.get(key).ok().and_then(text))
                    .unwrap_or_default();
                if keep {
                    // Only the name: a path shows where it was on someone's computer.
                    for key in [b"UF".as_slice(), b"F"] {
                        if let Some(path) = d.get(key).ok().and_then(text)
                            && let Some(bare) = bare_name(&path)
                        {
                            d.set(key, Object::string_literal(bare.as_str()));
                            paths.push(path);
                            file = bare;
                        }
                    }
                } else {
                    d.remove(b"EF");
                }
                for stream in streams {
                    files.entry(stream).or_insert_with(|| file.clone());
                }
            });
        }
    }
    // An embedded file the catalog's name tree still lists through an
    // indirect `Names` dictionary.
    if !keep
        && let Some(Object::Reference(names)) = doc
            .catalog()
            .ok()
            .and_then(|c| c.get(b"Names").ok())
            .cloned()
        && let Some(Object::Dictionary(names)) = doc.objects.get_mut(&names)
    {
        names.remove(b"EmbeddedFiles");
    }
    // Embedded files no specification points to.
    for (id, object) in &doc.objects {
        if let Object::Stream(s) = object
            && name(&s.dict, b"Type") == Some(b"EmbeddedFile")
        {
            files.entry(*id).or_default();
        }
    }

    for path in paths {
        findings.push(
            Finding::new(
                FindingKind::FilePath,
                Location::new(["Attachments"]),
                Disposition::Remove,
            )
            .with_value(Value::Text(path)),
        );
    }
    for (id, file) in files {
        let label = if file.is_empty() {
            format!("Object {}", id.0)
        } else {
            file.clone()
        };
        let location = Location::new(["Attachments", label.as_str()]);
        if !keep {
            let Some(Object::Stream(stream)) = doc
                .objects
                .get_mut(&id)
                .filter(|o| matches!(o, Object::Stream(_)))
                .map(|o| std::mem::replace(o, Object::Null))
            else {
                continue;
            };
            let finding = Finding::new(FindingKind::EmbeddedFile, location, Disposition::Remove);
            findings.push(if file.is_empty() {
                finding.with_value(Value::Bytes(stream.content.len() as u64))
            } else {
                finding.with_value(Value::Text(file))
            });
            continue;
        }
        let Some(Object::Stream(stream)) = doc.objects.get_mut(&id) else {
            continue;
        };
        findings.push(
            Finding::new(
                FindingKind::EmbeddedFile,
                location.clone(),
                Disposition::Keep(KeepReason::UserRequested),
            )
            .with_value(Value::Text(label.clone())),
        );
        if let Ok(Object::Dictionary(params)) = stream.dict.get_mut(b"Params") {
            for key in [b"CreationDate".as_slice(), b"ModDate"] {
                if let Some(date) = params.remove(key) {
                    let finding = Finding::new(
                        FindingKind::Timestamp,
                        location.child(String::from_utf8_lossy(key)),
                        Disposition::Remove,
                    );
                    findings.push(match text(&date) {
                        Some(d) => finding.with_value(Value::Text(super::date(&d))),
                        None => finding,
                    });
                }
            }
        }
        let Ok(content) = stream.decompressed_content_with_limit(MAX_PDF_STREAM_BYTES) else {
            findings.push(Finding::new(
                FindingKind::UnknownMetadata,
                location,
                Disposition::Warn,
            ));
            continue;
        };
        match clean_embedded(&content, options)? {
            Some(cleaned) => {
                findings.extend(
                    cleaned
                        .findings
                        .into_iter()
                        .map(|f| f.within(label.as_str())),
                );
                let size = cleaned.data.len() as i64;
                stream.set_plain_content(cleaned.data);
                if let Ok(Object::Dictionary(params)) = stream.dict.get_mut(b"Params") {
                    // The checksum is of the old content.
                    params.remove(b"CheckSum");
                    params.set("Size", size);
                }
            }
            None => findings.push(
                Finding::new(FindingKind::EmbeddedFile, location, Disposition::Warn)
                    .with_value(Value::Bytes(content.len() as u64)),
            ),
        }
    }
    Ok(())
}

/// The last part of a path, when it is one.
fn bare_name(path: &str) -> Option<String> {
    let bare = path.rsplit(['/', '\\']).next()?.trim();
    (bare != path && !bare.is_empty()).then(|| bare.to_owned())
}
