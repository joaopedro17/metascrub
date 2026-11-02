//! What the document property parts say, for the report. These parts are
//! removed whole, so reading them never affects the cleaned file.

use super::xml::{self, Element, Node};
use crate::model::{Disposition, Finding, FindingKind, Location, Value};

const W15: &str = "http://schemas.microsoft.com/office/word/2012/wordml";

/// `docProps/core.xml`: author, dates, title, keywords…
pub(super) fn core(data: &[u8], location: &Location) -> Vec<Finding> {
    fields(data, location, |name| match name {
        "creator" | "lastModifiedBy" => FindingKind::PersonName,
        "created" | "modified" | "lastPrinted" => FindingKind::Timestamp,
        "title" | "subject" | "description" | "keywords" | "category" | "contentStatus" => {
            FindingKind::Description
        }
        "identifier" => FindingKind::DocumentId,
        _ => FindingKind::TechnicalInfo,
    })
}

/// `docProps/app.xml`: application, company, editing time, statistics…
pub(super) fn app(data: &[u8], location: &Location) -> Vec<Finding> {
    let mut statistics = 0;
    let mut findings = fields(data, location, |name| match name {
        "Application" | "AppVersion" => FindingKind::Software,
        "Company" => FindingKind::Organization,
        "Manager" => FindingKind::PersonName,
        "HyperlinkBase" => FindingKind::FilePath,
        "Template" => FindingKind::Software,
        _ => FindingKind::TechnicalInfo,
    });
    // Page counts, word counts and the like: one line for all of them.
    findings.retain(|f| {
        let is_statistic = f.kind == FindingKind::TechnicalInfo;
        statistics += usize::from(is_statistic);
        !is_statistic
    });
    for finding in &mut findings {
        // A template with a folder in its name reveals where it was stored.
        if let Some(Value::Text(text)) = &finding.value
            && finding.kind == FindingKind::Software
            && finding
                .location
                .parts()
                .last()
                .is_some_and(|p| p == "Template")
            && (text.contains('\\') || text.contains('/'))
        {
            finding.kind = FindingKind::FilePath;
            finding.risk = FindingKind::FilePath.risk();
        }
    }
    if statistics > 0 {
        findings.push(
            Finding::new(
                FindingKind::TechnicalInfo,
                location.clone(),
                Disposition::Remove,
            )
            .with_value(Value::Integer(statistics as i64)),
        );
    }
    findings
}

/// `docProps/custom.xml`: properties any program or person can add.
pub(super) fn custom(data: &[u8], location: &Location) -> Vec<Finding> {
    let Some(root) = root(data) else {
        return unreadable(data, location);
    };
    root.elements()
        .map(|property| {
            let name = property.attribute(&[""], "name").unwrap_or_default();
            Finding::new(
                FindingKind::CustomProperty,
                location.child(name),
                Disposition::Remove,
            )
            .with_value(Value::Text(property.text().trim().to_owned()))
        })
        .collect()
}

/// `word/people.xml`: everyone who edited or commented.
pub(super) fn people(data: &[u8], location: &Location) -> Vec<Finding> {
    let Some(root) = root(data) else {
        return unreadable(data, location);
    };
    root.elements()
        .filter_map(|person| person.attribute(&[W15], "author"))
        .map(|author| {
            Finding::new(
                FindingKind::PersonName,
                location.clone(),
                Disposition::Remove,
            )
            .with_value(Value::Text(author.to_owned()))
        })
        .collect()
}

/// One finding per non-empty child element of the root.
fn fields(data: &[u8], location: &Location, kind: impl Fn(&str) -> FindingKind) -> Vec<Finding> {
    let Some(root) = root(data) else {
        return unreadable(data, location);
    };
    root.elements()
        .filter_map(|field| {
            let text = field.text();
            let text = text.trim();
            (!text.is_empty()).then(|| {
                Finding::new(
                    kind(&field.local),
                    location.child(field.local.as_str()),
                    Disposition::Remove,
                )
                .with_value(Value::Text(text.to_owned()))
            })
        })
        .collect()
}

fn root(data: &[u8]) -> Option<Element> {
    xml::parse(data)
        .ok()?
        .into_iter()
        .find_map(|node| match node {
            Node::Element(element) => Some(element),
            Node::Other(_) => None,
        })
}

fn unreadable(data: &[u8], location: &Location) -> Vec<Finding> {
    vec![
        Finding::new(
            FindingKind::UnknownMetadata,
            location.clone(),
            Disposition::Remove,
        )
        .with_value(Value::Bytes(data.len() as u64)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(findings: &[Finding]) -> Vec<(FindingKind, String, Option<Value>)> {
        findings
            .iter()
            .map(|f| (f.kind, f.location.to_string(), f.value.clone()))
            .collect()
    }

    #[test]
    fn reads_core_properties() {
        let xml = br#"<cp:coreProperties xmlns:cp="urn:cp" xmlns:dc="urn:dc"><dc:creator>Jane Doe</dc:creator><dc:title/><dcterms:modified xmlns:dcterms="urn:t">2026-03-14T09:26:53Z</dcterms:modified><cp:revision>4</cp:revision></cp:coreProperties>"#;
        let findings = core(xml, &Location::new(["docProps/core.xml"]));
        assert_eq!(
            summary(&findings),
            [
                (
                    FindingKind::PersonName,
                    "docProps/core.xml > creator".into(),
                    Some(Value::Text("Jane Doe".into()))
                ),
                (
                    FindingKind::Timestamp,
                    "docProps/core.xml > modified".into(),
                    Some(Value::Text("2026-03-14T09:26:53Z".into()))
                ),
                (
                    FindingKind::TechnicalInfo,
                    "docProps/core.xml > revision".into(),
                    Some(Value::Text("4".into()))
                ),
            ]
        );
    }

    #[test]
    fn reads_app_properties_and_groups_statistics() {
        let xml = br#"<Properties><Template>C:\Users\jane\Templates\Report.dotx</Template><TotalTime>94</TotalTime><Pages>2</Pages><Company>Fictional Ltd</Company></Properties>"#;
        let findings = app(xml, &Location::new(["docProps/app.xml"]));
        let kinds: Vec<_> = findings.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            [
                FindingKind::FilePath,
                FindingKind::Organization,
                FindingKind::TechnicalInfo
            ]
        );
        assert_eq!(findings[2].value, Some(Value::Integer(2)));
    }

    #[test]
    fn reads_custom_properties_and_people() {
        let custom_xml = br#"<Properties><property name="Client"><vt:lpwstr xmlns:vt="urn:vt">Acme</vt:lpwstr></property></Properties>"#;
        let findings = custom(custom_xml, &Location::new(["docProps/custom.xml"]));
        assert_eq!(findings[0].kind, FindingKind::CustomProperty);
        assert_eq!(findings[0].value, Some(Value::Text("Acme".into())));

        let people_xml = format!(
            r#"<w15:people xmlns:w15="{W15}"><w15:person w15:author="Jane Doe"/></w15:people>"#
        );
        let findings = people(people_xml.as_bytes(), &Location::new(["word/people.xml"]));
        assert_eq!(findings[0].value, Some(Value::Text("Jane Doe".into())));
    }

    #[test]
    fn unreadable_parts_are_still_reported() {
        let findings = core(b"<broken", &Location::new(["docProps/core.xml"]));
        assert_eq!(findings[0].kind, FindingKind::UnknownMetadata);
    }
}
