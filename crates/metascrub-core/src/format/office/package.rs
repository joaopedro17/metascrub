//! The Open Packaging Conventions layer: which content type each part has,
//! and the relationships that link parts together.

use quick_xml::escape::escape;

use crate::format::FormatError;
use crate::format::xml::{self, Node};

pub(super) const CONTENT_TYPES: &str = "[Content_Types].xml";
pub(super) const PACKAGE_RELS: &str = "_rels/.rels";

const CONTENT_TYPES_NS: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
const RELATIONSHIPS_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const DECLARATION: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n";

#[derive(Debug, Clone, Default)]
pub(super) struct ContentTypes {
    /// File extension → content type.
    defaults: Vec<(String, String)>,
    /// Part name (with its leading `/`) → content type.
    overrides: Vec<(String, String)>,
}

impl ContentTypes {
    pub(super) fn parse(data: &[u8]) -> Result<Self, FormatError> {
        let nodes = xml::parse(data)?;
        let types = root(&nodes, CONTENT_TYPES_NS, "Types")?;
        let mut content_types = Self::default();
        for element in types.elements() {
            let attribute = |name| {
                element
                    .attribute(&[""], name)
                    .unwrap_or_default()
                    .to_owned()
            };
            match element.local.as_str() {
                "Default" => content_types
                    .defaults
                    .push((attribute("Extension"), attribute("ContentType"))),
                "Override" => content_types
                    .overrides
                    .push((attribute("PartName"), attribute("ContentType"))),
                _ => {}
            }
        }
        Ok(content_types)
    }

    /// `part` is a ZIP entry name, without the leading `/`.
    pub(super) fn of(&self, part: &str) -> Option<&str> {
        let found = self
            .overrides
            .iter()
            .find(|(name, _)| name.trim_start_matches('/').eq_ignore_ascii_case(part));
        if let Some((_, content_type)) = found {
            return Some(content_type);
        }
        let extension = part.rsplit_once('.').map(|(_, e)| e)?;
        self.defaults
            .iter()
            .find(|(e, _)| e.eq_ignore_ascii_case(extension))
            .map(|(_, content_type)| content_type.as_str())
    }

    /// Keeps only the overrides for parts `keep` accepts.
    pub(super) fn retain_parts(&mut self, keep: impl Fn(&str) -> bool) {
        self.overrides
            .retain(|(name, _)| keep(name.trim_start_matches('/')));
    }

    pub(super) fn to_xml(&self) -> Vec<u8> {
        let mut out = format!("{DECLARATION}<Types xmlns=\"{CONTENT_TYPES_NS}\">");
        for (extension, content_type) in &self.defaults {
            out.push_str(&format!(
                "<Default Extension=\"{}\" ContentType=\"{}\"/>",
                escape(extension.as_str()),
                escape(content_type.as_str())
            ));
        }
        for (part, content_type) in &self.overrides {
            out.push_str(&format!(
                "<Override PartName=\"{}\" ContentType=\"{}\"/>",
                escape(part.as_str()),
                escape(content_type.as_str())
            ));
        }
        out.push_str("</Types>");
        out.into_bytes()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Relationship {
    pub(super) id: String,
    pub(super) kind: String,
    pub(super) target: String,
    pub(super) external: bool,
}

impl Relationship {
    /// The last segment of the type URI, the same in every schema version:
    /// `…/relationships/image` → `image`.
    pub(super) fn short_kind(&self) -> &str {
        self.kind.rsplit('/').next().unwrap_or_default()
    }
}

pub(super) fn parse_relationships(data: &[u8]) -> Result<Vec<Relationship>, FormatError> {
    let nodes = xml::parse(data)?;
    let relationships = root(&nodes, RELATIONSHIPS_NS, "Relationships")?;
    Ok(relationships
        .elements()
        .filter(|e| e.local == "Relationship")
        .map(|e| {
            let attribute = |name| e.attribute(&[""], name).unwrap_or_default().to_owned();
            Relationship {
                id: attribute("Id"),
                kind: attribute("Type"),
                target: attribute("Target"),
                external: attribute("TargetMode") == "External",
            }
        })
        .collect())
}

pub(super) fn relationships_xml(relationships: &[Relationship]) -> Vec<u8> {
    let mut out = format!("{DECLARATION}<Relationships xmlns=\"{RELATIONSHIPS_NS}\">");
    for r in relationships {
        out.push_str(&format!(
            "<Relationship Id=\"{}\" Type=\"{}\" Target=\"{}\"{}/>",
            escape(r.id.as_str()),
            escape(r.kind.as_str()),
            escape(r.target.as_str()),
            if r.external {
                " TargetMode=\"External\""
            } else {
                ""
            }
        ));
    }
    out.push_str("</Relationships>");
    out.into_bytes()
}

fn root<'a>(nodes: &'a [Node], ns: &str, local: &str) -> Result<&'a xml::Element, FormatError> {
    nodes
        .iter()
        .find_map(|node| match node {
            Node::Element(e) if e.is(&[ns], local) => Some(e),
            _ => None,
        })
        .ok_or_else(|| FormatError::malformed(format!("missing <{local}>")))
}

/// Where the relationships of `part` are stored: `word/document.xml` →
/// `word/_rels/document.xml.rels`; the package's own are in `_rels/.rels`.
pub(super) fn relationships_part(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((dir, name)) => format!("{dir}/_rels/{name}.rels"),
        None if part.is_empty() => PACKAGE_RELS.to_owned(),
        None => format!("_rels/{part}.rels"),
    }
}

/// The part a relationship of `source` points to, as a ZIP entry name.
/// `None` if the target leaves the package.
pub(super) fn resolve(source: &str, target: &str) -> Option<String> {
    let target = percent_decode(target.split('#').next().unwrap_or_default());
    let mut parts: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        let dir = source.rsplit_once('/').map_or("", |(dir, _)| dir);
        dir.split('/').filter(|p| !p.is_empty()).collect()
    };
    for segment in target.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            segment => parts.push(segment),
        }
    }
    Some(parts.join("/")).filter(|p| !p.is_empty())
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&byte) = bytes.get(i) {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (byte, hex) {
            (b'%', Some(decoded)) => {
                out.push(decoded);
                i += 3;
            }
            _ => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_targets_relative_to_the_source() {
        assert_eq!(
            resolve("word/document.xml", "media/image1.png").as_deref(),
            Some("word/media/image1.png")
        );
        assert_eq!(
            resolve("word/document.xml", "../customXml/item1.xml").as_deref(),
            Some("customXml/item1.xml")
        );
        assert_eq!(
            resolve("", "word/document.xml").as_deref(),
            Some("word/document.xml")
        );
        assert_eq!(
            resolve("xl/workbook.xml", "/xl/styles.xml").as_deref(),
            Some("xl/styles.xml")
        );
        assert_eq!(
            resolve("a.xml", "image%201.png").as_deref(),
            Some("image 1.png")
        );
        assert_eq!(resolve("a.xml", "../../x"), None);
    }

    #[test]
    fn names_the_relationships_part() {
        assert_eq!(relationships_part(""), "_rels/.rels");
        assert_eq!(
            relationships_part("word/document.xml"),
            "word/_rels/document.xml.rels"
        );
    }

    #[test]
    fn content_types_round_trip() {
        let xml = br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="doc+xml"/></Types>"#;
        let mut types = ContentTypes::parse(xml).unwrap();
        assert_eq!(types.of("word/document.xml"), Some("doc+xml"));
        assert_eq!(types.of("word/styles.XML"), Some("application/xml"));
        assert_eq!(types.of("word/image.png"), None);
        types.retain_parts(|part| part != "word/document.xml");
        let again = ContentTypes::parse(&types.to_xml()).unwrap();
        assert_eq!(again.of("word/document.xml"), Some("application/xml"));
    }

    #[test]
    fn relationships_round_trip() {
        let relationships = vec![Relationship {
            id: "rId1".into(),
            kind: "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink"
                .into(),
            target: "https://example.com/?a=1&b=2".into(),
            external: true,
        }];
        let parsed = parse_relationships(&relationships_xml(&relationships)).unwrap();
        assert_eq!(parsed, relationships);
        assert_eq!(parsed[0].short_kind(), "hyperlink");
    }
}
