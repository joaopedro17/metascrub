//! OpenDocument files (ODT, ODS, ODP, ODG), as LibreOffice writes them: a
//! ZIP package whose `mimetype` entry names the kind of document and whose
//! manifest lists every file. Document properties (`meta.xml`), the
//! thumbnail and leftovers go; `settings.xml` keeps only the options the
//! layout depends on; `content.xml` and `styles.xml` lose comments and
//! editing-session IDs. Pictures and embedded files are cleaned with their
//! own rules. Signed or encrypted files, and files with tracked changes,
//! are left alone.

mod content;
mod meta;
mod settings;

use std::collections::{BTreeMap, HashSet};
use std::io::Write;

use super::archive::{self, Entry};
use super::xml::{self, Element};
use super::{Depth, FormatError, FormatHandler, ReadSeek, clean_embedded, read_image};
use crate::model::{Disposition, Finding, FindingKind, FormatId, Location, Value};
use crate::options::Options;

pub(super) const MANIFEST: &[&str] = &["urn:oasis:names:tc:opendocument:xmlns:manifest:1.0"];
const MANIFEST_PATH: &str = "META-INF/manifest.xml";

/// The kind of document a `mimetype` entry names; templates count too.
pub(super) fn format(mimetype: &str) -> Option<FormatId> {
    let kind = mimetype.strip_prefix("application/vnd.oasis.opendocument.")?;
    Some(match kind.strip_suffix("-template").unwrap_or(kind) {
        "text" => FormatId::Odt,
        "spreadsheet" => FormatId::Ods,
        "presentation" => FormatId::Odp,
        "graphics" => FormatId::Odg,
        _ => return None,
    })
}

#[derive(Debug)]
pub(super) struct Odf;

impl FormatHandler for Odf {
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

/// What happens to one file of the package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    /// Document properties: removed, read for the report.
    Meta,
    /// Reduced to the options the layout depends on.
    Settings,
    /// The document itself: comments and editing-session IDs removed.
    Content,
    /// Package metadata in RDF; LibreOffice writes only the file's type.
    Rdf,
    Thumbnail,
    /// A cache LibreOffice rebuilds when it opens the file.
    LayoutCache,
    /// Toolbar and menu settings, kept as they are.
    Configuration,
    /// Basic macros and scripts: kept, reported.
    Macros,
    /// Anything else: cleaned if MetaScrub can read it, else kept.
    File,
}

fn part(name: &str, documents: &HashSet<String>) -> Part {
    let (dir, base) = match name.rsplit_once('/') {
        Some((dir, base)) => (format!("{dir}/"), base),
        None => (String::new(), name),
    };
    let top = name.split('/').next().unwrap_or_default();
    if documents.contains(&dir) {
        match base {
            "meta.xml" => return Part::Meta,
            "settings.xml" => return Part::Settings,
            "content.xml" | "styles.xml" => return Part::Content,
            "manifest.rdf" => return Part::Rdf,
            "layout-cache" => return Part::LayoutCache,
            _ => {}
        }
    }
    match top {
        "Thumbnails" => Part::Thumbnail,
        "Configurations2" => Part::Configuration,
        "Basic" | "Scripts" => Part::Macros,
        _ => Part::File,
    }
}

fn analyze(data: &[u8], options: &Options) -> Result<Analysis, FormatError> {
    let _depth = Depth::enter()?;
    let archive = archive::read(data)?;
    let mut findings = archive.findings();
    let entry = |name: &str| archive.entries.iter().find(|e| e.name == name);
    let mimetype = entry("mimetype")
        .ok_or_else(|| FormatError::malformed("an OpenDocument file without its mimetype"))?;
    let manifest = entry(MANIFEST_PATH)
        .ok_or_else(|| FormatError::malformed("an OpenDocument file without its manifest"))?;
    if archive.entries.iter().any(|e| {
        e.name.starts_with("META-INF/") && e.name.to_ascii_lowercase().contains("signatures")
    }) {
        return Err(FormatError::Unsupported(
            "digitally signed OpenDocument files: cleaning would break the signature".into(),
        ));
    }
    let listed = manifest_paths(&manifest.data)?;
    // The package itself and embedded documents (`Object 1/`, a chart).
    let documents: HashSet<String> = listed
        .iter()
        .filter(|(path, media_type)| {
            (path.as_str() == "/" || path.ends_with('/')) && format_or_part(media_type)
        })
        .map(|(path, _)| {
            if path == "/" {
                String::new()
            } else {
                path.clone()
            }
        })
        .collect();
    let is_listed = |name: &str| {
        listed.contains_key(name)
            || listed
                .keys()
                .any(|dir| dir.ends_with('/') && dir != "/" && name.starts_with(dir.as_str()))
    };

    let mut removed = HashSet::new();
    let mut entries = vec![Entry {
        name: "mimetype".to_owned(),
        data: mimetype.data.clone(),
    }];
    let mut macros = BTreeMap::new();
    for entry in &archive.entries {
        let name = entry.name.as_str();
        if name == "mimetype" || name == MANIFEST_PATH {
            continue;
        }
        let location = Location::new([name]);
        let whole = |kind| {
            Finding::new(kind, location.clone(), Disposition::Remove)
                .with_value(Value::Bytes(entry.data.len() as u64))
        };
        if name.starts_with("META-INF/") || !is_listed(name) {
            findings.push(whole(FindingKind::UnknownMetadata));
            removed.insert(name.to_owned());
            continue;
        }
        let data = match part(name, &documents) {
            Part::Meta => {
                findings.extend(meta::findings(&entry.data, &location));
                removed.insert(name.to_owned());
                continue;
            }
            Part::Thumbnail => {
                findings.push(whole(FindingKind::Thumbnail));
                removed.insert(name.to_owned());
                continue;
            }
            Part::LayoutCache => {
                findings.push(whole(FindingKind::TechnicalInfo));
                removed.insert(name.to_owned());
                continue;
            }
            Part::Rdf => {
                if !is_default_rdf(&entry.data) {
                    findings.push(whole(FindingKind::UnknownMetadata));
                }
                removed.insert(name.to_owned());
                continue;
            }
            Part::Settings => settings::reduce(&entry.data, &location, &mut findings)?,
            Part::Content => content::clean(&entry.data, &location, options, &mut findings)?,
            Part::Configuration => None,
            Part::Macros => {
                let top = name.split('/').next().unwrap_or_default().to_owned();
                *macros.entry(top).or_insert(0) += entry.data.len() as u64;
                None
            }
            Part::File => match clean_embedded(&entry.data, options)? {
                Some(cleaned) => {
                    findings.extend(cleaned.findings.into_iter().map(|f| f.within(name)));
                    Some(cleaned.data)
                }
                None if is_embedded_object(name, &listed) => {
                    findings.push(
                        Finding::new(FindingKind::EmbeddedFile, location, Disposition::Warn)
                            .with_value(Value::Bytes(entry.data.len() as u64)),
                    );
                    None
                }
                None => None,
            },
        };
        entries.push(Entry {
            name: entry.name.clone(),
            data: data.unwrap_or_else(|| entry.data.clone()),
        });
    }
    for (dir, len) in macros {
        findings.push(
            Finding::new(FindingKind::Macros, Location::new([dir]), Disposition::Warn)
                .with_value(Value::Bytes(len)),
        );
    }
    let kept: HashSet<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    entries.push(Entry {
        name: MANIFEST_PATH.to_owned(),
        data: manifest_without(&manifest.data, &removed, &kept)?,
    });
    Ok(Analysis { findings, entries })
}

/// A media type of an OpenDocument document or of a part LibreOffice
/// treats as one (`application/vnd.oasis.opendocument.chart`).
fn format_or_part(media_type: &str) -> bool {
    media_type.starts_with("application/vnd.oasis.opendocument.")
}

/// Embedded objects other programs made (OLE): MetaScrub can't look inside.
fn is_embedded_object(name: &str, listed: &BTreeMap<String, String>) -> bool {
    !name.starts_with("Pictures/")
        && !name.starts_with("ObjectReplacements/")
        && !name.starts_with("Fonts/")
        && listed
            .get(name)
            .is_none_or(|media_type| !media_type.starts_with("image/"))
}

/// Each file the manifest lists, with its media type. Encrypted files
/// can't be read, so the package is skipped.
fn manifest_paths(data: &[u8]) -> Result<BTreeMap<String, String>, FormatError> {
    let nodes = xml::parse(data)?;
    let mut paths = BTreeMap::new();
    let mut encrypted = false;
    xml::visit_elements(&nodes, &mut |e: &Element| {
        if e.is(MANIFEST, "file-entry") {
            let path = e.attribute(MANIFEST, "full-path").unwrap_or_default();
            let media_type = e.attribute(MANIFEST, "media-type").unwrap_or_default();
            paths.insert(path.to_owned(), media_type.to_owned());
        }
        encrypted |= e.is(MANIFEST, "encryption-data");
    });
    if encrypted {
        return Err(FormatError::Encrypted);
    }
    Ok(paths)
}

/// The manifest without the files cleaning removed, nor folders left empty.
fn manifest_without(
    data: &[u8],
    removed: &HashSet<String>,
    kept: &HashSet<&str>,
) -> Result<Vec<u8>, FormatError> {
    let mut nodes = xml::parse(data)?;
    let gone = xml::remove_elements(&mut nodes, &mut |e: &Element| {
        if !e.is(MANIFEST, "file-entry") {
            return false;
        }
        let path = e.attribute(MANIFEST, "full-path").unwrap_or_default();
        if path == "/" {
            return false;
        }
        if path.ends_with('/') {
            return !kept.iter().any(|k| k.starts_with(path));
        }
        removed.contains(path) || !kept.contains(path)
    });
    if gone.is_empty() {
        Ok(data.to_vec())
    } else {
        xml::write(&nodes)
    }
}

/// What LibreOffice writes in `manifest.rdf`: only that this is a document
/// with a content and a styles file.
fn is_default_rdf(data: &[u8]) -> bool {
    const RDF: &[&str] = &["http://www.w3.org/1999/02/22-rdf-syntax-ns#"];
    // `pkg#Document`, `odf#ContentFile`, `odf#StylesFile`.
    const ODF_META: &str = "http://docs.oasis-open.org/ns/office/1.2/meta/";
    const PKG: &str = "http://docs.oasis-open.org/ns/office/1.2/meta/pkg#";
    let Ok(nodes) = xml::parse(data) else {
        return false;
    };
    let mut default = true;
    xml::visit_elements(&nodes, &mut |e: &Element| {
        let known = e.is(RDF, "RDF")
            || e.is(RDF, "Description")
            || (e.is(RDF, "type")
                && e.attribute(RDF, "resource")
                    .is_some_and(|r| r.starts_with(ODF_META)))
            || e.is(&[PKG], "hasPart");
        default &= known;
    });
    default
}

#[cfg(test)]
mod tests;
