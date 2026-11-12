//! Content that is part of the document but not shown: hidden sheets,
//! hidden text, hidden slides and speaker notes. It's the user's content,
//! not metadata, so it's reported but never changed.

use super::{P, S, W};
use crate::format::xml::{self, Element, Node};
use crate::model::{Disposition, Finding, FindingKind, Location, Value};

const A: &[&str] = &[
    "http://schemas.openxmlformats.org/drawingml/2006/main",
    "http://purl.oclc.org/ooxml/drawingml/main",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Family {
    Word,
    Excel,
    PowerPoint,
}

pub(super) fn findings(
    nodes: &[Node],
    family: Family,
    content_type: &str,
    location: &Location,
) -> Vec<Finding> {
    let warn = |name: &str, value: Option<String>| {
        let finding = Finding::new(
            FindingKind::HiddenContent,
            location.child(name),
            Disposition::Warn,
        );
        match value {
            Some(text) => finding.with_value(Value::Text(text)),
            None => finding,
        }
    };
    let mut found = Vec::new();
    match family {
        Family::Word => {
            let mut hidden = Vec::new();
            xml::visit_elements(nodes, &mut |e| {
                if e.is(W, "r") && is_hidden_run(e) {
                    hidden.push(e.text());
                }
            });
            let text = hidden.join(" ");
            if !text.trim().is_empty() {
                found.push(warn("vanish", Some(text.trim().to_owned())));
            }
        }
        Family::Excel => xml::visit_elements(nodes, &mut |e| {
            if e.is(S, "sheet")
                && matches!(e.attribute(&[""], "state"), Some("hidden" | "veryHidden"))
            {
                let name = e.attribute(&[""], "name").unwrap_or_default();
                found.push(warn("sheet", Some(name.to_owned())));
            }
        }),
        Family::PowerPoint => {
            xml::visit_elements(nodes, &mut |e| {
                if e.is(P, "sld") && matches!(e.attribute(&[""], "show"), Some("0" | "false")) {
                    found.push(warn("show", None));
                }
            });
            if content_type.contains("presentationml.notesslide+xml") {
                let notes = speaker_notes(nodes);
                if !notes.is_empty() {
                    found.push(warn("notes", Some(notes)));
                }
            }
        }
    }
    found
}

fn is_hidden_run(run: &Element) -> bool {
    run.elements()
        .filter(|p| p.is(W, "rPr"))
        .flat_map(Element::elements)
        .any(|v| v.is(W, "vanish") && !matches!(v.attribute(W, "val"), Some("0" | "false" | "off")))
}

/// The text of the notes placeholder, without the slide image and number.
fn speaker_notes(nodes: &[Node]) -> String {
    let mut paragraphs = Vec::new();
    xml::visit_elements(nodes, &mut |e| {
        let is_body = e.is(P, "sp")
            && e.find(P, "ph")
                .is_some_and(|ph| ph.attribute(&[""], "type") == Some("body"));
        if !is_body {
            return;
        }
        xml::visit_elements(&e.children, &mut |p| {
            if p.is(A, "p") {
                paragraphs.push(p.text());
            }
        });
    });
    paragraphs
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(xml: &str, family: Family, content_type: &str) -> Vec<(String, Option<Value>)> {
        let nodes = xml::parse(xml.as_bytes()).unwrap();
        findings(&nodes, family, content_type, &Location::new(["part.xml"]))
            .into_iter()
            .map(|f| (f.location.to_string(), f.value))
            .collect()
    }

    #[test]
    fn hidden_word_text() {
        let xml = format!(
            r#"<w:p xmlns:w="{}"><w:r><w:rPr><w:vanish/></w:rPr><w:t>Secret</w:t></w:r><w:r><w:rPr><w:vanish w:val="0"/></w:rPr><w:t>Shown</w:t></w:r><w:r><w:rPr><w:vanish/></w:rPr><w:fldChar w:fldCharType="begin"/></w:r></w:p>"#,
            W[0]
        );
        assert_eq!(
            found(&xml, Family::Word, ""),
            [(
                "part.xml > vanish".to_owned(),
                Some(Value::Text("Secret".into()))
            )]
        );
    }

    #[test]
    fn hidden_sheets() {
        let xml = format!(
            r#"<workbook xmlns="{}"><sheets><sheet name="Summary" sheetId="1"/><sheet name="Salaries" sheetId="2" state="veryHidden"/></sheets></workbook>"#,
            S[0]
        );
        assert_eq!(
            found(&xml, Family::Excel, ""),
            [(
                "part.xml > sheet".to_owned(),
                Some(Value::Text("Salaries".into()))
            )]
        );
    }

    #[test]
    fn hidden_slides_and_speaker_notes() {
        let p = P[0];
        let a = A[0];
        let slide = format!(r#"<p:sld xmlns:p="{p}" show="0"><p:cSld/></p:sld>"#);
        assert_eq!(found(&slide, Family::PowerPoint, "").len(), 1);
        let shape = |kind: &str, text: &str| {
            format!(
                r#"<p:sp><p:nvSpPr><p:nvPr><p:ph type="{kind}"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>{text}</a:t></a:r></a:p></p:txBody></p:sp>"#
            )
        };
        let notes = format!(
            r#"<p:notes xmlns:p="{p}" xmlns:a="{a}"><p:cSld><p:spTree>{}{}</p:spTree></p:cSld></p:notes>"#,
            shape("sldNum", "4"),
            shape("body", "Don't mention the layoffs.")
        );
        let content_type =
            "application/vnd.openxmlformats-officedocument.presentationml.notesslide+xml";
        assert_eq!(
            found(&notes, Family::PowerPoint, content_type),
            [(
                "part.xml > notes".to_owned(),
                Some(Value::Text("Don't mention the layoffs.".into()))
            )]
        );
        let empty = notes.replace("Don't mention the layoffs.", "");
        assert!(found(&empty, Family::PowerPoint, content_type).is_empty());
    }
}
