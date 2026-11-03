#![no_main]

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;
use metascrub_core::Options;
use metascrub_core::format::handler;
use metascrub_core::model::FormatId;

// Every input goes straight to the box parser that MP4, QuickTime, HEIC and
// AVIF share, with the same promises as `clean`: inspect says what clean
// does, nothing removable is left, and a second cleaning changes nothing.
fuzz_target!(|input: &[u8]| {
    let options = Options {
        strip_orientation: input.len() % 2 == 1,
        ..Options::default()
    };
    let media = handler(FormatId::Mp4).expect("MP4 is supported");
    let mut cleaned = Vec::new();
    let Ok(findings) = media.clean(&mut Cursor::new(input), &mut cleaned, &options) else {
        return;
    };
    let inspected = media
        .inspect(&mut Cursor::new(input), &options)
        .expect("inspect fails where clean succeeds");
    assert_eq!(inspected, findings, "inspect and clean disagree");
    let after = media
        .inspect(&mut Cursor::new(&cleaned), &options)
        .unwrap_or_else(|e| panic!("cleaned output can't be read: {e}"));
    let remaining: Vec<_> = after
        .iter()
        .filter(|f| !f.disposition.is_acceptable_after_cleaning())
        .collect();
    assert!(remaining.is_empty(), "cleaned output still has {remaining:?}");
    let mut again = Vec::new();
    media
        .clean(&mut Cursor::new(&cleaned), &mut again, &options)
        .unwrap_or_else(|e| panic!("cleaned output can't be cleaned again: {e}"));
    assert_eq!(again, cleaned, "cleaning twice changed the output");
});
