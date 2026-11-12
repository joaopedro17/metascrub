//! An Ultra HDR photo as phones write it: a JPEG with the HDR version and
//! a directory in its XMP, an MPF segment pointing to the gain map, then the
//! gain map (a second JPEG with its settings), then a Motion Photo video.

const XMP: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
const ISO: &[u8] = b"urn:iso:std:iso:ts:21496:-1\0";

fn segment(marker: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0xFF, marker];
    out.extend(u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
    out.extend(payload);
    out
}

/// Inserts segments right after SOI.
fn with_segments(jpeg: &[u8], segments: &[Vec<u8>]) -> Vec<u8> {
    [&jpeg[..2], &segments.concat(), &jpeg[2..]].concat()
}

/// An MPF segment's payload listing the main image and the gain map right
/// after it; `at` is where `MPF\0` is in the file.
fn mpf(at: usize, primary_len: usize, gain_map_len: usize) -> Vec<u8> {
    let n = |v: usize| u32::try_from(v).unwrap().to_be_bytes();
    let mut out = b"MPF\0MM\0*".to_vec();
    out.extend(8u32.to_be_bytes());
    out.extend(3u16.to_be_bytes());
    out.extend([0xB0, 0x00, 0, 7, 0, 0, 0, 4]);
    out.extend(b"0100");
    out.extend([0xB0, 0x01, 0, 4, 0, 0, 0, 1]);
    out.extend(2u32.to_be_bytes());
    out.extend([0xB0, 0x02, 0, 7, 0, 0, 0, 32]);
    out.extend(50u32.to_be_bytes());
    out.extend(0u32.to_be_bytes());
    out.extend(0x2003_0000u32.to_be_bytes());
    out.extend(n(primary_len));
    out.extend([0; 8]);
    out.extend([0; 4]);
    out.extend(n(gain_map_len));
    out.extend(n(primary_len - at - 4));
    out.extend([0; 4]);
    out
}

pub fn photo(primary: &[u8], gain_map: &[u8], video: &[u8]) -> Vec<u8> {
    let gain_xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Adobe XMP Core 5.1.2"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/" xmlns:xmp="http://ns.adobe.com/xap/1.0/" hdrgm:Version="1.0" hdrgm:GainMapMin="0" hdrgm:GainMapMax="2.3" hdrgm:OffsetSDR="0.015625" hdrgm:OffsetHDR="0.015625" hdrgm:HDRCapacityMin="0" hdrgm:HDRCapacityMax="2.3" hdrgm:BaseRenditionIsHDR="False" xmp:CreatorTool="Pixel Camera 9.4"><hdrgm:Gamma><rdf:Seq><rdf:li>1</rdf:li><rdf:li>1</rdf:li><rdf:li>1</rdf:li></rdf:Seq></hdrgm:Gamma></rdf:Description></rdf:RDF></x:xmpmeta>"#;
    // ISO 21496-1 settings: versions, flags, then numbers (kept as they are).
    let iso_settings: Vec<u8> = [&[0u8, 0, 0, 0, 0x80][..], &[0x11; 40]].concat();
    let gain = with_segments(
        gain_map,
        &[
            segment(0xE1, &[XMP, gain_xmp.as_bytes()].concat()),
            segment(0xE2, &[ISO, &iso_settings].concat()),
        ],
    );
    let primary_xmp = format!(
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/" xmlns:GCamera="http://ns.google.com/photos/1.0/camera/" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:Container="http://ns.google.com/photos/1.0/container/" xmlns:Item="http://ns.google.com/photos/1.0/container/item/" hdrgm:Version="1.0" GCamera:MotionPhoto="1" GCamera:MotionPhotoVersion="1" xmp:CreateDate="2026-03-14T09:26:52" xmp:CreatorTool="Pixel Camera 9.4"><dc:creator><rdf:Seq><rdf:li>Jane Doe</rdf:li></rdf:Seq></dc:creator><Container:Directory><rdf:Seq><rdf:li rdf:parseType="Resource"><Container:Item Item:Semantic="Primary" Item:Mime="image/jpeg"/></rdf:li><rdf:li rdf:parseType="Resource"><Container:Item Item:Semantic="GainMap" Item:Mime="image/jpeg" Item:Length="{}"/></rdf:li><rdf:li rdf:parseType="Resource"><Container:Item Item:Semantic="MotionPhoto" Item:Mime="video/mp4" Item:Length="{}"/></rdf:li></rdf:Seq></Container:Directory></rdf:Description></rdf:RDF></x:xmpmeta>"#,
        gain.len(),
        video.len()
    );
    let xmp = segment(0xE1, &[XMP, primary_xmp.as_bytes()].concat());
    let at = 2 + xmp.len() + 4;
    let blank = segment(0xE2, &mpf(at, at + 4, 0));
    let mut main = with_segments(
        primary,
        &[
            xmp,
            blank,
            segment(0xE2, &[ISO, &[0, 0, 0, 0][..]].concat()),
        ],
    );
    let filled = mpf(at, main.len(), gain.len());
    main[at..at + filled.len()].copy_from_slice(&filled);
    [main, gain, video.to_vec()].concat()
}
