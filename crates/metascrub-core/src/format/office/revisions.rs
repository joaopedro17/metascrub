//! Tracked changes in Word documents. Each insertion, deletion, move or
//! formatting change keeps the old version and who made it, when.
//! Accepting or rejecting them all leaves one version with no history.

use std::collections::{BTreeSet, HashSet};

use super::W;
use crate::format::FormatError;
use crate::format::xml::{self, Element, Node};

/// Elements that record a change.
const CHANGES: &[&str] = &[
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

/// Where a moved or changed range begins and ends.
const RANGE_MARKERS: &[&str] = &[
    "moveFromRangeStart",
    "moveFromRangeEnd",
    "moveToRangeStart",
    "moveToRangeEnd",
    "customXmlInsRangeStart",
    "customXmlInsRangeEnd",
    "customXmlDelRangeStart",
    "customXmlDelRangeEnd",
    "customXmlMoveFromRangeStart",
    "customXmlMoveFromRangeEnd",
    "customXmlMoveToRangeStart",
    "customXmlMoveToRangeEnd",
];

/// Inside properties, these mark a paragraph mark, table row or numbering
/// as inserted or deleted.
const MARKS: &[&str] = &["ins", "del", "moveFrom", "moveTo"];

/// Formatting changes keep the old properties inside the change element.
/// Rejecting puts them back; the parent's other children (listed by where
/// the schema puts them) aren't part of the old copy and stay.
struct PropertyChange {
    properties: &'static str,
    change: &'static str,
    kept_before: &'static [&'static str],
    kept_after: &'static [&'static str],
}

const PROPERTY_CHANGES: &[PropertyChange] = &[
    PropertyChange {
        properties: "rPr",
        change: "rPrChange",
        kept_before: MARKS,
        kept_after: &[],
    },
    PropertyChange {
        properties: "pPr",
        change: "pPrChange",
        kept_before: &[],
        kept_after: &["rPr", "sectPr"],
    },
    PropertyChange {
        properties: "sectPr",
        change: "sectPrChange",
        kept_before: &["headerReference", "footerReference"],
        kept_after: &[],
    },
    PropertyChange {
        properties: "tblPr",
        change: "tblPrChange",
        kept_before: &[],
        kept_after: &[],
    },
    PropertyChange {
        properties: "trPr",
        change: "trPrChange",
        kept_before: &[],
        kept_after: &["ins", "del"],
    },
    PropertyChange {
        properties: "tcPr",
        change: "tcPrChange",
        kept_before: &[],
        kept_after: &[],
    },
    PropertyChange {
        properties: "tblGrid",
        change: "tblGridChange",
        kept_before: &[],
        kept_after: &[],
    },
    PropertyChange {
        properties: "tblPrEx",
        change: "tblPrExChange",
        kept_before: &[],
        kept_after: &[],
    },
];

#[derive(Debug, Default)]
pub(super) struct Found {
    pub(super) changes: usize,
    pub(super) authors: BTreeSet<String>,
    markers: usize,
}

impl Found {
    pub(super) fn any(&self) -> bool {
        self.changes + self.markers > 0
    }
}

pub(super) fn find(nodes: &[Node]) -> Found {
    let mut found = Found::default();
    xml::visit_elements(nodes, &mut |e| {
        if is_any(e, CHANGES) {
            found.changes += 1;
        } else if is_any(e, RANGE_MARKERS) {
            found.markers += 1;
        } else {
            return;
        }
        if let Some(author) = e.attribute(W, "author") {
            found.authors.insert(author.to_owned());
        }
    });
    found
}

/// Accepts or rejects every change in a Word part. Refuses what it can't
/// apply safely, and checks that nothing is left and no field was cut in two.
pub(super) fn apply(nodes: &mut Vec<Node>, accept: bool) -> Result<(), FormatError> {
    let mut unsupported = None;
    xml::visit_elements(nodes, &mut |e| {
        if is_any(e, &["cellIns", "cellDel", "cellMerge"]) {
            unsupported = Some("inserted, deleted or merged table cells");
        } else if !accept && e.is(W, "numberingChange") {
            unsupported = Some("rejected numbering changes");
        }
    });
    if let Some(what) = unsupported {
        return Err(FormatError::Unsupported(format!("tracked changes: {what}")));
    }
    let fields_were_whole = fields_are_whole(nodes);

    xml::for_each_element(nodes, &mut |e| restore_properties(e, accept));
    let gone: &[&str] = if accept {
        &["del", "moveFrom"]
    } else {
        &["ins", "moveTo"]
    };
    let stays: &[&str] = if accept {
        &["ins", "moveTo"]
    } else {
        &["del", "moveFrom"]
    };
    content(nodes, gone, stays);
    if !accept {
        xml::for_each_element(nodes, &mut |e| {
            if e.is(W, "delText") {
                e.rename("t");
            } else if e.is(W, "delInstrText") {
                e.rename("instrText");
            }
        });
    }
    xml::for_each_element(nodes, &mut |e| numbering(e, accept));
    structure(nodes, gone);
    xml::remove_elements(nodes, &mut |e| {
        is_any(e, RANGE_MARKERS) || e.is(W, "numberingChange")
    });
    remove_unpaired_bookmarks(nodes);

    if find(nodes).any() {
        return Err(FormatError::Unsupported(
            "tracked changes in an unexpected place".into(),
        ));
    }
    if fields_were_whole && !fields_are_whole(nodes) {
        return Err(FormatError::Unsupported(
            "tracked changes that cut through a field".into(),
        ));
    }
    Ok(())
}

fn is_any(e: &Element, names: &[&str]) -> bool {
    names.iter().any(|name| e.is(W, name))
}

fn is_element_in(node: &Node, names: &[&str]) -> bool {
    matches!(node, Node::Element(e) if is_any(e, names))
}

fn child_mut<'a>(e: &'a mut Element, local: &str) -> Option<&'a mut Element> {
    e.children.iter_mut().find_map(|node| match node {
        Node::Element(child) if child.is(W, local) => Some(child),
        _ => None,
    })
}

/// Accepting a formatting change drops the record; rejecting it puts the
/// old properties back.
fn restore_properties(e: &mut Element, accept: bool) {
    let Some(rule) = PROPERTY_CHANGES.iter().find(|r| e.is(W, r.properties)) else {
        return;
    };
    let Some(index) = e
        .children
        .iter()
        .position(|n| is_element_in(n, &[rule.change]))
    else {
        return;
    };
    let change = e.children.remove(index);
    if accept {
        return;
    }
    let Node::Element(change) = change else {
        return;
    };
    let kept = |names: &[&str]| -> Vec<Node> {
        e.children
            .iter()
            .filter(|n| is_element_in(n, names))
            .cloned()
            .collect()
    };
    let mut children = kept(rule.kept_before);
    children.extend(
        change
            .children
            .into_iter()
            .filter_map(|n| match n {
                Node::Element(old) if old.is(W, rule.properties) => Some(old.children),
                _ => None,
            })
            .flatten()
            .filter(|n| !is_element_in(n, rule.kept_before) && !is_element_in(n, rule.kept_after)),
    );
    children.extend(kept(rule.kept_after));
    e.children = children;
}

/// Drops the text that goes and unwraps the text that stays. Marks inside
/// properties are handled with their paragraph, row or numbering.
fn content(nodes: &mut Vec<Node>, gone: &[&str], stays: &[&str]) {
    let mut kept = Vec::with_capacity(nodes.len());
    for node in nodes.drain(..) {
        match node {
            Node::Element(e) if is_any(&e, gone) => {}
            Node::Element(mut e) => {
                if !is_any(&e, &["rPr", "trPr", "numPr"]) {
                    content(&mut e.children, gone, stays);
                }
                if is_any(&e, stays) {
                    kept.append(&mut e.children);
                } else {
                    kept.push(Node::Element(e));
                }
            }
            other => kept.push(other),
        }
    }
    *nodes = kept;
}

/// Numbering added to a paragraph: kept when accepting, removed when rejecting.
fn numbering(e: &mut Element, accept: bool) {
    if !e.is(W, "pPr") {
        return;
    }
    let inserted = e
        .children
        .iter()
        .any(|n| matches!(n, Node::Element(p) if p.is(W, "numPr") && p.elements().any(|m| m.is(W, "ins"))));
    if !inserted {
        return;
    }
    if accept {
        if let Some(numbering) = child_mut(e, "numPr") {
            numbering.children.retain(|n| !is_element_in(n, &["ins"]));
        }
    } else {
        e.children.retain(|n| !is_element_in(n, &["numPr"]));
    }
}

/// Paragraph marks and table rows that go, in every container.
fn structure(nodes: &mut Vec<Node>, gone: &[&str]) {
    for node in nodes.iter_mut() {
        if let Node::Element(e) = node {
            structure(&mut e.children, gone);
        }
    }
    paragraphs(nodes, gone);
    nodes.retain_mut(|n| match n {
        Node::Element(row) if row.is(W, "tr") => !mark_goes(row, "trPr", gone),
        _ => true,
    });
    nodes.retain(|n| !matches!(n, Node::Element(t) if t.is(W, "tbl") && !t.elements().any(|r| r.is(W, "tr"))));
}

/// Removes the inserted/deleted marks from `properties`; whether one of
/// them said the element goes.
fn mark_goes(e: &mut Element, properties: &str, gone: &[&str]) -> bool {
    let Some(properties) = child_mut(e, properties) else {
        return false;
    };
    let goes = properties.elements().any(|m| is_any(m, gone));
    properties.children.retain(|n| !is_element_in(n, MARKS));
    goes
}

/// A paragraph whose mark goes joins the next paragraph, which keeps its
/// own properties, as in Word. When there's no next paragraph to join
/// (a table follows, or a section ends there), the paragraph stays.
fn paragraphs(nodes: &mut Vec<Node>, gone: &[&str]) {
    let mut i = 0;
    while i < nodes.len() {
        let (goes, ends_section) = match nodes.get_mut(i) {
            Some(Node::Element(p)) if p.is(W, "p") => {
                let ends_section = child_mut(p, "pPr")
                    .is_some_and(|properties| properties.elements().any(|e| e.is(W, "sectPr")));
                let goes = child_mut(p, "pPr")
                    .is_some_and(|properties| mark_goes(properties, "rPr", gone));
                (goes, ends_section)
            }
            _ => (false, false),
        };
        let next = next_paragraph(nodes, i);
        if let (true, false, Some(next)) = (goes, ends_section, next) {
            let Node::Element(p) = nodes.remove(i) else {
                i += 1;
                continue;
            };
            let moved: Vec<Node> = p
                .children
                .into_iter()
                .filter(|n| !is_element_in(n, &["pPr"]))
                .collect();
            if let Some(Node::Element(next)) = nodes.get_mut(next - 1) {
                let at = next
                    .children
                    .iter()
                    .position(|n| is_element_in(n, &["pPr"]))
                    .map_or(0, |i| i + 1);
                next.children.splice(at..at, moved);
            }
            continue;
        }
        i += 1;
    }
}

/// The index of the paragraph right after `i`, if the next element is one.
fn next_paragraph(nodes: &[Node], i: usize) -> Option<usize> {
    nodes
        .iter()
        .enumerate()
        .skip(i + 1)
        .find_map(|(j, n)| match n {
            Node::Element(e) => Some((j, e)),
            Node::Other(_) => None,
        })
        .filter(|(_, e)| e.is(W, "p"))
        .map(|(j, _)| j)
}

/// A field (page number, table of contents, …) starts and ends with
/// `w:fldChar` runs; removing only one end breaks it.
fn fields_are_whole(nodes: &[Node]) -> bool {
    let mut depth = 0_i64;
    let mut whole = true;
    xml::visit_elements(nodes, &mut |e| {
        if e.is(W, "fldChar") {
            match e.attribute(W, "fldCharType") {
                Some("begin") => depth += 1,
                Some("end") => depth -= 1,
                _ => {}
            }
            whole &= depth >= 0;
        }
    });
    whole && depth == 0
}

/// Bookmarks whose start or end went with deleted text.
fn remove_unpaired_bookmarks(nodes: &mut Vec<Node>) {
    let mut starts = HashSet::new();
    let mut ends = HashSet::new();
    xml::visit_elements(nodes, &mut |e| {
        let id = e.attribute(W, "id").map(str::to_owned);
        if e.is(W, "bookmarkStart") {
            starts.extend(id);
        } else if e.is(W, "bookmarkEnd") {
            ends.extend(id);
        }
    });
    if starts == ends {
        return;
    }
    xml::remove_elements(nodes, &mut |e| {
        let id = e.attribute(W, "id").unwrap_or_default();
        (e.is(W, "bookmarkStart") && !ends.contains(id))
            || (e.is(W, "bookmarkEnd") && !starts.contains(id))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(content: &str) -> String {
        format!(
            r#"<w:document xmlns:w="{}"><w:body>{content}</w:body></w:document>"#,
            W[0]
        )
    }

    fn applied(content: &str, accept: bool) -> Result<String, FormatError> {
        let mut nodes = xml::parse(body(content).as_bytes())?;
        apply(&mut nodes, accept)?;
        let xml = String::from_utf8(xml::write(&nodes)?).unwrap();
        Ok(xml
            .trim_start_matches(&body("").replace("</w:body></w:document>", ""))
            .trim_end_matches("</w:body></w:document>")
            .to_owned())
    }

    fn both(content: &str) -> (String, String) {
        (
            applied(content, true).unwrap(),
            applied(content, false).unwrap(),
        )
    }

    const EDIT: &str = r#"<w:p><w:r><w:t>Budget: </w:t></w:r><w:del w:id="1" w:author="Jane Doe" w:date="2026-03-14T09:26:00Z"><w:r><w:delText>40,000</w:delText></w:r></w:del><w:ins w:id="2" w:author="Jane Doe"><w:r><w:t>25,000</w:t></w:r></w:ins></w:p>"#;

    #[test]
    fn finds_changes_and_their_authors() {
        let nodes = xml::parse(body(EDIT).as_bytes()).unwrap();
        let found = find(&nodes);
        assert_eq!(found.changes, 2);
        assert_eq!(found.authors.into_iter().collect::<Vec<_>>(), ["Jane Doe"]);
        assert!(!find(&xml::parse(body("<w:p/>").as_bytes()).unwrap()).any());
    }

    #[test]
    fn insertions_and_deletions() {
        let (accepted, rejected) = both(EDIT);
        assert_eq!(
            accepted,
            "<w:p><w:r><w:t>Budget: </w:t></w:r><w:r><w:t>25,000</w:t></w:r></w:p>"
        );
        assert_eq!(
            rejected,
            "<w:p><w:r><w:t>Budget: </w:t></w:r><w:r><w:t>40,000</w:t></w:r></w:p>"
        );
    }

    #[test]
    fn moves_keep_one_copy() {
        let moved = r#"<w:p><w:moveFromRangeStart w:id="1" w:name="move1"/><w:moveFrom w:id="2"><w:r><w:t>Moved</w:t></w:r></w:moveFrom><w:moveFromRangeEnd w:id="1"/></w:p><w:p><w:moveToRangeStart w:id="3" w:name="move1"/><w:moveTo w:id="4"><w:r><w:t>Moved</w:t></w:r></w:moveTo><w:moveToRangeEnd w:id="3"/></w:p>"#;
        let (accepted, rejected) = both(moved);
        assert_eq!(
            accepted,
            "<w:p></w:p><w:p><w:r><w:t>Moved</w:t></w:r></w:p>"
        );
        assert_eq!(
            rejected,
            "<w:p><w:r><w:t>Moved</w:t></w:r></w:p><w:p></w:p>"
        );
    }

    #[test]
    fn formatting_changes() {
        let bold = r#"<w:p><w:pPr><w:jc w:val="center"/><w:pPrChange w:id="1" w:author="Jane"><w:pPr><w:jc w:val="left"/></w:pPr></w:pPrChange></w:pPr><w:r><w:rPr><w:b/><w:rPrChange w:id="2" w:author="Jane"><w:rPr><w:i/></w:rPr></w:rPrChange></w:rPr><w:t>x</w:t></w:r></w:p>"#;
        let (accepted, rejected) = both(bold);
        assert_eq!(
            accepted,
            r#"<w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t>x</w:t></w:r></w:p>"#
        );
        assert_eq!(
            rejected,
            r#"<w:p><w:pPr><w:jc w:val="left"/></w:pPr><w:r><w:rPr><w:i/></w:rPr><w:t>x</w:t></w:r></w:p>"#
        );
    }

    #[test]
    fn rejected_paragraph_properties_keep_the_mark_and_section() {
        let section = r#"<w:p><w:pPr><w:jc w:val="center"/><w:rPr><w:b/></w:rPr><w:sectPr><w:pgSz w:w="1"/></w:sectPr><w:pPrChange w:id="1"><w:pPr/></w:pPrChange></w:pPr></w:p>"#;
        assert_eq!(
            applied(section, false).unwrap(),
            r#"<w:p><w:pPr><w:rPr><w:b/></w:rPr><w:sectPr><w:pgSz w:w="1"/></w:sectPr></w:pPr></w:p>"#
        );
    }

    /// A whole paragraph deleted: its text and its mark.
    const DELETED_PARAGRAPH: &str = r#"<w:p><w:pPr><w:rPr><w:del w:id="1" w:author="Jane"/></w:rPr></w:pPr><w:del w:id="2" w:author="Jane"><w:r><w:delText>Secret.</w:delText></w:r></w:del></w:p><w:p><w:pPr><w:jc w:val="right"/></w:pPr><w:r><w:t>Next.</w:t></w:r></w:p>"#;

    #[test]
    fn deleted_paragraph_marks_join_the_next_paragraph() {
        let (accepted, rejected) = both(DELETED_PARAGRAPH);
        assert_eq!(
            accepted,
            r#"<w:p><w:pPr><w:jc w:val="right"/></w:pPr><w:r><w:t>Next.</w:t></w:r></w:p>"#
        );
        assert_eq!(
            rejected,
            r#"<w:p><w:pPr><w:rPr></w:rPr></w:pPr><w:r><w:t>Secret.</w:t></w:r></w:p><w:p><w:pPr><w:jc w:val="right"/></w:pPr><w:r><w:t>Next.</w:t></w:r></w:p>"#
        );
    }

    #[test]
    fn a_paragraph_before_a_table_stays() {
        let before_table = DELETED_PARAGRAPH.replace(
            r#"<w:p><w:pPr><w:jc"#,
            r#"<w:tbl><w:tr><w:tc><w:p/></w:tc></w:tr></w:tbl><w:p><w:pPr><w:jc"#,
        );
        assert!(
            applied(&before_table, true)
                .unwrap()
                .starts_with("<w:p><w:pPr><w:rPr></w:rPr></w:pPr></w:p><w:tbl>")
        );
    }

    #[test]
    fn inserted_and_deleted_rows() {
        let table = r#"<w:tbl><w:tr><w:trPr><w:ins w:id="1"/></w:trPr><w:tc><w:p/></w:tc></w:tr><w:tr><w:trPr><w:del w:id="2"/></w:trPr><w:tc><w:p/></w:tc></w:tr></w:tbl>"#;
        let (accepted, rejected) = both(table);
        let row = "<w:tr><w:trPr></w:trPr><w:tc><w:p/></w:tc></w:tr>";
        assert_eq!(accepted, format!("<w:tbl>{row}</w:tbl>"));
        assert_eq!(rejected, format!("<w:tbl>{row}</w:tbl>"));
        let inserted = r#"<w:tbl><w:tr><w:trPr><w:ins w:id="1"/></w:trPr><w:tc><w:p/></w:tc></w:tr></w:tbl><w:p/>"#;
        assert_eq!(applied(inserted, false).unwrap(), "<w:p/>");
    }

    #[test]
    fn deleted_bookmark_ends_take_their_start_along() {
        let bookmark = r#"<w:p><w:bookmarkStart w:id="0" w:name="x"/><w:r><w:t>a</w:t></w:r><w:del w:id="1"><w:bookmarkEnd w:id="0"/><w:r><w:delText>b</w:delText></w:r></w:del></w:p>"#;
        assert_eq!(
            applied(bookmark, true).unwrap(),
            "<w:p><w:r><w:t>a</w:t></w:r></w:p>"
        );
    }

    #[test]
    fn unsafe_changes_are_refused() {
        let refused = |content: &str, accept| {
            matches!(applied(content, accept), Err(FormatError::Unsupported(_)))
        };
        let cell = r#"<w:tbl><w:tr><w:tc><w:tcPr><w:cellIns w:id="1"/></w:tcPr><w:p/></w:tc></w:tr></w:tbl>"#;
        assert!(refused(cell, true));
        let half_field = r#"<w:p><w:del w:id="1"><w:r><w:fldChar w:fldCharType="begin"/></w:r></w:del><w:r><w:instrText>PAGE</w:instrText></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#;
        assert!(refused(half_field, true));
        assert!(applied(half_field, false).is_ok());
        let numbering = r#"<w:p><w:pPr><w:numPr><w:numberingChange w:id="1" w:original="%1."/></w:numPr></w:pPr></w:p>"#;
        assert!(refused(numbering, false));
        assert!(applied(numbering, true).is_ok());
    }
}
