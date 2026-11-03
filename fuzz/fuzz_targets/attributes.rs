#![no_main]

use libfuzzer_sys::fuzz_target;

/// Attributes whose values MetaScrub reads.
const NAMES: &[&str] = &[
    "Zone.Identifier",
    "com.apple.metadata:kMDItemWhereFroms",
    "com.apple.metadata:_kMDItemUserTags",
    "com.apple.metadata:kMDItemFinderComment",
    "com.apple.quarantine",
    "user.xdg.origin.url",
];

// The first byte picks the attribute; the rest is its value.
fuzz_target!(|data: &[u8]| {
    let Some((&pick, value)) = data.split_first() else {
        return;
    };
    let name = NAMES[usize::from(pick) % NAMES.len()];
    let _ = metascrub_core::describe_attribute(name, value);
});
