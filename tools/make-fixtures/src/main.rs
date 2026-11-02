//! Writes the synthetic test files in `fixtures/generated/`.
//!
//! Every file is built from scratch with fictional metadata, so the test
//! suite never contains a real person's data. Run with:
//! `cargo run -p make-fixtures`

// A developer tool: failing loudly on a bug is the right behavior.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::ZlibEncoder;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::codecs::webp::WebPEncoder;
use image::{ImageEncoder, Rgb, RgbImage};

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/generated");
    let jpeg_dir = root.join("jpeg");
    let png_dir = root.join("png");
    let webp_dir = root.join("webp");
    let other_dir = root.join("other");
    for dir in [&jpeg_dir, &png_dir, &webp_dir, &other_dir] {
        fs::create_dir_all(dir).unwrap();
    }

    let clean_jpeg = encode_jpeg(64, 48);
    let thumbnail = encode_jpeg(8, 6);
    let clean_png = encode_png(64, 48);

    let write = |dir: &PathBuf, name: &str, data: &[u8]| {
        fs::write(dir.join(name), data).unwrap();
        println!("wrote {}", dir.join(name).display());
    };

    // JPEG
    write(&jpeg_dir, "clean.jpg", &clean_jpeg);
    let camera_exif = [b"Exif\0\0".as_slice(), &camera_tiff(Some(&thumbnail))].concat();
    let camera = with_segments(&clean_jpeg, &[segment(0xE1, &camera_exif)], b"");
    write(&jpeg_dir, "camera.jpg", &camera);
    write(&jpeg_dir, "truncated.jpg", &camera[..camera.len() * 7 / 10]);
    write(
        &jpeg_dir,
        "xmp-iptc.jpg",
        &with_segments(
            &clean_jpeg,
            &[
                segment(
                    0xE1,
                    &[b"http://ns.adobe.com/xap/1.0/\0".as_slice(), XMP.as_bytes()].concat(),
                ),
                segment(0xED, &photoshop_resources(&thumbnail)),
            ],
            b"",
        ),
    );
    write(
        &jpeg_dir,
        "icc-standard.jpg",
        &with_segments(
            &clean_jpeg,
            &[icc_segment(&icc_profile("sRGB IEC61966-2.1", SRGB, false))],
            b"",
        ),
    );
    let mut measured = SRGB;
    measured[0][0] += 0.012;
    measured[1][1] -= 0.009;
    write(
        &jpeg_dir,
        "icc-custom.jpg",
        &with_segments(
            &clean_jpeg,
            &[icc_segment(&icc_profile(
                "Studio monitor, calibrated 2026-03-14",
                measured,
                true,
            ))],
            b"",
        ),
    );
    write(
        &jpeg_dir,
        "motion-photo.jpg",
        &with_segments(&clean_jpeg, &[], &fake_mp4()),
    );
    write(
        &jpeg_dir,
        "multi-picture.jpg",
        &with_segments(
            &clean_jpeg,
            &[segment(0xE2, b"MPF\0MM\0\x2a\0\0\0\x08")],
            &thumbnail,
        ),
    );
    write(
        &jpeg_dir,
        "comment.jpg",
        &with_segments(
            &clean_jpeg,
            &[segment(0xFE, b"Edited on Jane's laptop")],
            b"",
        ),
    );
    write(
        &jpeg_dir,
        "unknown-segment.jpg",
        &with_segments(
            &clean_jpeg,
            &[segment(0xEC, b"Ducky\0\x01\0\x04\0\0\0\x50")],
            b"",
        ),
    );
    write(&jpeg_dir, "jpeg-named.png", &clean_jpeg);

    // PNG
    write(&png_dir, "clean.png", &clean_png);
    write(
        &png_dir,
        "text-chunks.png",
        &with_chunks(
            &clean_png,
            &[
                chunk(b"tEXt", b"Author\0Jane Doe"),
                chunk(b"tEXt", b"Software\0PhotoTool 2.1"),
                chunk(b"tEXt", b"Creation Time\0Sat, 14 Mar 2026 09:26:53 +0100"),
                chunk(
                    b"zTXt",
                    &[
                        b"Comment\0\0".as_slice(),
                        &deflate(b"Picture of our new flat"),
                    ]
                    .concat(),
                ),
                chunk(
                    b"iTXt",
                    &[b"XML:com.adobe.xmp\0\0\0\0\0".as_slice(), XMP.as_bytes()].concat(),
                ),
                chunk(b"tIME", &[0x07, 0xEA, 3, 14, 8, 26, 53]),
                chunk(b"bKGD", &[0, 0xFF, 0, 0xFF, 0, 0xFF]),
                chunk(b"prVt", b"private vendor data"),
            ],
            b"",
        ),
    );
    write(
        &png_dir,
        "exif.png",
        &with_chunks(
            &clean_png,
            &[
                chunk(b"eXIf", &camera_tiff(None)),
                chunk(b"tEXt", raw_iptc_profile().as_slice()),
            ],
            b"",
        ),
    );
    let icc = icc_profile("Display P3", DISPLAY_P3, false);
    write(
        &png_dir,
        "icc.png",
        &with_chunks(
            &clean_png,
            &[chunk(
                b"iCCP",
                &[b"Display P3\0\0".as_slice(), &deflate(&icc)].concat(),
            )],
            b"",
        ),
    );
    write(
        &png_dir,
        "trailing.png",
        &with_chunks(&clean_png, &[], b"appended after the image"),
    );
    let mut bad = clean_png.clone();
    let last = bad.len() - 1;
    bad[last] ^= 0xFF;
    write(&png_dir, "bad-checksum.png", &bad);

    // WebP
    let image = encode_webp(64, 48);
    write(
        &webp_dir,
        "clean.webp",
        &riff(std::slice::from_ref(&image), b""),
    );
    let camera = riff(
        &[
            vp8x(0x08 | 0x04, 64, 48),
            image.clone(),
            riff_chunk(b"EXIF", &camera_tiff(None)),
            riff_chunk(b"XMP ", XMP.as_bytes()),
            riff_chunk(b"prVt", b"private vendor data"),
        ],
        b"",
    );
    write(&webp_dir, "camera.webp", &camera);
    write(
        &webp_dir,
        "truncated.webp",
        &camera[..camera.len() * 7 / 10],
    );
    write(
        &webp_dir,
        "icc.webp",
        &riff(
            &[
                vp8x(0x20, 64, 48),
                riff_chunk(b"ICCP", &icc_profile("sRGB IEC61966-2.1", SRGB, false)),
                image.clone(),
            ],
            b"",
        ),
    );
    let frame = |x: u8| {
        let mut data = vec![x, 0, 0, 0, 0, 0];
        data.extend([31, 0, 0, 23, 0, 0]); // 32×24
        data.extend([100, 0, 0, 0]); // 100 ms, no blending
        data.extend(riff_chunk(b"VP8L", &vp8l_data(&encode_webp(32, 24))));
        data.extend(riff_chunk(b"prVt", b"frame note"));
        riff_chunk(b"ANMF", &data)
    };
    write(
        &webp_dir,
        "animated.webp",
        &riff(
            &[
                vp8x(0x02 | 0x04, 64, 48),
                riff_chunk(b"ANIM", &[0, 0, 0, 0, 0, 0]),
                frame(0),
                frame(16),
                riff_chunk(b"XMP ", XMP.as_bytes()),
            ],
            b"",
        ),
    );
    write(
        &webp_dir,
        "trailing.webp",
        &riff(&[image], b"appended after the image"),
    );

    // Other
    write(&other_dir, "notes.txt", b"Not an image.\n");
}

fn gradient(width: u32, height: u32) -> RgbImage {
    RgbImage::from_fn(width, height, |x, y| {
        Rgb([(x * 255 / width) as u8, (y * 255 / height) as u8, 128])
    })
}

fn encode_jpeg(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 85)
        .encode_image(&gradient(width, height))
        .unwrap();
    out
}

fn encode_png(width: u32, height: u32) -> Vec<u8> {
    let image = gradient(width, height);
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(
            image.as_raw(),
            width,
            height,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
    out
}

/// A lossless WebP, as its `VP8L` chunk.
fn encode_webp(width: u32, height: u32) -> Vec<u8> {
    let image = gradient(width, height);
    let mut out = Vec::new();
    WebPEncoder::new_lossless(&mut out)
        .write_image(
            image.as_raw(),
            width,
            height,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
    // RIFF header (12 bytes), then the only chunk.
    out[12..].to_vec()
}

fn vp8l_data(chunk: &[u8]) -> Vec<u8> {
    let size = u32::from_le_bytes(chunk[4..8].try_into().unwrap()) as usize;
    chunk[8..8 + size].to_vec()
}

fn riff_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = kind.to_vec();
    out.extend(u32::try_from(data.len()).unwrap().to_le_bytes());
    out.extend(data);
    if data.len() % 2 == 1 {
        out.push(0);
    }
    out
}

fn vp8x(flags: u8, width: u32, height: u32) -> Vec<u8> {
    let mut data = vec![flags, 0, 0, 0];
    data.extend(&(width - 1).to_le_bytes()[..3]);
    data.extend(&(height - 1).to_le_bytes()[..3]);
    riff_chunk(b"VP8X", &data)
}

fn riff(chunks: &[Vec<u8>], trailing: &[u8]) -> Vec<u8> {
    let body = [b"WEBP".to_vec(), chunks.concat()].concat();
    let mut out = b"RIFF".to_vec();
    out.extend(u32::try_from(body.len()).unwrap().to_le_bytes());
    out.extend(body);
    out.extend(trailing);
    out
}

fn segment(marker: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0xFF, marker];
    out.extend(u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
    out.extend(payload);
    out
}

/// Inserts segments right after SOI and appends data after EOI.
fn with_segments(jpeg: &[u8], segments: &[Vec<u8>], trailing: &[u8]) -> Vec<u8> {
    let mut out = jpeg[..2].to_vec();
    for s in segments {
        out.extend(s);
    }
    out.extend(&jpeg[2..]);
    out.extend(trailing);
    out
}

fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(data.len()).unwrap().to_be_bytes().to_vec();
    let body = [kind.as_slice(), data].concat();
    let mut crc = flate2::Crc::new();
    crc.update(&body);
    out.extend(&body);
    out.extend(crc.sum().to_be_bytes());
    out
}

/// Inserts chunks after IHDR (signature + IHDR chunk = 33 bytes) and
/// appends data after IEND.
fn with_chunks(png: &[u8], chunks: &[Vec<u8>], trailing: &[u8]) -> Vec<u8> {
    let mut out = png[..33].to_vec();
    for c in chunks {
        out.extend(c);
    }
    out.extend(&png[33..]);
    out.extend(trailing);
    out
}

fn deflate(data: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

// --- EXIF (big-endian TIFF) ---

enum Val<'a> {
    Ascii(&'a str),
    Short(u16),
    Long(u32),
    Rational(&'a [(u32, u32)]),
}

impl Val<'_> {
    /// TIFF type code, count and raw bytes.
    fn encode(&self) -> (u16, u32, Vec<u8>) {
        match self {
            Val::Ascii(s) => {
                let mut bytes = s.as_bytes().to_vec();
                bytes.push(0);
                (2, bytes.len() as u32, bytes)
            }
            Val::Short(v) => (3, 1, v.to_be_bytes().to_vec()),
            Val::Long(v) => (4, 1, v.to_be_bytes().to_vec()),
            Val::Rational(parts) => (
                5,
                parts.len() as u32,
                parts
                    .iter()
                    .flat_map(|(n, d)| [n.to_be_bytes(), d.to_be_bytes()].concat())
                    .collect(),
            ),
        }
    }
}

fn ifd_len(entries: &[(u16, Val<'_>)]) -> usize {
    let data: usize = entries
        .iter()
        .map(|(_, v)| v.encode().2.len())
        .filter(|&len| len > 4)
        .map(|len| len + len % 2)
        .sum();
    2 + entries.len() * 12 + 4 + data
}

/// An IFD at `start`, followed by its out-of-line values.
fn write_ifd(entries: &mut [(u16, Val<'_>)], start: usize, next: u32) -> Vec<u8> {
    entries.sort_by_key(|(tag, _)| *tag);
    let mut out = (entries.len() as u16).to_be_bytes().to_vec();
    let mut data = Vec::new();
    let mut data_pos = start + 2 + entries.len() * 12 + 4;
    for (tag, value) in entries.iter() {
        let (kind, count, bytes) = value.encode();
        out.extend(tag.to_be_bytes());
        out.extend(kind.to_be_bytes());
        out.extend(count.to_be_bytes());
        if bytes.len() <= 4 {
            let mut inline = bytes.clone();
            inline.resize(4, 0);
            out.extend(inline);
        } else {
            out.extend((data_pos as u32).to_be_bytes());
            data.extend(&bytes);
            if bytes.len() % 2 == 1 {
                data.push(0);
            }
            data_pos += bytes.len() + bytes.len() % 2;
        }
    }
    out.extend(next.to_be_bytes());
    out.extend(data);
    out
}

fn camera_tiff(thumbnail: Option<&[u8]>) -> Vec<u8> {
    let mut ifd0 = vec![
        (0x010F, Val::Ascii("Fictional Camera Co.")),
        (0x0110, Val::Ascii("FC-100")),
        (0x0112, Val::Short(if thumbnail.is_some() { 6 } else { 3 })),
        (0x0131, Val::Ascii("PhotoTool 2.1")),
        (0x0132, Val::Ascii("2026:03:14 09:26:53")),
        (0x013B, Val::Ascii("Jane Doe")),
        (0x8298, Val::Ascii("Copyright Jane Doe")),
        (0x8769, Val::Long(0)), // Exif IFD pointer, patched below
        (0x8825, Val::Long(0)), // GPS IFD pointer, patched below
    ];
    let mut exif = vec![
        (0x829A, Val::Rational(&[(1, 125)])),
        (0x829D, Val::Rational(&[(28, 10)])),
        (0x9003, Val::Ascii("2026:03:14 09:26:53")),
        (0xA431, Val::Ascii("SN-0042-7781")),
        (0xA434, Val::Ascii("FC 24-70mm")),
    ];
    // The Eiffel Tower: 48°51'30.24" N, 2°17'40.20" E.
    let mut gps = vec![
        (0x0001, Val::Ascii("N")),
        (0x0002, Val::Rational(&[(48, 1), (51, 1), (3024, 100)])),
        (0x0003, Val::Ascii("E")),
        (0x0004, Val::Rational(&[(2, 1), (17, 1), (4020, 100)])),
    ];

    let ifd0_pos = 8;
    let exif_pos = ifd0_pos + ifd_len(&ifd0);
    let gps_pos = exif_pos + ifd_len(&exif);
    let ifd1_pos = gps_pos + ifd_len(&gps);
    ifd0[7].1 = Val::Long(exif_pos as u32);
    ifd0[8].1 = Val::Long(gps_pos as u32);

    let mut out = b"MM\0\x2a\0\0\0\x08".to_vec();
    let next = if thumbnail.is_some() {
        ifd1_pos as u32
    } else {
        0
    };
    out.extend(write_ifd(&mut ifd0, ifd0_pos, next));
    out.extend(write_ifd(&mut exif, exif_pos, 0));
    out.extend(write_ifd(&mut gps, gps_pos, 0));
    if let Some(thumbnail) = thumbnail {
        let mut ifd1 = vec![
            (0x0103, Val::Short(6)),
            (0x0201, Val::Long(0)),
            (0x0202, Val::Long(thumbnail.len() as u32)),
        ];
        let thumb_pos = ifd1_pos + ifd_len(&ifd1);
        ifd1[1].1 = Val::Long(thumb_pos as u32);
        out.extend(write_ifd(&mut ifd1, ifd1_pos, 0));
        out.extend(thumbnail);
    }
    out
}

// --- XMP and IPTC ---

const XMP: &str = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/"
    xmlns:exif="http://ns.adobe.com/exif/1.0/"
    xmlns:xmpMM="http://ns.adobe.com/xap/1.0/mm/"
    xmlns:stRef="http://ns.adobe.com/xap/1.0/sType/ResourceRef#"
    xmlns:vendor="http://example.com/fictional-vendor/"
    xmp:CreatorTool="PhotoTool 2.1"
    xmp:Rating="4"
    photoshop:City="Paris"
    exif:GPSLatitude="48,51.504N"
    xmpMM:DocumentID="xmp.did:5f1c2e9a-0b7d-4c3e-9a61-2d8f4b7e1c03"
    vendor:Session="a81f">
   <dc:creator><rdf:Seq><rdf:li>Jane Doe</rdf:li></rdf:Seq></dc:creator>
   <xmpMM:DerivedFrom stRef:filePath="C:\Users\jane\Pictures\holiday.psd"/>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;

fn iptc_record(dataset: u8, text: &str) -> Vec<u8> {
    let mut out = vec![0x1C, 2, dataset];
    out.extend((text.len() as u16).to_be_bytes());
    out.extend(text.as_bytes());
    out
}

fn iptc() -> Vec<u8> {
    [
        iptc_record(80, "Jane Doe"),
        iptc_record(90, "Paris"),
        iptc_record(120, "Our first day in Paris"),
        iptc_record(25, "holiday"),
        iptc_record(25, "family"),
    ]
    .concat()
}

fn photoshop_resource(id: u16, data: &[u8]) -> Vec<u8> {
    let mut out = b"8BIM".to_vec();
    out.extend(id.to_be_bytes());
    out.extend([0, 0]); // empty, padded name
    out.extend((data.len() as u32).to_be_bytes());
    out.extend(data);
    if data.len() % 2 == 1 {
        out.push(0);
    }
    out
}

fn photoshop_resources(thumbnail: &[u8]) -> Vec<u8> {
    [
        b"Photoshop 3.0\0".to_vec(),
        photoshop_resource(0x0404, &iptc()),
        photoshop_resource(0x040C, thumbnail),
    ]
    .concat()
}

/// ImageMagick's hex text form of an IPTC block.
fn raw_iptc_profile() -> Vec<u8> {
    let data = iptc();
    let hex: String = data.iter().map(|b| format!("{b:02x}")).collect();
    format!("Raw profile type iptc\0\niptc\n{:8}\n{hex}\n", data.len()).into_bytes()
}

// --- ICC ---

type Xyz = [f64; 3];
const SRGB: [Xyz; 3] = [
    [0.4361, 0.2225, 0.0139],
    [0.3851, 0.7169, 0.0971],
    [0.1431, 0.0606, 0.7141],
];
const DISPLAY_P3: [Xyz; 3] = [
    [0.5151, 0.2412, -0.0010],
    [0.2920, 0.6922, 0.0419],
    [0.1571, 0.0666, 0.7841],
];

fn icc_profile(description: &str, primaries: [Xyz; 3], calibration: bool) -> Vec<u8> {
    let mut tags: Vec<([u8; 4], Vec<u8>)> = Vec::new();
    let mut desc = b"desc\0\0\0\0".to_vec();
    desc.extend((description.len() as u32 + 1).to_be_bytes());
    desc.extend(description.as_bytes());
    desc.push(0);
    tags.push((*b"desc", desc));
    for (sig, xyz) in [*b"rXYZ", *b"gXYZ", *b"bXYZ"].into_iter().zip(primaries) {
        let mut data = b"XYZ \0\0\0\0".to_vec();
        for v in xyz {
            data.extend(((v * 65536.0).round() as i32).to_be_bytes());
        }
        tags.push((sig, data));
    }
    if calibration {
        tags.push((*b"vcgt", vec![0; 12]));
    }

    let mut out = vec![0u8; 128];
    out[12..16].copy_from_slice(b"mntr");
    out[16..20].copy_from_slice(b"RGB ");
    out[36..40].copy_from_slice(b"acsp");
    out.extend((tags.len() as u32).to_be_bytes());
    let mut offset = 128 + 4 + tags.len() * 12;
    let mut body: Vec<u8> = Vec::new();
    for (sig, data) in &tags {
        out.extend(sig);
        out.extend((offset as u32).to_be_bytes());
        out.extend((data.len() as u32).to_be_bytes());
        offset += data.len();
        body.extend(data);
    }
    out.extend(body);
    let size = out.len() as u32;
    out[0..4].copy_from_slice(&size.to_be_bytes());
    out
}

fn icc_segment(profile: &[u8]) -> Vec<u8> {
    segment(
        0xE2,
        &[b"ICC_PROFILE\0\x01\x01".as_slice(), profile].concat(),
    )
}

/// Just enough of an MP4 to look like a Motion Photo's appended video.
fn fake_mp4() -> Vec<u8> {
    let mut out = b"\0\0\0\x18ftypmp42\0\0\0\0mp42isom".to_vec();
    out.extend(b"\0\0\0\x10mdat");
    out.extend([0x42; 8]);
    out
}
