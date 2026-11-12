//! Ultra HDR and ISO 21496-1 photos: a JPEG followed by a second JPEG, the
//! gain map, that brightens the picture on HDR screens. The Multi-Picture
//! (MPF) segment of the first says where the second is; Ultra HDR also lists
//! it in the XMP. Both are rewritten for the cleaned file.

use std::ops::Range;

use super::jpeg::{self, Parsed};
use super::xmp::{self, Setting};

pub(super) const MPF: &[u8] = b"MPF\0";
pub(super) const ISO: &[u8] = b"urn:iso:std:iso:ts:21496:-1\0";
pub(super) const XMP: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
const APP1: u8 = 0xE1;
const APP2: u8 = 0xE2;

/// The MPF segment MetaScrub writes, `MPF\0` included.
pub(super) const MPF_LEN: usize = 4 + 8 + 2 + 3 * 12 + 4 + 2 * 16;

#[derive(Debug)]
pub(super) struct GainMap<'a> {
    /// Where the gain map is in the file.
    pub range: Range<usize>,
    pub parsed: Parsed<'a>,
    /// The gain map's own settings, written back in its XMP.
    pub settings: Vec<Setting>,
    /// The main image's settings (the version), when it has Ultra HDR XMP.
    pub primary_settings: Option<Vec<Setting>>,
}

/// The settings in the first standard XMP segment, if any are gain map ones.
fn settings(parsed: &Parsed<'_>) -> Option<Vec<Setting>> {
    let segment = parsed
        .segments
        .iter()
        .find(|s| s.marker == APP1 && s.data.starts_with(XMP))?;
    let settings = xmp::gain_map_settings(segment.data.get(XMP.len()..)?)?;
    (!settings.is_empty()).then_some(settings)
}

/// Whether the image has an ISO 21496-1 segment; for a gain map, one that
/// holds the settings, not only the version.
fn has_iso(parsed: &Parsed<'_>, with_settings: bool) -> bool {
    parsed.segments.iter().any(|s| {
        s.marker == APP2
            && s.data.starts_with(ISO)
            && (!with_settings || s.data.len() > ISO.len() + 4)
    })
}

/// The images an MPF segment lists after the first: where each starts in
/// the file and its size.
fn entries(file_offset: usize, data: &[u8]) -> Option<Vec<(usize, usize)>> {
    let tiff = data.get(MPF.len()..)?;
    let little = match tiff.get(..4)? {
        b"II*\0" => true,
        b"MM\0*" => false,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let b = *tiff.get(at..)?.first_chunk::<2>()?;
        Some(if little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    };
    let u32_at = |at: usize| -> Option<usize> {
        let b = *tiff.get(at..)?.first_chunk::<4>()?;
        let n = if little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        };
        usize::try_from(n).ok()
    };
    let ifd = u32_at(4)?;
    let count = usize::from(u16_at(ifd)?);
    let entry = (0..count)
        .map(|i| ifd + 2 + i * 12)
        .find(|&at| u16_at(at) == Some(0xB002))?;
    let len = u32_at(entry + 4)?;
    let table = tiff.get(u32_at(entry + 8)?..)?.get(..len)?;
    // Offsets count from the TIFF header; the first image's is zero.
    let base = file_offset + MPF.len();
    table
        .as_chunks::<16>()
        .0
        .iter()
        .skip(1)
        .map(|e| {
            let field = |at: usize| -> Option<usize> {
                let b = *e.get(at..)?.first_chunk::<4>()?;
                let n = if little {
                    u32::from_le_bytes(b)
                } else {
                    u32::from_be_bytes(b)
                };
                usize::try_from(n).ok()
            };
            Some((base.checked_add(field(8)?)?, field(4)?))
        })
        .collect()
}

/// Finds the gain map of an Ultra HDR or ISO 21496-1 photo. `None` for any
/// other JPEG, including one whose MPF or gain map can't be read: those
/// are cleaned like other multi-picture files, without the gain map.
pub(super) fn find<'a>(file: &'a [u8], primary: &Parsed<'a>) -> Option<GainMap<'a>> {
    let primary_settings = settings(primary);
    if primary_settings.is_none() && !has_iso(primary, false) {
        return None;
    }
    let primary_end = file.len() - primary.trailing.len();
    let mpf = primary
        .segments
        .iter()
        .find(|s| s.marker == APP2 && s.data.starts_with(MPF))?;
    for (start, len) in entries(mpf.at, mpf.data)? {
        let end = start.checked_add(len)?;
        if start < primary_end || end > file.len() {
            continue;
        }
        let Ok(parsed) = jpeg::parse(file.get(start..end)?) else {
            continue;
        };
        let settings = settings(&parsed).unwrap_or_default();
        let is_gain_map =
            settings.iter().any(|(name, _)| name == "GainMapMax") || has_iso(&parsed, true);
        if is_gain_map {
            return Some(GainMap {
                range: start..end,
                parsed,
                settings,
                primary_settings,
            });
        }
    }
    None
}

/// An MPF segment (`MPF\0` included) listing the main image of
/// `primary_len` bytes and the gain map of `gain_map_len` bytes, which
/// starts right after it. `at` is where the segment's `MPF\0` is in the file.
pub(super) fn mpf(at: usize, primary_len: usize, gain_map_len: usize) -> Vec<u8> {
    let n = |v: usize| (v as u32).to_be_bytes();
    let mut out = MPF.to_vec();
    out.extend(b"MM\0*");
    out.extend(8u32.to_be_bytes());
    out.extend(3u16.to_be_bytes());
    // MPFVersion, NumberOfImages, MPEntry (after the IFD).
    out.extend([0xB0, 0x00, 0, 7, 0, 0, 0, 4]);
    out.extend(b"0100");
    out.extend([0xB0, 0x01, 0, 4, 0, 0, 0, 1]);
    out.extend(2u32.to_be_bytes());
    out.extend([0xB0, 0x02, 0, 7, 0, 0, 0, 32]);
    out.extend(50u32.to_be_bytes());
    out.extend(0u32.to_be_bytes());
    // The main image: representative, baseline JPEG, at offset zero.
    out.extend(0x2003_0000u32.to_be_bytes());
    out.extend(n(primary_len));
    out.extend([0; 8]);
    // The gain map.
    out.extend([0; 4]);
    out.extend(n(gain_map_len));
    out.extend(n(primary_len.saturating_sub(at + MPF.len())));
    out.extend([0; 4]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_mpf_reads_back() {
        let segment = mpf(20, 1000, 300);
        assert_eq!(segment.len(), MPF_LEN);
        assert_eq!(entries(20, &segment), Some(vec![(1000, 300)]));
    }

    #[test]
    fn broken_mpf_is_ignored() {
        let mut segment = mpf(20, 1000, 300);
        for len in 0..segment.len() - 32 {
            assert_eq!(entries(20, &segment[..len]), None, "{len}");
        }
        segment[5] = b'X';
        assert_eq!(entries(20, &segment), None);
    }
}
