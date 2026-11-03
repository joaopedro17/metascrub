//! Minimal Word and Excel files that still open in Office and LibreOffice,
//! with every kind of document metadata MetaScrub removes.

use std::io::{Cursor, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

const RELS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const DOC_RELS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const W15: &str = "http://schemas.microsoft.com/office/word/2012/wordml";
const S: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const DECLARATION: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n";

pub struct Files {
    pub clean_docx: Vec<u8>,
    pub report_docx: Vec<u8>,
    pub tracked_changes_docx: Vec<u8>,
    pub comments_docx: Vec<u8>,
    pub budget_xlsx: Vec<u8>,
    pub comments_xlsx: Vec<u8>,
}

pub fn files(photo: &[u8], thumbnail: &[u8]) -> Files {
    Files {
        clean_docx: docx(&paragraph("Hello."), &Extras::default(), false),
        report_docx: docx(
            &paragraph("Quarterly report."),
            &Extras {
                photo: Some(photo),
                thumbnail: Some(thumbnail),
                metadata: true,
                comments: false,
            },
            true,
        ),
        tracked_changes_docx: docx(
            r#"<w:p><w:r><w:t xml:space="preserve">The budget is </w:t></w:r><w:del w:id="1" w:author="Jane Doe" w:date="2026-03-14T09:26:00Z"><w:r><w:delText>€40,000</w:delText></w:r></w:del><w:ins w:id="2" w:author="Jane Doe" w:date="2026-03-14T09:27:00Z"><w:r><w:t>€25,000</w:t></w:r></w:ins><w:r><w:t>.</w:t></w:r></w:p>"#,
            &Extras::default(),
            true,
        ),
        comments_docx: docx(
            r#"<w:p><w:commentRangeStart w:id="0"/><w:r><w:t>Draft.</w:t></w:r><w:commentRangeEnd w:id="0"/><w:r><w:commentReference w:id="0"/></w:r></w:p>"#,
            &Extras {
                comments: true,
                ..Extras::default()
            },
            true,
        ),
        budget_xlsx: xlsx(),
        comments_xlsx: comments_xlsx(),
    }
}

#[derive(Default)]
struct Extras<'a> {
    photo: Option<&'a [u8]>,
    thumbnail: Option<&'a [u8]>,
    metadata: bool,
    comments: bool,
}

fn paragraph(text: &str) -> String {
    format!(
        r#"<w:p w:rsidR="00A1B2C3" w:rsidRDefault="00A1B2C3" w:rsidP="00D4E5F6"><w:r w:rsidRPr="00D4E5F6"><w:t>{text}</w:t></w:r></w:p>"#
    )
}

fn docx(body: &str, extras: &Extras<'_>, dated: bool) -> Vec<u8> {
    let mut types = vec![
        override_(
            "/word/document.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml",
        ),
        override_(
            "/word/settings.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml",
        ),
        override_(
            "/word/styles.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml",
        ),
    ];
    let mut package_rels = vec![rel(
        "rId1",
        &format!("{DOC_RELS}/officeDocument"),
        "word/document.xml",
        false,
    )];
    let mut document_rels = vec![
        rel(
            "rId1",
            &format!("{DOC_RELS}/settings"),
            "settings.xml",
            false,
        ),
        rel("rId2", &format!("{DOC_RELS}/styles"), "styles.xml", false),
    ];
    let mut settings_rels = Vec::new();
    let mut parts: Vec<(String, Vec<u8>)> = Vec::new();
    let mut body = body.to_owned();
    let mut settings = String::from(r#"<w:zoom w:percent="100"/>"#);

    if extras.metadata {
        types.push(override_(
            "/docProps/core.xml",
            "application/vnd.openxmlformats-package.core-properties+xml",
        ));
        types.push(override_(
            "/docProps/app.xml",
            "application/vnd.openxmlformats-officedocument.extended-properties+xml",
        ));
        types.push(override_(
            "/docProps/custom.xml",
            "application/vnd.openxmlformats-officedocument.custom-properties+xml",
        ));
        types.push(override_(
            "/word/people.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.people+xml",
        ));
        types.push(override_(
            "/customXml/itemProps1.xml",
            "application/vnd.openxmlformats-officedocument.customXmlProperties+xml",
        ));
        package_rels.push(rel(
            "rId2",
            &format!("{RELS}/metadata/core-properties"),
            "docProps/core.xml",
            false,
        ));
        package_rels.push(rel(
            "rId3",
            &format!("{DOC_RELS}/extended-properties"),
            "docProps/app.xml",
            false,
        ));
        package_rels.push(rel(
            "rId4",
            &format!("{DOC_RELS}/custom-properties"),
            "docProps/custom.xml",
            false,
        ));
        document_rels.push(rel(
            "rId3",
            "http://schemas.microsoft.com/office/2011/relationships/people",
            "people.xml",
            false,
        ));
        document_rels.push(rel(
            "rId4",
            &format!("{DOC_RELS}/customXml"),
            "../customXml/item1.xml",
            false,
        ));
        document_rels.push(rel(
            "rId5",
            &format!("{DOC_RELS}/hyperlink"),
            "https://example.com/",
            true,
        ));
        settings_rels.push(rel(
            "rId1",
            &format!("{DOC_RELS}/attachedTemplate"),
            r"file:///C:\Users\jane.doe\AppData\Roaming\Microsoft\Templates\Report.dotx",
            true,
        ));
        settings = r#"<w:zoom w:percent="100"/><w:attachedTemplate r:id="rId1"/><w:docVars><w:docVar w:name="ClientCode" w:val="ACME-2026"/></w:docVars><w:rsids><w:rsidRoot w:val="00A1B2C3"/><w:rsid w:val="00A1B2C3"/><w:rsid w:val="00D4E5F6"/></w:rsids><w15:docId w15:val="{3F2504E0-4F89-11D3-9A0C-0305E82C3301}"/>"#.to_owned();
        body.push_str(
            r#"<w:p><w:hyperlink r:id="rId5"><w:r><w:t>Website</w:t></w:r></w:hyperlink></w:p>"#,
        );
        parts.push(("docProps/core.xml".into(), CORE.as_bytes().to_vec()));
        parts.push(("docProps/app.xml".into(), APP_WORD.as_bytes().to_vec()));
        parts.push(("docProps/custom.xml".into(), CUSTOM.as_bytes().to_vec()));
        parts.push((
            "word/people.xml".into(),
            format!(r#"{DECLARATION}<w15:people xmlns:w15="{W15}"><w15:person w15:author="Jane Doe"><w15:presenceInfo w15:providerId="AD" w15:userId="S::jane.doe@example.com::0a1b"/></w15:person></w15:people>"#).into_bytes(),
        ));
        parts.push((
            "customXml/item1.xml".into(),
            br#"<p:properties xmlns:p="http://schemas.microsoft.com/office/2006/metadata/properties"><documentManagement><Owner>Jane Doe</Owner></documentManagement></p:properties>"#.to_vec(),
        ));
        parts.push((
            "customXml/_rels/item1.xml.rels".into(),
            rels(&[rel(
                "rId1",
                &format!("{DOC_RELS}/customXmlProps"),
                "itemProps1.xml",
                false,
            )]),
        ));
        parts.push((
            "customXml/itemProps1.xml".into(),
            br#"<ds:datastoreItem ds:itemID="{5F1C2E9A-0B7D-4C3E-9A61-2D8F4B7E1C03}" xmlns:ds="http://schemas.openxmlformats.org/officeDocument/2006/customXml"/>"#.to_vec(),
        ));
    }
    if let Some(thumbnail) = extras.thumbnail {
        types.push(default_("jpeg", "image/jpeg"));
        package_rels.push(rel(
            "rId5",
            &format!("{RELS}/metadata/thumbnail"),
            "docProps/thumbnail.jpeg",
            false,
        ));
        parts.push(("docProps/thumbnail.jpeg".into(), thumbnail.to_vec()));
    }
    if let Some(photo) = extras.photo {
        if extras.thumbnail.is_none() {
            types.push(default_("jpeg", "image/jpeg"));
        }
        document_rels.push(rel(
            "rId6",
            &format!("{DOC_RELS}/image"),
            "media/image1.jpeg",
            false,
        ));
        body.push_str(&picture("rId6"));
        parts.push(("word/media/image1.jpeg".into(), photo.to_vec()));
    }
    if extras.comments {
        types.push(override_(
            "/word/comments.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml",
        ));
        document_rels.push(rel(
            "rId7",
            &format!("{DOC_RELS}/comments"),
            "comments.xml",
            false,
        ));
        parts.push((
            "word/comments.xml".into(),
            format!(r#"{DECLARATION}<w:comments xmlns:w="{W}"><w:comment w:id="0" w:author="Jane Doe" w:date="2026-03-14T09:30:00Z" w:initials="JD"><w:p><w:r><w:t>Check the numbers with Acme first.</w:t></w:r></w:p></w:comment></w:comments>"#).into_bytes(),
        ));
    }

    let document = format!(
        r#"{DECLARATION}<w:document xmlns:w="{W}" xmlns:r="{DOC_RELS}" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><w:body>{body}<w:sectPr w:rsidR="00A1B2C3"><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="708" w:footer="708" w:gutter="0"/></w:sectPr></w:body></w:document>"#
    );
    let settings = format!(
        r#"{DECLARATION}<w:settings xmlns:w="{W}" xmlns:r="{DOC_RELS}" xmlns:w15="{W15}">{settings}</w:settings>"#
    );
    let styles = format!(
        r#"{DECLARATION}<w:styles xmlns:w="{W}"><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style></w:styles>"#
    );

    let mut entries: Vec<(String, Vec<u8>)> = vec![
        ("[Content_Types].xml".into(), content_types(&types)),
        ("_rels/.rels".into(), rels(&package_rels)),
        ("word/document.xml".into(), document.into_bytes()),
        ("word/_rels/document.xml.rels".into(), rels(&document_rels)),
        ("word/settings.xml".into(), settings.into_bytes()),
        ("word/styles.xml".into(), styles.into_bytes()),
    ];
    if !settings_rels.is_empty() {
        entries.push(("word/_rels/settings.xml.rels".into(), rels(&settings_rels)));
    }
    entries.extend(parts);
    zip(&entries, dated)
}

/// An inline picture, 2 × 1.5 cm.
fn picture(id: &str) -> String {
    format!(
        r#"<w:p><w:r><w:drawing><wp:inline><wp:extent cx="720000" cy="540000"/><wp:docPr id="1" name="Picture 1"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic><pic:nvPicPr><pic:cNvPr id="1" name="IMG_2026.jpg"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="{id}"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="720000" cy="540000"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#
    )
}

fn xlsx() -> Vec<u8> {
    let types = [
        override_(
            "/xl/workbook.xml",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml",
        ),
        override_(
            "/xl/worksheets/sheet1.xml",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml",
        ),
        override_(
            "/docProps/core.xml",
            "application/vnd.openxmlformats-package.core-properties+xml",
        ),
        override_(
            "/docProps/app.xml",
            "application/vnd.openxmlformats-officedocument.extended-properties+xml",
        ),
        default_(
            "bin",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.printerSettings",
        ),
    ];
    let workbook = format!(
        r#"{DECLARATION}<workbook xmlns="{S}" xmlns:r="{DOC_RELS}" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:x15="http://schemas.microsoft.com/office/spreadsheetml/2010/11/main" xmlns:xr="http://schemas.microsoft.com/office/spreadsheetml/2014/revision" mc:Ignorable="x15 xr"><fileVersion appName="xl" lastEdited="7" lowestEdited="7" rupBuild="27328"/><mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"><mc:Choice Requires="x15"><x15ac:absPath url="C:\Users\jane.doe\OneDrive - Fictional Ltd\Finance\" xmlns:x15ac="http://schemas.microsoft.com/office/spreadsheetml/2010/11/ac"/></mc:Choice></mc:AlternateContent><xr:revisionPtr revIDLastSave="12" documentId="13_ncr:1_{{8C2D6F1A-0E4B-4F7A-9D3C-5B1E2A7F9C40}}" xr6:coauthVersionLast="47" xr6:coauthVersionMax="47" xr10:uidLastSave="{{00000000-0000-0000-0000-000000000000}}" xmlns:xr6="http://schemas.microsoft.com/office/spreadsheetml/2016/revision6" xmlns:xr10="http://schemas.microsoft.com/office/spreadsheetml/2016/revision10"/><sheets><sheet name="Budget" sheetId="1" r:id="rId1"/></sheets></workbook>"#
    );
    let sheet = format!(
        r#"{DECLARATION}<worksheet xmlns="{S}" xmlns:r="{DOC_RELS}"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Item</t></is></c><c r="B1" t="inlineStr"><is><t>Cost</t></is></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>Travel</t></is></c><c r="B2"><v>1250</v></c></row></sheetData><pageMargins left="0.7" right="0.7" top="0.75" bottom="0.75" header="0.3" footer="0.3"/><pageSetup paperSize="9" orientation="portrait" r:id="rId1"/></worksheet>"#
    );
    // A real DEVMODE structure starts with the printer's name in UTF-16.
    let mut printer: Vec<u8> = "\\\\FIN-PRN02\\Finance LaserJet"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    printer.resize(220, 0);
    zip(
        &[
            ("[Content_Types].xml".into(), content_types(&types)),
            (
                "_rels/.rels".into(),
                rels(&[
                    rel(
                        "rId1",
                        &format!("{DOC_RELS}/officeDocument"),
                        "xl/workbook.xml",
                        false,
                    ),
                    rel(
                        "rId2",
                        &format!("{RELS}/metadata/core-properties"),
                        "docProps/core.xml",
                        false,
                    ),
                    rel(
                        "rId3",
                        &format!("{DOC_RELS}/extended-properties"),
                        "docProps/app.xml",
                        false,
                    ),
                ]),
            ),
            ("xl/workbook.xml".into(), workbook.into_bytes()),
            (
                "xl/_rels/workbook.xml.rels".into(),
                rels(&[rel(
                    "rId1",
                    &format!("{DOC_RELS}/worksheet"),
                    "worksheets/sheet1.xml",
                    false,
                )]),
            ),
            ("xl/worksheets/sheet1.xml".into(), sheet.into_bytes()),
            (
                "xl/worksheets/_rels/sheet1.xml.rels".into(),
                rels(&[rel(
                    "rId1",
                    &format!("{DOC_RELS}/printerSettings"),
                    "../printerSettings/printerSettings1.bin",
                    false,
                )]),
            ),
            ("xl/printerSettings/printerSettings1.bin".into(), printer),
            ("docProps/core.xml".into(), CORE.as_bytes().to_vec()),
            ("docProps/app.xml".into(), APP_EXCEL.as_bytes().to_vec()),
        ],
        true,
    )
}

/// A note on A1 and a threaded comment with a reply on B2, as Excel writes
/// them: the threaded comment also has a stand-in note for older versions.
fn comments_xlsx() -> Vec<u8> {
    const TC: &str = "http://schemas.microsoft.com/office/spreadsheetml/2018/threadedcomments";
    const MS: &str = "http://schemas.microsoft.com/office/2017/10/relationships";
    let types = [
        override_(
            "/xl/workbook.xml",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml",
        ),
        override_(
            "/xl/worksheets/sheet1.xml",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml",
        ),
        override_(
            "/xl/comments1.xml",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.comments+xml",
        ),
        override_(
            "/xl/threadedComments/threadedComment1.xml",
            "application/vnd.ms-excel.threadedcomments+xml",
        ),
        override_(
            "/xl/persons/person.xml",
            "application/vnd.ms-excel.person+xml",
        ),
        default_(
            "vml",
            "application/vnd.openxmlformats-officedocument.vmlDrawing",
        ),
    ];
    let workbook = format!(
        r#"{DECLARATION}<workbook xmlns="{S}" xmlns:r="{DOC_RELS}"><sheets><sheet name="Budget" sheetId="1" r:id="rId1"/></sheets></workbook>"#
    );
    let sheet = format!(
        r#"{DECLARATION}<worksheet xmlns="{S}" xmlns:r="{DOC_RELS}"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Travel</t></is></c><c r="B1"><v>1250</v></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>Hotel</t></is></c><c r="B2"><v>980</v></c></row></sheetData><pageMargins left="0.7" right="0.7" top="0.75" bottom="0.75" header="0.3" footer="0.3"/><legacyDrawing r:id="rId2"/></worksheet>"#
    );
    let notes = format!(
        r#"{DECLARATION}<comments xmlns="{S}"><authors><author>Jane Doe</author><author>tc={{8D2C6F1A-0E4B-4F7A-9D3C-5B1E2A7F9C41}}</author></authors><commentList><comment ref="A1" authorId="0" shapeId="0"><text><r><rPr><b/><sz val="9"/><rFont val="Tahoma"/></rPr><t>Jane Doe:</t></r><r><rPr><sz val="9"/><rFont val="Tahoma"/></rPr><t xml:space="preserve">
Booked through Acme Travel.</t></r></text></comment><comment ref="B2" authorId="1" shapeId="0"><text><t xml:space="preserve">[Threaded comment]

Your version of Excel allows you to read this threaded comment; however, any edits to it will get removed if the file is opened in a newer version of Excel. Learn more: https://go.microsoft.com/fwlink/?linkid=870924

Comment:
    Is this the final price?
Reply:
    Yes, confirmed with the hotel.</t></text></comment></commentList></comments>"#
    );
    let threaded = format!(
        r#"{DECLARATION}<ThreadedComments xmlns="{TC}" xmlns:x="{S}"><threadedComment ref="B2" dT="2026-03-14T09:30:12.34" personId="{{5F1C2E9A-0B7D-4C3E-9A61-2D8F4B7E1C03}}" id="{{8D2C6F1A-0E4B-4F7A-9D3C-5B1E2A7F9C41}}"><text>Is this the final price?</text></threadedComment><threadedComment ref="B2" dT="2026-03-14T10:02:45.67" personId="{{6A2D3F0B-1C8E-4D4F-8B72-3E9A5C8F2D14}}" id="{{9E3D7A2B-1F5C-4A8B-8E4D-6C2F3B8A0D52}}" parentId="{{8D2C6F1A-0E4B-4F7A-9D3C-5B1E2A7F9C41}}"><text>Yes, confirmed with the hotel.</text></threadedComment></ThreadedComments>"#
    );
    let persons = format!(
        r#"{DECLARATION}<personList xmlns="{TC}" xmlns:x="{S}"><person displayName="Jane Doe" id="{{5F1C2E9A-0B7D-4C3E-9A61-2D8F4B7E1C03}}" userId="jane.doe@example.com" providerId="AD"/><person displayName="John Roe" id="{{6A2D3F0B-1C8E-4D4F-8B72-3E9A5C8F2D14}}" userId="S::john.roe@example.com::7c1e" providerId="AD"/></personList>"#
    );
    let note_shape = |id: u32, row: u32, column: u32| {
        format!(
            r##"<v:shape id="_x0000_s{id}" type="#_x0000_t202" style="position:absolute;margin-left:59.25pt;margin-top:1.5pt;width:108pt;height:59.25pt;z-index:1;visibility:hidden" fillcolor="#ffffe1" o:insetmode="auto"><v:fill color2="#ffffe1"/><v:shadow on="t" color="black" obscured="t"/><v:path o:connecttype="none"/><v:textbox style="mso-direction-alt:auto"><div style="text-align:left"></div></v:textbox><x:ClientData ObjectType="Note"><x:MoveWithCells/><x:SizeWithCells/><x:Anchor>1, 15, 0, 2, 3, 15, 4, 4</x:Anchor><x:AutoFill>False</x:AutoFill><x:Row>{row}</x:Row><x:Column>{column}</x:Column></x:ClientData></v:shape>"##
        )
    };
    let vml = format!(
        r##"<xml xmlns:v="urn:schemas-microsoft-com:vml" xmlns:o="urn:schemas-microsoft-com:office:office" xmlns:x="urn:schemas-microsoft-com:office:excel"><o:shapelayout v:ext="edit"><o:idmap v:ext="edit" data="1"/></o:shapelayout><v:shapetype id="_x0000_t202" coordsize="21600,21600" o:spt="202" path="m,l,21600r21600,l21600,xe"><v:stroke joinstyle="miter"/><v:path gradientshapeok="t" o:connecttype="rect"/></v:shapetype>{}{}</xml>"##,
        note_shape(1025, 0, 0),
        note_shape(1026, 1, 1)
    );
    zip(
        &[
            ("[Content_Types].xml".into(), content_types(&types)),
            (
                "_rels/.rels".into(),
                rels(&[rel(
                    "rId1",
                    &format!("{DOC_RELS}/officeDocument"),
                    "xl/workbook.xml",
                    false,
                )]),
            ),
            ("xl/workbook.xml".into(), workbook.into_bytes()),
            (
                "xl/_rels/workbook.xml.rels".into(),
                rels(&[
                    rel(
                        "rId1",
                        &format!("{DOC_RELS}/worksheet"),
                        "worksheets/sheet1.xml",
                        false,
                    ),
                    rel("rId2", &format!("{MS}/person"), "persons/person.xml", false),
                ]),
            ),
            ("xl/worksheets/sheet1.xml".into(), sheet.into_bytes()),
            (
                "xl/worksheets/_rels/sheet1.xml.rels".into(),
                rels(&[
                    rel(
                        "rId1",
                        &format!("{DOC_RELS}/comments"),
                        "../comments1.xml",
                        false,
                    ),
                    rel(
                        "rId2",
                        &format!("{DOC_RELS}/vmlDrawing"),
                        "../drawings/vmlDrawing1.vml",
                        false,
                    ),
                    rel(
                        "rId3",
                        &format!("{MS}/threadedComment"),
                        "../threadedComments/threadedComment1.xml",
                        false,
                    ),
                ]),
            ),
            ("xl/comments1.xml".into(), notes.into_bytes()),
            ("xl/drawings/vmlDrawing1.vml".into(), vml.into_bytes()),
            (
                "xl/threadedComments/threadedComment1.xml".into(),
                threaded.into_bytes(),
            ),
            ("xl/persons/person.xml".into(), persons.into_bytes()),
        ],
        true,
    )
}

const CORE: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><dc:title>Q1 report — draft</dc:title><dc:subject>Finance</dc:subject><dc:creator>Jane Doe</dc:creator><cp:keywords>confidential, acme</cp:keywords><dc:description>Numbers still to be checked with Acme.</dc:description><cp:lastModifiedBy>John Roe</cp:lastModifiedBy><cp:revision>7</cp:revision><cp:lastPrinted>2026-03-13T17:02:00Z</cp:lastPrinted><dcterms:created xsi:type="dcterms:W3CDTF">2026-03-10T08:15:00Z</dcterms:created><dcterms:modified xsi:type="dcterms:W3CDTF">2026-03-14T09:26:00Z</dcterms:modified></cp:coreProperties>"#;

const APP_WORD: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><Template>Report.dotx</Template><TotalTime>94</TotalTime><Pages>1</Pages><Words>3</Words><Application>Microsoft Office Word</Application><DocSecurity>0</DocSecurity><Company>Fictional Ltd</Company><Manager>Richard Miles</Manager><AppVersion>16.0000</AppVersion></Properties>"#;

const APP_EXCEL: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><Application>Microsoft Excel</Application><DocSecurity>0</DocSecurity><Company>Fictional Ltd</Company><AppVersion>16.0300</AppVersion></Properties>"#;

const CUSTOM: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/custom-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><property fmtid="{D5CDD505-2E9C-101B-9397-08002B2CF9AE}" pid="2" name="Client"><vt:lpwstr>Acme Corporation</vt:lpwstr></property><property fmtid="{D5CDD505-2E9C-101B-9397-08002B2CF9AE}" pid="3" name="Project"><vt:lpwstr>Falcon</vt:lpwstr></property></Properties>"#;

fn override_(part: &str, content_type: &str) -> String {
    format!(r#"<Override PartName="{part}" ContentType="{content_type}"/>"#)
}

fn default_(extension: &str, content_type: &str) -> String {
    format!(r#"<Default Extension="{extension}" ContentType="{content_type}"/>"#)
}

fn content_types(entries: &[String]) -> Vec<u8> {
    format!(
        r#"{DECLARATION}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/>{}</Types>"#,
        entries.concat()
    )
    .into_bytes()
}

fn rel(id: &str, kind: &str, target: &str, external: bool) -> String {
    let mode = if external {
        r#" TargetMode="External""#
    } else {
        ""
    };
    format!(r#"<Relationship Id="{id}" Type="{kind}" Target="{target}"{mode}/>"#)
}

fn rels(relationships: &[String]) -> Vec<u8> {
    format!(
        r#"{DECLARATION}<Relationships xmlns="{RELS}">{}</Relationships>"#,
        relationships.concat()
    )
    .into_bytes()
}

/// Office itself writes 1980-01-01 for every entry; other tools write the
/// real date, which `dated` imitates.
pub fn zip(entries: &[(String, Vec<u8>)], dated: bool) -> Vec<u8> {
    let date = if dated {
        DateTime::from_date_and_time(2026, 3, 14, 9, 26, 52).unwrap()
    } else {
        DateTime::default()
    };
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .last_modified_time(date)
        .system(zip::System::Dos);
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in entries {
        zip.start_file(name.as_str(), options).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap().into_inner()
}
