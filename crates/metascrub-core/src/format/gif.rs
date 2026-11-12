//! GIF: a screen description and color table, then blocks: images,
//! extensions and the trailer. Extensions hold comments, XMP, color
//! profiles and whatever programs add; only animation settings (looping,
//! frame timing) and text the image displays are needed to show it.
//! Anything after the trailer isn't part of the image.

use std::io::{self, Write};

use super::bytes::latin1;
use super::{
    FormatError, FormatHandler, ReadSeek, apply_options, drop_all, icc, is_kept, read_image, xmp,
};
use crate::model::{Disposition, Finding, FindingKind, Location, Value};
use crate::options::Options;

const EXTENSION: u8 = 0x21;
const IMAGE: u8 = 0x2C;
const TRAILER: u8 = 0x3B;
const PLAIN_TEXT: u8 = 0x01;
const GRAPHIC_CONTROL: u8 = 0xF9;
const COMMENT: u8 = 0xFE;
const APPLICATION: u8 = 0xFF;
/// Looping, in Netscape's and an older equivalent's extension.
const LOOPING: &[&[u8]] = &[b"NETSCAPE2.0", b"ANIMEXTS1.0"];
const XMP_ID: &[u8] = b"XMP DataXMP";
const ICC_ID: &[u8] = b"ICCRGBG1012";

#[derive(Debug)]
pub(super) struct Gif;

impl FormatHandler for Gif {
    fn inspect(
        &self,
        input: &mut dyn ReadSeek,
        options: &Options,
    ) -> Result<Vec<Finding>, FormatError> {
        let data = read_image(input)?;
        Ok(findings(&parse(&data)?, options))
    }

    fn clean(
        &self,
        input: &mut dyn ReadSeek,
        output: &mut dyn Write,
        options: &Options,
    ) -> Result<Vec<Finding>, FormatError> {
        let data = read_image(input)?;
        let parsed = parse(&data)?;
        let findings = findings(&parsed, options);
        write_clean(&parsed, &findings, output)?;
        Ok(findings)
    }
}

#[derive(Debug)]
struct Block<'a> {
    /// `IMAGE`, or the extension's label.
    kind: Option<u8>,
    /// The whole block, as written.
    raw: &'a [u8],
    /// An extension's data, its sub-blocks joined.
    data: Vec<u8>,
    /// The XMP packet, written as raw bytes instead of sub-blocks.
    xmp: &'a [u8],
}

#[derive(Debug)]
struct Parsed<'a> {
    /// Signature, screen description and global color table.
    head: &'a [u8],
    blocks: Vec<Block<'a>>,
    trailing: &'a [u8],
}

fn malformed(detail: &str) -> FormatError {
    FormatError::malformed(detail)
}

fn byte(data: &[u8], at: usize) -> Result<u8, FormatError> {
    data.get(at)
        .copied()
        .ok_or_else(|| malformed("GIF cut short"))
}

/// A color table's length, from a packed flags byte.
fn table_len(flags: u8) -> usize {
    if flags & 0x80 == 0 {
        0
    } else {
        3 << ((flags & 0x07) + 1)
    }
}

/// Reads sub-blocks from `at` up to and including the empty one that ends
/// them. Returns where they end and their data joined.
fn sub_blocks(data: &[u8], mut at: usize) -> Result<(usize, Vec<u8>), FormatError> {
    let mut joined = Vec::new();
    loop {
        let len = usize::from(byte(data, at)?);
        at += 1;
        if len == 0 {
            return Ok((at, joined));
        }
        joined.extend_from_slice(
            data.get(at..at + len)
                .ok_or_else(|| malformed("GIF cut short"))?,
        );
        at += len;
    }
}

fn parse(data: &[u8]) -> Result<Parsed<'_>, FormatError> {
    if !matches!(data.get(..6), Some(b"GIF87a" | b"GIF89a")) {
        return Err(malformed("missing the GIF signature"));
    }
    let head_end = 13 + table_len(byte(data, 10)?);
    let head = data
        .get(..head_end)
        .ok_or_else(|| malformed("GIF cut short"))?;
    let mut blocks = Vec::new();
    let mut at = head_end;
    loop {
        let start = at;
        match data.get(at) {
            // Ending without a trailer: decoders show it, so it's kept.
            None => {
                return Ok(Parsed {
                    head,
                    blocks,
                    trailing: &[],
                });
            }
            Some(&TRAILER) => {
                return Ok(Parsed {
                    head,
                    blocks,
                    trailing: data.get(at + 1..).unwrap_or_default(),
                });
            }
            Some(&IMAGE) => {
                let flags = byte(data, at + 9)?;
                at += 10 + table_len(flags);
                // The LZW minimum code size, then the compressed image.
                byte(data, at)?;
                let (end, _) = sub_blocks(data, at + 1)?;
                at = end;
                blocks.push(Block {
                    kind: None,
                    raw: data.get(start..at).unwrap_or_default(),
                    data: Vec::new(),
                    xmp: &[],
                });
            }
            Some(&EXTENSION) => {
                let label = byte(data, at + 1)?;
                let (end, joined) = sub_blocks(data, at + 2)?;
                let raw = data.get(start..end).unwrap_or_default();
                // XMP isn't split into sub-blocks: its bytes follow the
                // identifier, ended by a "magic trailer" that makes them
                // read as sub-blocks.
                let xmp = if label == APPLICATION && joined.starts_with(XMP_ID) {
                    raw.get(3 + XMP_ID.len()..).map_or(&[][..], xmp_packet)
                } else {
                    &[]
                };
                at = end;
                blocks.push(Block {
                    kind: Some(label),
                    raw,
                    data: joined,
                    xmp,
                });
            }
            Some(_) => return Err(malformed("an unknown GIF block")),
        }
    }
}

/// The packet itself, up to the end of `<?xpacket end=…?>` or of the last
/// element.
fn xmp_packet(raw: &[u8]) -> &[u8] {
    let raw = match find(raw, &[0x01, 0xFF, 0xFE]) {
        Some(trailer) => raw.get(..trailer).unwrap_or_default(),
        None => raw,
    };
    let end = find(raw, b"<?xpacket end")
        .and_then(|at| find(raw.get(at..)?, b"?>").map(|e| at + e + 2))
        .or_else(|| raw.iter().rposition(|&b| b == b'>').map(|p| p + 1))
        .unwrap_or(0);
    raw.get(..end).unwrap_or_default()
}

fn find(data: &[u8], needle: &[u8]) -> Option<usize> {
    data.windows(needle.len()).position(|w| w == needle)
}

fn findings(parsed: &Parsed<'_>, options: &Options) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut seen_icc = false;
    for block in &parsed.blocks {
        let Some(label) = block.kind else { continue };
        match label {
            GRAPHIC_CONTROL | PLAIN_TEXT => {}
            COMMENT => findings.push(
                Finding::new(
                    FindingKind::Comment,
                    Location::new(["Comment"]),
                    Disposition::Remove,
                )
                .with_value(Value::Text(latin1(&block.data).trim().to_owned())),
            ),
            APPLICATION => {
                let id = block.data.get(..11).unwrap_or_default();
                if LOOPING.contains(&id) {
                    continue;
                }
                if id == XMP_ID {
                    let mut xmp = xmp::inspect(block.xmp, &Location::new(["XMP"]));
                    drop_all(&mut xmp);
                    findings.extend(xmp);
                } else if id == ICC_ID {
                    let mut icc = [icc::inspect(
                        block.data.get(11..).unwrap_or_default(),
                        Location::new(["ICC"]),
                    )];
                    if seen_icc {
                        drop_all(&mut icc);
                    } else {
                        apply_options(&mut icc, options);
                    }
                    seen_icc = true;
                    findings.extend(icc);
                } else {
                    findings.push(
                        Finding::new(
                            FindingKind::UnknownMetadata,
                            Location::new(["Application".to_owned(), latin1(id)]),
                            Disposition::Remove,
                        )
                        .with_value(Value::Bytes(block.raw.len() as u64)),
                    );
                }
            }
            other => findings.push(
                Finding::new(
                    FindingKind::UnknownMetadata,
                    Location::new([format!("Extension {other:#04x}")]),
                    Disposition::Remove,
                )
                .with_value(Value::Bytes(block.raw.len() as u64)),
            ),
        }
    }
    if parsed.trailing.iter().any(|&b| b != 0) {
        findings.push(
            Finding::new(
                FindingKind::TrailingData,
                Location::new(["After end of image"]),
                Disposition::Remove,
            )
            .with_value(Value::Bytes(parsed.trailing.len() as u64)),
        );
    }
    findings
}

/// Writes the images and what's needed to show them, plus a color
/// profile the findings say is kept.
fn write_clean(parsed: &Parsed<'_>, findings: &[Finding], out: &mut dyn Write) -> io::Result<()> {
    let keep_icc = is_kept(findings, FindingKind::ColorProfile, &Location::new(["ICC"]));
    let mut icc_written = false;
    out.write_all(parsed.head)?;
    for block in &parsed.blocks {
        let keep = match block.kind {
            None | Some(GRAPHIC_CONTROL | PLAIN_TEXT) => true,
            Some(APPLICATION) => {
                let id = block.data.get(..11).unwrap_or_default();
                if id == ICC_ID && keep_icc && !icc_written {
                    icc_written = true;
                    true
                } else {
                    LOOPING.contains(&id)
                }
            }
            Some(_) => false,
        };
        if keep {
            out.write_all(block.raw)?;
        }
    }
    out.write_all(&[TRAILER])
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Cursor;

    use super::*;

    /// A 2×2 image: each pixel sent as a literal code after a clear code,
    /// so no LZW dictionary is needed.
    pub(crate) fn image() -> Vec<u8> {
        let mut gif = b"GIF89a\x02\x00\x02\x00\x80\x00\x00".to_vec();
        gif.extend_from_slice(&[0, 0, 0, 255, 255, 255]);
        gif.extend_from_slice(&[IMAGE, 0, 0, 0, 0, 2, 0, 2, 0, 0, 2]);
        // Codes of 3 bits, least significant first: clear (4), pixel… end (5).
        let codes = [4u32, 0, 4, 1, 4, 1, 4, 0, 5];
        let mut bits = 0u64;
        for (i, code) in codes.iter().enumerate() {
            bits |= u64::from(*code) << (3 * i);
        }
        let bytes: Vec<u8> = (0..4).map(|i| (bits >> (8 * i)) as u8).collect();
        gif.push(bytes.len() as u8);
        gif.extend_from_slice(&bytes);
        gif.push(0);
        gif
    }

    fn extension(label: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![EXTENSION, label];
        for chunk in payload.chunks(255) {
            out.push(chunk.len() as u8);
            out.extend_from_slice(chunk);
        }
        out.push(0);
        out
    }

    fn with(extensions: &[Vec<u8>], trailing: &[u8]) -> Vec<u8> {
        let image = image();
        let (head, body) = image.split_at(19);
        [head, &extensions.concat(), body, &[TRAILER], trailing].concat()
    }

    fn clean(data: &[u8]) -> (Vec<u8>, Vec<Finding>) {
        let mut out = Vec::new();
        let findings = Gif
            .clean(&mut Cursor::new(data), &mut out, &Options::default())
            .unwrap();
        (out, findings)
    }

    #[test]
    fn comments_xmp_and_unknown_extensions_go() {
        let looping = extension(APPLICATION, b"NETSCAPE2.0\x03\x01\x00\x00");
        let timing = extension(GRAPHIC_CONTROL, &[0, 10, 0, 0]);
        // XMP's raw bytes, then the 258-byte magic trailer: 0x01, 0xFF down
        // to 0x00, and the block terminator.
        let mut xmp = vec![EXTENSION, APPLICATION, 11];
        xmp.extend_from_slice(XMP_ID);
        xmp.extend_from_slice(
            br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/" dc:creator="Jane Doe"/></rdf:RDF></x:xmpmeta>"#,
        );
        xmp.push(1);
        xmp.extend((0..=255u8).rev());
        xmp.push(0);
        let data = with(
            &[
                looping.clone(),
                extension(COMMENT, b"Made by Jane"),
                xmp,
                extension(APPLICATION, b"FOOBAR00123data"),
                timing.clone(),
            ],
            b"appended",
        );
        let (out, findings) = clean(&data);
        assert_eq!(out, with(&[looping, timing], b""));
        let kinds: Vec<_> = findings.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            [
                FindingKind::Comment,
                FindingKind::PersonName,
                FindingKind::UnknownMetadata,
                FindingKind::TrailingData
            ]
        );
        assert_eq!(clean(&out), (out.clone(), Vec::new()));
    }

    #[test]
    fn a_damaged_gif_is_refused() {
        let image = image();
        let mut cut = image.clone();
        cut.truncate(image.len() - 3);
        assert!(
            Gif.clean(&mut Cursor::new(&cut), &mut Vec::new(), &Options::default())
                .is_err()
        );
        let mut odd = image;
        odd.push(0x99);
        assert!(
            Gif.clean(&mut Cursor::new(&odd), &mut Vec::new(), &Options::default())
                .is_err()
        );
    }
}
