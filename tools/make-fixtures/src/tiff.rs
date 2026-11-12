//! TIFF and DNG files built by hand (a small gradient), with the metadata
//! scanners, cameras, raw converters, Photoshop and map software add.

use std::io::Write;

use flate2::Compression;
use flate2::write::ZlibEncoder;

const WIDTH: u16 = 16;
const HEIGHT: u16 = 12;

pub struct Files {
    pub clean: Vec<u8>,
    pub tagged: Vec<u8>,
    pub pages: Vec<u8>,
    pub bigtiff: Vec<u8>,
    pub deflate: Vec<u8>,
    pub jpeg: Vec<u8>,
    pub rotated: Vec<u8>,
    pub icc: Vec<u8>,
    pub srgb: Vec<u8>,
    pub geotiff: Vec<u8>,
    pub layers: Vec<u8>,
    pub clean_dng: Vec<u8>,
    pub camera_dng: Vec<u8>,
}

/// A tag's value; [`build`] places it in the file.
#[derive(Clone)]
enum V {
    Short(u16),
    Shorts(Vec<u16>),
    Long(u32),
    Ascii(&'static str),
    Rationals(Vec<(u32, u32)>),
    SRationals(Vec<(i32, i32)>),
    Doubles(Vec<f64>),
    Bytes(Vec<u8>),
    Undefined(Vec<u8>),
    /// Picture data: StripOffsets, with StripByteCounts added.
    Strip(Vec<u8>),
    /// A pointer to another directory (EXIF, GPS).
    Ifd(Vec<(u16, V)>),
    /// SubIFDs.
    Ifds(Vec<Vec<(u16, V)>>),
    /// Bytes no tag points to.
    Unreferenced(Vec<u8>),
}

struct Builder {
    little: bool,
    big: bool,
    out: Vec<u8>,
}

impl Builder {
    fn n16(&self, n: u16) -> Vec<u8> {
        if self.little {
            n.to_le_bytes()
        } else {
            n.to_be_bytes()
        }
        .to_vec()
    }

    fn n32(&self, n: u32) -> Vec<u8> {
        if self.little {
            n.to_le_bytes()
        } else {
            n.to_be_bytes()
        }
        .to_vec()
    }

    fn n64(&self, n: u64) -> Vec<u8> {
        if self.little {
            n.to_le_bytes()
        } else {
            n.to_be_bytes()
        }
        .to_vec()
    }

    fn offset(&self, n: u64) -> (u16, Vec<u8>) {
        if self.big {
            (16, self.n64(n))
        } else {
            (4, self.n32(n as u32))
        }
    }

    fn place(&mut self, data: &[u8]) -> u64 {
        if self.out.len() % 2 == 1 {
            self.out.push(0);
        }
        let at = self.out.len() as u64;
        self.out.extend_from_slice(data);
        at
    }

    /// Writes the values, then the directory; returns where it is.
    fn ifd(&mut self, entries: &[(u16, V)], next: u64) -> u64 {
        let mut flat: Vec<(u16, u16, u64, Vec<u8>)> = Vec::new();
        for (tag, value) in entries {
            let tag = *tag;
            match value {
                V::Short(n) => flat.push((tag, 3, 1, self.n16(*n))),
                V::Shorts(ns) => {
                    let b = ns.iter().flat_map(|n| self.n16(*n)).collect();
                    flat.push((tag, 3, ns.len() as u64, b));
                }
                V::Long(n) => flat.push((tag, 4, 1, self.n32(*n))),
                V::Ascii(s) => {
                    let mut b = s.as_bytes().to_vec();
                    b.push(0);
                    flat.push((tag, 2, b.len() as u64, b));
                }
                V::Rationals(rs) => {
                    let b = rs
                        .iter()
                        .flat_map(|(n, d)| [self.n32(*n), self.n32(*d)].concat())
                        .collect();
                    flat.push((tag, 5, rs.len() as u64, b));
                }
                V::SRationals(rs) => {
                    let b = rs
                        .iter()
                        .flat_map(|(n, d)| [self.n32(*n as u32), self.n32(*d as u32)].concat())
                        .collect();
                    flat.push((tag, 10, rs.len() as u64, b));
                }
                V::Doubles(ds) => {
                    let b = ds.iter().flat_map(|d| self.n64(d.to_bits())).collect();
                    flat.push((tag, 12, ds.len() as u64, b));
                }
                V::Bytes(b) => flat.push((tag, 1, b.len() as u64, b.clone())),
                V::Undefined(b) => flat.push((tag, 7, b.len() as u64, b.clone())),
                V::Strip(data) => {
                    let at = self.place(data);
                    let (kind, b) = self.offset(at);
                    flat.push((273, kind, 1, b));
                    let (kind, b) = self.offset(data.len() as u64);
                    flat.push((279, kind, 1, b));
                }
                V::Ifd(child) => {
                    let at = self.ifd(child, 0);
                    let (kind, b) = self.offset(at);
                    flat.push((tag, kind, 1, b));
                }
                V::Ifds(children) => {
                    let mut b = Vec::new();
                    let mut kind = 4;
                    for child in children {
                        let at = self.ifd(child, 0);
                        let (k, bytes) = self.offset(at);
                        kind = k;
                        b.extend(bytes);
                    }
                    flat.push((tag, kind, children.len() as u64, b));
                }
                V::Unreferenced(data) => {
                    self.place(data);
                }
            }
        }
        flat.sort_by_key(|e| e.0);
        let inline = if self.big { 8 } else { 4 };
        let mut placed = Vec::new();
        for (_, _, _, b) in &flat {
            placed.push((b.len() > inline).then(|| self.place(b)));
        }
        if self.out.len() % 2 == 1 {
            self.out.push(0);
        }
        let at = self.out.len() as u64;
        let mut table = if self.big {
            self.n64(flat.len() as u64)
        } else {
            self.n16(flat.len() as u16)
        };
        for ((tag, kind, count, b), placed) in flat.iter().zip(placed) {
            table.extend(self.n16(*tag));
            table.extend(self.n16(*kind));
            table.extend(if self.big {
                self.n64(*count)
            } else {
                self.n32(*count as u32)
            });
            let mut value = match placed {
                Some(at) => self.offset(at).1,
                None => b.clone(),
            };
            value.resize(inline, 0);
            table.extend(value);
        }
        table.extend(self.offset(next).1);
        self.out.extend(table);
        at
    }
}

fn build(pages: &[Vec<(u16, V)>], little: bool, big: bool) -> Vec<u8> {
    let mut b = Builder {
        little,
        big,
        out: Vec::new(),
    };
    b.out.extend(if little { b"II" } else { b"MM" });
    if big {
        b.out.extend(b.n16(43));
        b.out.extend(b.n16(8));
        b.out.extend(b.n16(0));
        b.out.extend([0; 8]);
    } else {
        b.out.extend(b.n16(42));
        b.out.extend([0; 4]);
    }
    let mut next = 0;
    for page in pages.iter().rev() {
        next = b.ifd(page, next);
    }
    let first = b.offset(next).1;
    let at = if big { 8 } else { 4 };
    b.out[at..at + first.len()].copy_from_slice(&first);
    b.out
}

/// The gradient as 8-bit RGB, shifted by `offset`.
fn rgb(offset: usize) -> Vec<u8> {
    let (w, h) = (usize::from(WIDTH), usize::from(HEIGHT));
    (0..h)
        .flat_map(|y| {
            (0..w).flat_map(move |x| {
                [
                    ((x * 255 / w + offset) % 256) as u8,
                    (y * 255 / h) as u8,
                    128,
                ]
            })
        })
        .collect()
}

/// The tags of an RGB picture, with `pixels` stored as compression `compression`.
fn picture(pixels: Vec<u8>, compression: u16) -> Vec<(u16, V)> {
    vec![
        (256, V::Short(WIDTH)),
        (257, V::Short(HEIGHT)),
        (258, V::Shorts(vec![8, 8, 8])),
        (259, V::Short(compression)),
        (262, V::Short(if compression == 7 { 6 } else { 2 })),
        (273, V::Strip(pixels)),
        (277, V::Short(3)),
        (278, V::Short(HEIGHT)),
        (282, V::Rationals(vec![(300, 1)])),
        (283, V::Rationals(vec![(300, 1)])),
        (284, V::Short(1)),
        (296, V::Short(2)),
    ]
}

fn with(mut tags: Vec<(u16, V)>, more: Vec<(u16, V)>) -> Vec<(u16, V)> {
    tags.extend(more);
    tags
}

const XMP: &str = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:xmp="http://ns.adobe.com/xap/1.0/" dc:creator="Jane Doe" xmp:CreatorTool="ScanTool 2.1" xmp:CreateDate="2026-03-14T09:26:52"/></rdf:RDF></x:xmpmeta><?xpacket end="w"?>"#;

/// An IPTC record: the by-line (author) and city.
fn iptc() -> Vec<u8> {
    let mut out = Vec::new();
    for (dataset, text) in [(80u8, "Jane Doe"), (90, "Lisbon")] {
        out.extend([0x1C, 2, dataset]);
        out.extend((text.len() as u16).to_be_bytes());
        out.extend(text.as_bytes());
    }
    out
}

fn gps() -> Vec<(u16, V)> {
    vec![
        (0, V::Bytes(vec![2, 3, 0, 0])),
        (1, V::Ascii("N")),
        (2, V::Rationals(vec![(38, 1), (42, 1), (3018, 100)])),
        (3, V::Ascii("W")),
        (4, V::Rationals(vec![(9, 1), (8, 1), (3492, 100)])),
    ]
}

fn camera_exif() -> Vec<(u16, V)> {
    vec![
        (0x829A, V::Rationals(vec![(1, 250)])),
        (0x9003, V::Ascii("2026:03:14 09:26:52")),
        (
            0x9286,
            V::Undefined(b"ASCII\0\0\0At the Lisbon office".to_vec()),
        ),
        (
            0x927C,
            V::Undefined(b"private camera data, serial 4012345".to_vec()),
        ),
        (0xA431, V::Ascii("4012345")),
        (0xA435, V::Ascii("LENS-0077")),
        (0xA005, V::Ifd(vec![(1, V::Ascii("R98"))])),
    ]
}

/// Photoshop image resources: an IPTC record and its digest.
fn photoshop() -> Vec<u8> {
    let mut out = Vec::new();
    for (id, data) in [(0x0404u16, iptc()), (0x0425, vec![0xAB; 16])] {
        out.extend(b"8BIM");
        out.extend(id.to_be_bytes());
        out.extend([0, 0]);
        out.extend((data.len() as u32).to_be_bytes());
        out.extend(&data);
        if data.len() % 2 == 1 {
            out.push(0);
        }
    }
    out
}

/// DNG's color tags for a camera that sees the world as XYZ, roughly.
fn dng_color() -> Vec<(u16, V)> {
    vec![
        (50706, V::Bytes(vec![1, 6, 0, 0])),
        (50707, V::Bytes(vec![1, 4, 0, 0])),
        (50708, V::Ascii("Canon EOS R5")),
        (
            50721,
            V::SRationals(vec![
                (6532, 10000),
                (-1003, 10000),
                (-640, 10000),
                (-3922, 10000),
                (11498, 10000),
                (2694, 10000),
                (-779, 10000),
                (1599, 10000),
                (6004, 10000),
            ]),
        ),
        (50728, V::Rationals(vec![(473, 1000), (1, 1), (650, 1000)])),
        (50778, V::Short(21)),
    ]
}

/// A 16-bit color filter array (RGGB) of the gradient.
fn raw() -> Vec<(u16, V)> {
    let pixels = rgb(0);
    let (w, h) = (usize::from(WIDTH), usize::from(HEIGHT));
    let mut data = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let channel = match (y % 2, x % 2) {
                (0, 0) => 0,
                (1, 1) => 2,
                _ => 1,
            };
            let value = u16::from(pixels[(y * w + x) * 3 + channel]) * 16 + 64;
            data.extend(value.to_le_bytes());
        }
    }
    vec![
        (254, V::Long(0)),
        (256, V::Short(WIDTH)),
        (257, V::Short(HEIGHT)),
        (258, V::Short(16)),
        (259, V::Short(1)),
        (262, V::Short(32803)),
        (273, V::Strip(data)),
        (277, V::Short(1)),
        (278, V::Short(HEIGHT)),
        (284, V::Short(1)),
        (33421, V::Shorts(vec![2, 2])),
        (33422, V::Bytes(vec![0, 1, 1, 2])),
        (50710, V::Bytes(vec![0, 1, 2])),
        (50711, V::Short(1)),
        (50714, V::Shorts(vec![64])),
        (50717, V::Shorts(vec![4200])),
        (50719, V::Shorts(vec![0, 0])),
        (50720, V::Shorts(vec![WIDTH, HEIGHT])),
    ]
}

/// A DNG: a preview in the first directory, the raw data in a sub-image.
fn dng(more: Vec<(u16, V)>) -> Vec<u8> {
    let preview = with(
        picture(rgb(0), 1),
        [
            vec![(254, V::Long(1)), (330, V::Ifds(vec![raw()]))],
            dng_color(),
            more,
        ]
        .concat(),
    );
    build(&[preview], true, false)
}

/// A baseline JPEG with a comment and EXIF of its own, as a TIFF strip.
fn jpeg_strip(jpeg: &[u8]) -> Vec<u8> {
    let mut comment = vec![0xFF, 0xFE];
    let text = b"Scanned by Jane Doe";
    comment.extend(((text.len() + 2) as u16).to_be_bytes());
    comment.extend(text);
    [&jpeg[..2], &comment, &jpeg[2..]].concat()
}

pub fn files(standard_icc: &[u8], calibrated_icc: &[u8], jpeg: &[u8]) -> Files {
    let mut deflated = ZlibEncoder::new(Vec::new(), Compression::best());
    deflated.write_all(&rgb(0)).unwrap();
    let mut tagged = build(
        &[with(
            picture(rgb(0), 1),
            vec![
                (269, V::Ascii("C:\\Users\\jane\\Scans\\passport.tif")),
                (270, V::Ascii("Passport, page 2")),
                (271, V::Ascii("Epson")),
                (272, V::Ascii("Perfection V600")),
                (305, V::Ascii("ScanTool 2.1")),
                (306, V::Ascii("2026:03:14 09:26:52")),
                (315, V::Ascii("Jane Doe")),
                (316, V::Ascii("JANE-LAPTOP")),
                (700, V::Bytes(XMP.as_bytes().to_vec())),
                (33432, V::Ascii("(c) 2026 Jane Doe")),
                (33723, V::Bytes(iptc())),
                (34665, V::Ifd(camera_exif())),
                (34853, V::Ifd(gps())),
                (0, V::Unreferenced(b"Jane Doe, scan 14".to_vec())),
            ],
        )],
        true,
        false,
    );
    tagged.extend(b"appended after the last strip");
    let page = |shift: usize, artist: &'static str, number: u16| {
        with(
            picture(rgb(shift), 1),
            vec![
                (254, V::Long(2)),
                (297, V::Shorts(vec![number, 2])),
                (315, V::Ascii(artist)),
                (305, V::Ascii("FaxTool 3")),
            ],
        )
    };
    Files {
        clean: build(&[picture(rgb(0), 1)], true, false),
        tagged,
        pages: build(
            &[page(0, "Jane Doe", 0), page(64, "John Roe", 1)],
            true,
            false,
        ),
        bigtiff: build(
            &[with(
                picture(rgb(0), 1),
                vec![(305, V::Ascii("GDAL 3.8")), (315, V::Ascii("Jane Doe"))],
            )],
            false,
            true,
        ),
        deflate: build(
            &[with(
                picture(deflated.finish().unwrap(), 8),
                vec![(305, V::Ascii("ScanTool 2.1"))],
            )],
            true,
            false,
        ),
        jpeg: build(
            &[with(
                picture(jpeg_strip(jpeg), 7),
                vec![(305, V::Ascii("ScanTool 2.1"))],
            )],
            true,
            false,
        ),
        rotated: build(
            &[with(
                picture(rgb(0), 1),
                vec![(274, V::Short(6)), (272, V::Ascii("Pixel 9"))],
            )],
            false,
            false,
        ),
        icc: build(
            &[with(
                picture(rgb(0), 1),
                vec![(34675, V::Undefined(calibrated_icc.to_vec()))],
            )],
            true,
            false,
        ),
        srgb: build(
            &[with(
                picture(rgb(0), 1),
                vec![(34675, V::Undefined(standard_icc.to_vec()))],
            )],
            true,
            false,
        ),
        geotiff: build(
            &[with(
                picture(rgb(0), 1),
                vec![
                    (33550, V::Doubles(vec![0.0001, 0.0001, 0.0])),
                    (
                        33922,
                        V::Doubles(vec![0.0, 0.0, 0.0, -9.1427, 38.7369, 0.0]),
                    ),
                    // Geographic (WGS 84), pixels are areas.
                    (
                        34735,
                        V::Shorts(vec![
                            1, 1, 0, 3, 1024, 0, 1, 2, 1025, 0, 1, 1, 2048, 0, 1, 4326,
                        ]),
                    ),
                    (305, V::Ascii("QGIS 3.34")),
                ],
            )],
            true,
            false,
        ),
        layers: build(
            &[with(
                picture(rgb(0), 1),
                vec![
                    (305, V::Ascii("Adobe Photoshop 25.0 (Windows)")),
                    (34377, V::Bytes(photoshop())),
                    (
                        37724,
                        V::Undefined(
                            [
                                b"Adobe Photoshop Document Data Block\0".as_slice(),
                                b"8BIMLayr",
                                &32u32.to_be_bytes(),
                                b"layer: Jane's signature (hidden)",
                            ]
                            .concat(),
                        ),
                    ),
                ],
            )],
            true,
            false,
        ),
        clean_dng: dng(Vec::new()),
        camera_dng: dng(vec![
            (271, V::Ascii("Canon")),
            (272, V::Ascii("Canon EOS R5")),
            (305, V::Ascii("Adobe DNG Converter 16.0")),
            (306, V::Ascii("2026:03:14 09:26:52")),
            (315, V::Ascii("Jane Doe")),
            (34665, V::Ifd(camera_exif())),
            (34853, V::Ifd(gps())),
            (50735, V::Ascii("012345678901")),
            (
                50740,
                V::Bytes(b"private camera data, serial 0123".to_vec()),
            ),
            (50781, V::Bytes(vec![0x5A; 16])),
            (50827, V::Ascii("IMG_0042.CR3")),
            (50966, V::Ascii("Adobe Lightroom")),
            (50971, V::Ascii("2026-03-14T10:02:11")),
        ]),
    }
}
