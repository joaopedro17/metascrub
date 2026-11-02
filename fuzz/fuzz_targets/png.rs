#![no_main]

use libfuzzer_sys::fuzz_target;

const CHUNK_TYPES: [&[u8; 4]; 8] = [
    b"tEXt", b"zTXt", b"iTXt", b"eXIf", b"iCCP", b"tIME", b"bKGD", b"prVt",
];

fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let body = [kind.as_slice(), data].concat();
    let mut crc = flate2::Crc::new();
    crc.update(&body);
    [
        (data.len() as u32).to_be_bytes().as_slice(),
        &body,
        &crc.sum().to_be_bytes(),
    ]
    .concat()
}

// Random bytes almost never have a valid chunk checksum, so the PNG parser
// would reject them at the first chunk. This target wraps the input in a
// chunk with a correct checksum, so the code that reads each chunk type is
// actually exercised. The first byte picks the chunk type.
fuzz_target!(|data: &[u8]| {
    let Some((&selector, payload)) = data.split_first() else {
        return;
    };
    let kind = CHUNK_TYPES[usize::from(selector) % CHUNK_TYPES.len()];
    let input = [
        b"\x89PNG\r\n\x1a\n".as_slice(),
        &chunk(b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 0, 0, 0, 0]),
        &chunk(kind, payload),
        &chunk(b"IEND", &[]),
    ]
    .concat();
    let _ = metascrub_core::format::inspect_bytes(&input);
});
