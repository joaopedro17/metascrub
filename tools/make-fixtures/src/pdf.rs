//! Small PDFs written by hand, so the fixtures don't depend on the PDF
//! library MetaScrub itself uses.

use std::io::Write;

use flate2::Compression;
use flate2::write::ZlibEncoder;

pub struct Files {
    pub clean: Vec<u8>,
    pub report: Vec<u8>,
    pub comments: Vec<u8>,
    pub attachments: Vec<u8>,
    pub form: Vec<u8>,
    pub encrypted: Vec<u8>,
}

pub fn files(photo: &[u8], thumbnail: &[u8], xmp: &str) -> Files {
    Files {
        clean: simple("", "", &[]).0,
        report: report(photo, thumbnail, xmp),
        comments: comments(),
        attachments: attachments(photo),
        form: form(),
        encrypted: encrypted(),
    }
}

/// Objects 1 to 5 of every fixture: catalog, page tree, one A4 page with
/// a line of text, its content and its font. Objects from 6 on are the
/// `extra` ones; `catalog` and `page` add entries to those dictionaries.
fn base(catalog: &str, page: &str, content: &str) -> Vec<Vec<u8>> {
    vec![
        format!("<< /Type /Catalog /Pages 2 0 R {catalog}>>").into_bytes(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 5 0 R >> /XObject << /Im1 8 0 R >> >> /Contents 4 0 R {page}>>"
        )
        .into_bytes(),
        stream(
            "",
            format!("BT /F1 24 Tf 72 760 Td (Quarterly report.) Tj ET{content}").as_bytes(),
        ),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
    ]
}

/// A one-page PDF; returns it with the offset of its cross-reference table.
fn simple(catalog: &str, page: &str, extra: &[Vec<u8>]) -> (Vec<u8>, usize) {
    let mut objects = base(catalog, page, "");
    // The page names an image; without one it's object 8 = null.
    objects.extend_from_slice(extra);
    while objects.len() < 8 {
        objects.push(b"null".to_vec());
    }
    write(&objects, "")
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut out = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream");
    out
}

fn deflate(data: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

/// Object `n` is `objects[n - 1]`.
fn write(objects: &[Vec<u8>], trailer: &str) -> (Vec<u8>, usize) {
    let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(object);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R {trailer}>>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    (out, xref)
}

/// Appends an incremental update replacing object `number`, as editors do
/// when they save changes at the end of the file.
fn update(
    mut pdf: Vec<u8>,
    previous_xref: usize,
    size: usize,
    number: usize,
    object: &str,
    trailer: &str,
) -> Vec<u8> {
    let offset = pdf.len();
    pdf.extend_from_slice(format!("{number} 0 obj\n{object}\nendobj\n").as_bytes());
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!(
            "xref\n0 1\n0000000000 65535 f \n{number} 1\n{offset:010} 00000 n \ntrailer\n<< /Size {size} /Root 1 0 R /Prev {previous_xref} {trailer}>>\nstartxref\n{xref}\n%%EOF\n"
        )
        .as_bytes(),
    );
    pdf
}

const INFO: &str = "/Author (Jane Doe) /Creator (Microsoft Word) /Producer (Fictional PDF Library 2.1) /CreationDate (D:20260314092652+01'00') /Client (Acme Corporation)";
const ID: &str = "/ID [<3F2504E04F8911D39A0C0305E82C3301> <3F2504E04F8911D39A0C0305E82C3301>]";

/// Every kind of document metadata: Info, XMP, an ID, application data,
/// a page thumbnail, a photo with GPS, and an earlier version left behind
/// by an incremental save.
fn report(photo: &[u8], thumbnail: &[u8], xmp: &str) -> Vec<u8> {
    let mut objects = base(
        "/Metadata 6 0 R ",
        "/Thumb 7 0 R /LastModified (D:20260315100000Z) /PieceInfo << /Illustrator << /LastModified (D:20260315100000Z) /Private 9 0 R >> >> ",
        " q 120 0 0 90 72 600 cm /Im1 Do Q",
    );
    objects.push(stream("/Type /Metadata /Subtype /XML", xmp.as_bytes()));
    objects.push(stream(
        "/Type /XObject /Subtype /Image /Width 8 /Height 6 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode",
        thumbnail,
    ));
    objects.push(stream(
        "/Type /XObject /Subtype /Image /Width 64 /Height 48 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode",
        photo,
    ));
    objects.push(stream(
        "",
        b"Illustrator private data: the original artwork by Jane Doe",
    ));
    objects.push(
        format!("<< {INFO} /Title (Draft budget) /ModDate (D:20260314092652+01'00') >>")
            .into_bytes(),
    );
    let size = objects.len() + 1;
    let (pdf, xref) = write(&objects, &format!("/Info 10 0 R {ID} "));
    update(
        pdf,
        xref,
        size,
        10,
        &format!("<< {INFO} /Title (Final budget) /ModDate (D:20260316110000+01'00') >>"),
        &format!("/Info 10 0 R {ID} "),
    )
}

/// A sticky note with its popup, a highlight, and a link that must stay.
fn comments() -> Vec<u8> {
    simple(
        "",
        "/Annots [6 0 R 7 0 R 9 0 R 10 0 R] ",
        &[
            b"<< /Type /Annot /Subtype /Text /Rect [500 760 520 780] /Name /Comment /Contents (Check the totals with Acme.) /T (Jane Doe) /M (D:20260314093000Z) /CreationDate (D:20260314093000Z) /Popup 7 0 R >>".to_vec(),
            b"<< /Type /Annot /Subtype /Popup /Rect [520 680 700 780] /Parent 6 0 R >>".to_vec(),
            b"null".to_vec(),
            b"<< /Type /Annot /Subtype /Highlight /Rect [70 755 270 785] /QuadPoints [70 785 270 785 70 755 270 755] /C [1 1 0] /T (John Roe) /M (D:20260315101500Z) >>".to_vec(),
            b"<< /Type /Annot /Subtype /Link /Rect [72 700 200 720] /Border [0 0 0] /A << /S /URI /URI (https://example.com/) >> >>".to_vec(),
        ],
    )
    .0
}

/// Two attached files listed by the document, one of them a photo saved
/// with its full path, and one attached to the page as a comment.
fn attachments(photo: &[u8]) -> Vec<u8> {
    let notes = b"Salaries 2026: Jane Doe 52,000; John Roe 48,000\n";
    simple(
        "/Names << /EmbeddedFiles << /Names [(notes.txt) 6 0 R (photo.jpg) 9 0 R] >> >> ",
        "/Annots [11 0 R] ",
        &[
            b"<< /Type /Filespec /F (notes.txt) /UF (notes.txt) /EF << /F 7 0 R >> >>".to_vec(),
            stream(
                &format!("/Type /EmbeddedFile /Subtype /text#2Fplain /Filter /FlateDecode /Params << /Size {} /CreationDate (D:20260314092652+01'00') /ModDate (D:20260314092652+01'00') >>", notes.len()),
                &deflate(notes),
            ),
            b"null".to_vec(),
            br"<< /Type /Filespec /F (C:\\Users\\jane.doe\\Pictures\\photo.jpg) /UF (photo.jpg) /EF << /F 10 0 R >> >>".to_vec(),
            stream(
                &format!("/Type /EmbeddedFile /Subtype /image#2Fjpeg /Params << /Size {} /ModDate (D:20260314092652+01'00') >>", photo.len()),
                photo,
            ),
            b"<< /Type /Annot /Subtype /FileAttachment /Rect [500 760 520 780] /Name /PushPin /Contents (Signed contract) /T (Jane Doe) /M (D:20260314093000Z) /FS 12 0 R >>".to_vec(),
            b"<< /Type /Filespec /F (contract.txt) /UF (contract.txt) /EF << /F 13 0 R >> >>".to_vec(),
            stream("/Type /EmbeddedFile", b"Contract between Acme Corporation and Jane Doe.\n"),
        ],
    )
    .0
}

/// A filled-in form field and a script that runs when the file opens.
fn form() -> Vec<u8> {
    simple(
        "/AcroForm << /Fields [6 0 R] /DA (/Helv 12 Tf 0 g) >> /OpenAction 7 0 R ",
        "/Annots [6 0 R] ",
        &[
            b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (Name) /V (Jane Doe) /Rect [72 700 300 720] /P 3 0 R /DA (/Helv 12 Tf 0 g) >>".to_vec(),
            b"<< /S /JavaScript /JS (app.alert('Welcome');) >>".to_vec(),
        ],
    )
    .0
}

/// Only the encryption dictionary: enough to be recognized and skipped.
fn encrypted() -> Vec<u8> {
    let mut objects = base("", "", "");
    objects.extend([
        b"<< /Filter /Standard /V 1 /R 2 /P -4 /O <28BF4E5E4E758A4164004E56FFFA01082E2E00B6D0683E802F0CA9FE6453697A> /U <28BF4E5E4E758A4164004E56FFFA01082E2E00B6D0683E802F0CA9FE6453697A> >>".to_vec(),
        b"null".to_vec(),
        b"null".to_vec(),
    ]);
    write(&objects, &format!("/Encrypt 6 0 R {ID} ")).0
}
