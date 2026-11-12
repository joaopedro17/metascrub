#![no_main]

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;
use metascrub_core::Options;
use metascrub_core::format::handler;
use metascrub_core::model::FormatId;

// Every input goes straight to the Matroska and WebM cleaner, with the same
// promises as `clean`: inspect says what clean does, nothing removable is
// left, the file keeps its size or gets shorter, and a second cleaning
// changes nothing.
fuzz_target!(|input: &[u8]| {
    let options = Options::default();
    let picture = handler(FormatId::Mkv).expect("supported");
    let mut cleaned = Vec::new();
    let Ok(findings) = picture.clean(&mut Cursor::new(input), &mut cleaned, &options) else {
        return;
    };
    let inspected = picture
        .inspect(&mut Cursor::new(input), &options)
        .expect("inspect fails where clean succeeds");
    assert_eq!(inspected, findings, "inspect and clean disagree");
    assert!(cleaned.len() <= input.len(), "cleaning made the file longer");
    let after = picture
        .inspect(&mut Cursor::new(&cleaned), &options)
        .unwrap_or_else(|e| panic!("cleaned output can't be read: {e}"));
    let remaining: Vec<_> = after
        .iter()
        .filter(|f| !f.disposition.is_acceptable_after_cleaning())
        .collect();
    assert!(
        remaining.is_empty(),
        "cleaned output still has {remaining:?}"
    );
    let mut again = Vec::new();
    picture
        .clean(&mut Cursor::new(&cleaned), &mut again, &options)
        .unwrap_or_else(|e| panic!("cleaned output can't be cleaned again: {e}"));
    assert_eq!(again, cleaned, "cleaning twice changed the output");
});
