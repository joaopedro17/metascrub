//! The package document (OPF): the book's metadata, and the list of its
//! files. The title and language stay; author, publisher, dates, other
//! identifiers and descriptions go (author, publisher and ISBN stay with
//! `--keep book-info`). What reading apps need to show the book (the
//! cover, fixed-layout and media-overlay settings, accessibility details)
//! stays too.

use std::collections::HashSet;

use crate::format::FormatError;
use crate::format::xml::{self, Element, Node};
use crate::model::{Disposition, Finding, FindingKind, KeepReason, Location, Value};
use crate::options::Options;

const OPF: &[&str] = &["http://www.idpf.org/2007/opf"];
const DC: &[&str] = &["http://purl.org/dc/elements/1.1/"];
/// The date every cleaned book was last modified (EPUB 3 requires one).
const NEUTRAL_MODIFIED: &str = "1980-01-01T00:00:00Z";
/// `meta` properties reading apps need to display the book.
const DISPLAY: &[&str] = &[
    "rendition:",
    "media:",
    "schema:access",
    "ibooks:specified-fonts",
];

#[derive(Debug, Clone)]
pub(super) struct Item {
    /// The file's name in the ZIP.
    pub(super) path: String,
    pub(super) media_type: String,
}

#[derive(Debug)]
pub(super) struct Package {
    nodes: Vec<Node>,
    /// The identifier that names this book, before cleaning.
    pub(super) unique_id: String,
    unique_ref: String,
    /// A `urn:uuid:` identifier, the key of Adobe's font obfuscation.
    pub(super) adobe_key: Option<String>,
    pub(super) items: Vec<Item>,
    /// Kept as an ISBN with `--keep book-info`.
    keeps_id: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Keep,
    /// Kept because the user asked.
    KeepAsked(FindingKind),
    Remove(FindingKind),
    /// Follows the element it refines.
    Refines,
}

impl Package {
    pub(super) fn read(
        data: &[u8],
        path: &str,
        options: &Options,
        findings: &mut Vec<Finding>,
    ) -> Result<Self, FormatError> {
        let mut nodes = xml::parse(data)?;
        let location = Location::new([path]);
        let dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
        let mut unique_ref = String::new();
        let mut items = Vec::new();
        xml::visit_elements(&nodes, &mut |e: &Element| {
            if e.is(OPF, "package") {
                unique_ref = e
                    .attribute(&[""], "unique-identifier")
                    .unwrap_or_default()
                    .to_owned();
            }
            if e.is(OPF, "item")
                && let Some(href) = e.attribute(&[""], "href")
            {
                items.push(Item {
                    path: resolve(dir, href),
                    media_type: e
                        .attribute(&[""], "media-type")
                        .unwrap_or_default()
                        .to_owned(),
                });
            }
        });
        let mut unique_id = None;
        let mut adobe_key = None;
        xml::visit_elements(&nodes, &mut |e: &Element| {
            if e.is(DC, "identifier") {
                let text = e.text().trim().to_owned();
                if e.attribute(&[""], "id") == Some(unique_ref.as_str()) {
                    unique_id = Some(text.clone());
                }
                if adobe_key.is_none() && text.starts_with("urn:uuid:") {
                    adobe_key = text.strip_prefix("urn:uuid:").map(str::to_owned);
                }
            }
        });
        let unique_id = unique_id
            .ok_or_else(|| FormatError::malformed("an EPUB without its unique identifier"))?;
        let keeps_id = options.keep_book_info && is_isbn(&unique_id);

        // Which elements go, by `id`, so refinements can follow them.
        let mut gone = HashSet::new();
        xml::visit_elements(&nodes, &mut |e: &Element| {
            if let (Action::Remove(_), Some(id)) =
                (action(e, &unique_ref, options), e.attribute(&[""], "id"))
            {
                gone.insert(id.to_owned());
            }
        });
        if !keeps_id {
            gone.insert(unique_ref.clone());
        }
        let decide = |e: &Element| -> Action {
            match action(e, &unique_ref, options) {
                Action::Refines => {
                    let target = e
                        .attribute(&[""], "refines")
                        .unwrap_or_default()
                        .trim_start_matches('#');
                    if gone.contains(target) {
                        Action::Remove(FindingKind::Description)
                    } else {
                        Action::Keep
                    }
                }
                other => other,
            }
        };
        let mut metadata = None;
        xml::visit_elements(&nodes, &mut |e: &Element| {
            if metadata.is_none() && e.is(OPF, "metadata") {
                metadata = Some(e);
            }
        });
        let Some(metadata) = metadata else {
            return Err(FormatError::malformed("an EPUB without metadata"));
        };
        xml::visit_elements(&metadata.children, &mut |e: &Element| {
            let refines = e.attribute(&[""], "refines").is_some();
            let (kind, disposition) = match decide(e) {
                Action::Keep | Action::Refines => return,
                Action::KeepAsked(kind) => (kind, Disposition::Keep(KeepReason::UserRequested)),
                // A refinement says more about something already reported.
                Action::Remove(_) if refines => return,
                Action::Remove(kind) => (kind, Disposition::Remove),
            };
            findings.push(
                Finding::new(kind, location.child(name(e)), disposition)
                    .with_value(Value::Text(value(e))),
            );
        });

        let mut modified = None;
        xml::for_each_element(&mut nodes, &mut |e| {
            if e.is(OPF, "metadata") {
                xml::remove_elements(&mut e.children, &mut |c: &Element| {
                    matches!(decide(c), Action::Remove(_))
                });
            }
            if e.is(OPF, "meta") && e.attribute(&[""], "property") == Some("dcterms:modified") {
                let old = e.text().trim().to_owned();
                if old != NEUTRAL_MODIFIED {
                    e.set_text(NEUTRAL_MODIFIED);
                    modified = Some(old);
                }
            }
        });
        if let Some(old) = modified {
            findings.push(
                Finding::new(
                    FindingKind::Timestamp,
                    location.child("dcterms:modified"),
                    Disposition::Normalize,
                )
                .with_value(Value::Text(old)),
            );
        }
        Ok(Self {
            nodes,
            unique_id,
            unique_ref,
            adobe_key,
            items,
            keeps_id,
        })
    }

    pub(super) fn replaces_id(&self) -> bool {
        !self.keeps_id
    }

    /// The package document with `id` as the book's identifier.
    pub(super) fn write(&mut self, id: &str) -> Result<Vec<u8>, FormatError> {
        let unique_ref = self.unique_ref.clone();
        let keeps_id = self.keeps_id;
        xml::for_each_element(&mut self.nodes, &mut |e| {
            if !keeps_id
                && e.is(DC, "identifier")
                && e.attribute(&[""], "id") == Some(unique_ref.as_str())
            {
                e.set_text(id);
                // An EPUB 2 scheme like `ISBN` no longer fits.
                e.remove_attributes(|ns, local, _| OPF.contains(&ns) && local == "scheme");
            }
        });
        xml::write(&self.nodes)
    }
}

/// What happens to a metadata element, before refinements are resolved.
fn action(e: &Element, unique_ref: &str, options: &Options) -> Action {
    use FindingKind::*;
    let keep = options.keep_book_info;
    let asked = |kind| {
        if keep {
            Action::KeepAsked(kind)
        } else {
            Action::Remove(kind)
        }
    };
    if DC.contains(&e.ns.as_str()) {
        return match e.local.as_str() {
            "title" | "language" => Action::Keep,
            "identifier" if e.attribute(&[""], "id") == Some(unique_ref) => Action::Keep,
            "identifier" if is_isbn(&e.text()) => asked(DocumentId),
            "identifier" => Action::Remove(DocumentId),
            "creator" => asked(PersonName),
            "publisher" => asked(Organization),
            "contributor" if is_software(&e.text()) => Action::Remove(Software),
            "contributor" | "rights" => Action::Remove(PersonName),
            "date" => Action::Remove(Timestamp),
            _ => Action::Remove(Description),
        };
    }
    if e.is(OPF, "meta") {
        if e.attribute(&[""], "refines").is_some() {
            return Action::Refines;
        }
        let property = e.attribute(&[""], "property").unwrap_or_default();
        let name = e.attribute(&[""], "name").unwrap_or_default();
        if property == "dcterms:modified"
            || name == "cover"
            || DISPLAY.iter().any(|p| property.starts_with(p))
        {
            return Action::Keep;
        }
        let label = format!("{property}{name}").to_ascii_lowercase();
        return Action::Remove(if label.contains("date") || label.contains("timestamp") {
            Timestamp
        } else if label.contains("author") || label.contains("creator") {
            PersonName
        } else if label.contains("generator") || label.contains("version") {
            Software
        } else {
            CustomProperty
        });
    }
    if e.is(OPF, "link") {
        return Action::Remove(UnknownMetadata);
    }
    Action::Keep
}

fn name(e: &Element) -> String {
    if DC.contains(&e.ns.as_str()) {
        return format!("dc:{}", e.local);
    }
    let named = e
        .attribute(&[""], "property")
        .or_else(|| e.attribute(&[""], "name"))
        .or_else(|| e.attribute(&[""], "rel"))
        .unwrap_or_default();
    format!("{} {named}", e.local)
}

fn value(e: &Element) -> String {
    let text = e.text().trim().to_owned();
    if text.is_empty() {
        e.attribute(&[""], "content")
            .or_else(|| e.attribute(&[""], "href"))
            .unwrap_or_default()
            .to_owned()
    } else {
        text
    }
}

/// `urn:isbn:9780000000000`, `ISBN 0-00-000000-0`, or the bare number.
fn is_isbn(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    let rest = lower
        .trim_start_matches("urn:isbn:")
        .trim_start_matches("isbn")
        .trim_start_matches([':', ' ']);
    let digits: Vec<char> = rest.chars().filter(|c| !matches!(c, '-' | ' ')).collect();
    matches!(digits.len(), 10 | 13)
        && digits
            .iter()
            .enumerate()
            .all(|(i, c)| c.is_ascii_digit() || (i == 9 && *c == 'x'))
}

/// Contributors that are the programs that made the book.
fn is_software(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "calibre",
        "sigil",
        "pandoc",
        "indesign",
        "pages",
        "vellum",
        "ebookmaker",
    ]
    .iter()
    .any(|s| lower.contains(s))
}

/// A path in the book from a URI: percent-decoded, without a fragment.
pub(super) fn decode(uri: &str) -> String {
    let uri = uri.split('#').next().unwrap_or_default();
    let bytes = uri.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (b, hex) {
            (b'%', Some(value)) => {
                out.push(value);
                i += 3;
            }
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `href` relative to the folder `dir`, as a ZIP entry name.
fn resolve(dir: &str, href: &str) -> String {
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    let decoded = decode(href);
    for part in decoded.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_isbns() {
        for isbn in [
            "urn:isbn:9780141036144",
            "ISBN 0-14-103614-2",
            "978-0-14-103614-4",
            "014103614X",
        ] {
            assert!(is_isbn(isbn), "{isbn}");
        }
        for other in ["urn:uuid:1234", "calibre:1234", "12345"] {
            assert!(!is_isbn(other), "{other}");
        }
    }

    #[test]
    fn resolves_paths_against_the_package_folder() {
        assert_eq!(
            resolve("OEBPS", "Text/ch%201.xhtml#p2"),
            "OEBPS/Text/ch 1.xhtml"
        );
        assert_eq!(
            resolve("OEBPS/content", "../Images/a.jpg"),
            "OEBPS/Images/a.jpg"
        );
        assert_eq!(resolve("", "cover.jpg"), "cover.jpg");
    }
}
