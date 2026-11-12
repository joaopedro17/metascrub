//! GIF and BMP pictures built by hand (a small gradient), with the
//! metadata each format can carry: GIF comments, XMP, color profiles and
//! programs' own extensions; BMP's linked or embedded color profile and
//! bytes the format leaves unused.

const WIDTH: usize = 16;
const HEIGHT: usize = 12;

pub struct Files {
    pub clean_gif: Vec<u8>,
    pub tagged_gif: Vec<u8>,
    pub animated_gif: Vec<u8>,
    pub icc_gif: Vec<u8>,
    pub clean_bmp: Vec<u8>,
    pub linked_bmp: Vec<u8>,
    pub calibrated_bmp: Vec<u8>,
    pub srgb_bmp: Vec<u8>,
    pub padded_bmp: Vec<u8>,
}

pub fn files(standard_icc: &[u8], calibrated_icc: &[u8]) -> Files {
    let looping = application(b"NETSCAPE2.0", &[1, 0, 0]);
    let frame_delay = extension(0xF9, &[0x04, 10, 0, 0]);
    let mut xmp = vec![0x21, 0xFF, 11];
    xmp.extend_from_slice(b"XMP DataXMP");
    xmp.extend_from_slice(XMP.as_bytes());
    // The "magic trailer" that lets readers skip the packet as sub-blocks.
    xmp.push(1);
    xmp.extend((0..=255u8).rev());
    xmp.push(0);
    Files {
        clean_gif: gif(&[], &[frame(0)], b""),
        tagged_gif: gif(
            &[
                extension(0xFE, b"Scanned by Jane Doe at the Lisbon office"),
                xmp,
                application(b"PHOTOTL1.00", b"PhotoTool 2.1, licensed to Jane Doe"),
            ],
            &[frame(0)],
            b"appended after the trailer",
        ),
        animated_gif: gif(
            &[looping],
            &[
                [frame_delay.clone(), frame(0)].concat(),
                [frame_delay, frame(64)].concat(),
            ],
            b"",
        ),
        icc_gif: gif(
            &[application(b"ICCRGBG1012", calibrated_icc)],
            &[frame(0)],
            b"",
        ),
        clean_bmp: bmp(None, &[], false),
        linked_bmp: bmp(
            Some((
                *b"KNIL",
                b"C:\\Users\\jane\\Color\\EIZO-CG2700S-calibrated.icc\0".to_vec(),
            )),
            &[],
            false,
        ),
        calibrated_bmp: bmp(Some((*b"DEBM", calibrated_icc.to_vec())), &[], false),
        srgb_bmp: bmp(Some((*b"DEBM", standard_icc.to_vec())), &[], false),
        padded_bmp: bmp(None, b"Jane Doe, scan 14", true),
    }
}

const XMP: &str = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:xmp="http://ns.adobe.com/xap/1.0/" dc:creator="Jane Doe" xmp:CreatorTool="PhotoTool 2.1" xmp:CreateDate="2026-03-14T09:26:52"/></rdf:RDF></x:xmpmeta><?xpacket end="w"?>"#;

fn extension(label: u8, data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x21, label];
    for chunk in data.chunks(255) {
        out.push(chunk.len() as u8);
        out.extend_from_slice(chunk);
    }
    out.push(0);
    out
}

/// An application extension: an 11-byte name, then its data.
fn application(id: &[u8; 11], data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x21, 0xFF, 11];
    out.extend_from_slice(id);
    for chunk in data.chunks(255) {
        out.push(chunk.len() as u8);
        out.extend_from_slice(chunk);
    }
    out.push(0);
    out
}

/// One frame of the gradient, shifted by `offset` gray levels. Each pixel
/// is a literal code after a clear code, so no LZW dictionary is needed.
fn frame(offset: usize) -> Vec<u8> {
    let mut out = vec![0x2C, 0, 0, 0, 0];
    out.extend_from_slice(&(WIDTH as u16).to_le_bytes());
    out.extend_from_slice(&(HEIGHT as u16).to_le_bytes());
    out.push(0);
    // 8-bit pixels: codes of 9 bits, clear = 256, end = 257.
    out.push(8);
    let mut codes = Vec::new();
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            codes.push(256u32);
            codes.push(((x * 255 / WIDTH + y * 4 + offset) % 256) as u32);
        }
    }
    codes.push(257);
    let mut bytes = Vec::new();
    let (mut bits, mut count) = (0u64, 0);
    for code in codes {
        bits |= u64::from(code) << count;
        count += 9;
        while count >= 8 {
            bytes.push(bits as u8);
            bits >>= 8;
            count -= 8;
        }
    }
    if count > 0 {
        bytes.push(bits as u8);
    }
    for chunk in bytes.chunks(255) {
        out.push(chunk.len() as u8);
        out.extend_from_slice(chunk);
    }
    out.push(0);
    out
}

/// A 256-gray GIF: `extensions`, then `frames`, the trailer and `after`.
fn gif(extensions: &[Vec<u8>], frames: &[Vec<u8>], after: &[u8]) -> Vec<u8> {
    let mut out = b"GIF89a".to_vec();
    out.extend_from_slice(&(WIDTH as u16).to_le_bytes());
    out.extend_from_slice(&(HEIGHT as u16).to_le_bytes());
    out.extend_from_slice(&[0xF7, 0, 0]);
    for gray in 0..=255u8 {
        out.extend_from_slice(&[gray, gray, gray]);
    }
    out.extend(extensions.concat());
    out.extend(frames.concat());
    out.push(0x3B);
    out.extend_from_slice(after);
    out
}

/// A 24-bit BMP with a V5 header. `profile` is placed after the pixels
/// (as Windows does) with its kind: linked (`KNIL`) or embedded (`DEBM`),
/// as the little-endian bytes the header stores. `after` follows; with
/// `reserved`, the file header's reserved bytes aren't zero.
fn bmp(profile: Option<([u8; 4], Vec<u8>)>, after: &[u8], reserved: bool) -> Vec<u8> {
    const HEADER: usize = 124;
    let row = (WIDTH * 3).div_ceil(4) * 4;
    let mut pixels = Vec::new();
    for y in (0..HEIGHT).rev() {
        let mut line: Vec<u8> = (0..WIDTH)
            .flat_map(|x| [128, (y * 255 / HEIGHT) as u8, (x * 255 / WIDTH) as u8])
            .collect();
        line.resize(row, 0);
        pixels.extend(line);
    }
    let mut header = vec![0u8; HEADER];
    header[..4].copy_from_slice(&(HEADER as u32).to_le_bytes());
    header[4..8].copy_from_slice(&(WIDTH as u32).to_le_bytes());
    header[8..12].copy_from_slice(&(HEIGHT as u32).to_le_bytes());
    header[12..14].copy_from_slice(&1u16.to_le_bytes());
    header[14..16].copy_from_slice(&24u16.to_le_bytes());
    header[20..24].copy_from_slice(&(pixels.len() as u32).to_le_bytes());
    // 2835 pixels per meter: 72 dpi.
    header[24..28].copy_from_slice(&2835u32.to_le_bytes());
    header[28..32].copy_from_slice(&2835u32.to_le_bytes());
    header[56..60].copy_from_slice(b"BGRs");
    // Rendering intent: "pictures" (LCS_GM_IMAGES).
    header[108..112].copy_from_slice(&4u32.to_le_bytes());
    let mut trailer = Vec::new();
    if let Some((kind, data)) = profile {
        header[56..60].copy_from_slice(&kind);
        header[112..116].copy_from_slice(&((HEADER + pixels.len()) as u32).to_le_bytes());
        header[116..120].copy_from_slice(&(data.len() as u32).to_le_bytes());
        trailer = data;
    }
    let pixels_at = 14 + HEADER;
    let size = pixels_at + pixels.len() + trailer.len() + after.len();
    let mut out = b"BM".to_vec();
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(if reserved { b"JD26" } else { &[0; 4] });
    out.extend_from_slice(&(pixels_at as u32).to_le_bytes());
    [out, header, pixels, trailer, after.to_vec()].concat()
}
