//! WebP: a RIFF container of little-endian, length-prefixed chunks. Metadata
//! (`EXIF`, `XMP `, `ICCP`) is only valid in the extended format, which
//! starts with a `VP8X` chunk whose flags announce it.

use std::io::{self, Write};

use super::bytes::{Cursor, latin1};
use super::{
    FormatError, FormatHandler, ReadSeek, apply_options, drop_all, exif, icc, is_kept,
    kept_orientation, read_image, xmp,
};
use crate::model::{Disposition, Finding, FindingKind, Location, Value};
use crate::options::Options;

/// Chunks needed to display the image, still or animated.
const RENDERING_CHUNKS: &[&[u8; 4]] = &[b"VP8X", b"VP8 ", b"VP8L", b"ALPH", b"ANIM", b"ANMF"];

/// What an animation frame (`ANMF`) may hold besides its 16-byte header.
const FRAME_CHUNKS: &[&[u8; 4]] = &[b"ALPH", b"VP8 ", b"VP8L"];

const FRAME_HEADER_LEN: usize = 16;
const VP8X_LEN: usize = 10;

const ICC_FLAG: u8 = 0x20;
const EXIF_FLAG: u8 = 0x08;
const XMP_FLAG: u8 = 0x04;

#[derive(Debug)]
pub(super) struct Webp;

impl FormatHandler for Webp {
    fn inspect(
        &self,
        input: &mut dyn ReadSeek,
        options: &Options,
    ) -> Result<Vec<Finding>, FormatError> {
        let data = read_image(input)?;
        Ok(inspect_chunks(&parse(&data)?, options))
    }

    fn clean(
        &self,
        input: &mut dyn ReadSeek,
        output: &mut dyn Write,
        options: &Options,
    ) -> Result<Vec<Finding>, FormatError> {
        let data = read_image(input)?;
        let parsed = parse(&data)?;
        let findings = inspect_chunks(&parsed, options);
        write_clean(&parsed, &findings, output)?;
        Ok(findings)
    }
}

#[derive(Debug)]
struct Chunk<'a> {
    kind: [u8; 4],
    /// For `ANMF`, only the frame header; its chunks are in `frame`.
    data: &'a [u8],
    frame: Vec<Chunk<'a>>,
}

#[derive(Debug)]
struct Parsed<'a> {
    chunks: Vec<Chunk<'a>>,
    trailing: &'a [u8],
}

impl Parsed<'_> {
    fn is_extended(&self) -> bool {
        self.chunks.first().is_some_and(|c| &c.kind == b"VP8X")
    }
}

fn parse(data: &[u8]) -> Result<Parsed<'_>, FormatError> {
    let mut cursor = Cursor::new(data);
    if cursor.take(4)? != b"RIFF" {
        return Err(FormatError::malformed("missing RIFF header"));
    }
    let size = cursor.u32_le()? as usize;
    let mut riff = Cursor::new(cursor.take(size)?);
    if riff.take(4)? != b"WEBP" {
        return Err(FormatError::malformed("not a WebP file"));
    }
    let chunks = read_chunks(riff.remaining())?;

    let first = chunks
        .first()
        .ok_or_else(|| FormatError::malformed("WebP file without chunks"))?;
    match &first.kind {
        b"VP8X" if first.data.len() < VP8X_LEN => {
            return Err(FormatError::malformed("short VP8X chunk"));
        }
        b"VP8X" | b"VP8 " | b"VP8L" => {}
        _ => return Err(FormatError::malformed("WebP file without an image")),
    }
    if !chunks
        .iter()
        .any(|c| matches!(&c.kind, b"VP8 " | b"VP8L" | b"ANMF"))
    {
        return Err(FormatError::malformed("WebP file without an image"));
    }
    Ok(Parsed {
        chunks,
        trailing: cursor.remaining(),
    })
}

fn read_chunks(data: &[u8]) -> Result<Vec<Chunk<'_>>, FormatError> {
    let mut cursor = Cursor::new(data);
    let mut chunks = Vec::new();
    while !cursor.is_empty() {
        let kind = cursor.array::<4>()?;
        let size = cursor.u32_le()? as usize;
        let mut data = cursor.take(size)?;
        // Chunks are padded to an even size; some writers omit the last pad byte.
        if size % 2 == 1 && !cursor.is_empty() {
            cursor.skip(1)?;
        }
        let mut frame = Vec::new();
        if &kind == b"ANMF" {
            let mut header = Cursor::new(data);
            let header_bytes = header.take(FRAME_HEADER_LEN)?;
            frame = read_chunks(header.remaining())?;
            data = header_bytes;
        }
        chunks.push(Chunk { kind, data, frame });
    }
    Ok(chunks)
}

fn inspect_chunks(parsed: &Parsed<'_>, options: &Options) -> Vec<Finding> {
    let extended = parsed.is_extended();
    let mut findings = Vec::new();
    let mut seen_exif = false;
    let mut seen_icc = false;
    for chunk in &parsed.chunks {
        let location = Location::new(["WebP".to_owned(), latin1(&chunk.kind)]);
        let mut chunk_findings = match &chunk.kind {
            b"ANMF" => {
                let unknown = chunk
                    .frame
                    .iter()
                    .filter(|c| !FRAME_CHUNKS.contains(&&c.kind));
                findings.extend(unknown.map(|c| unknown_chunk(c, &location)));
                continue;
            }
            kind if RENDERING_CHUNKS.contains(&kind) => continue,
            b"EXIF" => {
                // The TIFF data should start right away, but some writers
                // copy the JPEG `Exif\0\0` prefix too.
                let tiff = chunk.data.strip_prefix(b"Exif\0\0").unwrap_or(chunk.data);
                exif::inspect(tiff, &location)
            }
            b"ICCP" => vec![icc::inspect(chunk.data, location.clone())],
            b"XMP " => xmp::inspect(chunk.data, &location),
            _ => vec![unknown_chunk(chunk, &Location::new(["WebP"]))],
        };
        // Outside the extended format, decoders ignore metadata, so there is
        // nothing worth keeping. Only the first EXIF and ICCP can survive.
        match &chunk.kind {
            b"EXIF" if extended && !seen_exif => apply_options(&mut chunk_findings, options),
            b"ICCP" if extended && !seen_icc => apply_options(&mut chunk_findings, options),
            _ => drop_all(&mut chunk_findings),
        }
        seen_exif |= &chunk.kind == b"EXIF";
        seen_icc |= &chunk.kind == b"ICCP";
        findings.extend(chunk_findings);
    }
    if !parsed.trailing.is_empty() {
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

fn unknown_chunk(chunk: &Chunk<'_>, parent: &Location) -> Finding {
    Finding::new(
        FindingKind::UnknownMetadata,
        parent.child(latin1(&chunk.kind)),
        Disposition::Remove,
    )
    .with_value(Value::Bytes(chunk.data.len() as u64))
}

/// Writes the chunks needed to display the image, plus what the findings say
/// is kept, and updates the `VP8X` flags and the RIFF size to match.
fn write_clean(parsed: &Parsed<'_>, findings: &[Finding], out: &mut dyn Write) -> io::Result<()> {
    let keep_icc = is_kept(
        findings,
        FindingKind::ColorProfile,
        &Location::new(["WebP", "ICCP"]),
    );
    let orientation = kept_orientation(findings, &Location::new(["WebP", "EXIF", "Orientation"]));
    let mut seen_exif = false;
    let mut exif_written = false;
    let mut icc_written = false;
    let mut flags_at = None;

    let mut body = b"WEBP".to_vec();
    for chunk in &parsed.chunks {
        match &chunk.kind {
            b"VP8X" => {
                flags_at = Some(body.len() + 8);
                push_chunk(&mut body, &chunk.kind, chunk.data)?;
            }
            b"ANMF" => {
                let mut frame = chunk.data.to_vec();
                for inner in &chunk.frame {
                    if FRAME_CHUNKS.contains(&&inner.kind) {
                        push_chunk(&mut frame, &inner.kind, inner.data)?;
                    }
                }
                push_chunk(&mut body, b"ANMF", &frame)?;
            }
            kind if RENDERING_CHUNKS.contains(&kind) => push_chunk(&mut body, kind, chunk.data)?,
            b"ICCP" if keep_icc && !icc_written => {
                icc_written = true;
                push_chunk(&mut body, b"ICCP", chunk.data)?;
            }
            b"EXIF" if !seen_exif => {
                seen_exif = true;
                if let Some(orientation) = orientation {
                    exif_written = true;
                    push_chunk(&mut body, b"EXIF", &exif::orientation_only(orientation))?;
                }
            }
            _ => {}
        }
    }
    if let Some(flags) = flags_at.and_then(|at| body.get_mut(at)) {
        *flags &= !(ICC_FLAG | EXIF_FLAG | XMP_FLAG);
        if icc_written {
            *flags |= ICC_FLAG;
        }
        if exif_written {
            *flags |= EXIF_FLAG;
        }
    }

    out.write_all(b"RIFF")?;
    out.write_all(&size(body.len())?.to_le_bytes())?;
    out.write_all(&body)
}

fn push_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) -> io::Result<()> {
    out.extend(kind);
    out.extend(size(data.len())?.to_le_bytes());
    out.extend(data);
    if data.len() % 2 == 1 {
        out.push(0);
    }
    Ok(())
}

fn size(len: usize) -> io::Result<u32> {
    u32::try_from(len).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "WebP too large"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stands in for image data; these tests never decode it.
    const VP8L: &[u8] = &[
        0x2F, 0, 0, 0, 0x10, 0x07, 0x10, 0x11, 0x11, 0x88, 0x88, 0xFE, 0x07,
    ];

    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        push_chunk(&mut out, kind, data).unwrap();
        out
    }

    fn vp8x(flags: u8) -> Vec<u8> {
        chunk(b"VP8X", &[flags, 0, 0, 0, 0, 0, 0, 0, 0, 0])
    }

    fn webp(chunks: &[Vec<u8>], trailing: &[u8]) -> Vec<u8> {
        let body: Vec<u8> = [b"WEBP".to_vec(), chunks.concat()].concat();
        let mut out = b"RIFF".to_vec();
        out.extend((body.len() as u32).to_le_bytes());
        out.extend(body);
        out.extend(trailing);
        out
    }

    fn inspect(data: &[u8]) -> Vec<Finding> {
        Webp.inspect(&mut io::Cursor::new(data), &Options::default())
            .unwrap()
    }

    fn clean(data: &[u8], options: &Options) -> Vec<u8> {
        let mut out = Vec::new();
        Webp.clean(&mut io::Cursor::new(data), &mut out, options)
            .unwrap();
        out
    }

    fn standard_profile() -> Vec<u8> {
        let srgb = [
            [0.4361, 0.2225, 0.0139],
            [0.3851, 0.7169, 0.0971],
            [0.1431, 0.0606, 0.7141],
        ];
        icc::tests::profile("sRGB", srgb, None)
    }

    #[test]
    fn cleaning_keeps_only_rendering_chunks_and_fixes_the_flags() {
        let data = webp(
            &[
                vp8x(ICC_FLAG | EXIF_FLAG | XMP_FLAG | 0x10),
                chunk(b"ICCP", b"not a profile"),
                chunk(b"VP8L", VP8L),
                chunk(b"EXIF", &exif::orientation_only(1)),
                chunk(b"XMP ", b"<x:xmpmeta/>"),
                chunk(b"prVt", b"vendor"),
            ],
            b"after the end",
        );
        let expected = webp(&[vp8x(0x10), chunk(b"VP8L", VP8L)], b"");
        assert_eq!(clean(&data, &Options::default()), expected);
    }

    #[test]
    fn cleaning_rebuilds_exif_with_only_the_orientation() {
        let rotated = webp(
            &[
                vp8x(EXIF_FLAG),
                chunk(b"VP8L", VP8L),
                chunk(b"EXIF", &exif::orientation_only(6)),
            ],
            b"",
        );
        assert_eq!(clean(&rotated, &Options::default()), rotated);
        let strip = Options {
            strip_orientation: true,
            ..Options::default()
        };
        assert_eq!(
            clean(&rotated, &strip),
            webp(&[vp8x(0), chunk(b"VP8L", VP8L)], b"")
        );
    }

    #[test]
    fn exif_with_a_jpeg_prefix_is_read() {
        let prefixed = [b"Exif\0\0".as_slice(), &exif::orientation_only(6)].concat();
        let data = webp(
            &[
                vp8x(EXIF_FLAG),
                chunk(b"VP8L", VP8L),
                chunk(b"EXIF", &prefixed),
            ],
            b"",
        );
        assert_eq!(inspect(&data)[0].kind, FindingKind::Orientation);
    }

    #[test]
    fn cleaning_keeps_standard_profiles() {
        let data = webp(
            &[
                vp8x(ICC_FLAG),
                chunk(b"ICCP", &standard_profile()),
                chunk(b"VP8L", VP8L),
            ],
            b"",
        );
        assert_eq!(clean(&data, &Options::default()), data);
    }

    #[test]
    fn metadata_outside_the_extended_format_is_removed() {
        let data = webp(
            &[
                chunk(b"VP8L", VP8L),
                chunk(b"EXIF", &exif::orientation_only(6)),
                chunk(b"ICCP", &standard_profile()),
            ],
            b"",
        );
        let findings = inspect(&data);
        assert!(
            findings
                .iter()
                .all(|f| f.disposition == Disposition::Remove)
        );
        assert_eq!(
            clean(&data, &Options::default()),
            webp(&[chunk(b"VP8L", VP8L)], b"")
        );
    }

    #[test]
    fn animation_frames_keep_only_their_image() {
        let frame = |extra: &[u8]| {
            let mut frame = vec![0; FRAME_HEADER_LEN];
            frame.extend(chunk(b"VP8L", VP8L));
            frame.extend(extra);
            chunk(b"ANMF", &frame)
        };
        let data = webp(
            &[
                vp8x(0x02),
                chunk(b"ANIM", &[0, 0, 0, 0, 0, 0]),
                frame(&chunk(b"prVt", b"frame note")),
            ],
            b"",
        );
        let findings = inspect(&data);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].location.to_string(), "WebP > ANMF > prVt");
        assert_eq!(
            clean(&data, &Options::default()),
            webp(
                &[vp8x(0x02), chunk(b"ANIM", &[0, 0, 0, 0, 0, 0]), frame(&[])],
                b""
            )
        );
    }

    #[test]
    fn odd_sized_chunks_are_padded() {
        let data = webp(&[chunk(b"VP8L", VP8L), chunk(b"prVt", b"odd")], b"");
        assert_eq!(VP8L.len() % 2, 1);
        assert_eq!(
            clean(&data, &Options::default()),
            webp(&[chunk(b"VP8L", VP8L)], b"")
        );
    }

    #[test]
    fn files_without_an_image_are_errors() {
        assert!(parse(&webp(&[], b"")).is_err());
        assert!(parse(&webp(&[vp8x(0)], b"")).is_err());
        assert!(parse(&webp(&[chunk(b"EXIF", b"x"), chunk(b"VP8L", VP8L)], b"")).is_err());
        assert!(parse(&webp(&[chunk(b"VP8X", &[0; 4]), chunk(b"VP8L", VP8L)], b"")).is_err());
    }

    #[test]
    fn trailing_data_is_reported() {
        let findings = inspect(&webp(&[chunk(b"VP8L", VP8L)], b"extra"));
        assert_eq!(findings[0].kind, FindingKind::TrailingData);
    }

    #[test]
    fn truncated_files_are_errors_not_panics() {
        let data = webp(
            &[vp8x(EXIF_FLAG), chunk(b"VP8L", VP8L), chunk(b"EXIF", b"MM")],
            b"",
        );
        for len in 0..data.len() {
            assert!(parse(&data[..len]).is_err(), "length {len}");
        }
    }
}
