//! Comments added while reviewing: sticky notes, highlights, drawings,
//! stamps. Each records who made it and when. Like Office comments they
//! are removed unless the user keeps them, and kept ones are anonymized.
//! Links and form fields are annotations too, but are never touched.

use std::collections::HashSet;

use lopdf::{Dictionary, Document, Object, ObjectId};

use super::{Names, attachments, dict_of, name, text};
use crate::model::{Disposition, Finding, FindingKind, KeepReason, Location, Value};
use crate::options::Options;

const COMMENTS: &[&[u8]] = &[
    b"Text",
    b"FreeText",
    b"Line",
    b"Square",
    b"Circle",
    b"Polygon",
    b"PolyLine",
    b"Highlight",
    b"Underline",
    b"Squiggly",
    b"StrikeOut",
    b"Caret",
    b"Ink",
    b"Stamp",
    b"Sound",
    b"Redact",
];

pub(super) const NEUTRAL_AUTHOR: &str = "Author";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Comment,
    Attachment,
    Popup,
    Other,
}

fn kind(annotation: &Dictionary) -> Kind {
    match name(annotation, b"Subtype") {
        Some(b"FileAttachment") => Kind::Attachment,
        Some(b"Popup") => Kind::Popup,
        Some(subtype) if COMMENTS.contains(&subtype) => Kind::Comment,
        _ => Kind::Other,
    }
}

pub(super) fn clean(
    doc: &mut Document,
    names: &Names,
    options: &Options,
    findings: &mut Vec<Finding>,
) {
    let mut pages: Vec<_> = names.pages.iter().map(|(id, n)| (*n, *id)).collect();
    pages.sort();
    for (number, page) in pages {
        let page_name = format!("Page {number}");
        let Some(annotations) = annotation_ids(doc, page) else {
            continue;
        };
        let goes = |kind| match kind {
            Kind::Comment => !options.keep_comments,
            Kind::Attachment => !options.keep_attachments,
            Kind::Popup | Kind::Other => false,
        };
        let mut removed = HashSet::new();
        for id in &annotations {
            if let Some(annotation) = doc.get_dictionary(*id).ok()
                && goes(kind(annotation))
            {
                removed.insert(*id);
            }
        }
        // A popup shows its parent's text and goes with it.
        for id in &annotations {
            if let Some(annotation) = doc.get_dictionary(*id).ok()
                && kind(annotation) == Kind::Popup
                && annotation
                    .get(b"Parent")
                    .and_then(Object::as_reference)
                    .is_ok_and(|parent| removed.contains(&parent))
            {
                removed.insert(*id);
            }
        }

        // A damaged file may list the catalog as an annotation.
        if let Some(catalog) = super::catalog_id(doc) {
            removed.remove(&catalog);
        }
        for id in &annotations {
            let location = Location::new([page_name.as_str(), "Annotation"]);
            if removed.contains(id) {
                let Some(annotation) = doc
                    .objects
                    .get_mut(id)
                    .map(|o| std::mem::replace(o, Object::Null))
                else {
                    continue;
                };
                if let Some(annotation) = dict_of(&annotation) {
                    findings.extend(removed_findings(doc, annotation, &location));
                }
            } else if let Some(Object::Dictionary(annotation)) = doc.objects.get_mut(id) {
                let kind = kind(annotation);
                if kind == Kind::Other {
                    continue;
                }
                if kind == Kind::Comment {
                    let finding = Finding::new(
                        FindingKind::Comment,
                        location.clone(),
                        Disposition::Keep(KeepReason::UserRequested),
                    );
                    findings.push(match annotation.get(b"Contents").ok().and_then(text) {
                        Some(t) if !t.trim().is_empty() => finding.with_value(Value::Text(t)),
                        _ => finding,
                    });
                }
                anonymize(annotation, &location, findings);
            }
        }
        if !removed.is_empty() {
            set_annotation_ids(doc, page, &removed);
        }
    }
}

/// The page's annotations that are separate objects. Inline annotation
/// dictionaries can't be told apart by reference, so they're left alone;
/// writers don't produce them.
fn annotation_ids(doc: &Document, page: ObjectId) -> Option<Vec<ObjectId>> {
    let annots = doc.get_dictionary(page).ok()?.get(b"Annots").ok()?;
    let annots = super::resolve(doc, annots).as_array().ok()?;
    Some(
        annots
            .iter()
            .filter_map(|a| a.as_reference().ok())
            .collect(),
    )
}

fn set_annotation_ids(doc: &mut Document, page: ObjectId, removed: &HashSet<ObjectId>) {
    let target = match doc
        .get_dictionary(page)
        .ok()
        .and_then(|p| p.get(b"Annots").ok())
    {
        Some(Object::Reference(id)) => Some(*id),
        _ => None,
    };
    let keep = |items: &mut Vec<Object>| {
        items.retain(|item| !item.as_reference().is_ok_and(|id| removed.contains(&id)));
    };
    let annots = match target {
        Some(id) => doc.objects.get_mut(&id),
        None => doc.objects.get_mut(&page).and_then(|p| match p {
            Object::Dictionary(d) => d.get_mut(b"Annots").ok(),
            _ => None,
        }),
    };
    if let Some(Object::Array(items)) = annots {
        keep(items);
    }
}

fn removed_findings(doc: &Document, annotation: &Dictionary, location: &Location) -> Vec<Finding> {
    let mut findings = Vec::new();
    match kind(annotation) {
        Kind::Attachment => {
            let finding = Finding::new(
                FindingKind::EmbeddedFile,
                location.clone(),
                Disposition::Remove,
            );
            findings.push(
                match attachments::file_name(doc, annotation.get(b"FS").ok()) {
                    Some(name) => finding.with_value(Value::Text(name)),
                    None => finding,
                },
            );
        }
        Kind::Comment => {
            let finding = Finding::new(FindingKind::Comment, location.clone(), Disposition::Remove);
            let label = name(annotation, b"Subtype")
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .unwrap_or_default();
            let contents = annotation
                .get(b"Contents")
                .ok()
                .and_then(text)
                .filter(|t| !t.trim().is_empty())
                .unwrap_or(label);
            findings.push(finding.with_value(Value::Text(contents)));
        }
        Kind::Popup | Kind::Other => {}
    }
    if let Some(author) = annotation
        .get(b"T")
        .ok()
        .and_then(text)
        .filter(|t| !t.trim().is_empty())
    {
        findings.push(
            Finding::new(
                FindingKind::PersonName,
                location.child("T"),
                Disposition::Remove,
            )
            .with_value(Value::Text(author)),
        );
    }
    for key in [b"M".as_slice(), b"CreationDate"] {
        if let Some(date) = annotation.get(key).ok().and_then(text) {
            findings.push(
                Finding::new(
                    FindingKind::Timestamp,
                    location.child(String::from_utf8_lossy(key)),
                    Disposition::Remove,
                )
                .with_value(Value::Text(super::date(&date))),
            );
        }
    }
    findings
}

/// Kept annotations show "Author" and lose their dates.
fn anonymize(annotation: &mut Dictionary, location: &Location, findings: &mut Vec<Finding>) {
    if let Some(author) = annotation.get(b"T").ok().and_then(text)
        && author != NEUTRAL_AUTHOR
    {
        annotation.set("T", Object::string_literal(NEUTRAL_AUTHOR));
        findings.push(
            Finding::new(
                FindingKind::PersonName,
                location.child("T"),
                Disposition::Normalize,
            )
            .with_value(Value::Text(author)),
        );
    }
    for key in [b"M".as_slice(), b"CreationDate"] {
        if let Some(date) = annotation.remove(key) {
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
