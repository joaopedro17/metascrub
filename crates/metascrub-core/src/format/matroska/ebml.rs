//! EBML, the binary structure of Matroska and WebM: elements made of an ID,
//! a size and data, both ID and size written as variable-length numbers.

use std::io::SeekFrom;

use crate::format::{FormatError, ReadSeek};

/// A size field with every value bit set: the element's size isn't known
/// (live recordings write it this way).
const UNKNOWN: u64 = u64::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Header {
    pub id: u32,
    /// Where the element starts (its ID).
    pub start: u64,
    /// Where its data starts.
    pub data: u64,
    /// Where it ends; `None` when its size isn't known.
    pub end: Option<u64>,
}

impl Header {
    pub fn len(&self) -> Option<u64> {
        self.end.map(|end| end - self.data)
    }
}

/// The length of a variable-length number from its first byte: one more
/// than the number of leading zero bits.
fn width(first: u8) -> Option<usize> {
    (first != 0).then(|| first.leading_zeros() as usize + 1)
}

/// An element ID (marker bits kept, as specifications write them) and its length.
pub(super) fn id(bytes: &[u8]) -> Option<(u32, usize)> {
    let width = width(*bytes.first()?)?;
    if width > 4 {
        return None;
    }
    let id = bytes
        .get(..width)?
        .iter()
        .fold(0u32, |n, &b| (n << 8) | u32::from(b));
    Some((id, width))
}

/// A size (marker bit removed; [`UNKNOWN`] when all value bits are set) and its length.
pub(super) fn size(bytes: &[u8]) -> Option<(u64, usize)> {
    let first = *bytes.first()?;
    let width = width(first)?;
    let marker = 0x80u8 >> (width - 1);
    let mut value = u64::from(first & !marker);
    let mut all_ones = value == u64::from(marker - 1);
    for &b in bytes.get(1..width)? {
        value = (value << 8) | u64::from(b);
        all_ones &= b == 0xFF;
    }
    Some((if all_ones { UNKNOWN } else { value }, width))
}

/// Reads the header of the element at `at`, which must end by `limit`.
pub(super) fn header(input: &mut dyn ReadSeek, at: u64, limit: u64) -> Result<Header, FormatError> {
    let cut = || FormatError::malformed("a Matroska element cut short");
    let mut buf = [0u8; 12];
    let available = limit.saturating_sub(at).min(buf.len() as u64) as usize;
    let buf = buf.get_mut(..available).unwrap_or_default();
    input.seek(SeekFrom::Start(at))?;
    input.read_exact(buf)?;
    let (id, id_len) =
        id(buf).ok_or_else(|| FormatError::malformed("an invalid Matroska element ID"))?;
    let (size, size_len) = size(buf.get(id_len..).unwrap_or_default()).ok_or_else(cut)?;
    let data = at + (id_len + size_len) as u64;
    let end = if size == UNKNOWN {
        None
    } else {
        Some(
            data.checked_add(size)
                .filter(|&end| end <= limit)
                .ok_or_else(cut)?,
        )
    };
    Ok(Header {
        id,
        start: at,
        data,
        end,
    })
}

/// The headers of the children of a master element with a known size.
pub(super) fn children(
    input: &mut dyn ReadSeek,
    parent: &Header,
) -> Result<Vec<Header>, FormatError> {
    let end = parent
        .end
        .ok_or_else(|| FormatError::Unsupported("a Matroska element of unknown size".into()))?;
    let mut out = Vec::new();
    let mut at = parent.data;
    while at < end {
        let child = header(input, at, end)?;
        at = child
            .end
            .ok_or_else(|| FormatError::Unsupported("a Matroska element of unknown size".into()))?;
        out.push(child);
        if out.len() > 1_000_000 {
            return Err(FormatError::Unsupported(
                "a Matroska element with too many children".into(),
            ));
        }
    }
    Ok(out)
}

/// An element's data, or `None` when it's larger than `limit`.
pub(super) fn data(
    input: &mut dyn ReadSeek,
    element: &Header,
    limit: u64,
) -> Result<Option<Vec<u8>>, FormatError> {
    let Some(len) = element.len().filter(|&len| len <= limit) else {
        return Ok(None);
    };
    input.seek(SeekFrom::Start(element.data))?;
    let mut data = vec![0u8; len as usize];
    input.read_exact(&mut data)?;
    Ok(Some(data))
}

/// A string element's text, without the zeros that may pad it.
pub(super) fn text(data: &[u8]) -> String {
    let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    String::from_utf8_lossy(data.get(..end).unwrap_or_default())
        .trim()
        .to_owned()
}

/// An unsigned integer element's value.
pub(super) fn uint(data: &[u8]) -> u64 {
    data.iter().take(8).fold(0, |n, &b| (n << 8) | u64::from(b))
}

/// A `Void` element exactly `len` bytes long (at least 2), header only;
/// the rest is zeros.
pub(super) fn void_header(len: u64) -> Vec<u8> {
    match len.checked_sub(2) {
        Some(data) if data < 127 => vec![0xEC, 0x80 | data as u8],
        _ => {
            let mut out = vec![0xEC, 0x01];
            out.extend_from_slice(&(len - 9).to_be_bytes()[1..]);
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_ids_and_sizes() {
        assert_eq!(id(&[0x1A, 0x45, 0xDF, 0xA3]), Some((0x1A45_DFA3, 4)));
        assert_eq!(id(&[0xEC]), Some((0xEC, 1)));
        assert_eq!(id(&[0x08, 0, 0, 0, 0]), None);
        assert_eq!(size(&[0x81]), Some((1, 1)));
        assert_eq!(size(&[0x40, 0x02]), Some((2, 2)));
        assert_eq!(size(&[0xFF]), Some((UNKNOWN, 1)));
        assert_eq!(
            size(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]),
            Some((UNKNOWN, 8))
        );
        assert_eq!(size(&[0x00]), None);
        assert_eq!(size(&[0x40]), None);
    }

    #[test]
    fn voids_fill_exactly() {
        for len in [2u64, 3, 128, 129, 130, 5000] {
            let header = void_header(len);
            let (_, id_len) = id(&header).unwrap();
            let (data, size_len) = size(&header[id_len..]).unwrap();
            assert_eq!(id_len as u64 + size_len as u64 + data, len, "{len}");
        }
    }
}
