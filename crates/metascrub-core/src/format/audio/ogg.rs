//! Ogg Vorbis and Opus: packets of sound carried in pages, each page with
//! its own header and CRC-32. The first packets are the codec's headers;
//! the second holds the tags and the encoder's name. Cleaning replaces it
//! with an empty one, packs the headers into new pages, and copies every
//! page of sound with only its sequence number and checksum updated, so
//! the sound is kept byte for byte. Every page is checked first: several
//! streams in one file, missing pages or other data between pages get the
//! file refused, since that data could hold anything.

use std::io::{BufReader, Read, SeekFrom, Write};
use std::sync::LazyLock;

use super::{Tag, trailing_tags, vorbis};
use crate::format::bytes::Cursor;
use crate::format::{FormatError, FormatHandler, ReadSeek};
use crate::limits::MAX_AUDIO_TAG_BYTES;
use crate::model::{Disposition, Finding, FindingKind, KeepReason, Location, Value};
use crate::options::Options;

const CAPTURE: &[u8] = b"OggS";
const HEADER_LEN: usize = 27;
/// The page goes on with a packet the previous page started.
const CONTINUED: u8 = 0x01;
/// The first page of a stream.
const BEGINS: u8 = 0x02;
/// The last page of a stream.
const ENDS: u8 = 0x04;
/// A lacing value below this ends a packet.
const SEGMENT: u8 = 255;
/// Header pages that complete no packet have no position.
const NO_GRANULE: [u8; 8] = [0xFF; 8];

#[derive(Debug)]
pub(crate) struct Ogg;

impl FormatHandler for Ogg {
    fn inspect(
        &self,
        input: &mut dyn ReadSeek,
        _options: &Options,
    ) -> Result<Vec<Finding>, FormatError> {
        Ok(scan(input)?.findings)
    }

    fn clean(
        &self,
        input: &mut dyn ReadSeek,
        output: &mut dyn Write,
        _options: &Options,
    ) -> Result<Vec<Finding>, FormatError> {
        let layout = scan(input)?;
        output.write_all(&layout.first.to_bytes())?;
        let mut packets = vec![layout.codec.empty_comment()];
        if let Some(setup) = layout.setup {
            packets.push(setup);
        }
        let headers = pages(&packets, &layout.first);
        // Pages of sound follow the new header pages without a gap.
        let shift = i64::try_from(headers.len() + 1).unwrap_or(i64::MAX)
            - i64::try_from(layout.header_pages).unwrap_or(i64::MAX);
        for page in &headers {
            output.write_all(&page.to_bytes())?;
        }
        input.seek(SeekFrom::Start(layout.sound_start))?;
        let mut reader = BufReader::with_capacity(1 << 16, &mut *input);
        let mut pos = layout.sound_start;
        while pos < layout.sound_end {
            let mut page = Page::read(&mut reader, layout.sound_end - pos)?;
            pos += page.len();
            page.sequence = u32::try_from(i64::from(page.sequence) + shift)
                .map_err(|_| FormatError::malformed("too many Ogg pages"))?;
            output.write_all(&page.to_bytes())?;
        }
        Ok(layout.findings)
    }
}

/// Whether the Ogg stream at the start of `input` is Vorbis or Opus, the
/// two MetaScrub cleans; not Ogg FLAC, Speex or video. A damaged first
/// page still counts, so the file is reported as damaged.
pub(crate) fn is_supported(input: &mut dyn ReadSeek) -> Result<bool, FormatError> {
    let len = input.seek(SeekFrom::End(0))?;
    input.seek(SeekFrom::Start(0))?;
    match Page::read(&mut BufReader::new(&mut *input), len) {
        Ok(page) => Ok(Codec::of(page.first_packet()).is_some()),
        Err(FormatError::Io(error)) => Err(FormatError::Io(error)),
        Err(_) => Ok(true),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Codec {
    Vorbis,
    Opus,
}

impl Codec {
    fn of(first_packet: &[u8]) -> Option<Self> {
        if first_packet.starts_with(b"\x01vorbis") {
            Some(Self::Vorbis)
        } else if first_packet.starts_with(b"OpusHead") {
            Some(Self::Opus)
        } else {
            None
        }
    }

    /// Identification, comment and (Vorbis only) setup.
    const fn headers(self) -> usize {
        match self {
            Self::Vorbis => 3,
            Self::Opus => 2,
        }
    }

    const fn comment_magic(self) -> &'static [u8] {
        match self {
            Self::Vorbis => b"\x03vorbis",
            Self::Opus => b"OpusTags",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Vorbis => "Vorbis comment",
            Self::Opus => "OpusTags",
        }
    }

    /// No encoder name and no tags. Vorbis ends its headers with a
    /// framing bit.
    fn empty_comment(self) -> Vec<u8> {
        let mut packet = self.comment_magic().to_vec();
        packet.extend_from_slice(&[0; 8]);
        if self == Self::Vorbis {
            packet.push(1);
        }
        packet
    }
}

#[derive(Debug, Clone)]
struct Page {
    flags: u8,
    granule: [u8; 8],
    serial: u32,
    sequence: u32,
    lacing: Vec<u8>,
    body: Vec<u8>,
}

impl Page {
    /// Reads the page at the reader's position, which has `remaining`
    /// bytes left to the end of the sound, and checks its checksum.
    fn read(input: &mut dyn Read, remaining: u64) -> Result<Self, FormatError> {
        let mut header = [0u8; HEADER_LEN];
        if remaining < HEADER_LEN as u64 {
            return Err(not_sound());
        }
        input.read_exact(&mut header)?;
        let mut cursor = Cursor::new(&header);
        if cursor.take(4)? != CAPTURE {
            return Err(not_sound());
        }
        if cursor.u8()? != 0 {
            return Err(FormatError::malformed("an unknown Ogg version"));
        }
        let flags = cursor.u8()?;
        if flags & !(CONTINUED | BEGINS | ENDS) != 0 {
            return Err(FormatError::malformed("unknown Ogg page flags"));
        }
        let granule = cursor.array::<8>()?;
        let serial = cursor.u32_le()?;
        let sequence = cursor.u32_le()?;
        let checksum = cursor.u32_le()?;
        let mut lacing = vec![0u8; usize::from(cursor.u8()?)];
        input.read_exact(&mut lacing)?;
        let body_len: usize = lacing.iter().map(|&l| usize::from(l)).sum();
        if (HEADER_LEN + lacing.len() + body_len) as u64 > remaining {
            return Err(FormatError::malformed(
                "an Ogg page runs past the end of the file",
            ));
        }
        let mut body = vec![0u8; body_len];
        input.read_exact(&mut body)?;
        let page = Self {
            flags,
            granule,
            serial,
            sequence,
            lacing,
            body,
        };
        if page.checksum() != checksum {
            return Err(FormatError::malformed("Ogg page checksum mismatch"));
        }
        Ok(page)
    }

    fn len(&self) -> u64 {
        (HEADER_LEN + self.lacing.len() + self.body.len()) as u64
    }

    fn header(&self, checksum: u32) -> Vec<u8> {
        let mut header = Vec::with_capacity(HEADER_LEN + self.lacing.len());
        header.extend_from_slice(CAPTURE);
        header.extend_from_slice(&[0, self.flags]);
        header.extend_from_slice(&self.granule);
        header.extend_from_slice(&self.serial.to_le_bytes());
        header.extend_from_slice(&self.sequence.to_le_bytes());
        header.extend_from_slice(&checksum.to_le_bytes());
        header.push(self.lacing.len() as u8);
        header.extend_from_slice(&self.lacing);
        header
    }

    /// Computed with the checksum field set to zero.
    fn checksum(&self) -> u32 {
        let crc = crc32(0, &self.header(0));
        crc32(crc, &self.body)
    }

    fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = self.header(self.checksum());
        bytes.extend_from_slice(&self.body);
        bytes
    }

    /// The page's first packet, or as much of it as this page holds.
    fn first_packet(&self) -> &[u8] {
        let segments = self.lacing.iter().position(|&l| l < SEGMENT);
        let len: usize = match segments {
            Some(last) => self
                .lacing
                .iter()
                .take(last + 1)
                .map(|&l| usize::from(l))
                .sum(),
            None => self.body.len(),
        };
        self.body.get(..len).unwrap_or_default()
    }
}

fn not_sound() -> FormatError {
    FormatError::Unsupported(
        "data that isn't sound in the Ogg stream, which could hold anything".into(),
    )
}

#[derive(Debug)]
struct Layout {
    codec: Codec,
    /// The first page, which holds only the identification header.
    first: Page,
    /// Vorbis's third header: the decoder's setup, kept as it is.
    setup: Option<Vec<u8>>,
    /// How many pages the headers took, the first page included.
    header_pages: u64,
    sound_start: u64,
    sound_end: u64,
    findings: Vec<Finding>,
}

fn scan(input: &mut dyn ReadSeek) -> Result<Layout, FormatError> {
    let len = input.seek(SeekFrom::End(0))?;
    let mut trailing: Vec<Tag> = Vec::new();
    let end = trailing_tags(input, 0, len, &mut trailing)?;
    input.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::with_capacity(1 << 16, &mut *input);

    let mut pos = 0;
    let mut count: u64 = 0;
    let mut first: Option<Page> = None;
    let mut codec = None;
    let mut packets: Vec<Vec<u8>> = Vec::new();
    let mut packet = Vec::new();
    let mut header_pages = None;
    let mut sound_start = 0;
    // A packet runs on into the next page.
    let mut open = false;
    let mut ended = false;
    while pos < end {
        let page = Page::read(&mut reader, end - pos)?;
        let begins = page.flags & BEGINS != 0;
        match &first {
            None if !begins => {
                return Err(FormatError::malformed(
                    "an Ogg stream without its first page",
                ));
            }
            Some(_) if begins && ended => {
                return Err(FormatError::Unsupported(
                    "several Ogg streams one after another".into(),
                ));
            }
            Some(f) if begins || page.serial != f.serial => {
                return Err(FormatError::Unsupported(
                    "several streams in one Ogg file".into(),
                ));
            }
            Some(_) if ended => {
                return Err(FormatError::malformed(
                    "Ogg pages after the end of the stream",
                ));
            }
            _ => {}
        }
        let expected = first
            .as_ref()
            .map_or(page.sequence, |f| f.sequence.wrapping_add(count as u32));
        if page.sequence != expected {
            return Err(FormatError::malformed(
                "Ogg pages are missing or out of order",
            ));
        }
        if (page.flags & CONTINUED != 0) != open {
            return Err(FormatError::malformed(
                "an Ogg page doesn't continue the packet before it",
            ));
        }
        if !page.lacing.is_empty() {
            open = page.lacing.last() == Some(&SEGMENT);
        }
        ended = page.flags & ENDS != 0;

        if header_pages.is_none() {
            let mut at = 0;
            for &lace in &page.lacing {
                let next = at + usize::from(lace);
                packet.extend_from_slice(page.body.get(at..next).unwrap_or_default());
                at = next;
                if packet.len() as u64 > MAX_AUDIO_TAG_BYTES {
                    return Err(FormatError::TooLarge {
                        limit: MAX_AUDIO_TAG_BYTES,
                    });
                }
                if lace < SEGMENT {
                    packets.push(std::mem::take(&mut packet));
                }
            }
            if first.is_none() {
                codec = Codec::of(packets.first().map(Vec::as_slice).unwrap_or_default());
                if codec.is_none() {
                    return Err(FormatError::Unsupported(
                        "Ogg streams other than Vorbis and Opus".into(),
                    ));
                }
                if packets.len() != 1 || open {
                    return Err(FormatError::malformed(
                        "the first Ogg page must hold only the first header",
                    ));
                }
            }
            let needed = codec.map_or(0, Codec::headers);
            if packets.len() >= needed {
                if packets.len() > needed || open {
                    return Err(FormatError::malformed(
                        "the sound doesn't start on a new Ogg page",
                    ));
                }
                header_pages = Some(count + 1);
                sound_start = pos + page.len();
            }
        }
        if first.is_none() {
            first = Some(page.clone());
        }
        pos += page.len();
        count += 1;
    }

    let (Some(codec), Some(first), Some(header_pages)) = (codec, first, header_pages) else {
        return Err(FormatError::malformed("an Ogg file without its headers"));
    };
    if sound_start == end {
        return Err(FormatError::malformed("an Ogg file without any sound"));
    }
    let mut packets = packets.into_iter();
    let identification = packets.next().unwrap_or_default();
    let comment = packets.next().unwrap_or_default();
    let setup = packets.next();
    if !comment.starts_with(codec.comment_magic())
        || setup
            .as_ref()
            .is_some_and(|s| !s.starts_with(b"\x05vorbis"))
    {
        return Err(FormatError::malformed("Ogg headers in the wrong order"));
    }

    let mut findings = Vec::new();
    if codec == Codec::Opus {
        findings.extend(output_gain(&identification));
    }
    if comment != codec.empty_comment() {
        findings.extend(comment_findings(codec, &comment));
    }
    for tag in trailing.into_iter().rev() {
        findings.extend(tag.findings);
    }
    Ok(Layout {
        codec,
        first,
        setup,
        header_pages,
        sound_start,
        sound_end: end,
        findings,
    })
}

/// Opus's volume adjustment, which players apply to the sound. It's a
/// setting of the recording, not about whoever made it, so it stays.
fn output_gain(identification: &[u8]) -> Option<Finding> {
    let gain = i16::from_le_bytes(identification.get(16..18)?.try_into().ok()?);
    (gain != 0).then(|| {
        Finding::new(
            FindingKind::TechnicalInfo,
            Location::new(["OpusHead", "Output gain"]),
            Disposition::Keep(KeepReason::RequiredToPlay),
        )
        .with_value(Value::Text(format!("{:+.2} dB", f64::from(gain) / 256.0)))
    })
}

fn comment_findings(codec: Codec, packet: &[u8]) -> Vec<Finding> {
    let label = codec.label();
    let unread = |len: usize, location: Location| {
        Finding::new(FindingKind::UnknownMetadata, location, Disposition::Remove)
            .with_value(Value::Bytes(len as u64))
    };
    let data = packet
        .get(codec.comment_magic().len()..)
        .unwrap_or_default();
    let Ok((mut findings, used)) = vorbis::read(label, data) else {
        return vec![unread(data.len(), Location::new([label]))];
    };
    // Vorbis's framing bit; Opus allows binary data after the tags.
    let skip = usize::from(codec == Codec::Vorbis);
    let rest = data.get(used + skip..).unwrap_or_default();
    if rest.iter().any(|&b| b != 0) {
        findings.push(unread(rest.len(), Location::new([label, "Extra data"])));
    }
    findings
}

/// `packets` laid out in pages after `first`, each page holding as many
/// segments as fit, the way an encoder writes headers.
fn pages(packets: &[Vec<u8>], first: &Page) -> Vec<Page> {
    let mut segments: Vec<(u8, &[u8])> = Vec::new();
    for packet in packets {
        let mut chunks = packet.chunks(usize::from(SEGMENT));
        let mut last = 0;
        for chunk in chunks.by_ref() {
            segments.push((chunk.len() as u8, chunk));
            last = chunk.len();
        }
        // A packet that fills its last segment ends with an empty one.
        if last == usize::from(SEGMENT) || packet.is_empty() {
            segments.push((0, &[]));
        }
    }
    let mut pages = Vec::new();
    let mut continued = false;
    for (n, group) in segments.chunks(usize::from(SEGMENT)).enumerate() {
        let lacing: Vec<u8> = group.iter().map(|(len, _)| *len).collect();
        let ends_packet = lacing.iter().any(|&l| l < SEGMENT);
        pages.push(Page {
            flags: if continued { CONTINUED } else { 0 },
            granule: if ends_packet { [0; 8] } else { NO_GRANULE },
            serial: first.serial,
            sequence: first.sequence.wrapping_add(n as u32 + 1),
            body: group
                .iter()
                .flat_map(|(_, data)| data.iter().copied())
                .collect(),
            lacing,
        });
        continued = group.last().is_some_and(|(len, _)| *len == SEGMENT);
    }
    pages
}

/// CRC-32 with polynomial 0x04C11DB7, not reflected, as Ogg uses it.
static CRC32: LazyLock<[u32; 256]> = LazyLock::new(|| {
    std::array::from_fn(|i| {
        (0..8).fold((i as u32) << 24, |crc, _| {
            if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            }
        })
    })
});

fn crc32(crc: u32, data: &[u8]) -> u32 {
    data.iter().fold(crc, |crc, &byte| {
        let index = usize::from((crc >> 24) as u8 ^ byte);
        (crc << 8) ^ CRC32.get(index).copied().unwrap_or(0)
    })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor as IoCursor;

    use super::*;
    use crate::format::audio::vorbis::tests::block;

    const SERIAL: u32 = 0x1234_5678;

    fn page(flags: u8, sequence: u32, packets: &[&[u8]], open: bool) -> Vec<u8> {
        let mut lacing = Vec::new();
        for (i, packet) in packets.iter().enumerate() {
            let mut left = packet.len();
            while left >= 255 {
                lacing.push(255);
                left -= 255;
            }
            if !(open && i + 1 == packets.len()) {
                lacing.push(left as u8);
            }
        }
        Page {
            flags,
            granule: [0; 8],
            serial: SERIAL,
            sequence,
            lacing,
            body: packets.concat(),
        }
        .to_bytes()
    }

    fn opus_head(gain: i16) -> Vec<u8> {
        let mut head = b"OpusHead\x01\x01\x38\x01\x80\xbb\0\0".to_vec();
        head.extend_from_slice(&gain.to_le_bytes());
        head.push(0);
        head
    }

    fn opus_tags(vendor: &str, fields: &[&str]) -> Vec<u8> {
        [b"OpusTags".as_slice(), &block(vendor, fields)].concat()
    }

    /// Three pages of "sound", the last one ending the stream.
    fn sound(first_sequence: u32) -> Vec<u8> {
        [
            page(0, first_sequence, &[&[1; 300], &[2; 40]], false),
            page(0, first_sequence + 1, &[&[3; 50]], false),
            page(ENDS, first_sequence + 2, &[&[4; 60]], false),
        ]
        .concat()
    }

    fn opus(gain: i16, tags: &[u8]) -> Vec<u8> {
        [
            page(BEGINS, 0, &[&opus_head(gain)], false),
            page(0, 1, &[tags], false),
            sound(2),
        ]
        .concat()
    }

    fn clean(data: &[u8]) -> Result<(Vec<u8>, Vec<Finding>), FormatError> {
        let mut out = Vec::new();
        let findings = Ogg.clean(&mut IoCursor::new(data), &mut out, &Options::default())?;
        Ok((out, findings))
    }

    fn sound_bodies(data: &[u8]) -> Vec<Vec<u8>> {
        let mut reader = IoCursor::new(data);
        let mut bodies = Vec::new();
        let mut pos = 0;
        while pos < data.len() as u64 {
            let page = Page::read(&mut reader, data.len() as u64 - pos).unwrap();
            pos += page.len();
            bodies.push(page.body);
        }
        bodies
    }

    #[test]
    fn checksums_are_ogg_crc32() {
        // The standard check value for this CRC (no initial value, no final XOR).
        assert_eq!(crc32(0, b"123456789"), 0x89A1_897F);
    }

    #[test]
    fn tags_go_and_the_sound_stays() {
        let data = opus(
            0,
            &opus_tags("libopus 1.4", &["ARTIST=Jane Doe", "R128_TRACK_GAIN=-512"]),
        );
        let (out, findings) = clean(&data).unwrap();
        let kinds: Vec<_> = findings.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            [
                FindingKind::Software,
                FindingKind::PersonName,
                FindingKind::TechnicalInfo
            ]
        );
        let expected = opus(0, &Codec::Opus.empty_comment());
        assert_eq!(out, expected);
        assert_eq!(clean(&out).unwrap(), (out.clone(), Vec::new()));
    }

    #[test]
    fn the_output_gain_is_kept() {
        let data = opus(-768, &Codec::Opus.empty_comment());
        let (out, findings) = clean(&data).unwrap();
        assert_eq!(out, data);
        assert_eq!(
            findings[0].disposition,
            Disposition::Keep(KeepReason::RequiredToPlay)
        );
        assert_eq!(findings[0].value, Some(Value::Text("-3.00 dB".into())));
    }

    #[test]
    fn vorbis_headers_are_repacked_and_pages_renumbered() {
        let identification = b"\x01vorbis\0\0\0\0\x01\x44\xac\0\0\0\0\0\0\0\0\0\0\0\0\0\xb8\x01";
        // A large picture tag spreads the comment over two pages.
        let picture = format!("METADATA_BLOCK_PICTURE={}", "A".repeat(70_000));
        let comment = [
            b"\x03vorbis".as_slice(),
            &block("Xiph.Org libVorbis I 20200704", &[&picture]),
            &[1],
        ]
        .concat();
        let setup = [b"\x05vorbis".as_slice(), &[9; 3000]].concat();
        let headers = [comment.as_slice(), &setup].concat();
        let split = 255 * 255;
        let data = [
            page(BEGINS, 0, &[identification], false),
            page(0, 1, &[&headers[..split]], true),
            {
                // The rest of the comment, then the setup.
                let rest = &comment[split..];
                page(CONTINUED, 2, &[rest, &setup], false)
            },
            sound(3),
        ]
        .concat();
        let (out, findings) = clean(&data).unwrap();
        assert_eq!(findings[1].kind, FindingKind::CoverArt);
        let (again, none) = clean(&out).unwrap();
        assert_eq!((again, none), (out.clone(), Vec::new()));
        let bodies = sound_bodies(&out);
        assert_eq!(bodies.len(), 2 + 3);
        assert_eq!(&bodies[2..], &sound_bodies(&sound(0))[..]);
        // Sequence numbers run on without a gap: the output reads back.
        assert!(scan(&mut IoCursor::new(&out)).is_ok());
    }

    #[test]
    fn tags_appended_after_the_stream_go_too() {
        let bare = opus(0, &Codec::Opus.empty_comment());
        let mut v1 = vec![0u8; 128];
        v1[..3].copy_from_slice(b"TAG");
        v1[3..10].copy_from_slice(b"Holiday");
        let (out, findings) = clean(&[bare.clone(), v1].concat()).unwrap();
        assert_eq!(out, bare);
        assert_eq!(findings[0].kind, FindingKind::Description);
    }

    #[test]
    fn anything_but_one_stream_of_pages_is_refused() {
        let good = opus(0, &Codec::Opus.empty_comment());
        let header_len = page(BEGINS, 0, &[&opus_head(0)], false).len()
            + page(0, 1, &[&Codec::Opus.empty_comment()], false).len();
        let between = [&good[..header_len], b"hidden", &good[header_len..]].concat();
        assert!(matches!(clean(&between), Err(FormatError::Unsupported(_))));
        let chained = [good.clone(), good.clone()].concat();
        assert!(matches!(clean(&chained), Err(FormatError::Unsupported(_))));
        let mut gap = good[..header_len].to_vec();
        gap.extend(sound(3));
        assert!(matches!(clean(&gap), Err(FormatError::Malformed(_))));
        let mut damaged = good.clone();
        *damaged.last_mut().unwrap() ^= 1;
        assert!(matches!(clean(&damaged), Err(FormatError::Malformed(_))));
        let mut stranger = Page::read(&mut IoCursor::new(&sound(2)), 1 << 20).unwrap();
        stranger.serial = 1;
        let mixed = [&good[..header_len], &stranger.to_bytes(), &sound(3)].concat();
        assert!(matches!(clean(&mixed), Err(FormatError::Unsupported(_))));
    }

    #[test]
    fn other_codecs_are_not_taken() {
        let speex = page(BEGINS, 0, &[b"Speex   1.2"], false);
        assert!(!is_supported(&mut IoCursor::new(&speex)).unwrap());
        let data = opus(0, &Codec::Opus.empty_comment());
        assert!(is_supported(&mut IoCursor::new(&data)).unwrap());
    }
}
