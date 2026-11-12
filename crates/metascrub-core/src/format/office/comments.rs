//! Comments in Word, Excel and PowerPoint. Removed by default; with
//! `--keep comments`, kept with every author shown as `Author` and the
//! dates and account details removed or reset.

use crate::format::xml::{self, Element, Node};
use crate::model::{Disposition, Finding, FindingKind, KeepReason, Location, Value};

const W: &[&str] = &[
    "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
    "http://purl.oclc.org/ooxml/wordprocessingml/main",
];
pub(super) const W16CEX: &[&str] = &["http://schemas.microsoft.com/office/word/2018/wordml/cex"];
const S: &[&str] = &[
    "http://schemas.openxmlformats.org/spreadsheetml/2006/main",
    "http://purl.oclc.org/ooxml/spreadsheetml/main",
];
const TC: &[&str] = &["http://schemas.microsoft.com/office/spreadsheetml/2018/threadedcomments"];
const P: &[&str] = &[
    "http://schemas.openxmlformats.org/presentationml/2006/main",
    "http://purl.oclc.org/ooxml/presentationml/main",
];
const P15: &[&str] = &["http://schemas.microsoft.com/office/powerpoint/2012/main"];
const P188: &[&str] = &["http://schemas.microsoft.com/office/powerpoint/2018/8/main"];
const VML_EXCEL: &[&str] = &["urn:schemas-microsoft-com:office:excel"];

const NEUTRAL_AUTHOR: &str = "Author";
const NEUTRAL_INITIALS: &str = "A";
/// Modern PowerPoint comments must have a creation date.
const NEUTRAL_DATE: &str = "1980-01-01T00:00:00.000";
/// What PowerPoint writes for an author without an online account.
const LOCAL_PROVIDER: &str = "None";

/// Content types of the parts that hold comments and their authors
/// (lower case).
const COMMENT_PARTS: &[&str] = &[
    "wordprocessingml.comments+xml",
    "wordprocessingml.commentsextended+xml",
    "wordprocessingml.commentsids+xml",
    "wordprocessingml.commentsextensible+xml",
    "spreadsheetml.comments+xml",
    "ms-excel.threadedcomments+xml",
    "ms-excel.person+xml",
    "presentationml.comments+xml",
    "presentationml.commentauthors+xml",
    "ms-powerpoint.comments+xml",
    "ms-powerpoint.authors+xml",
];

pub(super) fn is_comment_part(content_type: &str) -> bool {
    let content_type = content_type.to_ascii_lowercase();
    COMMENT_PARTS.iter().any(|c| content_type.contains(c))
}

/// What a removed comment part contained, for the report.
pub(super) fn findings(data: &[u8], location: &Location) -> Vec<Finding> {
    let Ok(mut nodes) = xml::parse(data) else {
        return vec![
            Finding::new(FindingKind::Comment, location.clone(), Disposition::Remove)
                .with_value(Value::Bytes(data.len() as u64)),
        ];
    };
    let mut findings = Vec::new();
    xml::for_each_element(&mut nodes, &mut |e| {
        let location = location.child(e.local.as_str());
        if let Some(text) = comment_text(e) {
            findings.push(
                Finding::new(FindingKind::Comment, location.clone(), Disposition::Remove)
                    .with_value(Value::Text(text)),
            );
        }
        for (name, value, kind) in personal_details(e) {
            findings.push(
                Finding::new(kind, location.child(name), Disposition::Remove)
                    .with_value(Value::Text(value)),
            );
        }
    });
    findings
}

/// The text of a comment or reply element, if `e` is one.
fn comment_text(e: &Element) -> Option<String> {
    let text = if e.is(W, "comment") || e.is(S, "comment") {
        e.text()
    } else if e.is(TC, "threadedComment") || e.is(P, "cm") {
        child_text(e, "text")
    } else if e.is(P188, "cm") || e.is(P188, "reply") {
        child_text(e, "txBody")
    } else {
        return None;
    };
    Some(text.trim().to_owned())
}

/// The text of a direct child, so a comment doesn't include its replies.
fn child_text(e: &Element, local: &str) -> String {
    e.elements()
        .find(|c| c.local == local)
        .map(Element::text)
        .unwrap_or_default()
}

/// Names, initials, account IDs and dates on `e`.
fn personal_details(e: &Element) -> Vec<(&'static str, String, FindingKind)> {
    let attributes: &[(&[&str], &str, FindingKind)] = if e.is(W, "comment") {
        &[
            (W, "author", FindingKind::PersonName),
            (W, "initials", FindingKind::PersonName),
            (W, "date", FindingKind::Timestamp),
        ]
    } else if e.is(TC, "threadedComment") {
        &[(&[""], "dT", FindingKind::Timestamp)]
    } else if e.is(TC, "person") {
        &[
            (&[""], "displayName", FindingKind::PersonName),
            (&[""], "userId", FindingKind::PersonName),
        ]
    } else if e.is(P, "cmAuthor") {
        &[
            (&[""], "name", FindingKind::PersonName),
            (&[""], "initials", FindingKind::PersonName),
        ]
    } else if e.is(P, "cm") {
        &[(&[""], "dt", FindingKind::Timestamp)]
    } else if e.is(P188, "author") {
        &[
            (&[""], "name", FindingKind::PersonName),
            (&[""], "initials", FindingKind::PersonName),
            (&[""], "userId", FindingKind::PersonName),
        ]
    } else if e.is(P188, "cm") || e.is(P188, "reply") {
        &[(&[""], "created", FindingKind::Timestamp)]
    } else if e.is(P15, "presenceInfo") {
        &[(&[""], "userId", FindingKind::PersonName)]
    } else if e.is(W16CEX, "commentExtensible") {
        &[(W16CEX, "dateUtc", FindingKind::Timestamp)]
    } else if e.is(S, "author") {
        let name = e.text();
        return if is_placeholder_author(&name) {
            Vec::new()
        } else {
            vec![("author", name, FindingKind::PersonName)]
        };
    } else {
        return Vec::new();
    };
    attributes
        .iter()
        .filter_map(|(ns, name, kind)| Some((*name, e.attribute(ns, name)?.to_owned(), *kind)))
        .collect()
}

/// Excel's stand-ins for threaded comments use `tc={id}` as the author.
fn is_placeholder_author(name: &str) -> bool {
    name.starts_with("tc=") || name == NEUTRAL_AUTHOR
}

/// What to do with one attribute when keeping comments.
enum Change {
    Remove,
    Set(&'static str),
}

/// `--keep comments`: anonymizes a comment part in place. Returns whether
/// anything changed.
pub(super) fn anonymize(nodes: &mut [Node], location: &Location, found: &mut Vec<Finding>) -> bool {
    let mut changed = false;
    // Excel starts each note with its author's name in bold: "Jane Doe:".
    let mut excel_authors = Vec::new();
    xml::for_each_element(nodes, &mut |e| {
        let location = location.child(e.local.as_str());
        if let Some(text) = comment_text(e) {
            found.push(
                Finding::new(
                    FindingKind::Comment,
                    location.clone(),
                    Disposition::Keep(KeepReason::UserRequested),
                )
                .with_value(Value::Text(text)),
            );
        }
        if e.is(S, "author") {
            let name = e.text();
            if !is_placeholder_author(&name) {
                e.set_text(NEUTRAL_AUTHOR);
                changed = true;
                found.push(normalized(FindingKind::PersonName, &location, name.clone()));
                excel_authors.push(format!("{name}:"));
            }
            return;
        }
        if e.is(S, "t") && excel_authors.iter().any(|a| *a == e.text().trim()) {
            e.set_text(&format!("{NEUTRAL_AUTHOR}:"));
            changed = true;
            return;
        }
        let changes: &[(&[&str], &str, FindingKind, Change)] = if e.is(W, "comment") {
            &[
                (
                    W,
                    "author",
                    FindingKind::PersonName,
                    Change::Set(NEUTRAL_AUTHOR),
                ),
                (W, "initials", FindingKind::PersonName, Change::Remove),
                (W, "date", FindingKind::Timestamp, Change::Remove),
            ]
        } else if e.is(W16CEX, "commentExtensible") {
            &[(W16CEX, "dateUtc", FindingKind::Timestamp, Change::Remove)]
        } else if e.is(TC, "threadedComment") {
            &[(&[""], "dT", FindingKind::Timestamp, Change::Remove)]
        } else if e.is(TC, "person") {
            &[
                (
                    &[""],
                    "displayName",
                    FindingKind::PersonName,
                    Change::Set(NEUTRAL_AUTHOR),
                ),
                (&[""], "userId", FindingKind::PersonName, Change::Remove),
                (
                    &[""],
                    "providerId",
                    FindingKind::TechnicalInfo,
                    Change::Remove,
                ),
            ]
        } else if e.is(P, "cmAuthor") {
            &[
                (
                    &[""],
                    "name",
                    FindingKind::PersonName,
                    Change::Set(NEUTRAL_AUTHOR),
                ),
                (
                    &[""],
                    "initials",
                    FindingKind::PersonName,
                    Change::Set(NEUTRAL_INITIALS),
                ),
            ]
        } else if e.is(P, "cm") {
            &[(&[""], "dt", FindingKind::Timestamp, Change::Remove)]
        } else if e.is(P188, "author") {
            &[
                (
                    &[""],
                    "name",
                    FindingKind::PersonName,
                    Change::Set(NEUTRAL_AUTHOR),
                ),
                (
                    &[""],
                    "initials",
                    FindingKind::PersonName,
                    Change::Set(NEUTRAL_INITIALS),
                ),
                (
                    &[""],
                    "userId",
                    FindingKind::PersonName,
                    Change::Set(NEUTRAL_AUTHOR),
                ),
                (
                    &[""],
                    "providerId",
                    FindingKind::TechnicalInfo,
                    Change::Set(LOCAL_PROVIDER),
                ),
            ]
        } else if e.is(P188, "cm") || e.is(P188, "reply") {
            &[(
                &[""],
                "created",
                FindingKind::Timestamp,
                Change::Set(NEUTRAL_DATE),
            )]
        } else {
            &[]
        };
        for (ns, name, kind, change) in changes {
            let Some(value) = e.attribute(ns, name).map(str::to_owned) else {
                continue;
            };
            match change {
                Change::Remove => {
                    e.remove_attributes(|a_ns, local, _| ns.contains(&a_ns) && local == *name);
                    found.push(
                        Finding::new(*kind, location.child(*name), Disposition::Remove)
                            .with_value(Value::Text(value)),
                    );
                }
                Change::Set(neutral) if value != *neutral => {
                    e.set_attribute(ns, name, neutral);
                    found.push(normalized(*kind, &location.child(*name), value));
                }
                Change::Set(_) => continue,
            }
            changed = true;
        }
        // PowerPoint keeps the author's account in an extension.
        if e.is(P, "cmAuthor") {
            let removed = xml::remove_elements(&mut e.children, &mut |c| c.is(P15, "presenceInfo"));
            for presence in removed {
                changed = true;
                let user = presence.attribute(&[""], "userId").unwrap_or_default();
                found.push(
                    Finding::new(
                        FindingKind::PersonName,
                        location.child("presenceInfo"),
                        Disposition::Remove,
                    )
                    .with_value(Value::Text(user.to_owned())),
                );
            }
        }
    });
    changed
}

fn normalized(kind: FindingKind, location: &Location, value: String) -> Finding {
    Finding::new(kind, location.clone(), Disposition::Normalize).with_value(Value::Text(value))
}

/// The shapes in an Excel drawing layer (`vmlDrawing`): notes, and anything
/// else (form controls).
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct VmlShapes {
    pub(super) notes: usize,
    pub(super) others: usize,
}

pub(super) fn vml_shapes(data: &[u8]) -> Option<VmlShapes> {
    let mut nodes = xml::parse(data).ok()?;
    let mut shapes = VmlShapes::default();
    xml::for_each_element(&mut nodes, &mut |e| {
        if e.is(VML_EXCEL, "ClientData") {
            if e.attribute(&[""], "ObjectType") == Some("Note") {
                shapes.notes += 1;
            } else {
                shapes.others += 1;
            }
        }
    });
    Some(shapes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(xml: &str) -> (String, Vec<Finding>) {
        let mut nodes = xml::parse(xml.as_bytes()).unwrap();
        let mut found = Vec::new();
        anonymize(&mut nodes, &Location::new(["part"]), &mut found);
        (
            String::from_utf8(xml::write(&nodes).unwrap()).unwrap(),
            found,
        )
    }

    fn values(findings: &[Finding]) -> Vec<(FindingKind, Disposition, Option<Value>)> {
        findings
            .iter()
            .map(|f| (f.kind, f.disposition, f.value.clone()))
            .collect()
    }

    const EXCEL_NOTES: &str = r#"<comments xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><authors><author>Jane Doe</author><author>tc={1A2B}</author></authors><commentList><comment ref="A1" authorId="0"><text><r><rPr><b/></rPr><t>Jane Doe:</t></r><r><t xml:space="preserve">
Check with Acme.</t></r></text></comment></commentList></comments>"#;

    #[test]
    fn excel_notes_are_reported_and_anonymized() {
        let text = |f: &[Finding]| {
            values(f)
                .into_iter()
                .map(|(k, _, v)| (k, v))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            text(&findings(EXCEL_NOTES.as_bytes(), &Location::new(["part"]))),
            [
                (
                    FindingKind::PersonName,
                    Some(Value::Text("Jane Doe".into()))
                ),
                (
                    FindingKind::Comment,
                    Some(Value::Text("Jane Doe:\nCheck with Acme.".into()))
                ),
            ]
        );
        let (xml, found) = kept(EXCEL_NOTES);
        assert!(
            xml.contains("<author>Author</author><author>tc={1A2B}</author>"),
            "{xml}"
        );
        assert!(
            xml.contains("<t>Author:</t>") && !xml.contains("Jane"),
            "{xml}"
        );
        assert_eq!(found.len(), 2);
        let (again, found) = kept(&xml);
        assert_eq!(again, xml);
        assert!(
            found
                .iter()
                .all(|f| f.disposition.is_acceptable_after_cleaning())
        );
    }

    #[test]
    fn excel_threaded_comments_and_people_are_anonymized() {
        let tc = TC[0];
        let (xml, found) = kept(&format!(
            r#"<ThreadedComments xmlns="{tc}"><threadedComment ref="B2" dT="2026-03-14T09:30:00.00" personId="{{P1}}" id="{{C1}}"><text>Is this final?</text></threadedComment></ThreadedComments>"#
        ));
        assert!(!xml.contains("dT=") && xml.contains("Is this final?"));
        assert_eq!(found.len(), 2);
        let (xml, _) = kept(&format!(
            r#"<personList xmlns="{tc}"><person displayName="Jane Doe" id="{{P1}}" userId="jane.doe@example.com" providerId="AD"/></personList>"#
        ));
        assert_eq!(
            xml,
            format!(
                r#"<personList xmlns="{tc}"><person displayName="Author" id="{{P1}}"/></personList>"#
            )
        );
    }

    #[test]
    fn powerpoint_comments_and_authors_are_anonymized() {
        let (p, p15, p188) = (P[0], P15[0], P188[0]);
        let (xml, _) = kept(&format!(
            r#"<p:cmAuthorLst xmlns:p="{p}"><p:cmAuthor id="1" name="Jane Doe" initials="JD" lastIdx="1" clrIdx="0"><p:extLst><p:ext uri="{{19B8F6BF}}"><p15:presenceInfo xmlns:p15="{p15}" userId="jane.doe@example.com" providerId="AD"/></p:ext></p:extLst></p:cmAuthor></p:cmAuthorLst>"#
        ));
        assert!(
            xml.contains(r#"name="Author" initials="A""#) && !xml.contains("jane"),
            "{xml}"
        );

        let modern = format!(
            r#"<p188:cmLst xmlns:p188="{p188}" xmlns:a="urn:a"><p188:cm id="{{C1}}" authorId="{{A1}}" created="2026-03-14T09:30:00.000"><p188:txBody><a:p><a:r><a:t>Shorter title?</a:t></a:r></a:p></p188:txBody><p188:replyLst><p188:reply id="{{R1}}" authorId="{{A1}}" created="2026-03-14T10:00:00.000"><p188:txBody><a:p><a:r><a:t>Done.</a:t></a:r></a:p></p188:txBody></p188:reply></p188:replyLst></p188:cm></p188:cmLst>"#
        );
        let removed = findings(modern.as_bytes(), &Location::new(["part"]));
        let comments: Vec<_> = removed
            .iter()
            .filter(|f| f.kind == FindingKind::Comment)
            .collect();
        assert_eq!(comments.len(), 2);
        assert_eq!(
            comments[0].value,
            Some(Value::Text("Shorter title?".into()))
        );
        let (xml, _) = kept(&modern);
        assert_eq!(xml.matches(NEUTRAL_DATE).count(), 2);

        let (xml, _) = kept(&format!(
            r#"<p188:authorLst xmlns:p188="{p188}"><p188:author id="{{A1}}" name="Jane Doe" initials="JD" userId="jane.doe@example.com" providerId="AD"/></p188:authorLst>"#
        ));
        assert!(
            xml.contains(r#"name="Author" initials="A" userId="Author" providerId="None""#),
            "{xml}"
        );
    }

    #[test]
    fn counts_notes_and_other_shapes_in_drawing_layers() {
        let vml = |types: &[&str]| {
            let shapes: String = types
                .iter()
                .map(|t| format!(r#"<v:shape><x:ClientData ObjectType="{t}"/></v:shape>"#))
                .collect();
            format!(
                r#"<xml xmlns:v="urn:schemas-microsoft-com:vml" xmlns:x="urn:schemas-microsoft-com:office:excel">{shapes}</xml>"#
            )
        };
        assert_eq!(
            vml_shapes(vml(&["Note", "Note"]).as_bytes()),
            Some(VmlShapes {
                notes: 2,
                others: 0
            })
        );
        assert_eq!(
            vml_shapes(vml(&["Note", "Checkbox"]).as_bytes()),
            Some(VmlShapes {
                notes: 1,
                others: 1
            })
        );
    }
}
