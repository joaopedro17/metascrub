//! EPUB e-books built by hand: an EPUB 3 book as calibre and Sigil leave
//! them (author, publisher, dates, calibre's leftovers, a cover photo, an
//! obfuscated font, a page that names its editor), an EPUB 2 book whose
//! identifier is its ISBN, and a book with DRM, which is refused.

use std::io::{Cursor, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

const BOOK_ID: &str = "urn:uuid:3f9b6a0e-5c1d-4e2f-9a7b-8c6d5e4f3a2b";
const CONTAINER: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#;

pub struct Files {
    pub book_epub: Vec<u8>,
    pub legacy_epub: Vec<u8>,
    pub drm_epub: Vec<u8>,
}

pub fn files(photo: &[u8]) -> Files {
    Files {
        book_epub: book(photo),
        legacy_epub: legacy(),
        drm_epub: drm(),
    }
}

fn page(title: &str, text: &str, head: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops" lang="en" xml:lang="en"><head><meta charset="utf-8"/>{head}<title>{title}</title></head><body><p>{text}</p></body></html>"#
    )
}

const NAV: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops" lang="en" xml:lang="en"><head><title>Contents</title></head><body><nav epub:type="toc" id="toc"><h1>Contents</h1><ol><li><a href="chapter.xhtml">Holiday</a></li></ol></nav></body></html>"#;

fn ncx(id: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1"><head><meta name="dtb:uid" content="{id}"/><meta name="dtb:depth" content="1"/><meta name="dtb:totalPageCount" content="0"/><meta name="dtb:maxPageNumber" content="0"/><meta name="dtb:generator" content="calibre (7.2.0)"/></head><docTitle><text>Holiday</text></docTitle><docAuthor><text>Jane Doe</text></docAuthor><navMap><navPoint id="n1" playOrder="1"><navLabel><text>Holiday</text></navLabel><content src="chapter.xhtml"/></navPoint></navMap></ncx>"#
    )
}

/// A stand-in font: obfuscation only scrambles its first 1040 bytes, and
/// no reader opens it in these tests.
fn font() -> Vec<u8> {
    (0..2048u32).map(|i| (i % 251) as u8).collect()
}

/// IDPF font obfuscation: the start XORed with SHA-1 of the identifier.
fn obfuscate(data: &[u8], id: &str) -> Vec<u8> {
    let key = sha1(id.as_bytes());
    data.iter()
        .enumerate()
        .map(|(i, b)| if i < 1040 { b ^ key[i % 20] } else { *b })
        .collect()
}

fn book(photo: &[u8]) -> Vec<u8> {
    let opf = format!(
        r##"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="bookid" xml:lang="en">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf">
<dc:title>Holiday</dc:title>
<dc:language>en</dc:language>
<dc:identifier id="bookid">{BOOK_ID}</dc:identifier>
<dc:identifier id="isbn">urn:isbn:9780141036144</dc:identifier>
<dc:creator id="author">Jane Doe</dc:creator>
<meta refines="#author" property="role" scheme="marc:relators">aut</meta>
<meta refines="#author" property="file-as">Doe, Jane</meta>
<dc:publisher>Doe Press</dc:publisher>
<dc:contributor>calibre (7.2.0) [https://calibre-ebook.com]</dc:contributor>
<dc:date>2026-03-14</dc:date>
<dc:description>Written at home in Lisbon.</dc:description>
<meta name="calibre:timestamp" content="2026-03-14T09:26:00+00:00"/>
<meta name="cover" content="cover"/>
<meta property="dcterms:modified">2026-03-14T09:26:52Z</meta>
</metadata>
<manifest>
<item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
<item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>
<item id="cover" href="cover.jpg" media-type="image/jpeg" properties="cover-image"/>
<item id="font" href="serif.otf" media-type="font/otf"/>
<item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
</manifest>
<spine toc="ncx"><itemref idref="chapter"/></spine>
</package>"##
    );
    let encryption = r#"<?xml version="1.0" encoding="UTF-8"?>
<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container" xmlns:enc="http://www.w3.org/2001/04/xmlenc#"><enc:EncryptedData><enc:EncryptionMethod Algorithm="http://www.idpf.org/2008/embedding"/><enc:CipherData><enc:CipherReference URI="OEBPS/serif.otf"/></enc:CipherData></enc:EncryptedData></encryption>"#;
    zip(&[
        ("META-INF/container.xml", CONTAINER.as_bytes().to_vec()),
        ("META-INF/encryption.xml", encryption.as_bytes().to_vec()),
        (
            "META-INF/com.apple.ibooks.display-options.xml",
            br#"<?xml version="1.0" encoding="UTF-8"?><display_options><platform name="*"><option name="specified-fonts">true</option></platform></display_options>"#.to_vec(),
        ),
        ("OEBPS/content.opf", opf.into_bytes()),
        ("OEBPS/nav.xhtml", NAV.as_bytes().to_vec()),
        (
            "OEBPS/chapter.xhtml",
            page(
                "Holiday",
                "We left on a Tuesday.",
                r#"<meta name="generator" content="Sigil 2.0.2"/><meta name="author" content="Jane Doe"/>"#,
            )
            .into_bytes(),
        ),
        ("OEBPS/cover.jpg", photo.to_vec()),
        ("OEBPS/serif.otf", obfuscate(&font(), BOOK_ID)),
        ("OEBPS/toc.ncx", ncx(BOOK_ID).into_bytes()),
        ("calibre_bookmarks.txt", b"encoding=json+base64:\neyJwb3MiOiAxMn0=".to_vec()),
    ])
}

/// EPUB 2, with the ISBN as the book's identifier.
fn legacy() -> Vec<u8> {
    let isbn = "9780141036144";
    let opf = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="BookId">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf">
<dc:title>Holiday</dc:title>
<dc:language>en</dc:language>
<dc:identifier id="BookId" opf:scheme="ISBN">{isbn}</dc:identifier>
<dc:creator opf:role="aut" opf:file-as="Doe, Jane">Jane Doe</dc:creator>
<dc:publisher>Doe Press</dc:publisher>
<dc:date opf:event="publication">2026-03-14</dc:date>
<meta name="Sigil version" content="0.9.18"/>
</metadata>
<manifest>
<item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>
<item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
</manifest>
<spine toc="ncx"><itemref idref="chapter"/></spine>
</package>"#
    );
    let page = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.1//EN" "http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd">
<html xmlns="http://www.w3.org/1999/xhtml"><head><meta name="generator" content="Sigil 0.9.18"/><title>Holiday</title></head><body><p>We left on a Tuesday.</p></body></html>"#;
    zip(&[
        ("META-INF/container.xml", CONTAINER.as_bytes().to_vec()),
        ("OEBPS/content.opf", opf.into_bytes()),
        ("OEBPS/chapter.xhtml", page.as_bytes().to_vec()),
        ("OEBPS/toc.ncx", ncx(isbn).into_bytes()),
    ])
}

/// Encrypted with a key MetaScrub doesn't have.
fn drm() -> Vec<u8> {
    let encryption = r#"<?xml version="1.0" encoding="UTF-8"?>
<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container" xmlns:enc="http://www.w3.org/2001/04/xmlenc#"><enc:EncryptedData><enc:EncryptionMethod Algorithm="http://www.w3.org/2001/04/xmlenc#aes128-cbc"/><enc:CipherData><enc:CipherReference URI="OEBPS/chapter.xhtml"/></enc:CipherData></enc:EncryptedData></encryption>"#;
    let opf = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="bookid"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Holiday</dc:title><dc:language>en</dc:language><dc:identifier id="bookid">{BOOK_ID}</dc:identifier><meta property="dcterms:modified">2026-03-14T09:26:52Z</meta></metadata><manifest><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter"/></spine></package>"#
    );
    zip(&[
        ("META-INF/container.xml", CONTAINER.as_bytes().to_vec()),
        ("META-INF/encryption.xml", encryption.as_bytes().to_vec()),
        ("OEBPS/content.opf", opf.into_bytes()),
        ("OEBPS/nav.xhtml", NAV.as_bytes().to_vec()),
        ("OEBPS/chapter.xhtml", vec![0x8F; 256]),
    ])
}

/// `mimetype` first and stored, as EPUB requires; dated 2026-03-14.
fn zip(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let date = DateTime::from_date_and_time(2026, 3, 14, 9, 26, 52).unwrap();
    let options = SimpleFileOptions::default()
        .last_modified_time(date)
        .system(zip::System::Dos);
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(
        "mimetype",
        options.compression_method(CompressionMethod::Stored),
    )
    .unwrap();
    zip.write_all(b"application/epub+zip").unwrap();
    for (name, data) in entries {
        zip.start_file(
            *name,
            options.compression_method(CompressionMethod::Deflated),
        )
        .unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

/// SHA-1 (FIPS 180-4), written out here so the fixtures don't depend on
/// MetaScrub's own font code.
fn sha1(message: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let mut data = message.to_vec();
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&((message.len() as u64) * 8).to_be_bytes());
    for block in data.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(block[4 * i..4 * i + 4].try_into().unwrap());
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            (e, d, c, b, a) = (d, c, b.rotate_left(30), a, t);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_matches_the_standard() {
        let hex: String = sha1(b"abc").iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "a9993e364706816aba3e25717850c26c9cd0d89d");
    }
}
