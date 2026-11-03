//! The structure MP4, QuickTime, HEIC and AVIF files share: a list of boxes,
//! each a size, a four-letter type and a payload that may hold more boxes.

use std::io::{Read, SeekFrom};

use super::super::{FormatError, ReadSeek};
use crate::limits::{MAX_MEDIA_BOX_BYTES, MAX_MEDIA_BOXES};

pub(super) type FourCc = [u8; 4];

/// Boxes found at the top level of real files. One of these that runs past
/// the end means the file was cut short; anything else is data appended
/// after the last box.
const TOP_LEVEL: &[&FourCc] = &[
    b"ftyp", b"styp", b"moov", b"mdat", b"meta", b"free", b"skip", b"wide", b"udta", b"uuid",
    b"moof", b"mfra", b"sidx", b"ssix", b"emsg", b"prft", b"pdin", b"meco", b"pnot",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Header {
    pub(super) kind: FourCc,
    pub(super) start: u64,
    /// Where the type's 16-byte extension (`uuid`) or the payload starts.
    pub(super) body: u64,
    pub(super) payload: u64,
    pub(super) end: u64,
    pub(super) uuid: Option<[u8; 16]>,
}

impl Header {
    pub(super) fn len(&self) -> u64 {
        self.end - self.payload
    }

    /// The same box as seen from `base` bytes further into the file.
    pub(super) fn shifted(self, base: u64) -> Self {
        Self {
            start: self.start + base,
            body: self.body + base,
            payload: self.payload + base,
            end: self.end + base,
            ..self
        }
    }
}

/// The boxes in `from..to`, and where the first byte that isn't a box starts
/// (`to` when they fill the range). At the top level of a file a size of 0
/// means "up to the end"; inside a box it ends the list, as in QuickTime.
pub(super) fn list(
    input: &mut dyn ReadSeek,
    from: u64,
    to: u64,
    top_level: bool,
) -> Result<(Vec<Header>, u64), FormatError> {
    let mut boxes = Vec::new();
    let mut at = from;
    while to - at >= 8 {
        let Some(header) = header(input, at, to, top_level)? else {
            break;
        };
        if boxes.len() == MAX_MEDIA_BOXES {
            return Err(FormatError::Unsupported(format!(
                "more than {MAX_MEDIA_BOXES} boxes in one list"
            )));
        }
        at = header.end;
        boxes.push(header);
    }
    Ok((boxes, at))
}

fn header(
    input: &mut dyn ReadSeek,
    start: u64,
    limit: u64,
    top_level: bool,
) -> Result<Option<Header>, FormatError> {
    input.seek(SeekFrom::Start(start))?;
    let mut head = [0; 8];
    input.read_exact(&mut head)?;
    let [s0, s1, s2, s3, k0, k1, k2, k3] = head;
    let kind = [k0, k1, k2, k3];
    if top_level
        && !kind
            .iter()
            .all(|&b| b.is_ascii_graphic() || b == b' ' || b == 0xA9)
    {
        return Ok(None);
    }
    let mut body = start + 8;
    let size = match u32::from_be_bytes([s0, s1, s2, s3]) {
        0 if top_level => limit - start,
        0 => return Ok(None),
        1 => {
            let mut large = [0; 8];
            input.read_exact(&mut large)?;
            body += 8;
            u64::from_be_bytes(large)
        }
        size => u64::from(size),
    };
    let mut payload = body;
    let uuid = if kind == *b"uuid" {
        let mut uuid = [0; 16];
        input.read_exact(&mut uuid)?;
        payload += 16;
        Some(uuid)
    } else {
        None
    };
    let end = start.checked_add(size).filter(|&end| end >= payload);
    if top_level && end.is_none_or(|end| end > limit) && !TOP_LEVEL.contains(&&kind) {
        return Ok(None);
    }
    let end = end.filter(|&end| end <= limit).ok_or_else(|| {
        FormatError::malformed(format!(
            "the {} box doesn't fit in its container",
            name(kind)
        ))
    })?;
    Ok(Some(Header {
        kind,
        start,
        body,
        payload,
        end,
        uuid,
    }))
}

/// The boxes inside `parent`, skipping `skip` bytes of fields first.
pub(super) fn children(
    input: &mut dyn ReadSeek,
    parent: &Header,
    skip: u64,
) -> Result<Vec<Header>, FormatError> {
    let from = parent.payload.saturating_add(skip).min(parent.end);
    Ok(list(input, from, parent.end, false)?.0)
}

/// The boxes in `data` after `skip` bytes of fields, with their payloads.
pub(super) fn parse(data: &[u8], skip: usize) -> Result<Vec<(Header, &[u8])>, FormatError> {
    let len = data.len() as u64;
    let (boxes, _) = list(
        &mut std::io::Cursor::new(data),
        (skip as u64).min(len),
        len,
        false,
    )?;
    boxes
        .into_iter()
        .map(|header| {
            let payload = usize::try_from(header.payload)
                .ok()
                .zip(usize::try_from(header.end).ok())
                .and_then(|(start, end)| data.get(start..end))
                .ok_or_else(|| FormatError::malformed("a box outside its container"))?;
            Ok((header, payload))
        })
        .collect()
}

pub(super) fn read(input: &mut dyn ReadSeek, header: &Header) -> Result<Vec<u8>, FormatError> {
    read_range(input, header.payload, header.len())
}

pub(super) fn read_range(
    input: &mut dyn ReadSeek,
    at: u64,
    len: u64,
) -> Result<Vec<u8>, FormatError> {
    if len > MAX_MEDIA_BOX_BYTES {
        return Err(FormatError::TooLarge {
            limit: MAX_MEDIA_BOX_BYTES,
        });
    }
    input.seek(SeekFrom::Start(at))?;
    let mut data = Vec::new();
    input.take(len).read_to_end(&mut data)?;
    if data.len() as u64 != len {
        return Err(FormatError::malformed("the file ends early"));
    }
    Ok(data)
}

/// Whether `len` bytes from `at` are all zero, read in pieces.
pub(super) fn is_zero(input: &mut dyn ReadSeek, at: u64, len: u64) -> Result<bool, FormatError> {
    input.seek(SeekFrom::Start(at))?;
    let mut rest = len;
    let mut buffer = [0; 8192];
    while rest > 0 {
        let want = usize::try_from(rest.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let chunk = buffer.get_mut(..want).unwrap_or_default();
        input.read_exact(chunk)?;
        if chunk.iter().any(|&b| b != 0) {
            return Ok(false);
        }
        rest -= want as u64;
    }
    Ok(true)
}

/// `©xyz` → `©xyz`: type codes are Latin-1, with © for Apple's tags.
pub(super) fn name(kind: FourCc) -> String {
    super::super::bytes::latin1(&kind)
}

#[cfg(test)]
pub(super) mod tests {
    use std::io::Cursor;

    use super::*;

    /// A box with a 32-bit size.
    pub(crate) fn make(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let size = u32::try_from(payload.len() + 8).unwrap();
        [&size.to_be_bytes()[..], kind, payload].concat()
    }

    /// A box with version and flags before the payload.
    pub(crate) fn full(kind: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
        let mut head = flags.to_be_bytes();
        head[0] = version;
        make(kind, &[&head[..], payload].concat())
    }

    #[test]
    fn reads_sizes_of_every_kind() {
        let mut data = make(b"ftyp", b"isom\0\0\0\0");
        data.extend([0, 0, 0, 1]);
        data.extend(b"free");
        data.extend(20u64.to_be_bytes());
        data.extend([7; 4]);
        data.extend(make(b"uuid", &[1; 18]));
        data.extend([0, 0, 0, 0]);
        data.extend(b"mdat");
        data.extend(b"rest of the file");
        let len = data.len() as u64;
        let (boxes, end) = list(&mut Cursor::new(&data), 0, len, true).unwrap();
        let kinds: Vec<_> = boxes.iter().map(|b| &b.kind).collect();
        assert_eq!(kinds, [b"ftyp", b"free", b"uuid", b"mdat"]);
        assert_eq!(end, len);
        assert_eq!((boxes[1].payload, boxes[1].len()), (32, 4));
        assert_eq!(boxes[2].uuid, Some([1; 16]));
        assert_eq!(boxes[2].len(), 2);
        assert_eq!(boxes[3].len(), 16);
    }

    #[test]
    fn stops_at_bytes_that_are_not_a_box() {
        let mut data = make(b"ftyp", b"isom");
        data.extend(b"\0\0\0\x10\x01\x02\x03\x04 trailing");
        let (boxes, end) = list(&mut Cursor::new(&data), 0, data.len() as u64, true).unwrap();
        assert_eq!(boxes.len(), 1);
        assert_eq!(end, 12);
    }

    #[test]
    fn boxes_that_overflow_their_container_are_errors() {
        let mut data = make(b"moov", &make(b"udta", b"xx"));
        data[11] = 0xFF;
        let mut input = Cursor::new(&data);
        let (boxes, _) = list(&mut input, 0, data.len() as u64, true).unwrap();
        assert!(children(&mut input, &boxes[0], 0).is_err());
        let mut huge = vec![0, 0, 0, 1];
        huge.extend(b"mdat");
        huge.extend(u64::MAX.to_be_bytes());
        assert!(list(&mut Cursor::new(&huge), 0, 16, true).is_err());
        assert!(list(&mut Cursor::new(b"\0\0\0\x04free"), 0, 8, true).is_err());
    }

    #[test]
    fn a_zero_size_ends_a_list_inside_a_box() {
        let udta = make(b"udta", &[make(b"name", b"x").as_slice(), &[0; 8]].concat());
        let mut input = Cursor::new(&udta);
        let (boxes, _) = list(&mut input, 0, udta.len() as u64, true).unwrap();
        let inner = children(&mut input, &boxes[0], 0).unwrap();
        assert_eq!(inner.len(), 1);
    }
}
