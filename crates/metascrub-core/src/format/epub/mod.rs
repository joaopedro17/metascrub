//! EPUB e-books (versions 2 and 3): a ZIP package whose container file
//! points to the package document (OPF), which lists the book's metadata
//! and every file. Author, publisher, dates, identifiers and the extras
//! calibre and Apple Books add are removed; the title and language stay.
//! The book's unique identifier is replaced by one derived from the
//! cleaned book itself, so it reveals nothing, stays the same when the
//! book is cleaned again, and differs between books (reading apps use it
//! to tell books apart). Pictures are cleaned, and pages lose their
//! `<meta>` tags. Books with DRM are refused.

mod opf;
mod pages;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;

use sha1::Sha1;
use sha2::{Digest, Sha256};

use super::archive::{self, Entry};
use super::xml::{self, Element};
use super::{Depth, FormatError, FormatHandler, ReadSeek, clean_embedded, read_image};
use crate::model::{Disposition, Finding, FindingKind, Location, Value};
use crate::options::Options;

pub(super) const MIMETYPE: &str = "application/epub+zip";
const CONTAINER_PATH: &str = "META-INF/container.xml";
const ENCRYPTION_PATH: &str = "META-INF/encryption.xml";
const CONTAINER: &[&str] = &["urn:oasis:names:tc:opendocument:xmlns:container"];
const ENCRYPTION: &[&str] = &["http://www.w3.org/2001/04/xmlenc#"];
/// Font obfuscation, which isn't DRM: it only stops fonts from being
/// copied out of the book. Both use the book's identifier as the key.
const IDPF_FONTS: &str = "http://www.idpf.org/2008/embedding";
const ADOBE_FONTS: &str = "http://ns.adobe.com/pdf/enc#RC";
/// Stands in for the new identifier while the book is hashed to make it.
const PLACEHOLDER: &str = "urn:uuid:00000000-0000-8000-8000-000000000000";

#[derive(Debug)]
pub(super) struct Epub;

impl FormatHandler for Epub {
    fn inspect(
        &self,
        input: &mut dyn ReadSeek,
        options: &Options,
    ) -> Result<Vec<Finding>, FormatError> {
        let data = read_image(input)?;
        Ok(analyze(&data, options)?.findings)
    }

    fn clean(
        &self,
        input: &mut dyn ReadSeek,
        output: &mut dyn Write,
        options: &Options,
    ) -> Result<Vec<Finding>, FormatError> {
        let data = read_image(input)?;
        let analysis = analyze(&data, options)?;
        archive::write(&analysis.entries, output)?;
        Ok(analysis.findings)
    }
}

#[derive(Debug)]
struct Analysis {
    findings: Vec<Finding>,
    entries: Vec<Entry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Obfuscation {
    Idpf,
    Adobe,
}

fn analyze(data: &[u8], options: &Options) -> Result<Analysis, FormatError> {
    let _depth = Depth::enter()?;
    let archive = archive::read(data)?;
    let mut findings = archive.findings();
    let files: HashMap<&str, &[u8]> = archive
        .entries
        .iter()
        .map(|e| (e.name.as_str(), e.data.as_slice()))
        .collect();
    if !files.contains_key("mimetype") {
        return Err(FormatError::malformed("an EPUB without its mimetype"));
    }
    for drm in ["META-INF/rights.xml", "META-INF/signatures.xml"] {
        if files.contains_key(drm) {
            return Err(FormatError::Unsupported(
                "e-books with DRM or a digital signature".into(),
            ));
        }
    }
    let fonts = match files.get(ENCRYPTION_PATH) {
        Some(data) => obfuscated_fonts(data)?,
        None => HashMap::new(),
    };
    let container = files
        .get(CONTAINER_PATH)
        .ok_or_else(|| FormatError::malformed("an EPUB without its container file"))?;
    let opf_path = package_path(container)?;
    let opf_data = files
        .get(opf_path.as_str())
        .ok_or_else(|| FormatError::malformed("the EPUB's package document is missing"))?;

    let mut package = opf::Package::read(opf_data, &opf_path, options, &mut findings)?;
    let old_id = package.unique_id.clone();
    let adobe_key = package.adobe_key.clone();
    let listed: HashMap<String, opf::Item> = package
        .items
        .iter()
        .map(|item| (item.path.clone(), item.clone()))
        .collect();

    let mut cleaned: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut order = Vec::new();
    for entry in &archive.entries {
        let name = entry.name.as_str();
        let location = Location::new([name]);
        let keep_as_is = name == "mimetype"
            || name == CONTAINER_PATH
            || name == opf_path
            || (name == ENCRYPTION_PATH && !fonts.is_empty());
        if keep_as_is {
            order.push(name.to_owned());
            cleaned.insert(name.to_owned(), entry.data.clone());
            continue;
        }
        let Some(item) = listed.get(name) else {
            findings.push(
                Finding::new(FindingKind::UnknownMetadata, location, Disposition::Remove)
                    .with_value(Value::Bytes(entry.data.len() as u64)),
            );
            continue;
        };
        let data = if let Some(method) = fonts.get(name) {
            let key = key(*method, &old_id, adobe_key.as_deref())?;
            obfuscate(&entry.data, &key, *method)
        } else if item.media_type == "application/xhtml+xml" {
            pages::clean_page(&entry.data, &location, &mut findings)
                .unwrap_or_else(|| entry.data.clone())
        } else if item.media_type == "application/x-dtbncx+xml" {
            pages::clean_ncx(&entry.data, &location, options, &mut findings)?
        } else {
            match clean_embedded(&entry.data, options)? {
                Some(file) => {
                    findings.extend(file.findings.into_iter().map(|f| f.within(name)));
                    file.data
                }
                None => entry.data.clone(),
            }
        };
        order.push(name.to_owned());
        cleaned.insert(name.to_owned(), data);
    }

    // The new identifier is a hash of the cleaned book, with the
    // placeholder where the identifier goes and fonts as they are.
    let replacing = package.replaces_id();
    let opf = package.write(if replacing { PLACEHOLDER } else { &old_id })?;
    cleaned.insert(opf_path.clone(), opf);
    let new_id = if replacing {
        let id = content_id(&cleaned);
        for (name, data) in &mut cleaned {
            let is_ncx = listed
                .get(name.as_str())
                .is_some_and(|i| i.media_type == "application/x-dtbncx+xml");
            if *name == opf_path || is_ncx {
                *data = replace(data, PLACEHOLDER, &id);
            }
        }
        id
    } else {
        for (name, data) in &mut cleaned {
            if listed
                .get(name.as_str())
                .is_some_and(|i| i.media_type == "application/x-dtbncx+xml")
            {
                *data = replace(data, PLACEHOLDER, &old_id);
            }
        }
        old_id.clone()
    };
    let identifier = Location::new([opf_path.as_str(), "dc:identifier"]);
    if !replacing {
        findings.push(
            Finding::new(
                FindingKind::DocumentId,
                identifier,
                Disposition::Keep(crate::model::KeepReason::UserRequested),
            )
            .with_value(Value::Text(old_id.clone())),
        );
    } else if new_id != old_id {
        findings.push(
            Finding::new(FindingKind::DocumentId, identifier, Disposition::Normalize)
                .with_value(Value::Text(old_id.clone())),
        );
    }
    let new_adobe_key = adobe_uuid(&new_id).or(adobe_key);
    for (name, method) in &fonts {
        if let Some(data) = cleaned.get_mut(name.as_str()) {
            let key = key(*method, &new_id, new_adobe_key.as_deref())?;
            *data = obfuscate(data, &key, *method);
        }
    }

    // `mimetype` first, the rest in the book's order.
    let mut entries = Vec::with_capacity(order.len());
    let mut seen = HashSet::new();
    for name in std::iter::once("mimetype".to_owned()).chain(order) {
        if seen.insert(name.clone())
            && let Some(data) = cleaned.remove(&name)
        {
            entries.push(Entry { name, data });
        }
    }
    Ok(Analysis { findings, entries })
}

/// Where the package document is. Books with several renditions (a
/// fixed-layout and a reflowable version, say) aren't handled yet.
fn package_path(container: &[u8]) -> Result<String, FormatError> {
    let nodes = xml::parse(container)?;
    let mut paths = Vec::new();
    xml::visit_elements(&nodes, &mut |e: &Element| {
        if e.is(CONTAINER, "rootfile")
            && e.attribute(&[""], "media-type") == Some("application/oebps-package+xml")
        {
            paths.extend(e.attribute(&[""], "full-path").map(str::to_owned));
        }
    });
    match paths.as_slice() {
        [path] => Ok(path.clone()),
        [] => Err(FormatError::malformed("an EPUB without a package document")),
        _ => Err(FormatError::Unsupported(
            "e-books with several renditions".into(),
        )),
    }
}

/// The fonts `encryption.xml` lists, with how they're obfuscated. Anything
/// else in it is DRM.
fn obfuscated_fonts(data: &[u8]) -> Result<HashMap<String, Obfuscation>, FormatError> {
    let nodes = xml::parse(data)?;
    let mut fonts = HashMap::new();
    let mut drm = false;
    xml::visit_elements(&nodes, &mut |e: &Element| {
        if !e.is(ENCRYPTION, "EncryptedData") {
            return;
        }
        let method = e
            .find(ENCRYPTION, "EncryptionMethod")
            .and_then(|m| m.attribute(&[""], "Algorithm"));
        let uri = e
            .find(ENCRYPTION, "CipherReference")
            .and_then(|r| r.attribute(&[""], "URI"));
        match (method, uri) {
            (Some(IDPF_FONTS), Some(uri)) => {
                fonts.insert(opf::decode(uri), Obfuscation::Idpf);
            }
            (Some(ADOBE_FONTS), Some(uri)) => {
                fonts.insert(opf::decode(uri), Obfuscation::Adobe);
            }
            _ => drm = true,
        }
    });
    if drm {
        return Err(FormatError::Unsupported(
            "e-books with DRM or a digital signature".into(),
        ));
    }
    Ok(fonts)
}

fn key(method: Obfuscation, id: &str, adobe_uuid: Option<&str>) -> Result<Vec<u8>, FormatError> {
    match method {
        // SHA-1 of the identifier without white space.
        Obfuscation::Idpf => {
            let id: String = id
                .chars()
                .filter(|c| !matches!(c, ' ' | '\t' | '\r' | '\n'))
                .collect();
            Ok(Sha1::digest(id.as_bytes()).to_vec())
        }
        // The 16 bytes of a `urn:uuid:` identifier.
        Obfuscation::Adobe => {
            let hex: String = adobe_uuid
                .ok_or_else(|| FormatError::malformed("obfuscated fonts without their key"))?
                .chars()
                .filter(char::is_ascii_hexdigit)
                .collect();
            (0..hex.len() / 2)
                .map(|i| u8::from_str_radix(hex.get(2 * i..2 * i + 2).unwrap_or("x"), 16))
                .collect::<Result<Vec<u8>, _>>()
                .ok()
                .filter(|k| k.len() == 16)
                .ok_or_else(|| FormatError::malformed("obfuscated fonts without their key"))
        }
    }
}

/// XORs the start of a font with the key; doing it twice gives the font back.
fn obfuscate(data: &[u8], key: &[u8], method: Obfuscation) -> Vec<u8> {
    let len = match method {
        Obfuscation::Idpf => 1040,
        Obfuscation::Adobe => 1024,
    };
    data.iter()
        .zip(key.iter().cycle())
        .enumerate()
        .map(|(i, (byte, k))| if i < len { byte ^ k } else { *byte })
        .collect()
}

/// The UUID in a `urn:uuid:` identifier, for Adobe's font obfuscation.
fn adobe_uuid(id: &str) -> Option<String> {
    id.strip_prefix("urn:uuid:").map(str::to_owned)
}

/// A UUID (RFC 9562 version 8) from SHA-256 of every file, by name.
fn content_id(files: &BTreeMap<String, Vec<u8>>) -> String {
    let mut hash = Sha256::new();
    for (name, data) in files {
        hash.update(name.as_bytes());
        hash.update([0]);
        hash.update((data.len() as u64).to_le_bytes());
        hash.update(data);
    }
    let digest = hash.finalize();
    let mut bytes: Vec<u8> = digest.iter().take(16).copied().collect();
    if let Some(b) = bytes.get_mut(6) {
        *b = (*b & 0x0F) | 0x80;
    }
    if let Some(b) = bytes.get_mut(8) {
        *b = (*b & 0x3F) | 0x80;
    }
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let part = |range: std::ops::Range<usize>| hex.get(range).unwrap_or_default();
    format!(
        "urn:uuid:{}-{}-{}-{}-{}",
        part(0..8),
        part(8..12),
        part(12..16),
        part(16..20),
        part(20..32)
    )
}

fn replace(data: &[u8], from: &str, to: &str) -> Vec<u8> {
    match std::str::from_utf8(data) {
        Ok(text) => text.replace(from, to).into_bytes(),
        Err(_) => data.to_vec(),
    }
}

#[cfg(test)]
mod tests;
