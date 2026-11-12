//! BMP: a file header, a picture header, a color table, then the pixels.
//! The only metadata the format has is a color profile in the newest
//! header (V5): embedded (follows the color-profile rule) or linked, which
//! is a file path on the computer that made it. Bytes the format doesn't
//! use (reserved fields, gaps before the pixels, data after them) can hold
//! anything, so they're zeroed or dropped. The file keeps its layout.

use std::io::Write;

use super::{FormatError, FormatHandler, ReadSeek, apply_options, icc, is_kept, read_image};
use crate::model::{Disposition, Finding, FindingKind, Location, Value};
use crate::options::Options;

const FILE_HEADER_LEN: usize = 14;
/// Picture header sizes: OS/2 1.x, Windows 3, V2, V3, OS/2 2.x, V4, V5.
const HEADER_LENS: &[u32] = &[12, 40, 52, 56, 64, 108, 124];
const V5_LEN: usize = 124;
const PROFILE_EMBEDDED: &[u8; 4] = b"DEBM";
const PROFILE_LINKED: &[u8; 4] = b"KNIL";
/// `LCS_sRGB`, as the little-endian bytes of `'sRGB'`.
const SRGB: &[u8; 4] = b"BGRs";

#[derive(Debug)]
pub(super) struct Bmp;

impl FormatHandler for Bmp {
    fn inspect(
        &self,
        input: &mut dyn ReadSeek,
        options: &Options,
    ) -> Result<Vec<Finding>, FormatError> {
        let data = read_image(input)?;
        Ok(analyze(&data, options)?.findings)
    }

    fn clean(
        &self,
        input: &mut dyn ReadSeek,
        output: &mut dyn Write,
        options: &Options,
    ) -> Result<Vec<Finding>, FormatError> {
        let data = read_image(input)?;
        let analysis = analyze(&data, options)?;
        output.write_all(&analysis.output)?;
        Ok(analysis.findings)
    }
}

/// Whether `head` (the first 18 bytes) starts a BMP file: `BM` alone is
/// too common at the start of other files.
pub(super) fn is_bmp(head: &[u8]) -> bool {
    head.starts_with(b"BM")
        && u32_at(head, FILE_HEADER_LEN).is_some_and(|n| HEADER_LENS.contains(&n))
}

#[derive(Debug)]
struct Analysis {
    findings: Vec<Finding>,
    output: Vec<u8>,
}

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

fn malformed(detail: &str) -> FormatError {
    FormatError::malformed(detail)
}

fn field(data: &[u8], at: usize) -> Result<usize, FormatError> {
    u32_at(data, at)
        .map(|n| n as usize)
        .ok_or_else(|| malformed("BMP cut short"))
}

fn analyze(data: &[u8], options: &Options) -> Result<Analysis, FormatError> {
    if !data.starts_with(b"BM") {
        return Err(malformed("missing the BMP signature"));
    }
    let header_len = field(data, FILE_HEADER_LEN)?;
    if !HEADER_LENS.contains(&(header_len as u32)) {
        return Err(FormatError::Unsupported(format!(
            "a BMP picture header of {header_len} bytes"
        )));
    }
    let pixels_at = field(data, 10)?;
    let header = FILE_HEADER_LEN;
    let (width, height, bits, compression, image_size, colors) = if header_len == 12 {
        let n = |at| u16_at(data, header + at).map(i64::from);
        (n(4), n(6), n(10), Some(0), Some(0), Some(0))
    } else {
        let n = |at| u32_at(data, header + at);
        (
            n(4).map(|w| i64::from(w as i32)),
            n(8).map(|h| i64::from(h as i32)),
            u16_at(data, header + 14).map(i64::from),
            n(16),
            n(20),
            n(32),
        )
    };
    let (Some(width), Some(height), Some(bits), Some(compression), Some(image_size), Some(colors)) =
        (width, height, bits, compression, image_size, colors)
    else {
        return Err(malformed("BMP cut short"));
    };

    // Where the headers, masks and color table end.
    let masks = match (header_len, compression) {
        (40, 3) => 12,
        (40, 6) => 16,
        _ => 0,
    };
    let entry = if header_len == 12 { 3 } else { 4 };
    let palette = match (colors, bits) {
        (0, 1 | 2 | 4 | 8) => 1usize << bits,
        (0, _) => 0,
        (n, _) => n as usize,
    };
    // Sizes come from the file, so any of them can be absurd.
    let too_large = || malformed("BMP sizes larger than the file");
    let tables_end = palette
        .checked_mul(entry)
        .and_then(|p| p.checked_add(header + header_len + masks))
        .ok_or_else(too_large)?;
    // OS/2 2.x numbers its compressions differently: 3 is Huffman and 4 is
    // RLE24, where Windows uses 3 for bit masks and 4 and 5 for a whole JPEG
    // or PNG, whose own metadata we can't reach.
    let os2 = header_len == 64;
    if !os2 && matches!(compression, 4 | 5) {
        return Err(FormatError::Unsupported(
            "a BMP holding a JPEG or PNG picture".into(),
        ));
    }
    let pixel_len = match compression {
        // Uncompressed rows, each padded to 4 bytes.
        0 | 3 | 6 if !(os2 && compression == 3) => width
            .unsigned_abs()
            .checked_mul(bits.unsigned_abs())
            .map(|row_bits| row_bits.div_ceil(32) * 4)
            .and_then(|row| row.checked_mul(height.unsigned_abs()))
            .and_then(|len| usize::try_from(len).ok())
            .ok_or_else(too_large)?,
        _ if image_size > 0 => image_size as usize,
        _ => {
            return Err(FormatError::Unsupported(
                "a compressed BMP without its size".into(),
            ));
        }
    };
    let pixels_end = pixels_at
        .checked_add(pixel_len)
        .filter(|&end| end <= data.len())
        .ok_or_else(|| malformed("BMP pixels cut short"))?;
    if tables_end > pixels_at {
        return Err(malformed("BMP pixels overlap its headers"));
    }

    let mut out = data.to_vec();
    let mut findings = Vec::new();
    let unknown = |location: &str, len: usize| {
        Finding::new(
            FindingKind::UnknownMetadata,
            Location::new(["BMP", location]),
            Disposition::Remove,
        )
        .with_value(Value::Bytes(len as u64))
    };
    let zero = |out: &mut Vec<u8>, range: std::ops::Range<usize>| {
        if let Some(bytes) = out.get_mut(range) {
            bytes.fill(0);
        }
    };

    if data.get(6..10).is_some_and(|r| r.iter().any(|&b| b != 0)) {
        findings.push(unknown("Reserved", 4));
        zero(&mut out, 6..10);
    }

    // The color profile, in V5 headers only.
    let mut profile: Option<std::ops::Range<usize>> = None;
    let mut keep_profile = false;
    if header_len == V5_LEN {
        let kind = data.get(header + 56..header + 60).unwrap_or_default();
        let offset = field(data, header + 112)?;
        let len = field(data, header + 116)?;
        let range = (header + offset)..(header + offset).saturating_add(len);
        let valid = range.end <= data.len() && range.start >= tables_end && len > 0;
        if kind == PROFILE_EMBEDDED && valid {
            let location = Location::new(["BMP", "Color profile"]);
            let mut icc = [icc::inspect(
                data.get(range.clone()).unwrap_or_default(),
                location.clone(),
            )];
            apply_options(&mut icc, options);
            keep_profile = is_kept(&icc, FindingKind::ColorProfile, &location);
            findings.extend(icc);
            profile = Some(range);
        } else if kind == PROFILE_LINKED && valid {
            let path = data.get(range.clone()).unwrap_or_default();
            let path = String::from_utf8_lossy(path)
                .trim_end_matches('\0')
                .to_owned();
            findings.push(
                Finding::new(
                    FindingKind::FilePath,
                    Location::new(["BMP", "Linked color profile"]),
                    Disposition::Remove,
                )
                .with_value(Value::Text(path)),
            );
            profile = Some(range);
        } else if kind == PROFILE_EMBEDDED || kind == PROFILE_LINKED {
            return Err(malformed("a BMP color profile outside the file"));
        }
        if let (Some(range), false) = (&profile, keep_profile) {
            zero(&mut out, range.clone());
            if let Some(fields) = out.get_mut(header + 56..header + 60) {
                fields.copy_from_slice(SRGB);
            }
            zero(&mut out, header + 112..header + 120);
        }
    }
    let kept = |at: usize| keep_profile && profile.as_ref().is_some_and(|r| r.contains(&at));

    // Bytes between the color table and the pixels, other than a kept profile.
    let gap = tables_end..pixels_at;
    let hidden = gap
        .clone()
        .filter(|&at| !kept(at) && out.get(at) != Some(&0))
        .count();
    if hidden > 0 {
        findings.push(unknown("Before the pixels", gap.len()));
        for at in gap.filter(|&at| !kept(at)) {
            if let Some(b) = out.get_mut(at) {
                *b = 0;
            }
        }
    }

    // After the pixels, only a kept profile stays.
    let end = match &profile {
        Some(range) if keep_profile => pixels_end.max(range.end),
        _ => pixels_end,
    };
    // The profile was reported on its own.
    let in_profile = |at: usize| profile.as_ref().is_some_and(|r| r.contains(&at));
    let after = (end..data.len()).filter(|&at| !in_profile(at));
    if after.clone().any(|at| data.get(at) != Some(&0)) {
        findings.push(
            Finding::new(
                FindingKind::TrailingData,
                Location::new(["After end of image"]),
                Disposition::Remove,
            )
            .with_value(Value::Bytes((data.len() - end) as u64)),
        );
    }
    out.truncate(end);
    let size = u32::try_from(out.len()).map_err(|_| malformed("a BMP over 4 GB"))?;
    if let Some(field) = out.get_mut(2..6) {
        field.copy_from_slice(&size.to_le_bytes());
    }
    Ok(Analysis {
        findings,
        output: out,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Cursor;

    use super::*;

    /// A 2×2, 24-bit picture with a V5 header and `extra` after the pixels;
    /// `profile` says what the header points to there.
    pub(crate) fn picture(profile: Option<(&[u8; 4], usize)>, extra: &[u8]) -> Vec<u8> {
        let pixels = [
            [0u8, 0, 255, 255, 255, 255, 0, 0],
            [255, 0, 0, 0, 255, 0, 0, 0],
        ]
        .concat();
        let pixels_at = FILE_HEADER_LEN + V5_LEN;
        let mut header = vec![0u8; V5_LEN];
        header[..4].copy_from_slice(&(V5_LEN as u32).to_le_bytes());
        header[4..8].copy_from_slice(&2u32.to_le_bytes());
        header[8..12].copy_from_slice(&2u32.to_le_bytes());
        header[12..14].copy_from_slice(&1u16.to_le_bytes());
        header[14..16].copy_from_slice(&24u16.to_le_bytes());
        header[56..60].copy_from_slice(SRGB);
        if let Some((kind, len)) = profile {
            header[56..60].copy_from_slice(kind);
            let offset = (V5_LEN + pixels.len()) as u32;
            header[112..116].copy_from_slice(&offset.to_le_bytes());
            header[116..120].copy_from_slice(&(len as u32).to_le_bytes());
        }
        let size = (pixels_at + pixels.len() + extra.len()) as u32;
        let mut file = b"BM".to_vec();
        file.extend_from_slice(&size.to_le_bytes());
        file.extend_from_slice(&[0; 4]);
        file.extend_from_slice(&(pixels_at as u32).to_le_bytes());
        [file, header, pixels, extra.to_vec()].concat()
    }

    fn clean(data: &[u8]) -> (Vec<u8>, Vec<Finding>) {
        let mut out = Vec::new();
        let findings = Bmp
            .clean(&mut Cursor::new(data), &mut out, &Options::default())
            .unwrap();
        (out, findings)
    }

    #[test]
    fn a_linked_profile_and_appended_data_go() {
        let path = b"C:\\Users\\jane\\Profiles\\EIZO-calibrated.icc\0";
        let data = picture(
            Some((PROFILE_LINKED, path.len())),
            &[path.as_slice(), b"more"].concat(),
        );
        let (out, findings) = clean(&data);
        assert_eq!(out, picture(None, b""));
        let kinds: Vec<_> = findings.iter().map(|f| f.kind).collect();
        assert_eq!(kinds, [FindingKind::FilePath, FindingKind::TrailingData]);
        let only_path = picture(Some((PROFILE_LINKED, path.len())), path);
        assert_eq!(clean(&only_path).1.len(), 1);
        assert_eq!(clean(&out), (out.clone(), Vec::new()));
    }

    #[test]
    fn an_unknown_embedded_profile_goes() {
        let profile = [7u8; 200];
        let data = picture(Some((PROFILE_EMBEDDED, profile.len())), &profile);
        let (out, findings) = clean(&data);
        assert_eq!(out, picture(None, b""));
        assert_eq!(findings[0].kind, FindingKind::ColorProfile);
    }

    #[test]
    fn fuzz_huge_dimensions_overflow() {
        let mut data = picture(None, b"");
        data[18..22].copy_from_slice(&0x7FFF_FFFFu32.to_le_bytes());
        data[22..26].copy_from_slice(&0x8000_0000u32.to_le_bytes());
        data[28..30].copy_from_slice(&32u16.to_le_bytes());
        assert!(
            Bmp.clean(
                &mut Cursor::new(&data),
                &mut Vec::new(),
                &Options::default()
            )
            .is_err()
        );
        let mut colors = picture(None, b"");
        colors[46..50].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(
            Bmp.clean(
                &mut Cursor::new(&colors),
                &mut Vec::new(),
                &Options::default()
            )
            .is_err()
        );
    }

    #[test]
    fn os2_huffman_pixels_are_kept_whole() {
        // 3 means Huffman in an OS/2 2.x header, not bit masks: the pixels
        // run as long as the header says, past an uncompressed row's size.
        let pixels = [0x5Au8; 40];
        let mut header = vec![0u8; 64];
        header[..4].copy_from_slice(&64u32.to_le_bytes());
        header[4..8].copy_from_slice(&2u32.to_le_bytes());
        header[8..12].copy_from_slice(&2u32.to_le_bytes());
        header[12..14].copy_from_slice(&1u16.to_le_bytes());
        header[14..16].copy_from_slice(&1u16.to_le_bytes());
        header[16..20].copy_from_slice(&3u32.to_le_bytes());
        header[20..24].copy_from_slice(&(pixels.len() as u32).to_le_bytes());
        let pixels_at = FILE_HEADER_LEN + 64 + 8;
        let mut data = b"BM".to_vec();
        data.extend_from_slice(&((pixels_at + pixels.len()) as u32).to_le_bytes());
        data.extend_from_slice(&[0; 4]);
        data.extend_from_slice(&(pixels_at as u32).to_le_bytes());
        let data = [data, header, vec![0; 8], pixels.to_vec()].concat();
        assert_eq!(clean(&data), (data.clone(), Vec::new()));
    }

    #[test]
    fn hidden_bytes_are_zeroed_and_bad_files_refused() {
        let mut data = picture(None, b"");
        data[6] = b'x';
        let (out, findings) = clean(&data);
        assert_eq!(out, picture(None, b""));
        assert_eq!(findings.len(), 1);
        let mut cut = picture(None, b"");
        cut.truncate(cut.len() - 2);
        assert!(
            Bmp.clean(&mut Cursor::new(&cut), &mut Vec::new(), &Options::default())
                .is_err()
        );
        for compression in [4u32, 5] {
            let mut embedded = picture(None, b"");
            embedded[30..34].copy_from_slice(&compression.to_le_bytes());
            assert!(matches!(
                Bmp.inspect(&mut Cursor::new(&embedded), &Options::default()),
                Err(FormatError::Unsupported(_))
            ));
        }
        assert!(is_bmp(&picture(None, b"")));
        assert!(!is_bmp(b"BMW owners club meeting notes"));
    }
}
