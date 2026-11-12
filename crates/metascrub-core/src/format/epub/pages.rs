//! The book's pages (XHTML) and its EPUB 2 table of contents (NCX). Pages
//! lose the `<meta>` tags in their head (the program that made them, an
//! author); the character set and the viewport fixed layouts need stay.
//! The table of contents repeats the book's identifier, which follows the
//! new one, and names the program and author, which go.

use crate::format::FormatError;
use crate::format::xml::{self, Element};
use crate::model::{Disposition, Finding, FindingKind, KeepReason, Location, Value};
use crate::options::Options;

use super::PLACEHOLDER;

const XHTML: &[&str] = &["http://www.w3.org/1999/xhtml"];
const NCX: &[&str] = &["http://www.daisy.org/z3986/2005/ncx/"];

/// The cleaned page, or `None` when nothing changed. A page that isn't
/// well-formed XML is kept as it is, with a warning.
pub(super) fn clean_page(
    data: &[u8],
    location: &Location,
    findings: &mut Vec<Finding>,
) -> Option<Vec<u8>> {
    let Ok(mut nodes) = xml::parse(data) else {
        findings.push(
            Finding::new(FindingKind::UnreadFile, location.clone(), Disposition::Warn)
                .with_value(Value::Bytes(data.len() as u64)),
        );
        return None;
    };
    let mut removed = Vec::new();
    xml::for_each_element(&mut nodes, &mut |e| {
        if e.is(XHTML, "head") {
            removed.extend(xml::remove_elements(&mut e.children, &mut |m: &Element| {
                m.is(XHTML, "meta")
                    && m.attribute(&[""], "name")
                        .is_some_and(|n| !n.eq_ignore_ascii_case("viewport"))
            }));
        }
    });
    if removed.is_empty() {
        return None;
    }
    for meta in &removed {
        let name = meta.attribute(&[""], "name").unwrap_or_default();
        let lower = name.to_ascii_lowercase();
        let kind = match lower.as_str() {
            "generator" => FindingKind::Software,
            "author" | "creator" | "dc.creator" => FindingKind::PersonName,
            "description" | "keywords" | "subject" => FindingKind::Description,
            _ if lower.contains("date") || lower.contains("modified") => FindingKind::Timestamp,
            _ => FindingKind::CustomProperty,
        };
        findings.push(
            Finding::new(
                kind,
                location.child(format!("meta {name}")),
                Disposition::Remove,
            )
            .with_value(Value::Text(
                meta.attribute(&[""], "content")
                    .unwrap_or_default()
                    .to_owned(),
            )),
        );
    }
    xml::write(&nodes).ok()
}

/// The table of contents with the placeholder as the book's identifier
/// (the caller puts the new one in), and without the program's name or,
/// unless asked, the author.
pub(super) fn clean_ncx(
    data: &[u8],
    location: &Location,
    options: &Options,
    findings: &mut Vec<Finding>,
) -> Result<Vec<u8>, FormatError> {
    let mut nodes = xml::parse(data)?;
    xml::for_each_element(&mut nodes, &mut |e| {
        if e.is(NCX, "meta") && e.attribute(&[""], "name") == Some("dtb:uid") {
            e.set_attribute(&[""], "content", PLACEHOLDER);
        }
    });
    let removed = xml::remove_elements(&mut nodes, &mut |e: &Element| {
        (e.is(NCX, "meta") && e.attribute(&[""], "name") == Some("dtb:generator"))
            || (e.is(NCX, "docAuthor") && !options.keep_book_info)
    });
    for e in &removed {
        let (kind, name, value) = if e.is(NCX, "docAuthor") {
            (FindingKind::PersonName, "docAuthor", e.text())
        } else {
            (
                FindingKind::Software,
                "dtb:generator",
                e.attribute(&[""], "content").unwrap_or_default().to_owned(),
            )
        };
        findings.push(
            Finding::new(kind, location.child(name), Disposition::Remove)
                .with_value(Value::Text(value.trim().to_owned())),
        );
    }
    if options.keep_book_info {
        xml::visit_elements(&nodes, &mut |e: &Element| {
            if e.is(NCX, "docAuthor") {
                findings.push(
                    Finding::new(
                        FindingKind::PersonName,
                        location.child("docAuthor"),
                        Disposition::Keep(KeepReason::UserRequested),
                    )
                    .with_value(Value::Text(e.text().trim().to_owned())),
                );
            }
        });
    }
    xml::write(&nodes)
}
