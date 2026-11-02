#![no_main]

use libfuzzer_sys::fuzz_target;

// The JPEG signature is prepended so every input reaches the JPEG parser.
fuzz_target!(|data: &[u8]| {
    let input = [&[0xFF, 0xD8, 0xFF][..], data].concat();
    let _ = metascrub_core::format::inspect_bytes(&input);
});
