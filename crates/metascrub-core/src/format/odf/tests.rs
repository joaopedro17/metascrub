use std::io::Cursor;

use super::*;

fn package(entries: &[(&str, &str)]) -> Vec<u8> {
    let entries: Vec<Entry> = entries
        .iter()
        .map(|(name, data)| Entry {
            name: (*name).to_owned(),
            data: data.as_bytes().to_vec(),
        })
        .collect();
    let mut out = Vec::new();
    archive::write(&entries, &mut out).unwrap();
    out
}

fn manifest(paths: &[&str]) -> String {
    let entries: String = paths
        .iter()
        .map(|p| {
            format!(
                r#"<manifest:file-entry manifest:full-path="{p}" manifest:media-type="text/xml"/>"#
            )
        })
        .collect();
    format!(
        r#"<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0"><manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.text"/>{entries}</manifest:manifest>"#
    )
}

const MIMETYPE: &str = "application/vnd.oasis.opendocument.text";
const CONTENT: &str = r#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:officeooo="http://openoffice.org/2009/office"><office:automatic-styles><style:style style:name="P1" style:family="paragraph"><style:text-properties officeooo:rsid="0012ab34" officeooo:paragraph-rsid="0012ab34"/></style:style></office:automatic-styles><office:body><office:text><text:p text:style-name="P1">Hello<office:annotation office:name="c1"><dc:creator>Jane Doe</dc:creator><dc:date>2026-03-14T09:30:00</dc:date><meta:creator-initials>JD</meta:creator-initials><text:p>Check with Acme</text:p></office:annotation> world<office:annotation-end office:name="c1"/></text:p></office:text></office:body></office:document-content>"#;
const META: &str = r#"<office:document-meta xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/"><office:meta><dc:creator>Jane Doe</dc:creator></office:meta></office:document-meta>"#;

fn clean_with(data: &[u8], options: &Options) -> Result<(Vec<u8>, Vec<Finding>), FormatError> {
    let mut out = Vec::new();
    let findings = Odf.clean(&mut Cursor::new(data), &mut out, options)?;
    Ok((out, findings))
}

fn entry_text(data: &[u8], name: &str) -> Option<String> {
    let archive = archive::read(data).unwrap();
    archive
        .entries
        .into_iter()
        .find(|e| e.name == name)
        .map(|e| String::from_utf8(e.data).unwrap())
}

fn names(data: &[u8]) -> Vec<String> {
    archive::read(data)
        .unwrap()
        .entries
        .into_iter()
        .map(|e| e.name)
        .collect()
}

fn sample() -> Vec<u8> {
    package(&[
        ("mimetype", MIMETYPE),
        ("content.xml", CONTENT),
        ("meta.xml", META),
        ("Thumbnails/thumbnail.png", "png"),
        ("stray.txt", "left over"),
        (
            MANIFEST_PATH,
            &manifest(&["content.xml", "meta.xml", "Thumbnails/thumbnail.png"]),
        ),
    ])
}

#[test]
fn properties_thumbnail_comments_and_session_ids_go() {
    let (out, findings) = clean_with(&sample(), &Options::default()).unwrap();
    assert_eq!(names(&out), ["mimetype", "content.xml", MANIFEST_PATH]);
    let content = entry_text(&out, "content.xml").unwrap();
    assert!(!content.contains("annotation") && !content.contains("rsid"));
    assert!(content.contains("Hello") && content.contains(" world"));
    let manifest = entry_text(&out, MANIFEST_PATH).unwrap();
    assert!(!manifest.contains("meta.xml") && !manifest.contains("Thumbnails"));
    let kinds: HashSet<_> = findings.iter().map(|f| f.kind).collect();
    for kind in [
        FindingKind::Comment,
        FindingKind::PersonName,
        FindingKind::Timestamp,
        FindingKind::DocumentId,
        FindingKind::Thumbnail,
        FindingKind::UnknownMetadata,
    ] {
        assert!(kinds.contains(&kind), "{kind:?}");
    }
    let (again, none) = clean_with(&out, &Options::default()).unwrap();
    assert_eq!((again, none), (out, Vec::new()));
}

#[test]
fn mimetype_comes_first_and_uncompressed() {
    let (out, _) = clean_with(&sample(), &Options::default()).unwrap();
    // The local header's name, then the content right after it.
    assert_eq!(&out[30..38], b"mimetype");
    assert_eq!(&out[38..38 + MIMETYPE.len()], MIMETYPE.as_bytes());
}

#[test]
fn kept_comments_get_a_neutral_author() {
    let options = Options {
        keep_comments: true,
        ..Options::default()
    };
    let (out, findings) = clean_with(&sample(), &options).unwrap();
    let content = entry_text(&out, "content.xml").unwrap();
    assert!(content.contains("<dc:creator>Author</dc:creator>"));
    assert!(content.contains("<meta:creator-initials>A</meta:creator-initials>"));
    assert!(!content.contains("dc:date") && content.contains("Check with Acme"));
    assert!(findings.iter().any(|f| f.kind == FindingKind::Comment
        && f.disposition == Disposition::Keep(crate::model::KeepReason::UserRequested)));
    let (_, again) = clean_with(&out, &options).unwrap();
    assert!(
        again
            .iter()
            .all(|f| f.disposition.is_acceptable_after_cleaning())
    );
}

#[test]
fn signed_encrypted_and_tracked_files_are_left_alone() {
    let signed = package(&[
        ("mimetype", MIMETYPE),
        ("content.xml", CONTENT),
        ("META-INF/documentsignatures.xml", "<x/>"),
        (MANIFEST_PATH, &manifest(&["content.xml"])),
    ]);
    assert!(matches!(
        clean_with(&signed, &Options::default()),
        Err(FormatError::Unsupported(_))
    ));
    let encrypted_manifest = manifest(&["content.xml"]).replace(
        r#"media-type="text/xml"/>"#,
        r#"media-type="text/xml"><manifest:encryption-data/></manifest:file-entry>"#,
    );
    let encrypted = package(&[
        ("mimetype", MIMETYPE),
        ("content.xml", "binary"),
        (MANIFEST_PATH, &encrypted_manifest),
    ]);
    assert!(matches!(
        clean_with(&encrypted, &Options::default()),
        Err(FormatError::Encrypted)
    ));
    let tracked_content = CONTENT.replace(
        "<office:text>",
        "<office:text><text:tracked-changes><text:changed-region/></text:tracked-changes>",
    );
    let tracked = package(&[
        ("mimetype", MIMETYPE),
        ("content.xml", &tracked_content),
        (MANIFEST_PATH, &manifest(&["content.xml"])),
    ]);
    assert!(matches!(
        clean_with(&tracked, &Options::default()),
        Err(FormatError::Unsupported(_))
    ));
}

#[test]
fn recognizes_documents_and_templates() {
    assert_eq!(format(MIMETYPE), Some(FormatId::Odt));
    assert_eq!(
        format("application/vnd.oasis.opendocument.spreadsheet-template"),
        Some(FormatId::Ods)
    );
    assert_eq!(format("application/vnd.oasis.opendocument.formula"), None);
}
