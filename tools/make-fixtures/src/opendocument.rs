//! Minimal OpenDocument files that open in LibreOffice, built by hand so
//! they're the same on every run and every LibreOffice version, with every
//! kind of metadata MetaScrub removes from them.

use std::io::{Cursor, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

const NS: &str = concat!(
    r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" "#,
    r#"xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" "#,
    r#"xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" "#,
    r#"xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" "#,
    r#"xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" "#,
    r#"xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" "#,
    r#"xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" "#,
    r#"xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" "#,
    r#"xmlns:xlink="http://www.w3.org/1999/xlink" "#,
    r#"xmlns:dc="http://purl.org/dc/elements/1.1/" "#,
    r#"xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0" "#,
    r#"xmlns:config="urn:oasis:names:tc:opendocument:xmlns:config:1.0" "#,
    r#"xmlns:officeooo="http://openoffice.org/2009/office" "#,
    r#"office:version="1.3""#,
);
const DECLARATION: &str = r#"<?xml version="1.0" encoding="UTF-8"?>"#;

pub struct Files {
    pub clean_odt: Vec<u8>,
    pub report_odt: Vec<u8>,
    pub comments_odt: Vec<u8>,
    pub tracked_changes_odt: Vec<u8>,
    pub signed_odt: Vec<u8>,
    pub budget_ods: Vec<u8>,
    pub plans_odp: Vec<u8>,
}

#[derive(Default)]
struct Extras<'a> {
    photo: Option<&'a [u8]>,
    thumbnail: Option<&'a [u8]>,
    metadata: bool,
    macros: bool,
    signed: bool,
}

pub fn files(photo: &[u8], thumbnail_png: &[u8]) -> Files {
    let all = Extras {
        photo: Some(photo),
        thumbnail: Some(thumbnail_png),
        metadata: true,
        ..Extras::default()
    };
    Files {
        clean_odt: package(
            "text",
            &text_body("<text:p>Hello.</text:p>"),
            "",
            &Extras::default(),
        ),
        report_odt: package(
            "text",
            &text_body(concat!(
                r#"<text:p text:style-name="P1">Quarterly report.</text:p>"#,
                r#"<text:p><draw:frame draw:name="Photo" text:anchor-type="as-char" svg:width="2cm" svg:height="1.5cm"><draw:image xlink:href="Pictures/photo.jpg" xlink:type="simple" xlink:show="embed" xlink:actuate="onLoad"/></draw:frame></text:p>"#,
                r#"<text:p>Totals<text:hidden-text text:condition="ooow:1" text:string-value="Salary review for Jane: 90k"/></text:p>"#,
            )),
            r#"<style:style style:name="P1" style:family="paragraph"><style:text-properties officeooo:rsid="000a1b2c" officeooo:paragraph-rsid="000a1b2c"/></style:style>"#,
            &all,
        ),
        comments_odt: package(
            "text",
            &text_body(
                r#"<text:p>Draft.<office:annotation office:name="c1"><dc:creator>Jane Doe</dc:creator><dc:date>2026-03-14T09:30:00</dc:date><meta:creator-initials>JD</meta:creator-initials><text:p>Check the numbers with Acme first.</text:p></office:annotation> Numbers<office:annotation-end office:name="c1"/>.</text:p>"#,
            ),
            "",
            &Extras {
                metadata: true,
                ..Extras::default()
            },
        ),
        tracked_changes_odt: package(
            "text",
            &text_body(concat!(
                r#"<text:tracked-changes><text:changed-region text:id="ct1"><text:deletion><office:change-info><dc:creator>Jane Doe</dc:creator><dc:date>2026-03-14T09:26:00</dc:date></office:change-info><text:p>€40,000</text:p></text:deletion></text:changed-region></text:tracked-changes>"#,
                r#"<text:p>The budget is <text:change text:change-id="ct1"/>€25,000.</text:p>"#,
            )),
            "",
            &Extras::default(),
        ),
        signed_odt: package(
            "text",
            &text_body("<text:p>Signed by the board.</text:p>"),
            "",
            &Extras {
                signed: true,
                ..Extras::default()
            },
        ),
        // A cell comment, a hidden sheet, and a Basic macro.
        budget_ods: package(
            "spreadsheet",
            &body(
                "spreadsheet",
                concat!(
                    r#"<table:table table:name="Budget"><table:table-row><table:table-cell office:value-type="float" office:value="40000"><office:annotation><dc:creator>Jane Doe</dc:creator><dc:date>2026-03-14T09:30:00</dc:date><text:p>Check with Acme</text:p></office:annotation><text:p>40000</text:p></table:table-cell></table:table-row></table:table>"#,
                    r#"<table:table table:name="Salaries" table:style-name="ta2"><table:table-row><table:table-cell office:value-type="string"><text:p>Jane 90k</text:p></table:table-cell></table:table-row></table:table>"#,
                ),
            ),
            r#"<style:style style:name="ta2" style:family="table"><style:table-properties table:display="false"/></style:style>"#,
            &Extras {
                metadata: true,
                macros: true,
                ..all
            },
        ),
        // Speaker notes, a hidden slide and a slide comment.
        plans_odp: package(
            "presentation",
            &body(
                "presentation",
                concat!(
                    r#"<draw:page draw:name="Plans" draw:master-page-name="Default"><draw:frame presentation:class="title" svg:x="2cm" svg:y="2cm" svg:width="20cm" svg:height="3cm"><draw:text-box><text:p>Plans for 2027</text:p></draw:text-box></draw:frame><officeooo:annotation svg:x="1cm" svg:y="1cm" svg:width="1cm" svg:height="1cm"><dc:creator>Jane Doe</dc:creator><dc:date>2026-03-14T09:30:00</dc:date><text:p>Too early to show this.</text:p></officeooo:annotation><presentation:notes><draw:frame presentation:class="notes" svg:x="2cm" svg:y="14cm" svg:width="17cm" svg:height="10cm"><draw:text-box><text:p>Don't mention the layoffs.</text:p></draw:text-box></draw:frame></presentation:notes></draw:page>"#,
                    r#"<draw:page draw:name="Layoffs" draw:style-name="dp2" draw:master-page-name="Default"><draw:frame svg:x="2cm" svg:y="2cm" svg:width="20cm" svg:height="3cm"><draw:text-box><text:p>Who goes</text:p></draw:text-box></draw:frame></draw:page>"#,
                ),
            ),
            r#"<style:style style:name="dp2" style:family="drawing-page"><style:drawing-page-properties presentation:visibility="hidden"/></style:style>"#,
            &Extras {
                metadata: true,
                ..Extras::default()
            },
        ),
    }
}

fn text_body(paragraphs: &str) -> String {
    body("text", paragraphs)
}

fn body(kind: &str, inner: &str) -> String {
    format!("<office:body><office:{kind}>{inner}</office:{kind}></office:body>")
}

fn content(body: &str, automatic_styles: &str) -> String {
    format!(
        "{DECLARATION}<office:document-content {NS}><office:automatic-styles>{automatic_styles}</office:automatic-styles>{body}</office:document-content>"
    )
}

fn styles(kind: &str) -> String {
    let master = if kind == "presentation" {
        r#"<office:master-styles><style:master-page style:name="Default"/></office:master-styles>"#
    } else {
        ""
    };
    format!("{DECLARATION}<office:document-styles {NS}>{master}</office:document-styles>")
}

const META: &str = concat!(
    "<office:meta>",
    "<meta:generator>LibreOffice/24.2.7.2$Linux_X86_64 LibreOffice_project/420$Build-2</meta:generator>",
    "<dc:title>Q1 report — draft</dc:title>",
    "<dc:description>Numbers still to be checked with Acme.</dc:description>",
    "<meta:keyword>confidential</meta:keyword>",
    "<meta:initial-creator>Jane Doe</meta:initial-creator>",
    "<dc:creator>John Roe</dc:creator>",
    "<meta:printed-by>John Roe</meta:printed-by>",
    "<meta:creation-date>2026-03-10T08:15:00</meta:creation-date>",
    "<dc:date>2026-03-14T09:26:00</dc:date>",
    "<meta:print-date>2026-03-13T17:02:00</meta:print-date>",
    "<meta:editing-cycles>7</meta:editing-cycles>",
    "<meta:editing-duration>PT1H34M</meta:editing-duration>",
    r#"<meta:template xlink:type="simple" xlink:actuate="onRequest" xlink:title="Report" xlink:href="file:///home/jane/Templates/Report.ott"/>"#,
    r#"<meta:document-statistic meta:page-count="1" meta:word-count="3"/>"#,
    r#"<meta:user-defined meta:name="Client">Acme Corporation</meta:user-defined>"#,
    "</office:meta>",
);

const SETTINGS: &str = concat!(
    "<office:settings>",
    r#"<config:config-item-set config:name="ooo:view-settings"><config:config-item config:name="ViewAreaTop" config:type="long">0</config:config-item></config:config-item-set>"#,
    r#"<config:config-item-set config:name="ooo:configuration-settings">"#,
    r#"<config:config-item config:name="PrinterIndependentLayout" config:type="string">high-resolution</config:config-item>"#,
    r#"<config:config-item config:name="PrinterName" config:type="string">HP LaserJet 4 (Jane's office)</config:config-item>"#,
    r#"<config:config-item config:name="PrinterSetup" config:type="base64Binary">SFAgTGFzZXJKZXQgNA==</config:config-item>"#,
    r#"<config:config-item config:name="RsidRoot" config:type="int">138066</config:config-item>"#,
    r#"<config:config-item config:name="Rsid" config:type="int">655404</config:config-item>"#,
    r#"<config:config-item config:name="CurrentDatabaseDataSource" config:type="string">file:///home/jane/contacts.odb</config:config-item>"#,
    "</config:config-item-set>",
    "</office:settings>",
);

const RDF: &str = r#"<?xml version="1.0" encoding="utf-8"?><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about=""><rdf:type rdf:resource="http://docs.oasis-open.org/ns/office/1.2/meta/pkg#Document"/></rdf:Description></rdf:RDF>"#;

fn package(kind: &str, body: &str, automatic_styles: &str, extras: &Extras<'_>) -> Vec<u8> {
    let mimetype = format!("application/vnd.oasis.opendocument.{kind}");
    let mut files: Vec<(String, &str, Vec<u8>)> = vec![
        (
            "content.xml".into(),
            "text/xml",
            content(body, automatic_styles).into_bytes(),
        ),
        ("styles.xml".into(), "text/xml", styles(kind).into_bytes()),
    ];
    if extras.metadata {
        files.push((
            "meta.xml".into(),
            "text/xml",
            format!("{DECLARATION}<office:document-meta {NS}>{META}</office:document-meta>")
                .into_bytes(),
        ));
        files.push((
            "settings.xml".into(),
            "text/xml",
            format!(
                "{DECLARATION}<office:document-settings {NS}>{SETTINGS}</office:document-settings>"
            )
            .into_bytes(),
        ));
        files.push(("manifest.rdf".into(), "application/rdf+xml", RDF.into()));
    }
    if let Some(photo) = extras.photo {
        files.push(("Pictures/photo.jpg".into(), "image/jpeg", photo.to_vec()));
    }
    if let Some(thumbnail) = extras.thumbnail {
        files.push((
            "Thumbnails/thumbnail.png".into(),
            "image/png",
            thumbnail.to_vec(),
        ));
    }
    if extras.macros {
        files.push((
            "Basic/script-lc.xml".into(),
            "text/xml",
            format!(r#"{DECLARATION}<library:libraries xmlns:library="http://openoffice.org/2000/library"><library:library library:name="Standard" library:link="false"/></library:libraries>"#).into_bytes(),
        ));
        files.push((
            "Basic/Standard/Module1.xml".into(),
            "text/xml",
            format!(r#"{DECLARATION}<script:module xmlns:script="http://openoffice.org/2000/script" script:name="Module1" script:language="StarBasic">Sub Main
  MsgBox "Hello"
End Sub</script:module>"#).into_bytes(),
        ));
    }
    let mut manifest = format!(
        r#"{DECLARATION}<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0" manifest:version="1.3"><manifest:file-entry manifest:full-path="/" manifest:version="1.3" manifest:media-type="{mimetype}"/><manifest:file-entry manifest:full-path="Configurations2/" manifest:media-type="application/vnd.sun.xml.ui.configuration"/>"#
    );
    for (name, media_type, _) in &files {
        manifest.push_str(&format!(
            r#"<manifest:file-entry manifest:full-path="{name}" manifest:media-type="{media_type}"/>"#
        ));
    }
    if extras.macros {
        manifest.push_str(r#"<manifest:file-entry manifest:full-path="Basic/" manifest:media-type=""/><manifest:file-entry manifest:full-path="Basic/Standard/" manifest:media-type=""/>"#);
    }
    manifest.push_str("</manifest:manifest>");
    let mut entries: Vec<(String, Vec<u8>)> = files
        .into_iter()
        .map(|(name, _, data)| (name, data))
        .collect();
    entries.push(("META-INF/manifest.xml".into(), manifest.into_bytes()));
    if extras.signed {
        entries.push((
            "META-INF/documentsignatures.xml".into(),
            br#"<document-signatures xmlns="urn:oasis:names:tc:opendocument:xmlns:digitalsignature:1.0"/>"#.to_vec(),
        ));
    }
    let plain = !extras.metadata && extras.photo.is_none() && !extras.macros;
    zip(&mimetype, &entries, !plain)
}

/// `mimetype` first and stored, as OpenDocument requires; dated like a
/// file saved on 2026-03-14, or with no date.
pub fn zip(mimetype: &str, entries: &[(String, Vec<u8>)], dated: bool) -> Vec<u8> {
    let date = if dated {
        DateTime::from_date_and_time(2026, 3, 14, 9, 26, 52).unwrap()
    } else {
        DateTime::default()
    };
    let options = SimpleFileOptions::default()
        .last_modified_time(date)
        .system(zip::System::Dos);
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(
        "mimetype",
        options.compression_method(CompressionMethod::Stored),
    )
    .unwrap();
    zip.write_all(mimetype.as_bytes()).unwrap();
    for (name, data) in entries {
        zip.start_file(
            name.as_str(),
            options.compression_method(CompressionMethod::Deflated),
        )
        .unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap().into_inner()
}
