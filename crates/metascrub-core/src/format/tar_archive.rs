//! TAR archives: every file inside is cleaned with the same rules as a file
//! on disk and written into a new archive whose headers name no owner, carry
//! a neutral date and neutral permissions, and hold no extended records.

use std::collections::BTreeSet;
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use tar::{Archive, Builder, EntryType, Header};

use super::archive::check_name;
use super::unpack::{self, Allowance};
use super::zip_archive::{junk, link};
use super::{FormatError, FormatHandler, ReadSeek};
use crate::limits::{MAX_TAR_HEADER_BYTES, MAX_ZIP_ENTRIES};
use crate::model::{Disposition, Finding, FindingKind, Location, Value};
use crate::naming;
use crate::options::Options;

/// 1980-01-01, like every other date MetaScrub writes (requirements §13.3).
const NEUTRAL_MTIME: u64 = 315_532_800;

#[derive(Debug)]
pub(super) struct Tar;

impl FormatHandler for Tar {
    fn inspect(
        &self,
        input: &mut dyn ReadSeek,
        options: &Options,
    ) -> Result<Vec<Finding>, FormatError> {
        process(input, None, options)
    }

    fn clean(
        &self,
        input: &mut dyn ReadSeek,
        output: &mut dyn Write,
        options: &Options,
    ) -> Result<Vec<Finding>, FormatError> {
        process(input, Some(output), options)
    }
}

/// `ustar` at offset 257: POSIX and GNU archives. Older ones without it
/// aren't recognized.
pub(super) fn is_tar(head: &[u8]) -> bool {
    head.get(257..262) == Some(b"ustar")
}

/// What the headers say about the files beyond their content.
#[derive(Debug, Default)]
struct Details {
    latest: Option<u64>,
    owners: BTreeSet<String>,
    owner_ids: usize,
    findings: Vec<Finding>,
}

fn process(
    input: &mut dyn ReadSeek,
    output: Option<&mut dyn Write>,
    options: &Options,
) -> Result<Vec<Finding>, FormatError> {
    let _allowance = Allowance::start();
    check_headers(input)?;
    input.seek(SeekFrom::Start(0))?;

    let mut findings = Vec::new();
    let mut details = Details::default();
    let mut builder = output.map(Builder::new);
    let mut archive = Archive::new(input);
    let mut count = 0;
    for entry in archive.entries().map_err(unpack::read_error)? {
        let mut entry = entry.map_err(unpack::read_error)?;
        count += 1;
        if count > MAX_ZIP_ENTRIES {
            return Err(FormatError::TooLarge {
                limit: MAX_ZIP_ENTRIES as u64,
            });
        }
        let kind = entry.header().entry_type();
        details.read_header(entry.header());
        if let Some(extensions) = entry.pax_extensions().map_err(unpack::read_error)? {
            let records: Vec<_> = extensions
                .filter_map(Result::ok)
                .filter_map(|r| Some((r.key().ok()?.to_owned(), r.value_bytes().to_vec())))
                .collect();
            let name = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
            details.read_records(&name, records);
        }
        if kind.is_pax_global_extensions() {
            continue;
        }

        let path = entry.path_bytes().into_owned();
        let name = String::from_utf8_lossy(&path).into_owned();
        check_name(name.trim_end_matches('/'))?;
        if naming::is_junk(Path::new(&name)) {
            findings.push(junk(&name));
            continue;
        }
        let original_mode = entry.header().mode().ok();
        let mut header = Header::new_gnu();
        header.set_mtime(NEUTRAL_MTIME);
        header.set_uid(0);
        header.set_gid(0);
        match kind {
            EntryType::Regular | EntryType::Continuous => {
                let data = unpack::unpack(&mut entry, None)?;
                let mut unpacked = unpack::entry(&name, data, options, builder.is_some())?;
                findings.append(&mut unpacked.findings);
                if let Some(builder) = &mut builder {
                    let size = unpacked.content.seek(SeekFrom::End(0))?;
                    unpacked.content.seek(SeekFrom::Start(0))?;
                    header.set_entry_type(EntryType::Regular);
                    header.set_size(size);
                    header.set_mode(unpack::mode(original_mode, false));
                    builder
                        .append_data(&mut header, path_of(&path), &mut unpacked.content)
                        .map_err(unpack::read_error)?;
                }
            }
            EntryType::Directory => {
                if let Some(builder) = &mut builder {
                    header.set_entry_type(EntryType::Directory);
                    header.set_size(0);
                    header.set_mode(unpack::mode(original_mode, true));
                    builder
                        .append_data(&mut header, path_of(&path), io::empty())
                        .map_err(unpack::read_error)?;
                }
            }
            EntryType::Symlink | EntryType::Link => {
                let target = entry
                    .link_name_bytes()
                    .map(|t| t.into_owned())
                    .unwrap_or_default();
                findings.push(link(&name, &String::from_utf8_lossy(&target)));
                if let Some(builder) = &mut builder {
                    header.set_entry_type(kind);
                    header.set_size(0);
                    header.set_mode(0o777);
                    builder
                        .append_link(&mut header, path_of(&path), path_of(&target))
                        .map_err(unpack::read_error)?;
                }
            }
            other => {
                return Err(FormatError::Unsupported(format!(
                    "TAR entry type {:?}",
                    other.as_byte() as char
                )));
            }
        }
    }
    findings.extend(details.findings());
    if let Some(builder) = builder {
        builder.into_inner().map_err(unpack::read_error)?.flush()?;
    }
    Ok(findings)
}

/// The TAR reader holds long names and PAX records in memory whole, so their
/// sizes are checked first, skipping over the files' content. Sparse files
/// are refused: their maps are also held in memory.
fn check_headers(input: &mut dyn ReadSeek) -> Result<(), FormatError> {
    let mut archive = Archive::new(input);
    let entries = archive
        .entries_with_seek()
        .map_err(unpack::read_error)?
        .raw(true);
    for (count, entry) in entries.enumerate() {
        let entry = entry.map_err(unpack::read_error)?;
        // Each file can have a long name, a long link and a PAX record.
        if count > 4 * MAX_ZIP_ENTRIES {
            return Err(FormatError::TooLarge {
                limit: MAX_ZIP_ENTRIES as u64,
            });
        }
        let kind = entry.header().entry_type();
        if kind.is_gnu_sparse() {
            return Err(FormatError::Unsupported("sparse TAR entry".into()));
        }
        let extra = kind.is_gnu_longname()
            || kind.is_gnu_longlink()
            || kind.is_pax_local_extensions()
            || kind.is_pax_global_extensions();
        if extra && entry.size() > MAX_TAR_HEADER_BYTES {
            return Err(FormatError::TooLarge {
                limit: MAX_TAR_HEADER_BYTES,
            });
        }
    }
    Ok(())
}

impl Details {
    fn read_header(&mut self, header: &Header) {
        let mut times = vec![header.mtime().ok()];
        if let Some(gnu) = header.as_gnu() {
            times.extend([gnu.atime().ok(), gnu.ctime().ok()]);
        }
        for time in times.into_iter().flatten() {
            if time != 0 && time != NEUTRAL_MTIME {
                self.latest = self.latest.max(Some(time));
            }
        }
        for name in [header.username_bytes(), header.groupname_bytes()]
            .into_iter()
            .flatten()
        {
            let name = String::from_utf8_lossy(name).trim().to_owned();
            if !name.is_empty() {
                self.owners.insert(name);
            }
        }
        if header.uid().is_ok_and(|id| id != 0) || header.gid().is_ok_and(|id| id != 0) {
            self.owner_ids += 1;
        }
    }

    /// PAX records: extended attributes saved with the file (download
    /// addresses, Finder tags), more dates, owners, comments.
    fn read_records(&mut self, name: &str, records: Vec<(String, Vec<u8>)>) {
        let location = |key: &str| Location::new([name, "PAX", key]);
        for (key, value) in records {
            let text = || String::from_utf8_lossy(&value).trim().to_owned();
            if let Some(attribute) = key.strip_prefix("SCHILY.xattr.") {
                self.findings.extend(
                    crate::fsmeta::describe_attribute(attribute, &value)
                        .into_iter()
                        .map(|f| f.within(name)),
                );
                continue;
            }
            if let Some(attribute) = key.strip_prefix("LIBARCHIVE.xattr.") {
                let attribute = percent_decode(attribute);
                let value = base64(&value).unwrap_or_default();
                self.findings.extend(
                    crate::fsmeta::describe_attribute(&attribute, &value)
                        .into_iter()
                        .map(|f| f.within(name)),
                );
                continue;
            }
            let finding = match key.as_str() {
                // How the file is stored, not about it.
                "path" | "linkpath" | "size" => continue,
                "mtime" | "atime" | "ctime" | "LIBARCHIVE.creationtime" => {
                    Finding::new(FindingKind::Timestamp, location(&key), Disposition::Remove)
                }
                "uname" | "gname" => {
                    self.owners.insert(text());
                    continue;
                }
                "uid" | "gid" => {
                    self.owner_ids += 1;
                    continue;
                }
                "comment" => {
                    Finding::new(FindingKind::Comment, location(&key), Disposition::Remove)
                        .with_value(Value::Text(text()))
                }
                _ if key.starts_with("GNU.sparse.") => continue,
                _ => Finding::new(
                    FindingKind::UnknownMetadata,
                    location(&key),
                    Disposition::Remove,
                )
                .with_value(Value::Text(key.clone())),
            };
            self.findings.push(finding);
        }
    }

    fn findings(self) -> Vec<Finding> {
        let mut findings = self.findings;
        let location = |part: &str| Location::new(["TAR", part]);
        if let Some(latest) = self.latest {
            findings.push(
                Finding::new(
                    FindingKind::Timestamp,
                    location("Dates"),
                    Disposition::Remove,
                )
                .with_value(Value::Text(unpack::utc(latest))),
            );
        }
        for owner in self.owners.into_iter().filter(|o| !o.is_empty()) {
            findings.push(
                Finding::new(
                    FindingKind::PersonName,
                    location("Owner"),
                    Disposition::Remove,
                )
                .with_value(Value::Text(owner)),
            );
        }
        if self.owner_ids > 0 {
            findings.push(
                Finding::new(
                    FindingKind::TechnicalInfo,
                    location("Owner IDs"),
                    Disposition::Remove,
                )
                .with_value(Value::Integer(self.owner_ids as i64)),
            );
        }
        findings
    }
}

/// Names in TAR headers are bytes; on Unix they stay exactly as they were.
#[cfg(unix)]
fn path_of(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
fn path_of(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

/// `user.xdg.origin%2Eurl` → `user.xdg.origin.url` (libarchive escapes names).
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        let hex = bytes
            .get(index + 1..index + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (byte, hex) {
            (b'%', Some(decoded)) => {
                out.push(decoded);
                index += 3;
            }
            _ => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Standard base64, with or without padding; `None` if it isn't base64.
fn base64(text: &[u8]) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let text: Vec<u8> = text
        .iter()
        .copied()
        .filter(|c| !c.is_ascii_whitespace() && *c != b'=')
        .collect();
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    for chunk in text.chunks(4) {
        let mut bits = 0u32;
        for &c in chunk {
            bits = (bits << 6) | value(c)?;
        }
        let bytes = match chunk.len() {
            4 => vec![(bits >> 16) as u8, (bits >> 8) as u8, bits as u8],
            3 => vec![(bits >> 10) as u8, (bits >> 2) as u8],
            2 => vec![(bits >> 4) as u8],
            _ => return None,
        };
        out.extend(bytes);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read};

    use super::*;

    fn camera() -> Vec<u8> {
        std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/generated/jpeg/camera.jpg"),
        )
        .unwrap()
    }

    /// `<length> key=value\n`, where the length counts itself.
    fn pax_record(key: &str, value: &[u8]) -> Vec<u8> {
        let rest = key.len() + value.len() + 3;
        let mut length = rest + 1;
        while length != rest + length.to_string().len() {
            length = rest + length.to_string().len();
        }
        let mut record = format!("{length} {key}=").into_bytes();
        record.extend(value);
        record.push(b'\n');
        record
    }

    fn append_pax(builder: &mut Builder<Vec<u8>>, records: &[u8]) {
        let mut header = Header::new_ustar();
        header.set_entry_type(EntryType::XHeader);
        header.set_size(records.len() as u64);
        builder
            .append_data(&mut header, "PaxHeader", records)
            .unwrap();
    }

    /// A TAR as `tar` on a desktop writes it: owner names and IDs, dates, a
    /// PAX record with the download address.
    fn tar_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = Builder::new(Vec::new());
        append_pax(
            &mut builder,
            &pax_record("SCHILY.xattr.user.xdg.origin.url", b"https://example.com/a"),
        );
        for (name, data) in entries {
            let mut header = Header::new_ustar();
            header.set_size(data.len() as u64);
            header.set_mode(0o600);
            header.set_mtime(1_730_563_200);
            header.set_uid(1000);
            header.set_gid(1000);
            header.set_username("jane").unwrap();
            header.set_groupname("staff").unwrap();
            header.set_entry_type(EntryType::Regular);
            builder.append_data(&mut header, name, *data).unwrap();
        }
        builder.into_inner().unwrap()
    }

    fn inspect(data: &[u8]) -> Vec<Finding> {
        Tar.inspect(&mut Cursor::new(data), &Options::default())
            .unwrap()
    }

    fn clean(data: &[u8]) -> (Vec<u8>, Vec<Finding>) {
        let mut out = Vec::new();
        let findings = Tar
            .clean(&mut Cursor::new(data), &mut out, &Options::default())
            .unwrap();
        (out, findings)
    }

    fn kinds(findings: &[Finding]) -> BTreeSet<String> {
        findings.iter().map(|f| format!("{:?}", f.kind)).collect()
    }

    #[test]
    fn recognizes_tar_headers() {
        let data = tar_of(&[("a.txt", b"x")]);
        assert!(is_tar(&data));
        assert!(!is_tar(b"not a tar"));
    }

    #[test]
    fn cleans_every_file_and_header() {
        let photo = camera();
        let data = tar_of(&[
            ("photos/camera.jpg", &photo),
            ("photos/notes.txt", b"hello"),
            ("photos/._camera.jpg", b"\0\x05\x16\x07"),
        ]);
        let (cleaned, findings) = clean(&data);
        let found = kinds(&findings);
        for kind in [
            "GpsLocation",
            "UnreadFile",
            "JunkFile",
            "Timestamp",
            "PersonName",
            "TechnicalInfo",
            "DownloadUrl",
        ] {
            assert!(found.contains(kind), "{kind} in {found:?}");
        }
        assert_eq!(findings, inspect(&data), "inspect says what clean does");

        let removable: Vec<_> = inspect(&cleaned)
            .into_iter()
            .filter(|f| !f.disposition.is_acceptable_after_cleaning())
            .collect();
        assert!(removable.is_empty(), "{removable:?}");
        assert_eq!(clean(&cleaned).0, cleaned, "cleaning twice changes nothing");

        let mut archive = Archive::new(Cursor::new(&cleaned));
        let mut names = Vec::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let header = entry.header();
            assert_eq!(header.mtime().unwrap(), NEUTRAL_MTIME);
            assert_eq!(header.uid().unwrap(), 0);
            assert_eq!(header.username_bytes(), Some(&b""[..]));
            assert_eq!(header.mode().unwrap(), 0o644);
            names.push(entry.path().unwrap().into_owned());
            let mut content = Vec::new();
            entry.read_to_end(&mut content).unwrap();
            if names.last().unwrap().ends_with("notes.txt") {
                assert_eq!(content, b"hello");
            }
        }
        assert_eq!(
            names,
            [
                PathBuf::from("photos/camera.jpg"),
                PathBuf::from("photos/notes.txt")
            ]
        );
    }

    #[test]
    fn long_names_survive() {
        let name = format!("{}/notes.txt", "folder".repeat(30));
        let (cleaned, _) = clean(&tar_of(&[(name.as_str(), b"x")]));
        let mut archive = Archive::new(Cursor::new(&cleaned));
        let entry = archive.entries().unwrap().next().unwrap().unwrap();
        assert_eq!(entry.path().unwrap(), Path::new(&name));
    }

    #[test]
    fn oversized_extra_headers_are_refused() {
        let mut builder = Builder::new(Vec::new());
        let huge = vec![b'a'; 2 * 1024 * 1024];
        append_pax(&mut builder, &pax_record("comment", &huge));
        let mut header = Header::new_ustar();
        header.set_size(1);
        builder
            .append_data(&mut header, "a.txt", &b"x"[..])
            .unwrap();
        let data = builder.into_inner().unwrap();
        assert!(matches!(
            Tar.inspect(&mut Cursor::new(&data), &Options::default()),
            Err(FormatError::TooLarge { .. })
        ));
    }

    #[test]
    fn decodes_libarchive_attribute_names_and_values() {
        assert_eq!(
            percent_decode("user.xdg.origin%2Eurl"),
            "user.xdg.origin.url"
        );
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(
            base64(b"aHR0cHM6Ly9leGFtcGxlLmNvbQ==").unwrap(),
            b"https://example.com"
        );
        assert_eq!(base64(b"YQ").unwrap(), b"a");
        assert_eq!(base64(b"*"), None);
    }
}
