//! `content.xml` and `styles.xml`: the document itself. Comments are
//! removed (or kept with a neutral author with `--keep comments`), and so
//! are the editing-session IDs LibreOffice adds to styles. Hidden text,
//! sheets and slides and speaker notes are the user's content: reported,
//! never changed. Tracked changes would need accepting or rejecting, which
//! MetaScrub doesn't do for OpenDocument yet, so such files are refused.

use std::collections::HashSet;

use crate::format::FormatError;
use crate::format::xml::{self, Element, Node};
use crate::model::{Disposition, Finding, FindingKind, KeepReason, Location, Value};
use crate::options::Options;

const OFFICE: &[&str] = &["urn:oasis:names:tc:opendocument:xmlns:office:1.0"];
/// LibreOffice's own namespace for slide comments and editing-session IDs.
const OFFICEOOO: &[&str] = &["http://openoffice.org/2009/office"];
const TEXT: &[&str] = &["urn:oasis:names:tc:opendocument:xmlns:text:1.0"];
const TABLE: &[&str] = &["urn:oasis:names:tc:opendocument:xmlns:table:1.0"];
const STYLE: &[&str] = &["urn:oasis:names:tc:opendocument:xmlns:style:1.0"];
const DRAW: &[&str] = &["urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"];
const PRESENTATION: &[&str] = &["urn:oasis:names:tc:opendocument:xmlns:presentation:1.0"];
const META: &[&str] = &["urn:oasis:names:tc:opendocument:xmlns:meta:1.0"];
const DC: &[&str] = &["http://purl.org/dc/elements/1.1/"];

const NEUTRAL_AUTHOR: &str = "Author";
const NEUTRAL_INITIALS: &str = "A";

/// The cleaned part, or `None` when nothing in it had to change.
pub(super) fn clean(
    data: &[u8],
    location: &Location,
    options: &Options,
    findings: &mut Vec<Finding>,
) -> Result<Option<Vec<u8>>, FormatError> {
    let mut nodes = xml::parse(data)?;
    let mut tracked = false;
    xml::visit_elements(&nodes, &mut |e: &Element| {
        let is_list = e.is(TEXT, "tracked-changes") || e.is(TABLE, "tracked-changes");
        tracked |= is_list && e.elements().next().is_some();
    });
    if tracked {
        return Err(FormatError::Unsupported(
            "tracked changes in OpenDocument files (accept or reject them first)".into(),
        ));
    }
    findings.extend(hidden(&nodes, location));

    let mut changed = false;
    let annotation = location.child("annotation");
    if options.keep_comments {
        xml::for_each_element(&mut nodes, &mut |e| {
            if is_annotation(e) {
                changed |= anonymize(e, &annotation, findings);
            }
        });
    } else {
        let removed = xml::remove_elements(&mut nodes, &mut |e: &Element| {
            is_annotation(e) || e.is(OFFICE, "annotation-end") || e.is(OFFICEOOO, "annotation-end")
        });
        changed |= !removed.is_empty();
        for e in removed.iter().filter(|e| is_annotation(e)) {
            findings.extend(annotation_findings(e, &annotation));
        }
    }

    let mut session_ids = 0;
    xml::for_each_element(&mut nodes, &mut |e| {
        session_ids += e.remove_attributes(|ns, local, _| {
            OFFICEOOO.contains(&ns) && matches!(local, "rsid" | "paragraph-rsid")
        });
    });
    if session_ids > 0 {
        changed = true;
        findings.push(
            Finding::new(
                FindingKind::DocumentId,
                location.child("rsid"),
                Disposition::Remove,
            )
            .with_value(Value::Integer(session_ids as i64)),
        );
    }
    Ok(if changed {
        Some(xml::write(&nodes)?)
    } else {
        None
    })
}

fn is_annotation(e: &Element) -> bool {
    e.is(OFFICE, "annotation") || e.is(OFFICEOOO, "annotation")
}

/// The comment's own text, without its author and date.
fn comment_text(e: &Element) -> String {
    e.elements()
        .filter(|c| !c.is(DC, "creator") && !c.is(DC, "date") && !c.is(META, "creator-initials"))
        .filter(|c| !c.is(META, "date-string"))
        .map(Element::text)
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_owned()
}

fn annotation_findings(e: &Element, location: &Location) -> Vec<Finding> {
    let mut findings = vec![
        Finding::new(FindingKind::Comment, location.clone(), Disposition::Remove)
            .with_value(Value::Text(comment_text(e))),
    ];
    for child in e.elements() {
        let (kind, name) = if child.is(DC, "creator") {
            (FindingKind::PersonName, "dc:creator")
        } else if child.is(META, "creator-initials") {
            (FindingKind::PersonName, "meta:creator-initials")
        } else if child.is(DC, "date") || child.is(META, "date-string") {
            (FindingKind::Timestamp, "dc:date")
        } else {
            continue;
        };
        findings.push(
            Finding::new(kind, location.child(name), Disposition::Remove)
                .with_value(Value::Text(child.text().trim().to_owned())),
        );
    }
    findings
}

/// Keeps the comment with a neutral author and no date. Returns whether
/// anything changed.
fn anonymize(e: &mut Element, location: &Location, findings: &mut Vec<Finding>) -> bool {
    findings.push(
        Finding::new(
            FindingKind::Comment,
            location.clone(),
            Disposition::Keep(KeepReason::UserRequested),
        )
        .with_value(Value::Text(comment_text(e))),
    );
    let mut changed = false;
    for node in &mut e.children {
        let Node::Element(child) = node else { continue };
        let (neutral, name) = if child.is(DC, "creator") {
            (NEUTRAL_AUTHOR, "dc:creator")
        } else if child.is(META, "creator-initials") {
            (NEUTRAL_INITIALS, "meta:creator-initials")
        } else {
            continue;
        };
        let old = child.text().trim().to_owned();
        if old != neutral {
            child.set_text(neutral);
            changed = true;
            findings.push(
                Finding::new(
                    FindingKind::PersonName,
                    location.child(name),
                    Disposition::Normalize,
                )
                .with_value(Value::Text(old)),
            );
        }
    }
    let dates = xml::remove_elements(&mut e.children, &mut |c: &Element| {
        c.is(DC, "date") || c.is(META, "date-string")
    });
    for date in &dates {
        findings.push(
            Finding::new(
                FindingKind::Timestamp,
                location.child("dc:date"),
                Disposition::Remove,
            )
            .with_value(Value::Text(date.text().trim().to_owned())),
        );
    }
    changed || !dates.is_empty()
}

/// Hidden text, sheets and slides, and speaker notes.
fn hidden(nodes: &[Node], location: &Location) -> Vec<Finding> {
    let mut hidden_tables = HashSet::new();
    let mut hidden_pages = HashSet::new();
    xml::visit_elements(nodes, &mut |e: &Element| {
        if !e.is(STYLE, "style") {
            return;
        }
        let name = e.attribute(STYLE, "name").unwrap_or_default().to_owned();
        for properties in e.elements() {
            if properties.is(STYLE, "table-properties")
                && properties.attribute(TABLE, "display") == Some("false")
            {
                hidden_tables.insert(name.clone());
            }
            if properties.is(STYLE, "drawing-page-properties")
                && properties.attribute(PRESENTATION, "visibility") == Some("hidden")
            {
                hidden_pages.insert(name.clone());
            }
        }
    });
    let mut text = Vec::new();
    let mut sheets = Vec::new();
    let mut slides = Vec::new();
    let mut notes = Vec::new();
    xml::visit_elements(nodes, &mut |e: &Element| {
        if e.is(TEXT, "hidden-text") {
            text.extend(e.attribute(TEXT, "string-value").map(str::to_owned));
        } else if e.is(TEXT, "hidden-paragraph")
            || (e.is(TEXT, "section") && e.attribute(TEXT, "display") == Some("none"))
        {
            text.push(e.text());
        } else if e.is(TABLE, "table")
            && e.attribute(TABLE, "style-name")
                .is_some_and(|s| hidden_tables.contains(s))
        {
            sheets.push(e.attribute(TABLE, "name").unwrap_or_default().to_owned());
        } else if e.is(DRAW, "page")
            && e.attribute(DRAW, "style-name")
                .is_some_and(|s| hidden_pages.contains(s))
        {
            slides.push(e.attribute(DRAW, "name").unwrap_or_default().to_owned());
        } else if e.is(PRESENTATION, "notes") {
            for frame in e.elements() {
                if frame.attribute(PRESENTATION, "class") == Some("notes") {
                    notes.push(frame.text());
                }
            }
        }
    });
    let mut findings = Vec::new();
    for (name, values) in [
        ("hidden text", text),
        ("hidden sheet", sheets),
        ("hidden slide", slides),
        ("speaker notes", notes),
    ] {
        let joined = values
            .iter()
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if !joined.is_empty() {
            findings.push(
                Finding::new(
                    FindingKind::HiddenContent,
                    location.child(name),
                    Disposition::Warn,
                )
                .with_value(Value::Text(joined)),
            );
        }
    }
    findings
}
