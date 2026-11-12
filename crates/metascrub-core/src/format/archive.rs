//! ZIP containers: reading with limits, and writing in a fixed, neutral form.
//! Office files are ZIPs read whole into memory; ZIP archives, which can be
//! much larger, are read one file at a time instead (`zip_archive`).

use std::collections::HashSet;
use std::io::{Cursor, Read, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, System, ZipArchive, ZipWriter};

use super::FormatError;
use crate::limits::{
    MAX_ZIP_ENTRIES, MAX_ZIP_RATIO, MAX_ZIP_UNPACKED_BYTES, ZIP_RATIO_CHECK_ABOVE,
};
use crate::model::{Disposition, Finding, FindingKind, Location, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) name: String,
    pub(crate) data: Vec<u8>,
}

#[derive(Debug)]
pub(crate) struct Archive {
    pub(crate) entries: Vec<Entry>,
    /// The archive's own comment, free text chosen by whatever wrote it.
    pub(crate) comment: Vec<u8>,
    /// The latest entry date, unless every entry has the neutral 1980-01-01.
    pub(crate) latest_date: Option<DateTime>,
}

pub(crate) fn read(data: &[u8]) -> Result<Archive, FormatError> {
    let mut zip = ZipArchive::new(Cursor::new(data)).map_err(zip_error)?;
    if zip.len() > MAX_ZIP_ENTRIES {
        return Err(FormatError::TooLarge {
            limit: MAX_ZIP_ENTRIES as u64,
        });
    }
    let comment = zip.comment().to_vec();
    let mut entries = Vec::with_capacity(zip.len());
    let mut names = HashSet::new();
    let mut budget = MAX_ZIP_UNPACKED_BYTES;
    let mut latest_date: Option<DateTime> = None;
    for index in 0..zip.len() {
        let mut file = zip.by_index(index).map_err(zip_error)?;
        if file.encrypted() {
            return Err(FormatError::Unsupported("encrypted ZIP entry".into()));
        }
        if !matches!(
            file.compression(),
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            return Err(FormatError::Unsupported(format!(
                "ZIP compression method {}",
                file.compression()
            )));
        }
        let name = file.name().map_err(zip_error)?.into_owned();
        check_name(&name)?;
        // Part names are compared without regard to case, so two entries that
        // differ only in case could be read differently by different programs.
        if !names.insert(name.to_lowercase()) {
            return Err(FormatError::malformed(format!(
                "duplicate ZIP entry {name}"
            )));
        }
        if let Some(date) = file.last_modified().filter(|d| *d != DateTime::default()) {
            latest_date = latest_date.max(Some(date));
        }
        if file.is_dir() {
            continue;
        }
        let compressed = file.compressed_size();
        let mut data = Vec::new();
        Read::by_ref(&mut file)
            .take(budget.saturating_add(1))
            .read_to_end(&mut data)
            .map_err(|e| FormatError::malformed(format!("ZIP entry {name}: {e}")))?;
        let size = data.len() as u64;
        if size > budget {
            return Err(FormatError::TooLarge {
                limit: MAX_ZIP_UNPACKED_BYTES,
            });
        }
        if size > ZIP_RATIO_CHECK_ABOVE && size / compressed.max(1) > MAX_ZIP_RATIO {
            return Err(FormatError::TooLarge {
                limit: compressed.saturating_mul(MAX_ZIP_RATIO),
            });
        }
        budget -= size;
        entries.push(Entry { name, data });
    }
    Ok(Archive {
        entries,
        comment,
        latest_date,
    })
}

impl Archive {
    /// The archive's comment and file dates, which cleaning resets.
    pub(crate) fn findings(&self) -> Vec<Finding> {
        let mut findings = Vec::new();
        let comment = String::from_utf8_lossy(&self.comment).trim().to_owned();
        if !comment.is_empty() {
            findings.push(
                Finding::new(
                    FindingKind::Comment,
                    Location::new(["ZIP"]),
                    Disposition::Remove,
                )
                .with_value(Value::Text(comment)),
            );
        }
        if let Some(date) = self.latest_date {
            findings.push(
                Finding::new(
                    FindingKind::Timestamp,
                    Location::new(["ZIP", "File dates"]),
                    Disposition::Remove,
                )
                .with_value(Value::Text(date.to_string())),
            );
        }
        findings
    }
}

/// Entry names must be relative paths that stay inside the archive.
pub(super) fn check_name(name: &str) -> Result<(), FormatError> {
    let unsafe_name = name.is_empty()
        || name.starts_with('/')
        || name.contains('\\')
        || name.contains(':')
        || name.split('/').any(|part| part == "..");
    if unsafe_name {
        return Err(FormatError::UnsafePath(name.to_owned()));
    }
    Ok(())
}

/// Writes `entries` in the given order, all dated 1980-01-01, with no
/// comments, extra fields or operating-system details, so the same content
/// always gives the same bytes. A `mimetype` entry is stored uncompressed,
/// as OpenDocument and EPUB require of their first entry.
pub(crate) fn write(entries: &[Entry], out: &mut dyn Write) -> Result<(), FormatError> {
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(6))
        .last_modified_time(DateTime::default())
        .system(System::Dos);
    let stored = options
        .compression_method(CompressionMethod::Stored)
        .compression_level(None);
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for entry in entries {
        let options = if entry.name == "mimetype" {
            stored
        } else {
            options
        };
        zip.start_file(entry.name.as_str(), options)
            .map_err(zip_error)?;
        zip.write_all(&entry.data)?;
    }
    let buffer = zip.finish().map_err(zip_error)?.into_inner();
    out.write_all(&buffer)?;
    Ok(())
}

pub(crate) fn zip_error(error: zip::result::ZipError) -> FormatError {
    match error {
        zip::result::ZipError::Io(error) => FormatError::Io(error),
        zip::result::ZipError::UnsupportedArchive(detail) => {
            FormatError::Unsupported(detail.to_string())
        }
        other => FormatError::malformed(format!("ZIP: {other}")),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .last_modified_time(DateTime::from_date_and_time(2026, 3, 14, 9, 26, 52).unwrap());
        for (name, data) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn reads_entries_and_reports_dates() {
        let archive = read(&zip_of(&[("a.txt", b"hello"), ("b/c.txt", b"world")])).unwrap();
        let names: Vec<_> = archive.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["a.txt", "b/c.txt"]);
        assert_eq!(archive.entries[0].data, b"hello");
        assert_eq!(
            archive.latest_date.map(|d| d.to_string()).as_deref(),
            Some("2026-03-14 09:26:52")
        );
    }

    #[test]
    fn writing_is_neutral_and_deterministic() {
        let entries = vec![Entry {
            name: "a.txt".into(),
            data: b"hello".to_vec(),
        }];
        let mut first = Vec::new();
        write(&entries, &mut first).unwrap();
        let mut second = Vec::new();
        write(&entries, &mut second).unwrap();
        assert_eq!(first, second);
        let archive = read(&first).unwrap();
        assert_eq!(archive.entries, entries);
        assert_eq!(archive.latest_date, None);
        assert!(archive.comment.is_empty());
    }

    #[test]
    fn unsafe_names_are_refused() {
        for name in ["../evil", "/abs", "a\\b", "c:x", "a/../../b"] {
            assert!(check_name(name).is_err(), "{name}");
        }
        assert!(check_name("word/document.xml").is_ok());
    }

    #[test]
    fn names_differing_only_in_case_are_refused() {
        let data = zip_of(&[("A.xml", b"1"), ("a.xml", b"2")]);
        assert!(matches!(read(&data), Err(FormatError::Malformed(_))));
    }

    #[test]
    fn zip_bombs_are_refused() {
        let bomb = vec![0u8; 2 * 1024 * 1024];
        let data = zip_of(&[("zeros", &bomb)]);
        assert!(data.len() < 20_000);
        assert!(matches!(read(&data), Err(FormatError::TooLarge { .. })));
    }

    #[test]
    fn damaged_archives_are_errors() {
        let data = zip_of(&[("a.txt", b"hello")]);
        for len in [0, 10, data.len() / 2, data.len() - 1] {
            assert!(read(&data[..len]).is_err(), "length {len}");
        }
    }
}
