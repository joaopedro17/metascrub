//! What the file system stores next to a file's contents: extended
//! attributes on Linux and macOS, alternate data streams on Windows.
//! Browsers record there where a file was downloaded from. None of it is
//! part of the file, so it survives editing and copying with most tools.
//!
//! MetaScrub never copies it: every output is a new file. What's left is
//! finding it, and removing it where a file or folder isn't rewritten.

use std::ffi::OsString;
use std::io;
use std::path::Path;

use crate::model::{Disposition, Finding, FindingKind, Location, Value};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as sys;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as sys;

/// Values are read only for attributes MetaScrub knows, and only this much.
const MAX_VALUE_BYTES: u64 = 64 * 1024;

/// Set and guarded by macOS itself, which doesn't allow removing them. They
/// are IDs and permissions, not descriptions of the file; `provenance` is
/// added to every file written by an app downloaded from the internet,
/// MetaScrub included, so reporting it would only be noise.
const SYSTEM: &[&str] = &[
    "com.apple.provenance",
    "com.apple.macl",
    "com.apple.rootless",
];

#[derive(Debug)]
pub(crate) struct Attribute {
    /// As the operating system names it, to remove it.
    name: OsString,
    /// What it reveals; the disposition is set by [`findings`].
    findings: Vec<Finding>,
}

/// The attributes of `path` (a file or a folder), without following links.
pub(crate) fn list(path: &Path) -> io::Result<Vec<Attribute>> {
    Ok(sys::names(path)?
        .into_iter()
        .filter_map(|name| {
            let text = name.to_string_lossy().into_owned();
            if SYSTEM.contains(&text.as_str()) {
                return None;
            }
            let findings = describe(&text, || sys::read(path, &name));
            Some(Attribute { name, findings })
        })
        .collect())
}

/// Removes every attribute, and returns what was there.
pub(crate) fn strip(path: &Path) -> io::Result<Vec<Attribute>> {
    let attributes = list(path)?;
    for attribute in &attributes {
        sys::remove(path, &attribute.name)?;
    }
    Ok(attributes)
}

/// Records `url` the way a browser on this system does, for tests. False
/// when the file system doesn't allow it, unless
/// METASCRUB_REQUIRE_FS_ATTRIBUTES is set (as in CI), so tests that need
/// attributes can't be skipped there.
#[cfg(test)]
pub(crate) fn mark_downloaded(path: &Path, url: &str) -> bool {
    let marked = sys::mark_downloaded(path, url);
    let required =
        std::env::var_os("METASCRUB_REQUIRE_FS_ATTRIBUTES").is_some_and(|v| !v.is_empty());
    assert!(
        marked || !required,
        "this file system doesn't allow extended attributes"
    );
    marked
}

/// The findings for `attributes`, with what cleaning does to them.
pub(crate) fn findings(attributes: Vec<Attribute>, disposition: Disposition) -> Vec<Finding> {
    attributes
        .into_iter()
        .flat_map(|attribute| attribute.findings)
        .map(|mut finding| {
            finding.disposition = disposition;
            finding
        })
        .collect()
}

/// What an attribute reveals. Names from every operating system are
/// recognized everywhere: they don't overlap, and this way all of it is
/// tested on any system.
fn describe(name: &str, read: impl FnOnce() -> Option<Vec<u8>>) -> Vec<Finding> {
    let location = Location::new(["File system", name]);
    let finding = |kind| Finding::new(kind, location.clone(), Disposition::Remove);
    let texts = |values: Vec<String>, kind| -> Vec<Finding> {
        values
            .into_iter()
            .map(|value| finding(kind).with_value(Value::Text(value)))
            .collect()
    };
    let text = |v: Vec<u8>| String::from_utf8_lossy(&v).trim().to_owned();

    let findings = match name {
        "user.xdg.origin.url" | "user.xdg.referrer.url" => texts(
            read().map(text).into_iter().collect(),
            FindingKind::DownloadUrl,
        ),
        "user.xdg.comment" => texts(read().map(text).into_iter().collect(), FindingKind::Comment),
        "user.xdg.publisher" => texts(
            read().map(text).into_iter().collect(),
            FindingKind::Software,
        ),
        "Zone.Identifier" => texts(
            read().map(|v| zone_urls(&v)).unwrap_or_default(),
            FindingKind::DownloadUrl,
        ),
        "com.apple.metadata:kMDItemWhereFroms" => {
            let (urls, others): (Vec<_>, Vec<_>) = read()
                .map(|v| plist_strings(&v))
                .unwrap_or_default()
                .into_iter()
                .partition(|s| s.contains("://"));
            let mut findings = texts(urls, FindingKind::DownloadUrl);
            // Mail stores the sender and subject of an attachment here.
            findings.extend(texts(others, FindingKind::Description));
            findings
        }
        "com.apple.quarantine" => texts(
            read()
                .and_then(|v| quarantine_agent(&text(v)))
                .into_iter()
                .collect(),
            FindingKind::Software,
        ),
        "com.apple.metadata:_kMDItemUserTags" => texts(
            read().map(|v| plist_strings(&v)).unwrap_or_default(),
            FindingKind::Description,
        ),
        "com.apple.metadata:kMDItemFinderComment" => texts(
            read().map(|v| plist_strings(&v)).unwrap_or_default(),
            FindingKind::Comment,
        ),
        "com.apple.metadata:kMDItemDownloadedDate" | "com.apple.lastuseddate#PS" => {
            vec![finding(FindingKind::Timestamp)]
        }
        _ => Vec::new(),
    };
    if findings.is_empty() {
        vec![finding(FindingKind::FileAttribute).with_value(Value::Text(name.to_owned()))]
    } else {
        findings
    }
}

/// What an attribute with this name and value reveals. Public for fuzzing only.
#[doc(hidden)]
pub fn describe_attribute(name: &str, value: &[u8]) -> Vec<Finding> {
    describe(name, || Some(value.to_vec()))
}

/// `HostUrl` and `ReferrerUrl` from a Windows `Zone.Identifier` stream.
fn zone_urls(data: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(data)
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            let value = value.trim();
            let url = matches!(key.trim(), "HostUrl" | "ReferrerUrl")
                // Written instead of the address by private browsing windows.
                && !value.is_empty() && value != "about:internet";
            url.then(|| value.to_owned())
        })
        .collect()
}

/// The browser or app named in `com.apple.quarantine`:
/// `flags;hex time;agent;id`.
fn quarantine_agent(value: &str) -> Option<String> {
    let agent = value.split(';').nth(2)?.trim();
    (!agent.is_empty()).then(|| agent.to_owned())
}

/// The strings in a small binary property list (`bplist00`), read object by
/// object up to the offset table. macOS stores these attributes as a string
/// or an array of strings; anything else ends the reading.
fn plist_strings(data: &[u8]) -> Vec<String> {
    let mut strings = Vec::new();
    let Some((mut rest, reference_size)) = plist_objects(data) else {
        return strings;
    };
    while let Some((&marker, after)) = rest.split_first() {
        let Some((count, after)) = length(marker, after) else {
            break;
        };
        let bytes = match marker >> 4 {
            // ASCII string, UTF-16 string, array of references.
            0x5 => count,
            0x6 => count.saturating_mul(2),
            0xA => count.saturating_mul(reference_size),
            _ => break,
        };
        let Some((content, after)) = after.split_at_checked(bytes) else {
            break;
        };
        let text = match marker >> 4 {
            0x5 => Some(String::from_utf8_lossy(content).into_owned()),
            0x6 => Some(String::from_utf16_lossy(
                &content
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&pair| u16::from_be_bytes(pair))
                    .collect::<Vec<_>>(),
            )),
            _ => None,
        };
        strings.extend(text.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()));
        rest = after;
    }
    strings
}

/// The objects of a binary property list, and the size of a reference.
fn plist_objects(data: &[u8]) -> Option<(&[u8], usize)> {
    const TRAILER: usize = 32;
    data.strip_prefix(b"bplist00")?;
    let trailer = data.get(data.len().checked_sub(TRAILER)?..)?;
    let reference_size = usize::from(*trailer.get(7)?);
    let table = u64::from_be_bytes(trailer.get(24..)?.try_into().ok()?);
    let objects = data.get(8..usize::try_from(table).ok()?)?;
    Some((objects, reference_size))
}

/// The length in an object's marker, which for 15 and up follows as an
/// integer object of 1, 2, 4 or 8 bytes.
fn length(marker: u8, after: &[u8]) -> Option<(usize, &[u8])> {
    let short = usize::from(marker & 0x0F);
    if short < 0x0F {
        return Some((short, after));
    }
    let (&int_marker, after) = after.split_first()?;
    if int_marker >> 4 != 0x1 {
        return None;
    }
    let (bytes, after) = after.split_at_checked(1 << (int_marker & 0x0F).min(3))?;
    let value = bytes
        .iter()
        .fold(0u64, |value, &b| (value << 8) | u64::from(b));
    Some((usize::try_from(value).ok()?, after))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(findings: &[Finding]) -> Vec<FindingKind> {
        findings.iter().map(|f| f.kind).collect()
    }

    fn values(findings: &[Finding]) -> Vec<String> {
        findings
            .iter()
            .filter_map(|f| match &f.value {
                Some(Value::Text(t)) => Some(t.clone()),
                _ => None,
            })
            .collect()
    }

    /// A binary property list holding `strings` in an array, as macOS writes it.
    fn plist(strings: &[&str]) -> Vec<u8> {
        let mut data = b"bplist00".to_vec();
        data.push(0xA0 | strings.len() as u8);
        data.extend((1..=strings.len()).map(|r| r as u8));
        for s in strings {
            if s.len() < 15 {
                data.push(0x50 | s.len() as u8);
            } else {
                data.extend([0x5F, 0x10, s.len() as u8]);
            }
            data.extend(s.as_bytes());
        }
        // The offset table isn't read, only where it starts.
        let table = data.len() as u64;
        data.extend(std::iter::repeat_n(0x55, strings.len() + 1));
        data.extend([0, 0, 0, 0, 0, 0, 1, 1]);
        data.extend((strings.len() as u64 + 1).to_be_bytes());
        data.extend(0u64.to_be_bytes());
        data.extend(table.to_be_bytes());
        data
    }

    #[test]
    fn reads_download_urls_from_linux_attributes() {
        let findings = describe("user.xdg.origin.url", || {
            Some(b"https://example.com/photo.jpg".to_vec())
        });
        assert_eq!(kinds(&findings), [FindingKind::DownloadUrl]);
        assert_eq!(values(&findings), ["https://example.com/photo.jpg"]);
        assert_eq!(
            findings[0].location,
            Location::new(["File system", "user.xdg.origin.url"])
        );
    }

    #[test]
    fn reads_download_urls_from_zone_identifier() {
        let stream = b"[ZoneTransfer]\r\nZoneId=3\r\nReferrerUrl=https://example.com/\r\n\
                       HostUrl=https://example.com/report.pdf\r\n";
        let findings = describe("Zone.Identifier", || Some(stream.to_vec()));
        assert_eq!(
            values(&findings),
            ["https://example.com/", "https://example.com/report.pdf"]
        );
    }

    #[test]
    fn a_zone_identifier_without_urls_is_still_reported() {
        let stream = b"[ZoneTransfer]\r\nZoneId=3\r\nHostUrl=about:internet\r\n";
        let findings = describe("Zone.Identifier", || Some(stream.to_vec()));
        assert_eq!(kinds(&findings), [FindingKind::FileAttribute]);
        assert_eq!(values(&findings), ["Zone.Identifier"]);
    }

    #[test]
    fn reads_where_a_mac_file_came_from() {
        let value = plist(&[
            "https://example.com/files/report.pdf",
            "https://example.com/",
        ]);
        let findings = describe("com.apple.metadata:kMDItemWhereFroms", || {
            Some(value.clone())
        });
        assert_eq!(
            kinds(&findings),
            [FindingKind::DownloadUrl, FindingKind::DownloadUrl]
        );
        assert_eq!(
            values(&findings),
            [
                "https://example.com/files/report.pdf",
                "https://example.com/"
            ]
        );

        let mail = plist(&["Jane <jane@example.com>", "Quarterly report"]);
        let findings = describe("com.apple.metadata:kMDItemWhereFroms", || {
            Some(mail.clone())
        });
        assert_eq!(
            kinds(&findings),
            [FindingKind::Description, FindingKind::Description]
        );
    }

    #[test]
    fn reads_the_app_that_downloaded_a_mac_file() {
        let findings = describe("com.apple.quarantine", || {
            Some(b"0083;65f1c2a0;Safari;F1A2B3C4-0000".to_vec())
        });
        assert_eq!(kinds(&findings), [FindingKind::Software]);
        assert_eq!(values(&findings), ["Safari"]);
    }

    #[test]
    fn unknown_attributes_are_reported_by_name() {
        let findings = describe("user.custom", || None);
        assert_eq!(kinds(&findings), [FindingKind::FileAttribute]);
        assert_eq!(values(&findings), ["user.custom"]);
    }

    #[test]
    fn reads_utf16_plist_strings() {
        let mut data = plist(&["ab"]);
        // "ab" stored as UTF-16, as macOS does for text that isn't ASCII.
        data.splice(10..13, [0x62, 0, b'a', 0, b'b']);
        let table = data.len() - 32 - 2;
        let end = data.len();
        data.splice(end - 8.., (table as u64).to_be_bytes());
        assert_eq!(plist_strings(&data), ["ab"]);
    }

    #[test]
    fn plist_reading_stops_at_anything_unexpected() {
        assert!(plist_strings(b"").is_empty());
        assert!(plist_strings(b"bplist00").is_empty());
        let mut long = plist(&["x"]);
        // A string claiming to be longer than the data.
        long[10] = 0x5F;
        long.insert(11, 0x13);
        assert!(plist_strings(&long).is_empty());
        let long_string = "https://example.com/a/very/long/path";
        assert_eq!(plist_strings(&plist(&[long_string])), [long_string]);
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn a_file_without_attributes_has_none() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();
        let found = list(&file).unwrap();
        assert!(found.is_empty(), "{found:?}");
    }
}
