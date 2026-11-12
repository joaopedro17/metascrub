//! Reading the TIFF header and its IFDs (classic and BigTIFF) with seeks,
//! never loading the pictures.

use std::io::SeekFrom;

use crate::format::{FormatError, ReadSeek};

/// More entries than this in one IFD isn't a real file.
const MAX_ENTRIES: u64 = 65_535;

/// `N` bytes at `at`, or zeros past the end (callers check lengths first).
fn arr<const N: usize>(b: &[u8], at: usize) -> [u8; N] {
    b.get(at..)
        .and_then(<[u8]>::first_chunk::<N>)
        .copied()
        .unwrap_or([0; N])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Layout {
    pub little: bool,
    /// BigTIFF: 8-byte offsets and counts, 20-byte entries.
    pub big: bool,
}

impl Layout {
    pub fn u16(self, b: [u8; 2]) -> u16 {
        if self.little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        }
    }

    pub fn u32(self, b: [u8; 4]) -> u32 {
        if self.little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        }
    }

    pub fn u64(self, b: [u8; 8]) -> u64 {
        if self.little {
            u64::from_le_bytes(b)
        } else {
            u64::from_be_bytes(b)
        }
    }

    /// An offset or IFD count: 4 or 8 bytes, 2 or 8 for a count.
    fn number(self, b: &[u8]) -> u64 {
        match b.len() {
            2 => self.u16(arr(b, 0)).into(),
            4 => self.u32(arr(b, 0)).into(),
            _ => self.u64(arr(b, 0)),
        }
    }

    pub fn header_len(self) -> u64 {
        if self.big { 16 } else { 8 }
    }

    pub fn offset_len(self) -> u64 {
        if self.big { 8 } else { 4 }
    }

    fn count_len(self) -> u64 {
        if self.big { 8 } else { 2 }
    }

    pub fn entry_len(self) -> u64 {
        if self.big { 20 } else { 12 }
    }

    /// The size of an IFD with `entries` entries.
    pub fn ifd_len(self, entries: u64) -> u64 {
        self.count_len() + entries * self.entry_len() + self.offset_len()
    }

    /// An IFD's entry count, as stored.
    pub fn count_bytes(self, count: u64) -> Vec<u8> {
        match (self.big, self.little) {
            (true, true) => count.to_le_bytes().to_vec(),
            (true, false) => count.to_be_bytes().to_vec(),
            (false, true) => (count as u16).to_le_bytes().to_vec(),
            (false, false) => (count as u16).to_be_bytes().to_vec(),
        }
    }

    /// An offset, as stored.
    pub fn offset_bytes(self, offset: u64) -> Vec<u8> {
        match (self.big, self.little) {
            (true, true) => offset.to_le_bytes().to_vec(),
            (true, false) => offset.to_be_bytes().to_vec(),
            (false, true) => (offset as u32).to_le_bytes().to_vec(),
            (false, false) => (offset as u32).to_be_bytes().to_vec(),
        }
    }
}

/// Reads the header: the byte order, classic or BigTIFF, and where the first IFD is.
pub(super) fn header(input: &mut dyn ReadSeek) -> Result<(Layout, u64), FormatError> {
    input.seek(SeekFrom::Start(0))?;
    let mut head = [0u8; 16];
    let read = read_up_to(input, &mut head)?;
    let little = match head.get(..2) {
        Some(b"II") => true,
        Some(b"MM") => false,
        _ => return Err(FormatError::malformed("missing the TIFF byte order")),
    };
    let mut layout = Layout { little, big: false };
    match layout.u16([head[2], head[3]]) {
        42 if read >= 8 => Ok((
            layout,
            layout.u32([head[4], head[5], head[6], head[7]]).into(),
        )),
        43 if read >= 16 => {
            layout.big = true;
            if layout.u16([head[4], head[5]]) != 8 || layout.u16([head[6], head[7]]) != 0 {
                return Err(FormatError::Unsupported(
                    "a BigTIFF with offsets that aren't 8 bytes".into(),
                ));
            }
            Ok((layout, layout.number(&head[8..16])))
        }
        42 | 43 => Err(FormatError::malformed("TIFF header cut short")),
        _ => Err(FormatError::malformed("not a TIFF file")),
    }
}

fn read_up_to(input: &mut dyn ReadSeek, buf: &mut [u8]) -> Result<usize, FormatError> {
    let mut read = 0;
    while read < buf.len() {
        match input.read(buf.get_mut(read..).unwrap_or_default())? {
            0 => break,
            n => read += n,
        }
    }
    Ok(read)
}

/// Where an entry's value is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Place {
    /// Inside the entry: the value bytes, `len` long.
    Inline(Vec<u8>),
    At(u64),
}

#[derive(Debug, Clone)]
pub(super) struct Entry {
    pub tag: u16,
    pub kind: u16,
    pub count: u64,
    /// The entry exactly as stored, to write back unchanged.
    pub raw: Vec<u8>,
    /// `None` when the type is unknown, so the value's size is too.
    pub len: Option<u64>,
    pub place: Place,
}

impl Entry {
    /// Where the value lies in the file, when it's outside the entry.
    pub fn range(&self) -> Option<std::ops::Range<u64>> {
        match (&self.place, self.len) {
            (Place::At(at), Some(len)) => Some(*at..at.saturating_add(len)),
            _ => None,
        }
    }
}

/// The size of one value of a TIFF type.
fn type_size(kind: u16) -> Option<u64> {
    Some(match kind {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 | 16 | 17 | 18 => 8,
        _ => return None,
    })
}

#[derive(Debug, Clone)]
pub(super) struct Ifd {
    pub entries: Vec<Entry>,
    pub next: u64,
}

pub(super) fn read_ifd(
    input: &mut dyn ReadSeek,
    layout: Layout,
    at: u64,
    file_len: u64,
) -> Result<Ifd, FormatError> {
    let cut = || FormatError::malformed("a TIFF directory outside the file");
    let count_len = layout.count_len();
    if at.checked_add(count_len).is_none_or(|end| end > file_len) {
        return Err(cut());
    }
    input.seek(SeekFrom::Start(at))?;
    let mut count = vec![0u8; count_len as usize];
    input.read_exact(&mut count)?;
    let count = layout.number(&count);
    if count > MAX_ENTRIES {
        return Err(FormatError::malformed(
            "a TIFF directory with too many entries",
        ));
    }
    if at + layout.ifd_len(count) > file_len {
        return Err(cut());
    }
    let mut data = vec![0u8; (count * layout.entry_len() + layout.offset_len()) as usize];
    input.read_exact(&mut data)?;
    let (table, next) = data.split_at(data.len() - layout.offset_len() as usize);
    let entries = table
        .chunks_exact(layout.entry_len() as usize)
        .map(|raw| entry(layout, raw))
        .collect();
    Ok(Ifd {
        entries,
        next: layout.number(next),
    })
}

fn entry(layout: Layout, raw: &[u8]) -> Entry {
    let tag = layout.u16(arr(raw, 0));
    let kind = layout.u16(arr(raw, 2));
    let (count, value) = if layout.big {
        (layout.u64(arr(raw, 4)), raw.get(12..20))
    } else {
        (layout.u32(arr(raw, 4)).into(), raw.get(8..12))
    };
    let value = value.unwrap_or_default();
    let len = type_size(kind).and_then(|size| size.checked_mul(count));
    let place = match len.and_then(|len| value.get(..usize::try_from(len).ok()?)) {
        Some(inline) => Place::Inline(inline.to_vec()),
        None => Place::At(layout.number(value)),
    };
    Entry {
        tag,
        kind,
        count,
        raw: raw.to_vec(),
        len,
        place,
    }
}

/// The value's bytes, or `None` when it's larger than `limit` or its type
/// unknown. A value outside the file is an error.
pub(super) fn bytes(
    input: &mut dyn ReadSeek,
    entry: &Entry,
    file_len: u64,
    limit: u64,
) -> Result<Option<Vec<u8>>, FormatError> {
    match (&entry.place, entry.len) {
        (Place::Inline(bytes), _) => Ok(Some(bytes.clone())),
        (_, None) => Ok(None),
        (Place::At(_), Some(len)) if len > limit => Ok(None),
        (Place::At(at), Some(len)) => {
            if at.checked_add(len).is_none_or(|end| end > file_len) {
                return Err(FormatError::malformed("a TIFF value outside the file"));
            }
            input.seek(SeekFrom::Start(*at))?;
            let mut data = vec![0u8; len as usize];
            input.read_exact(&mut data)?;
            Ok(Some(data))
        }
    }
}

/// The value as whole numbers: offsets, byte counts, sizes.
pub(super) fn numbers(
    input: &mut dyn ReadSeek,
    layout: Layout,
    entry: &Entry,
    file_len: u64,
    limit: u64,
) -> Result<Vec<u64>, FormatError> {
    let size = match entry.kind {
        3 => 2,
        4 | 13 => 4,
        16 | 18 => 8,
        _ => return Err(FormatError::malformed("a TIFF offset of the wrong type")),
    };
    let data = bytes(input, entry, file_len, limit)?
        .ok_or_else(|| FormatError::Unsupported("a TIFF with too many strips or tiles".into()))?;
    Ok(match size {
        2 => data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| layout.u16(*b).into())
            .collect(),
        4 => data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| layout.u32(*b).into())
            .collect(),
        _ => data
            .as_chunks::<8>()
            .0
            .iter()
            .map(|b| layout.u64(*b))
            .collect(),
    })
}

/// The value as the `exif` crate would decode it, to report it the same way
/// as EXIF in other formats.
pub(super) fn exif_value(layout: Layout, entry: &Entry, data: &[u8]) -> exif::Value {
    use exif::Value as V;
    let n2 = |b: &[u8; 2]| layout.u16(*b);
    let n4 = |b: &[u8; 4]| layout.u32(*b);
    let halves = |b: &[u8; 8]| (n4(&arr(b, 0)), n4(&arr(b, 4)));
    let pairs = data.as_chunks::<2>().0;
    let quads = data.as_chunks::<4>().0;
    let eights = data.as_chunks::<8>().0;
    match entry.kind {
        1 => V::Byte(data.to_vec()),
        2 => {
            let mut strings: Vec<Vec<u8>> = data.split(|&b| b == 0).map(<[u8]>::to_vec).collect();
            if strings.last().is_some_and(Vec::is_empty) {
                strings.pop();
            }
            V::Ascii(strings)
        }
        3 => V::Short(pairs.iter().map(n2).collect()),
        4 | 13 => V::Long(quads.iter().map(n4).collect()),
        5 => V::Rational(
            eights
                .iter()
                .map(|b| {
                    let (num, denom) = halves(b);
                    exif::Rational { num, denom }
                })
                .collect(),
        ),
        6 => V::SByte(data.iter().map(|&b| b as i8).collect()),
        7 => V::Undefined(data.to_vec(), 0),
        8 => V::SShort(pairs.iter().map(|b| n2(b) as i16).collect()),
        9 => V::SLong(quads.iter().map(|b| n4(b) as i32).collect()),
        10 => V::SRational(
            eights
                .iter()
                .map(|b| {
                    let (num, denom) = halves(b);
                    exif::SRational {
                        num: num as i32,
                        denom: denom as i32,
                    }
                })
                .collect(),
        ),
        11 => V::Float(quads.iter().map(|b| f32::from_bits(n4(b))).collect()),
        12 => V::Double(
            eights
                .iter()
                .map(|b| f64::from_bits(layout.u64(*b)))
                .collect(),
        ),
        kind => V::Unknown(kind, entry.count.min(u32::MAX.into()) as u32, 0),
    }
}
