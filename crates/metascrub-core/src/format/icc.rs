//! ICC color profiles.
//!
//! A profile is "standard" when its primaries match sRGB, Display P3 or
//! Adobe RGB (1998) and it carries only display-related tags. Matching on
//! colorimetry instead of the description string means the same profile
//! from different vendors is still recognized, while a calibrated monitor
//! profile (measured primaries, `vcgt` tag) is not.

use super::FormatError;
use super::bytes::Cursor;
use crate::model::{Disposition, Finding, FindingKind, KeepReason, Location, Value};

const HEADER_LEN: usize = 128;
const MAX_TAGS: u32 = 256;
const TOLERANCE: f64 = 0.003;

type Xyz = [f64; 3];

/// Red, green and blue primaries, adapted to D50 as ICC profiles store them.
const STANDARD_PRIMARIES: [[Xyz; 3]; 3] = [
    // sRGB
    [
        [0.4361, 0.2225, 0.0139],
        [0.3851, 0.7169, 0.0971],
        [0.1431, 0.0606, 0.7141],
    ],
    // Display P3
    [
        [0.5151, 0.2412, -0.0010],
        [0.2920, 0.6922, 0.0419],
        [0.1571, 0.0666, 0.7841],
    ],
    // Adobe RGB (1998)
    [
        [0.6098, 0.3111, 0.0195],
        [0.2052, 0.6257, 0.0609],
        [0.1492, 0.0632, 0.7448],
    ],
];

const DISPLAY_TAGS: &[&[u8; 4]] = &[
    b"desc", b"cprt", b"wtpt", b"bkpt", b"rXYZ", b"gXYZ", b"bXYZ", b"rTRC", b"gTRC", b"bTRC",
    b"chad", b"chrm", b"lumi", b"meas", b"tech", b"view", b"dmnd", b"dmdd",
];

pub(super) fn inspect(data: &[u8], location: Location) -> Finding {
    match Profile::parse(data) {
        Ok(profile) => {
            let disposition = if profile.is_standard() {
                Disposition::Keep(KeepReason::StandardColorProfile)
            } else {
                Disposition::Remove
            };
            let finding = Finding::new(FindingKind::ColorProfile, location, disposition);
            match profile.description {
                Some(description) => finding.with_value(Value::Text(description)),
                None => finding,
            }
        }
        Err(_) => Finding::new(FindingKind::ColorProfile, location, Disposition::Remove)
            .with_value(Value::Bytes(data.len() as u64)),
    }
}

#[derive(Debug, Default)]
struct Profile {
    is_rgb_display: bool,
    description: Option<String>,
    primaries: [Option<Xyz>; 3],
    only_display_tags: bool,
}

impl Profile {
    fn parse(data: &[u8]) -> Result<Self, FormatError> {
        let mut header = Cursor::new(data);
        header.skip(12)?;
        let class = header.array::<4>()?;
        let color_space = header.array::<4>()?;
        header.skip(16)?;
        if header.array::<4>()? != *b"acsp" {
            return Err(FormatError::malformed("not an ICC profile"));
        }

        let mut profile = Self {
            is_rgb_display: class == *b"mntr" && color_space == *b"RGB ",
            only_display_tags: true,
            ..Self::default()
        };

        let mut table = Cursor::new(data);
        table.skip(HEADER_LEN)?;
        let count = table.u32_be()?;
        if count > MAX_TAGS {
            return Err(FormatError::malformed("too many ICC tags"));
        }
        for _ in 0..count {
            let signature = table.array::<4>()?;
            let offset = table.u32_be()? as usize;
            let size = table.u32_be()? as usize;
            let tag = offset
                .checked_add(size)
                .and_then(|end| data.get(offset..end))
                .ok_or_else(|| FormatError::malformed("ICC tag outside the profile"))?;

            if !DISPLAY_TAGS.contains(&&signature) {
                profile.only_display_tags = false;
            }
            match &signature {
                b"desc" => profile.description = description(tag),
                b"rXYZ" => profile.primaries[0] = xyz(tag),
                b"gXYZ" => profile.primaries[1] = xyz(tag),
                b"bXYZ" => profile.primaries[2] = xyz(tag),
                _ => {}
            }
        }
        Ok(profile)
    }

    fn is_standard(&self) -> bool {
        let [Some(r), Some(g), Some(b)] = self.primaries else {
            return false;
        };
        self.is_rgb_display
            && self.only_display_tags
            && STANDARD_PRIMARIES
                .iter()
                .any(|standard| close(standard, &[r, g, b]))
    }
}

fn close(a: &[Xyz; 3], b: &[Xyz; 3]) -> bool {
    a.iter()
        .flatten()
        .zip(b.iter().flatten())
        .all(|(x, y)| (x - y).abs() <= TOLERANCE)
}

fn xyz(tag: &[u8]) -> Option<Xyz> {
    let mut c = Cursor::new(tag);
    if c.array::<4>().ok()? != *b"XYZ " {
        return None;
    }
    c.skip(4).ok()?;
    let mut xyz = [0.0; 3];
    for v in &mut xyz {
        *v = f64::from(i32::from_be_bytes(c.array().ok()?)) / 65536.0;
    }
    Some(xyz)
}

fn description(tag: &[u8]) -> Option<String> {
    let mut c = Cursor::new(tag);
    let kind = c.array::<4>().ok()?;
    c.skip(4).ok()?;
    let text = match &kind {
        // ICC v2: ASCII with a length that includes the trailing NUL.
        b"desc" => {
            let len = c.u32_be().ok()? as usize;
            String::from_utf8_lossy(c.take(len).ok()?).into_owned()
        }
        // ICC v4: UTF-16BE records; the first one is enough.
        b"mluc" => {
            let records = c.u32_be().ok()?;
            if records == 0 {
                return None;
            }
            c.skip(8).ok()?;
            let len = c.u32_be().ok()? as usize;
            let offset = c.u32_be().ok()? as usize;
            let bytes = tag.get(offset..offset.checked_add(len)?)?;
            let units: Vec<u16> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&pair| u16::from_be_bytes(pair))
                .collect();
            String::from_utf16_lossy(&units)
        }
        _ => return None,
    };
    let text = text.trim_end_matches('\0').trim().to_owned();
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a minimal RGB display profile with the given primaries.
    pub(crate) fn profile(
        description: &str,
        primaries: [Xyz; 3],
        extra_tag: Option<&[u8; 4]>,
    ) -> Vec<u8> {
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
        if let Some(sig) = extra_tag {
            tags.push((*sig, vec![0; 8]));
        }

        let mut header = vec![0u8; HEADER_LEN];
        header[12..16].copy_from_slice(b"mntr");
        header[16..20].copy_from_slice(b"RGB ");
        header[36..40].copy_from_slice(b"acsp");
        let mut out = header;
        out.extend((tags.len() as u32).to_be_bytes());
        let mut offset = HEADER_LEN + 4 + tags.len() * 12;
        let mut body: Vec<u8> = Vec::new();
        for (sig, data) in &tags {
            out.extend(sig);
            out.extend((offset as u32).to_be_bytes());
            out.extend((data.len() as u32).to_be_bytes());
            offset += data.len();
            body.extend(data);
        }
        out.extend(body);
        out
    }

    #[test]
    fn recognizes_standard_profiles_by_primaries() {
        for standard in STANDARD_PRIMARIES {
            let finding = inspect(&profile("anything", standard, None), Location::new(["ICC"]));
            assert_eq!(
                finding.disposition,
                Disposition::Keep(KeepReason::StandardColorProfile)
            );
            assert_eq!(finding.value, Some(Value::Text("anything".into())));
        }
    }

    #[test]
    fn removes_profiles_with_measured_primaries() {
        let mut measured = STANDARD_PRIMARIES[0];
        measured[0][0] += 0.01;
        let finding = inspect(
            &profile("Studio monitor", measured, None),
            Location::new(["ICC"]),
        );
        assert_eq!(finding.disposition, Disposition::Remove);
    }

    #[test]
    fn removes_standard_primaries_with_calibration_data() {
        let data = profile("sRGB", STANDARD_PRIMARIES[0], Some(b"vcgt"));
        assert_eq!(
            inspect(&data, Location::new(["ICC"])).disposition,
            Disposition::Remove
        );
    }

    #[test]
    fn garbage_is_reported_and_removed() {
        let finding = inspect(b"not a profile", Location::new(["ICC"]));
        assert_eq!(finding.disposition, Disposition::Remove);
        assert_eq!(finding.value, Some(Value::Bytes(13)));
    }
}
