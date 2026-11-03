#![no_main]

use libfuzzer_sys::fuzz_target;
use metascrub_core::format::{clean_bytes, inspect_bytes};
use metascrub_core::model::Disposition;
use metascrub_core::{Options, TrackedChanges};

// Whatever the input, cleaning either fails, waits for the user's decision,
// or produces a file that passes verification and that a second cleaning
// leaves unchanged. The length picks the options, so the fixtures still
// work as seeds.
fuzz_target!(|input: &[u8]| {
    let options = Options {
        keep_color_profile: input.len() % 2 == 1,
        strip_orientation: input.len() % 4 >= 2,
        keep_comments: input.len() % 8 >= 4,
        keep_attachments: input.len() % 16 >= 8,
        tracked_changes: match input.len() % 3 {
            0 => TrackedChanges::Undecided,
            1 => TrackedChanges::Accept,
            _ => TrackedChanges::Reject,
        },
        ..Options::default()
    };
    let Ok(Some(cleaned)) = clean_bytes(input, &options) else {
        return;
    };
    if cleaned
        .findings
        .iter()
        .any(|f| matches!(f.disposition, Disposition::Blocks(_)))
    {
        return;
    }
    let (_, findings) = inspect_bytes(&cleaned.data, &options)
        .unwrap_or_else(|e| panic!("cleaned output can't be read: {e}"));
    let remaining: Vec<_> = findings
        .iter()
        .filter(|f| !f.disposition.is_acceptable_after_cleaning())
        .collect();
    assert!(
        remaining.is_empty(),
        "cleaned output still has {remaining:?}"
    );
    let again = clean_bytes(&cleaned.data, &options)
        .unwrap_or_else(|e| panic!("cleaned output can't be cleaned again: {e}"))
        .expect("cleaned output is no longer recognized");
    assert_eq!(
        again.data, cleaned.data,
        "cleaning twice changed the output"
    );
});
