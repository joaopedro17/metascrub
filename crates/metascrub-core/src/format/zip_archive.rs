//! ZIP archives: every file inside is cleaned with the same rules as a file
//! on disk and written into a new archive with neutral dates and
//! permissions, and without comments, extra fields or junk files.

use std::collections::{BTreeSet, HashSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use zip::extra_fields::ExtraField;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, System, ZipArchive, ZipWriter};

use super::archive::{check_name, zip_error};
use super::unpack::{self, Allowance, Spool};
use super::{FormatError, FormatHandler, ReadSeek};
use crate::limits::MAX_ZIP_ENTRIES;
use crate::model::{Disposition, Finding, FindingKind, Location, Value};
use crate::naming;
use crate::options::Options;

/// A symbolic link's target is a short path.
const MAX_LINK_BYTES: u64 = 4096;

/// Extra fields holding the Unix owner's user and group IDs.
const OWNER_FIELDS: &[u16] = &[0x7875, 0x7855, 0x000d];

#[derive(Debug)]
pub(super) struct Zip;

impl FormatHandler for Zip {
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
        // The ZIP writer goes back to fill in sizes, so it needs a file it
        // can seek in.
        let mut written = unpack::spool();
        let findings = process(input, Some(&mut written), options)?;
        unpack::copy_out(&mut written, output)?;
        Ok(findings)
    }
}

/// Programs and documents stored as ZIP files (Java and Android apps,
/// e-books, OpenDocument files, extensions) depend on their exact content
/// and are often signed, so they aren't treated as archives.
/// The `mimetype` entry that OpenDocument files and e-books start with,
/// naming what they are.
pub(super) fn mimetype(input: &mut dyn ReadSeek) -> Result<Option<String>, FormatError> {
    let mut zip = ZipArchive::new(input).map_err(zip_error)?;
    let Ok(mut file) = zip.by_name("mimetype") else {
        return Ok(None);
    };
    if file.size() > 256 {
        return Ok(None);
    }
    let mut text = String::new();
    if Read::by_ref(&mut file)
        .take(256)
        .read_to_string(&mut text)
        .is_err()
    {
        return Ok(None);
    }
    Ok(Some(text.trim().to_owned()))
}

pub(super) fn is_package(input: &mut dyn ReadSeek) -> Result<bool, FormatError> {
    let zip = ZipArchive::new(input).map_err(zip_error)?;
    Ok(zip.file_names().any(|name| {
        name.is_ok_and(|name| {
            matches!(
                name.as_ref(),
                "mimetype" | "[Content_Types].xml" | "AndroidManifest.xml"
            ) || name.starts_with("META-INF/")
        })
    }))
}

/// What the archive says about the files beyond their content.
#[derive(Debug, Default)]
struct Details {
    latest: Option<DateTime>,
    extra_times: usize,
    owners: usize,
    unknown_fields: BTreeSet<u16>,
}

fn process(
    input: &mut dyn ReadSeek,
    output: Option<&mut Spool>,
    options: &Options,
) -> Result<Vec<Finding>, FormatError> {
    let _allowance = Allowance::start();
    let mut zip = ZipArchive::new(input).map_err(zip_error)?;
    if zip.len() > MAX_ZIP_ENTRIES {
        return Err(FormatError::TooLarge {
            limit: MAX_ZIP_ENTRIES as u64,
        });
    }
    // Checked first, so nothing is cleaned when the archive can't be read whole.
    for index in 0..zip.len() {
        if zip.by_index_raw(index).map_err(zip_error)?.encrypted() {
            return Err(FormatError::Encrypted);
        }
    }

    let mut findings = Vec::new();
    let comment = String::from_utf8_lossy(zip.comment()).trim().to_owned();
    if !comment.is_empty() {
        findings.push(
            Finding::new(
                FindingKind::Comment,
                Location::new(["ZIP", "Comment"]),
                Disposition::Remove,
            )
            .with_value(Value::Text(comment)),
        );
    }
    let mut writer = output.map(ZipWriter::new);
    let mut details = Details::default();
    let mut names = HashSet::new();
    for index in 0..zip.len() {
        let mut file = zip.by_index(index).map_err(zip_error)?;
        let name = file.name().map_err(zip_error)?.into_owned();
        check_name(&name)?;
        if !names.insert(name.clone()) {
            return Err(FormatError::malformed(format!(
                "duplicate ZIP entry {name}"
            )));
        }
        let method = file.compression();
        if !matches!(
            method,
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            return Err(FormatError::Unsupported(format!(
                "ZIP compression method {method}"
            )));
        }
        if let Some(date) = file.last_modified().filter(|d| *d != DateTime::default()) {
            details.latest = details.latest.max(Some(date));
        }
        for field in file.extra_data_fields() {
            match field {
                ExtraField::Ntfs(_) | ExtraField::ExtendedTimestamp(_) => details.extra_times += 1,
                ExtraField::Custom(custom) if OWNER_FIELDS.contains(&custom.header_id) => {
                    details.owners += 1;
                }
                ExtraField::Custom(custom) => {
                    details.unknown_fields.insert(custom.header_id);
                }
                // How the entry is stored, not about it.
                _ => {}
            }
        }
        let entry_comment = file.comment().trim().to_owned();
        if !entry_comment.is_empty() {
            findings.push(
                Finding::new(
                    FindingKind::Comment,
                    Location::new([name.as_str(), "Comment"]),
                    Disposition::Remove,
                )
                .with_value(Value::Text(entry_comment)),
            );
        }

        if naming::is_junk(Path::new(&name)) {
            findings.push(junk(&name));
            continue;
        }
        let unix_mode = file.unix_mode();
        if file.is_dir() {
            if let Some(writer) = &mut writer {
                writer
                    .add_directory(
                        name.as_str(),
                        entry_options(method, unpack::mode(unix_mode, true), 0),
                    )
                    .map_err(zip_error)?;
            }
            continue;
        }
        if file.is_symlink() {
            let mut target = Vec::new();
            Read::by_ref(&mut file)
                .take(MAX_LINK_BYTES)
                .read_to_end(&mut target)
                .map_err(unpack::read_error)?;
            let target = String::from_utf8_lossy(&target).into_owned();
            findings.push(link(&name, &target));
            if let Some(writer) = &mut writer {
                writer
                    .add_symlink(
                        name.as_str(),
                        target.as_str(),
                        entry_options(method, 0o777, 0),
                    )
                    .map_err(zip_error)?;
            }
            continue;
        }

        let compressed = file.compressed_size();
        let data = unpack::unpack(&mut file, Some(compressed))?;
        drop(file);
        let mut unpacked = unpack::entry(&name, data, options, writer.is_some())?;
        findings.append(&mut unpacked.findings);
        if let Some(writer) = &mut writer {
            let size = unpacked.content.seek(SeekFrom::End(0))?;
            writer
                .start_file(
                    name.as_str(),
                    entry_options(method, unpack::mode(unix_mode, false), size),
                )
                .map_err(zip_error)?;
            unpack::copy_out(&mut unpacked.content, writer)?;
        }
    }
    findings.extend(details.findings());
    if let Some(writer) = writer {
        writer.finish().map_err(zip_error)?;
    }
    Ok(findings)
}

impl Details {
    fn findings(self) -> Vec<Finding> {
        let mut findings = Vec::new();
        let location = |part: &str| Location::new(["ZIP", part]);
        if let Some(latest) = self.latest {
            findings.push(
                Finding::new(
                    FindingKind::Timestamp,
                    location("Dates"),
                    Disposition::Remove,
                )
                .with_value(Value::Text(latest.to_string())),
            );
        }
        if self.extra_times > 0 {
            findings.push(Finding::new(
                FindingKind::Timestamp,
                location("Extra fields"),
                Disposition::Remove,
            ));
        }
        if self.owners > 0 {
            findings.push(
                Finding::new(
                    FindingKind::TechnicalInfo,
                    location("Owner IDs"),
                    Disposition::Remove,
                )
                .with_value(Value::Integer(self.owners as i64)),
            );
        }
        if !self.unknown_fields.is_empty() {
            findings.push(
                Finding::new(
                    FindingKind::UnknownMetadata,
                    location("Extra fields"),
                    Disposition::Remove,
                )
                .with_value(Value::Integer(self.unknown_fields.len() as i64)),
            );
        }
        findings
    }
}

pub(super) fn junk(name: &str) -> Finding {
    Finding::new(
        FindingKind::JunkFile,
        Location::new([name]),
        Disposition::Remove,
    )
}

pub(super) fn link(name: &str, target: &str) -> Finding {
    Finding::new(
        FindingKind::SymbolicLink,
        Location::new([name]),
        Disposition::Warn,
    )
    .with_value(Value::Text(target.to_owned()))
}

/// The same for every entry, so the same files always give the same bytes.
/// Always marked as made on Unix: the zip crate picks DOS on Windows, which
/// drops the permissions and changes the bytes.
fn entry_options(method: CompressionMethod, mode: u32, size: u64) -> SimpleFileOptions {
    let level = (method == CompressionMethod::Deflated).then_some(6);
    SimpleFileOptions::default()
        .compression_method(method)
        .compression_level(level)
        .last_modified_time(DateTime::default())
        .system(System::Unix)
        .unix_permissions(mode)
        .large_file(size >= u64::from(u32::MAX))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::format::archive::tests::zip_of;
    use crate::format::{FormatId, detect};

    fn camera() -> Vec<u8> {
        std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/generated/jpeg/camera.jpg"),
        )
        .unwrap()
    }

    fn inspect(data: &[u8]) -> Vec<Finding> {
        Zip.inspect(&mut Cursor::new(data), &Options::default())
            .unwrap()
    }

    fn clean(data: &[u8]) -> (Vec<u8>, Vec<Finding>) {
        let mut out = Vec::new();
        let findings = Zip
            .clean(&mut Cursor::new(data), &mut out, &Options::default())
            .unwrap();
        (out, findings)
    }

    fn names(data: &[u8]) -> Vec<String> {
        let zip = ZipArchive::new(Cursor::new(data)).unwrap();
        zip.file_names().map(|n| n.unwrap().into_owned()).collect()
    }

    fn has(findings: &[Finding], kind: FindingKind, first: &str) -> bool {
        findings.iter().any(|f| {
            f.kind == kind && f.location.parts().first().map(String::as_str) == Some(first)
        })
    }

    fn removable(findings: &[Finding]) -> Vec<&Finding> {
        findings
            .iter()
            .filter(|f| !f.disposition.is_acceptable_after_cleaning())
            .collect()
    }

    #[test]
    fn cleans_every_file_inside() {
        let photo = camera();
        let data = zip_of(&[
            ("photos/camera.jpg", &photo),
            ("notes.txt", b"hello"),
            ("__MACOSX/photos/._camera.jpg", b"\0\x05\x16\x07"),
            ("photos/.DS_Store", b"Bud1"),
        ]);
        let (cleaned, findings) = clean(&data);
        assert!(has(
            &findings,
            FindingKind::GpsLocation,
            "photos/camera.jpg"
        ));
        assert!(has(&findings, FindingKind::UnreadFile, "notes.txt"));
        assert!(has(&findings, FindingKind::JunkFile, "photos/.DS_Store"));
        assert!(has(&findings, FindingKind::Timestamp, "ZIP"));
        assert_eq!(findings, inspect(&data), "inspect says what clean does");

        assert_eq!(names(&cleaned), ["photos/camera.jpg", "notes.txt"]);
        assert!(
            removable(&inspect(&cleaned)).is_empty(),
            "{:?}",
            inspect(&cleaned)
        );
        assert_eq!(clean(&cleaned).0, cleaned, "cleaning twice changes nothing");
        assert_eq!(
            clean(&data).0,
            cleaned,
            "the same input gives the same bytes"
        );

        let mut zip = ZipArchive::new(Cursor::new(&cleaned)).unwrap();
        let mut notes = String::new();
        zip.by_name("notes.txt")
            .unwrap()
            .read_to_string(&mut notes)
            .unwrap();
        assert_eq!(notes, "hello");
        assert_eq!(
            zip.by_name("notes.txt").unwrap().unix_mode(),
            Some(0o100644)
        );
    }

    #[test]
    fn archives_inside_archives_are_cleaned_too() {
        let inner = zip_of(&[("camera.jpg", &camera())]);
        let data = zip_of(&[("inner.zip", &inner)]);
        let (cleaned, findings) = clean(&data);
        assert!(findings.iter().any(|f| f.kind == FindingKind::GpsLocation
            && f.location.parts()[..2] == ["inner.zip", "camera.jpg"]));
        assert!(removable(&inspect(&cleaned)).is_empty());
    }

    #[test]
    fn too_deep_archives_are_kept_with_a_warning() {
        let mut data = zip_of(&[("camera.jpg", &camera())]);
        for _ in 0..4 {
            data = zip_of(&[("nested.zip", &data)]);
        }
        let findings = inspect(&data);
        assert!(findings.iter().any(|f| f.kind == FindingKind::UnreadFile));
        assert!(!findings.iter().any(|f| f.kind == FindingKind::GpsLocation));
    }

    #[test]
    fn comments_are_removed() {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        zip.set_comment("made by Jane").unwrap();
        zip.start_file(
            "a.txt",
            zip::write::FullFileOptions::default().with_file_comment("draft from Jane"),
        )
        .unwrap();
        zip.write_all(b"x").unwrap();
        let data = zip.finish().unwrap().into_inner();
        let (cleaned, findings) = clean(&data);
        let comments = findings
            .iter()
            .filter(|f| f.kind == FindingKind::Comment)
            .count();
        assert_eq!(comments, 2);
        let zip = ZipArchive::new(Cursor::new(&cleaned)).unwrap();
        assert!(zip.comment().is_empty());
    }

    #[test]
    fn links_are_kept_and_reported() {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        zip.add_symlink("latest", "/home/jane/photos", SimpleFileOptions::default())
            .unwrap();
        let data = zip.finish().unwrap().into_inner();
        let (cleaned, findings) = clean(&data);
        assert_eq!(findings[0].kind, FindingKind::SymbolicLink);
        assert_eq!(
            findings[0].value,
            Some(Value::Text("/home/jane/photos".into()))
        );
        let mut zip = ZipArchive::new(Cursor::new(&cleaned)).unwrap();
        assert!(zip.by_name("latest").unwrap().is_symlink());
    }

    #[test]
    fn encrypted_archives_are_skipped() {
        let mut data = zip_of(&[("a.txt", b"secret")]);
        // Set the "encrypted" flag in the local and central headers.
        for signature in [&b"PK\x03\x04"[..], b"PK\x01\x02"] {
            let at = data.windows(4).position(|w| w == signature).unwrap();
            let flags = at + if signature[2] == 3 { 6 } else { 8 };
            data[flags] |= 1;
        }
        assert!(matches!(
            Zip.inspect(&mut Cursor::new(&data), &Options::default()),
            Err(FormatError::Encrypted)
        ));
    }

    #[test]
    fn unsafe_and_oversized_archives_are_refused() {
        let traversal = zip_of(&[("../evil.txt", b"x")]);
        assert!(matches!(
            Zip.inspect(&mut Cursor::new(&traversal), &Options::default()),
            Err(FormatError::UnsafePath(name)) if name == "../evil.txt"
        ));
        let bomb = zip_of(&[("zeros", &vec![0u8; 4 * 1024 * 1024])]);
        assert!(matches!(
            Zip.inspect(&mut Cursor::new(&bomb), &Options::default()),
            Err(FormatError::TooLarge { .. })
        ));
    }

    #[test]
    fn large_files_go_through_a_temporary_file() {
        // Above the in-memory limit, and stored so it isn't slow to compress.
        let big: Vec<u8> = (0..20 * 1024 * 1024u32)
            .map(|i| (i * 7 % 251) as u8)
            .collect();
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file(
            "video.bin",
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(&big).unwrap();
        let data = zip.finish().unwrap().into_inner();
        let (cleaned, _) = clean(&data);
        let mut zip = ZipArchive::new(Cursor::new(&cleaned)).unwrap();
        let mut content = Vec::new();
        zip.by_name("video.bin")
            .unwrap()
            .read_to_end(&mut content)
            .unwrap();
        assert!(content == big);
    }

    #[test]
    fn apps_and_ebooks_are_not_archives() {
        let detect_of = |data: Vec<u8>| detect(&data[..12], &mut Cursor::new(&data)).unwrap();
        assert_eq!(detect_of(zip_of(&[("a.txt", b"x")])), Some(FormatId::Zip));
        for marker in ["mimetype", "META-INF/MANIFEST.MF", "AndroidManifest.xml"] {
            assert_eq!(detect_of(zip_of(&[(marker, b"x")])), None, "{marker}");
        }
    }
}
