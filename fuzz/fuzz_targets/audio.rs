#![no_main]

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;
use metascrub_core::Options;
use metascrub_core::format::handler;
use metascrub_core::model::FormatId;

// Every input goes straight to the music parsers, with the same promises as
// `clean`: inspect says what clean does, nothing removable is left, and a
// second cleaning changes nothing.
fuzz_target!(|input: &[u8]| {
    let options = Options::default();
    let audio = handler(FormatId::Mp3).expect("MP3 is supported");
    let mut cleaned = Vec::new();
    let Ok(findings) = audio.clean(&mut Cursor::new(input), &mut cleaned, &options) else {
        return;
    };
    let inspected = audio
        .inspect(&mut Cursor::new(input), &options)
        .expect("inspect fails where clean succeeds");
    assert_eq!(inspected, findings, "inspect and clean disagree");
    let after = audio
        .inspect(&mut Cursor::new(&cleaned), &options)
        .unwrap_or_else(|e| panic!("cleaned output can't be read: {e}"));
    let remaining: Vec<_> = after
        .iter()
        .filter(|f| !f.disposition.is_acceptable_after_cleaning())
        .collect();
    assert!(remaining.is_empty(), "cleaned output still has {remaining:?}");
    let mut again = Vec::new();
    audio
        .clean(&mut Cursor::new(&cleaned), &mut again, &options)
        .unwrap_or_else(|e| panic!("cleaned output can't be cleaned again: {e}"));
    assert_eq!(again, cleaned, "cleaning twice changed the output");
});
