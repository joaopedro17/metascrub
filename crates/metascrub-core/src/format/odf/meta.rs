//! `meta.xml`: who wrote the document, when, with what, and any properties
//! someone added. The file is removed whole; it's read only for the report.

use crate::format::xml::{self, Element};
use crate::model::{Disposition, Finding, FindingKind, Location, Value};

const OFFICE: &[&str] = &["urn:oasis:names:tc:opendocument:xmlns:office:1.0"];
const META: &str = "urn:oasis:names:tc:opendocument:xmlns:meta:1.0";
const DC: &str = "http://purl.org/dc/elements/1.1/";
const XLINK: &[&str] = &["http://www.w3.org/1999/xlink"];

pub(super) fn findings(data: &[u8], location: &Location) -> Vec<Finding> {
    let unreadable = || {
        vec![
            Finding::new(
                FindingKind::UnknownMetadata,
                location.clone(),
                Disposition::Remove,
            )
            .with_value(Value::Bytes(data.len() as u64)),
        ]
    };
    let Ok(nodes) = xml::parse(data) else {
        return unreadable();
    };
    let mut meta = None;
    xml::visit_elements(&nodes, &mut |e: &Element| {
        if meta.is_none() && e.is(OFFICE, "meta") {
            meta = Some(e);
        }
    });
    let Some(meta) = meta else {
        return unreadable();
    };
    let mut findings: Vec<Finding> = Vec::new();
    for e in meta.elements() {
        let (name, kind, value) = describe(e);
        let finding = Finding::new(kind, location.child(name.clone()), Disposition::Remove);
        let finding = match value {
            Some(value) => finding.with_value(Value::Text(value)),
            None => finding,
        };
        // Repeated properties (keywords, several statistics) become one.
        match findings.iter_mut().find(|f| f.location == finding.location) {
            Some(Finding {
                value: Some(Value::Text(existing)),
                ..
            }) => {
                if let Some(Value::Text(more)) = finding.value {
                    existing.push_str(", ");
                    existing.push_str(&more);
                }
            }
            _ => findings.push(finding),
        }
    }
    findings
}

fn describe(e: &Element) -> (String, FindingKind, Option<String>) {
    use FindingKind::*;
    let text = e.text().trim().to_owned();
    let text = (!text.is_empty()).then_some(text);
    let prefix = match e.ns.as_str() {
        DC => "dc",
        META => "meta",
        _ => "",
    };
    let name = if prefix.is_empty() {
        e.local.clone()
    } else {
        format!("{prefix}:{}", e.local)
    };
    let (kind, value) = match (prefix, e.local.as_str()) {
        ("dc", "title" | "description" | "subject") | ("meta", "keyword") => (Description, text),
        ("dc", "creator") | ("meta", "initial-creator" | "printed-by") => (PersonName, text),
        ("dc", "date") | ("meta", "creation-date" | "print-date") => (Timestamp, text),
        ("meta", "generator") => (Software, text),
        ("meta", "template") => {
            let href = e.attribute(XLINK, "href").filter(|h| !h.is_empty());
            let title = e.attribute(XLINK, "title");
            (FilePath, href.or(title).map(str::to_owned))
        }
        ("meta", "auto-reload") => (DownloadUrl, e.attribute(XLINK, "href").map(str::to_owned)),
        ("meta", "user-defined") => {
            let field = e.attribute(&[META], "name").unwrap_or_default();
            return (format!("{name} {field}"), CustomProperty, text);
        }
        ("dc", "language")
        | ("meta", "editing-cycles" | "editing-duration" | "document-statistic")
        | ("meta", "hyperlink-behaviour") => (TechnicalInfo, text),
        _ => (UnknownMetadata, text),
    };
    (name, kind, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_every_property() {
        let data = br#"<office:document-meta xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:xlink="http://www.w3.org/1999/xlink"><office:meta><meta:initial-creator>Jane Doe</meta:initial-creator><meta:keyword>a</meta:keyword><meta:keyword>b</meta:keyword><meta:generator>LibreOffice</meta:generator><meta:user-defined meta:name="Client">Acme</meta:user-defined><meta:template xlink:href="file:///home/jane/Report.ott"/><meta:document-statistic meta:page-count="1"/></office:meta></office:document-meta>"#;
        let found: Vec<_> = findings(data, &Location::new(["meta.xml"]))
            .into_iter()
            .map(|f| (f.kind, f.location.to_string(), f.value))
            .collect();
        let text = |t: &str| Some(Value::Text(t.into()));
        assert_eq!(
            found,
            [
                (
                    FindingKind::PersonName,
                    "meta.xml > meta:initial-creator".into(),
                    text("Jane Doe")
                ),
                (
                    FindingKind::Description,
                    "meta.xml > meta:keyword".into(),
                    text("a, b")
                ),
                (
                    FindingKind::Software,
                    "meta.xml > meta:generator".into(),
                    text("LibreOffice")
                ),
                (
                    FindingKind::CustomProperty,
                    "meta.xml > meta:user-defined Client".into(),
                    text("Acme")
                ),
                (
                    FindingKind::FilePath,
                    "meta.xml > meta:template".into(),
                    text("file:///home/jane/Report.ott")
                ),
                (
                    FindingKind::TechnicalInfo,
                    "meta.xml > meta:document-statistic".into(),
                    None
                ),
            ]
        );
    }
}
