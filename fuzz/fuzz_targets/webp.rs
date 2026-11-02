#![no_main]

use libfuzzer_sys::fuzz_target;

const CHUNK_TYPES: [&[u8; 4]; 6] = [b"EXIF", b"XMP ", b"ICCP", b"ANMF", b"VP8X", b"prVt"];

fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = kind.to_vec();
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(data);
    if data.len() % 2 == 1 {
        out.push(0);
    }
    out
}

// Wraps the input in a well-formed extended WebP, so the reader of each
// chunk type is reached instead of failing at the container. The first byte
// picks the chunk type.
fuzz_target!(|data: &[u8]| {
    let Some((&selector, payload)) = data.split_first() else {
        return;
    };
    let kind = CHUNK_TYPES[usize::from(selector) % CHUNK_TYPES.len()];
    let body = [
        b"WEBP".as_slice(),
        &chunk(b"VP8X", &[0x2C, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
        &chunk(b"VP8L", &[0x2F, 0, 0, 0, 0]),
        &chunk(kind, payload),
    ]
    .concat();
    let input = [
        b"RIFF".as_slice(),
        &(body.len() as u32).to_le_bytes(),
        &body,
    ]
    .concat();
    let _ = metascrub_core::format::clean_bytes(&input, &Default::default());
});
