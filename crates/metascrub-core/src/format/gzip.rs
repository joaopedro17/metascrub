//! gzip-compressed files (`.tar.gz`, `.gz`): the file inside is cleaned with
//! the same rules as a file on disk, and compressed again under a header
//! without the original file name or a date.

use std::io::{BufReader, SeekFrom, Write};

use flate2::read::MultiGzDecoder;
use flate2::{Compression, GzBuilder};

use super::unpack::{self, Allowance};
use super::{FormatError, FormatHandler, ReadSeek};
use crate::model::{Disposition, Finding, FindingKind, Location, Value};
use crate::options::Options;

/// "Unknown", the value that says nothing about the system that wrote it.
const UNKNOWN_SYSTEM: u8 = 255;

#[derive(Debug)]
pub(super) struct Gzip;

impl FormatHandler for Gzip {
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

fn process(
    input: &mut dyn ReadSeek,
    output: Option<&mut dyn Write>,
    options: &Options,
) -> Result<Vec<Finding>, FormatError> {
    let _allowance = Allowance::start();
    let compressed = input.seek(SeekFrom::End(0))?;
    input.seek(SeekFrom::Start(0))?;
    let mut decoder = MultiGzDecoder::new(BufReader::new(input));
    let data = unpack::unpack(&mut decoder, Some(compressed))?;

    let mut findings = Vec::new();
    let location = |part: &str| Location::new(["gzip", part]);
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).trim().to_owned();
    let mut name = "content".to_owned();
    if let Some(header) = decoder.header() {
        if let Some(original) = header.filename().map(text).filter(|n| !n.is_empty()) {
            findings.push(
                Finding::new(
                    FindingKind::FilePath,
                    location("File name"),
                    Disposition::Remove,
                )
                .with_value(Value::Text(original.clone())),
            );
            name = original;
        }
        if header.mtime() != 0 {
            findings.push(
                Finding::new(
                    FindingKind::Timestamp,
                    location("Date"),
                    Disposition::Remove,
                )
                .with_value(Value::Text(unpack::utc(u64::from(header.mtime())))),
            );
        }
        if let Some(comment) = header.comment().map(text).filter(|c| !c.is_empty()) {
            findings.push(
                Finding::new(
                    FindingKind::Comment,
                    location("Comment"),
                    Disposition::Remove,
                )
                .with_value(Value::Text(comment)),
            );
        }
        if header.extra().is_some_and(|e| !e.is_empty()) {
            findings.push(Finding::new(
                FindingKind::UnknownMetadata,
                location("Extra field"),
                Disposition::Remove,
            ));
        }
    }

    let mut unpacked = unpack::entry(&name, data, options, output.is_some())?;
    // The file inside is the file itself, not something embedded in it.
    for finding in &mut unpacked.findings {
        finding.location = Location::new(finding.location.parts().iter().skip(1));
        if finding.location.parts().is_empty() {
            finding.location = Location::new(["gzip", "Content"]);
        }
    }
    findings.append(&mut unpacked.findings);
    if let Some(output) = output {
        let mut encoder = GzBuilder::new()
            .mtime(0)
            .operating_system(UNKNOWN_SYSTEM)
            .write(output, Compression::new(6));
        unpack::copy_out(&mut unpacked.content, &mut encoder)?;
        encoder.finish()?;
    }
    Ok(findings)
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read};

    use flate2::write::GzEncoder;

    use super::*;

    fn gzip(data: &[u8], name: Option<&str>) -> Vec<u8> {
        let mut builder = GzBuilder::new().mtime(1_730_563_200).comment("backup");
        if let Some(name) = name {
            builder = builder.filename(name);
        }
        let mut encoder = builder.write(Vec::new(), Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    fn clean(data: &[u8]) -> (Vec<u8>, Vec<Finding>) {
        let mut out = Vec::new();
        let findings = Gzip
            .clean(&mut Cursor::new(data), &mut out, &Options::default())
            .unwrap();
        (out, findings)
    }

    #[test]
    fn the_header_is_neutral_and_the_content_cleaned() {
        let photo = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/generated/jpeg/camera.jpg"),
        )
        .unwrap();
        let data = gzip(&photo, Some("IMG_20261102.jpg"));
        let (cleaned, findings) = clean(&data);
        let kinds: Vec<_> = findings.iter().map(|f| f.kind).collect();
        for kind in [
            FindingKind::FilePath,
            FindingKind::Timestamp,
            FindingKind::Comment,
            FindingKind::GpsLocation,
        ] {
            assert!(kinds.contains(&kind), "{kind:?}");
        }
        let gps = findings
            .iter()
            .find(|f| f.kind == FindingKind::GpsLocation)
            .unwrap();
        assert_eq!(
            gps.location.parts().first().map(String::as_str),
            Some("EXIF")
        );

        // Header: magic, method, no flags, no date, default level, unknown system.
        assert_eq!(cleaned[..10], [0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255]);
        let removable: Vec<_> = Gzip
            .inspect(&mut Cursor::new(&cleaned), &Options::default())
            .unwrap()
            .into_iter()
            .filter(|f| !f.disposition.is_acceptable_after_cleaning())
            .collect();
        assert!(removable.is_empty(), "{removable:?}");
        assert_eq!(clean(&cleaned).0, cleaned);
    }

    #[test]
    fn unreadable_content_is_kept() {
        let (cleaned, findings) = clean(&gzip(b"hello", None));
        assert!(findings.iter().any(|f| f.kind == FindingKind::UnreadFile));
        let mut text = String::new();
        MultiGzDecoder::new(cleaned.as_slice())
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "hello");
    }

    #[test]
    fn gzip_bombs_are_refused() {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&vec![0u8; 8 * 1024 * 1024]).unwrap();
        let bomb = encoder.finish().unwrap();
        assert!(matches!(
            Gzip.inspect(&mut Cursor::new(&bomb), &Options::default()),
            Err(FormatError::TooLarge { .. })
        ));
    }

    #[test]
    fn damaged_files_are_errors() {
        let data = gzip(b"hello world", None);
        assert!(
            Gzip.inspect(
                &mut Cursor::new(&data[..data.len() - 4]),
                &Options::default()
            )
            .is_err()
        );
    }
}
