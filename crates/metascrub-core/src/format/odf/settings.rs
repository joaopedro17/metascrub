//! `settings.xml`: how the document was last viewed, which printer it was
//! set up for, and document options. The file isn't removed: its
//! configuration settings hold layout options (documents converted from
//! Word depend on them). View settings, the printer and its driver data,
//! editing-session IDs and file paths go.

use crate::format::FormatError;
use crate::format::xml::{self, Element};
use crate::model::{Disposition, Finding, FindingKind, Location, Value};

const CONFIG: &[&str] = &["urn:oasis:names:tc:opendocument:xmlns:config:1.0"];
/// The set of options the layout depends on.
const CONFIGURATION: &str = "ooo:configuration-settings";

/// The reduced file, or `None` when nothing in it had to go.
pub(super) fn reduce(
    data: &[u8],
    location: &Location,
    findings: &mut Vec<Finding>,
) -> Result<Option<Vec<u8>>, FormatError> {
    let mut nodes = xml::parse(data)?;
    let mut found = Vec::new();
    let removed = xml::remove_elements(&mut nodes, &mut |e: &Element| {
        is_top_set(e) && e.attribute(CONFIG, "name") != Some(CONFIGURATION)
    });
    for set in &removed {
        let name = set.attribute(CONFIG, "name").unwrap_or_default();
        let kind = if name == "ooo:view-settings" {
            FindingKind::TechnicalInfo
        } else {
            FindingKind::UnknownMetadata
        };
        found.push(Finding::new(
            kind,
            location.child(name),
            Disposition::Remove,
        ));
    }
    let items = xml::remove_elements(&mut nodes, &mut |e: &Element| {
        e.is(CONFIG, "config-item") && item_kind(e).is_some()
    });
    for item in &items {
        let name = item.attribute(CONFIG, "name").unwrap_or_default();
        let Some(kind) = item_kind(item) else {
            continue;
        };
        let text = item.text();
        let value = if item.attribute(CONFIG, "type") == Some("base64Binary") {
            Value::Bytes(text.trim().len() as u64 * 3 / 4)
        } else {
            Value::Text(text.trim().to_owned())
        };
        found.push(Finding::new(kind, location.child(name), Disposition::Remove).with_value(value));
    }
    if removed.is_empty() && items.is_empty() {
        return Ok(None);
    }
    findings.extend(found);
    Ok(Some(xml::write(&nodes)?))
}

/// One of the sets directly inside `office:settings`, all named `ooo:…`.
fn is_top_set(e: &Element) -> bool {
    e.is(CONFIG, "config-item-set")
        && e.attribute(CONFIG, "name")
            .is_some_and(|n| n.starts_with("ooo:"))
}

/// What a removed configuration item reveals; `None` for one that stays.
fn item_kind(e: &Element) -> Option<FindingKind> {
    let name = e.attribute(CONFIG, "name").unwrap_or_default();
    let text = e.text();
    let text = text.trim();
    match name {
        "PrinterName" | "PrinterSetup" | "PrintFaxName" if !text.is_empty() => {
            Some(FindingKind::DeviceModel)
        }
        "Rsid" | "RsidRoot" => Some(FindingKind::DocumentId),
        "CurrentDatabaseDataSource" | "CurrentDatabaseCommand" if !text.is_empty() => {
            Some(FindingKind::FilePath)
        }
        _ if looks_like_path(text) => Some(FindingKind::FilePath),
        _ => None,
    }
}

/// `file:///home/jane/…`, `C:\Users\…`, `\\server\share`, `/Users/…`.
fn looks_like_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    let drive = matches!(bytes, [letter, b':', b'\\' | b'/', ..] if letter.is_ascii_alphabetic());
    text.starts_with("file:")
        || text.starts_with("\\\\")
        || drive
        || ["/home/", "/Users/", "/media/", "/mnt/"]
            .iter()
            .any(|p| text.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SETTINGS: &str = r#"<office:document-settings xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:config="urn:oasis:names:tc:opendocument:xmlns:config:1.0"><office:settings><config:config-item-set config:name="ooo:view-settings"><config:config-item config:name="ViewAreaTop" config:type="long">0</config:config-item></config:config-item-set><config:config-item-set config:name="ooo:configuration-settings"><config:config-item config:name="PrinterIndependentLayout" config:type="string">high-resolution</config:config-item><config:config-item config:name="PrinterName" config:type="string">HP LaserJet in Jane's office</config:config-item><config:config-item config:name="PrinterSetup" config:type="base64Binary">AAAA</config:config-item><config:config-item config:name="RsidRoot" config:type="int">138066</config:config-item><config:config-item config:name="CurrentDatabaseDataSource" config:type="string"/><config:config-item config:name="LoadReadonly" config:type="boolean">false</config:config-item></config:config-item-set></office:settings></office:document-settings>"#;

    #[test]
    fn keeps_layout_options_only() {
        let mut findings = Vec::new();
        let reduced = reduce(
            SETTINGS.as_bytes(),
            &Location::new(["settings.xml"]),
            &mut findings,
        )
        .unwrap()
        .unwrap();
        let reduced = String::from_utf8(reduced).unwrap();
        assert!(reduced.contains("PrinterIndependentLayout"));
        assert!(reduced.contains("LoadReadonly"));
        assert!(reduced.contains("CurrentDatabaseDataSource"));
        for gone in ["view-settings", "PrinterName", "PrinterSetup", "RsidRoot"] {
            assert!(!reduced.contains(gone), "{gone}");
        }
        let kinds: Vec<_> = findings.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            [
                FindingKind::TechnicalInfo,
                FindingKind::DeviceModel,
                FindingKind::DeviceModel,
                FindingKind::DocumentId
            ]
        );
        let mut again = Vec::new();
        assert!(
            reduce(reduced.as_bytes(), &Location::new(["s"]), &mut again)
                .unwrap()
                .is_none()
        );
        assert!(again.is_empty());
    }

    #[test]
    fn recognizes_paths() {
        for path in [
            "file:///home/jane/a.odt",
            "C:\\Users\\jane",
            "/Users/jane/x",
        ] {
            assert!(looks_like_path(path), "{path}");
        }
        for text in ["high-resolution", "false", "0", "1/2"] {
            assert!(!looks_like_path(text), "{text}");
        }
    }
}
