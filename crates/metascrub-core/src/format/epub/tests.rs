use std::io::Cursor;

use super::*;
use crate::model::KeepReason;

const BOOK_ID: &str = "urn:uuid:12345678-1234-4234-8234-123456789abc";

fn opf(identifier: &str) -> String {
    format!(
        r##"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="bookid">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf">
<dc:title>Holiday</dc:title>
<dc:language>en</dc:language>
<dc:identifier id="bookid">{identifier}</dc:identifier>
<dc:identifier id="isbn">urn:isbn:9780141036144</dc:identifier>
<meta refines="#isbn" property="identifier-type">15</meta>
<dc:creator id="author">Jane Doe</dc:creator>
<meta refines="#author" property="file-as">Doe, Jane</meta>
<dc:publisher>Doe Press</dc:publisher>
<dc:contributor>calibre (7.2.0) [https://calibre-ebook.com]</dc:contributor>
<dc:date>2026-03-14</dc:date>
<dc:description>Written at home.</dc:description>
<meta name="calibre:timestamp" content="2026-03-14T09:26:00+00:00"/>
<meta name="cover" content="cover"/>
<meta property="rendition:layout">reflowable</meta>
<meta property="dcterms:modified">2026-03-14T09:26:52Z</meta>
</metadata>
<manifest>
<item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
<item id="ch1" href="Text/ch1.xhtml" media-type="application/xhtml+xml"/>
<item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
<item id="font" href="Fonts/serif.otf" media-type="font/otf"/>
</manifest>
<spine toc="ncx"><itemref idref="ch1"/></spine>
</package>"##
    )
}

const CONTAINER_XML: &str = r#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#;
const PAGE: &str = r#"<?xml version="1.0" encoding="UTF-8"?><html xmlns="http://www.w3.org/1999/xhtml"><head><meta charset="utf-8"/><meta name="generator" content="Sigil 2.0"/><meta name="viewport" content="width=600, height=800"/><title>One</title></head><body><p>Once upon a time.</p></body></html>"#;
const NAV: &str = r#"<?xml version="1.0" encoding="UTF-8"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>Contents</title></head><body><nav><ol><li><a href="Text/ch1.xhtml">One</a></li></ol></nav></body></html>"#;
const ENCRYPTION_XML: &str = r#"<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container" xmlns:enc="http://www.w3.org/2001/04/xmlenc#"><enc:EncryptedData><enc:EncryptionMethod Algorithm="http://www.idpf.org/2008/embedding"/><enc:CipherData><enc:CipherReference URI="OEBPS/Fonts/serif.otf"/></enc:CipherData></enc:EncryptedData></encryption>"#;

fn ncx(id: &str) -> String {
    format!(
        r#"<?xml version="1.0"?><ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1"><head><meta name="dtb:uid" content="{id}"/><meta name="dtb:depth" content="1"/><meta name="dtb:generator" content="calibre (7.2.0)"/></head><docTitle><text>Holiday</text></docTitle><docAuthor><text>Jane Doe</text></docAuthor><navMap><navPoint id="n1" playOrder="1"><navLabel><text>One</text></navLabel><content src="Text/ch1.xhtml"/></navPoint></navMap></ncx>"#
    )
}

/// A font's plain bytes: 2 KB, so the obfuscated first 1040 bytes and the
/// plain rest are both there.
fn font() -> Vec<u8> {
    (0..2048u32).map(|i| (i % 251) as u8).collect()
}

fn book() -> Vec<u8> {
    let key = key(Obfuscation::Idpf, BOOK_ID, None).unwrap();
    let entries = [
        ("mimetype", MIMETYPE.as_bytes().to_vec()),
        (CONTAINER_PATH, CONTAINER_XML.as_bytes().to_vec()),
        (ENCRYPTION_PATH, ENCRYPTION_XML.as_bytes().to_vec()),
        ("OEBPS/content.opf", opf(BOOK_ID).into_bytes()),
        ("OEBPS/nav.xhtml", NAV.as_bytes().to_vec()),
        ("OEBPS/Text/ch1.xhtml", PAGE.as_bytes().to_vec()),
        ("OEBPS/toc.ncx", ncx(BOOK_ID).into_bytes()),
        (
            "OEBPS/Fonts/serif.otf",
            obfuscate(&font(), &key, Obfuscation::Idpf),
        ),
        ("calibre_bookmarks.txt", b"page 12".to_vec()),
    ];
    let entries: Vec<Entry> = entries
        .into_iter()
        .map(|(name, data)| Entry {
            name: name.to_owned(),
            data,
        })
        .collect();
    let mut out = Vec::new();
    archive::write(&entries, &mut out).unwrap();
    out
}

fn clean_with(data: &[u8], options: &Options) -> Result<(Vec<u8>, Vec<Finding>), FormatError> {
    let mut out = Vec::new();
    let findings = Epub.clean(&mut Cursor::new(data), &mut out, options)?;
    Ok((out, findings))
}

fn file(data: &[u8], name: &str) -> Vec<u8> {
    archive::read(data)
        .unwrap()
        .entries
        .into_iter()
        .find(|e| e.name == name)
        .unwrap()
        .data
}

fn text(data: &[u8], name: &str) -> String {
    String::from_utf8(file(data, name)).unwrap()
}

fn new_id(opf: &str) -> String {
    let start = opf.find(r#"id="bookid">"#).unwrap() + r#"id="bookid">"#.len();
    let end = start + opf[start..].find('<').unwrap();
    opf[start..end].to_owned()
}

#[test]
fn metadata_goes_and_title_language_and_display_settings_stay() {
    let (out, findings) = clean_with(&book(), &Options::default()).unwrap();
    let opf = text(&out, "OEBPS/content.opf");
    for kept in [
        "Holiday",
        "<dc:language>en",
        "rendition:layout",
        r#"name="cover""#,
    ] {
        assert!(opf.contains(kept), "{kept} missing");
    }
    for gone in [
        "Jane Doe",
        "Doe, Jane",
        "Doe Press",
        "calibre",
        "9780141036144",
        "identifier-type",
        "Written at home",
        "2026-03-14",
    ] {
        assert!(!opf.contains(gone), "{gone} still there");
    }
    assert!(opf.contains("1980-01-01T00:00:00Z"));
    let page = text(&out, "OEBPS/Text/ch1.xhtml");
    assert!(!page.contains("Sigil") && page.contains("viewport") && page.contains("charset"));
    let ncx = text(&out, "OEBPS/toc.ncx");
    assert!(!ncx.contains("calibre") && !ncx.contains("Jane Doe"));
    assert!(
        archive::read(&out)
            .unwrap()
            .entries
            .iter()
            .all(|e| e.name != "calibre_bookmarks.txt")
    );
    let kinds: HashSet<_> = findings.iter().map(|f| f.kind).collect();
    for kind in [
        FindingKind::PersonName,
        FindingKind::Organization,
        FindingKind::Software,
        FindingKind::Timestamp,
        FindingKind::DocumentId,
        FindingKind::Description,
        FindingKind::UnknownMetadata,
    ] {
        assert!(kinds.contains(&kind), "{kind:?}");
    }
}

#[test]
fn the_identifier_comes_from_the_content_and_fonts_follow_it() {
    let (out, _) = clean_with(&book(), &Options::default()).unwrap();
    let id = new_id(&text(&out, "OEBPS/content.opf"));
    assert!(id.starts_with("urn:uuid:") && id != BOOK_ID, "{id}");
    assert!(text(&out, "OEBPS/toc.ncx").contains(&id));
    // The font, unscrambled with the new identifier, is the original font.
    let new_key = key(Obfuscation::Idpf, &id, None).unwrap();
    let font_out = file(&out, "OEBPS/Fonts/serif.otf");
    assert_eq!(obfuscate(&font_out, &new_key, Obfuscation::Idpf), font());
    // Cleaning again changes nothing and finds nothing.
    let (again, findings) = clean_with(&out, &Options::default()).unwrap();
    assert_eq!(again, out);
    assert!(findings.is_empty(), "{findings:?}");
}

#[test]
fn book_info_can_be_kept() {
    let options = Options {
        keep_book_info: true,
        ..Options::default()
    };
    let (out, findings) = clean_with(&book(), &options).unwrap();
    let opf = text(&out, "OEBPS/content.opf");
    for kept in ["Jane Doe", "Doe, Jane", "Doe Press", "9780141036144"] {
        assert!(opf.contains(kept), "{kept} missing");
    }
    assert!(!opf.contains("calibre") && !opf.contains("Written at home"));
    assert!(text(&out, "OEBPS/toc.ncx").contains("Jane Doe"));
    assert!(findings.iter().any(|f| f.kind == FindingKind::PersonName
        && f.disposition == Disposition::Keep(KeepReason::UserRequested)));
    let (_, again) = clean_with(&out, &options).unwrap();
    assert!(
        again
            .iter()
            .all(|f| f.disposition.is_acceptable_after_cleaning()),
        "{again:?}"
    );
}

#[test]
fn drm_is_refused() {
    let drm = ENCRYPTION_XML.replace(IDPF_FONTS, "http://www.w3.org/2001/04/xmlenc#aes128-cbc");
    let book = book();
    let mut entries = archive::read(&book).unwrap().entries;
    for entry in &mut entries {
        if entry.name == ENCRYPTION_PATH {
            entry.data = drm.clone().into_bytes();
        }
    }
    let mut data = Vec::new();
    archive::write(&entries, &mut data).unwrap();
    assert!(matches!(
        clean_with(&data, &Options::default()),
        Err(FormatError::Unsupported(_))
    ));
}

#[test]
fn identifiers_are_valid_uuids() {
    let mut files = BTreeMap::new();
    files.insert("a".to_owned(), b"x".to_vec());
    let id = content_id(&files);
    let uuid = id.strip_prefix("urn:uuid:").unwrap();
    assert_eq!(uuid.len(), 36);
    assert_eq!(&uuid[14..15], "8");
    assert!(matches!(&uuid[19..20], "8" | "9" | "a" | "b"));
    files.insert("a".to_owned(), b"y".to_vec());
    assert_ne!(content_id(&files), id);
}
