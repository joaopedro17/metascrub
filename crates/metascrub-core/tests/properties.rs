//! Properties every cleaner must have, checked on every fixture.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // In tests, a panic is a test failure.

use std::fs;
use std::path::{Path, PathBuf};

use metascrub_core::Options;
use metascrub_core::format::{Cleaned, clean_bytes, inspect_bytes};

/// Every fixture that a cleaner accepts, with its contents.
fn cleanable_fixtures() -> Vec<(PathBuf, Vec<u8>)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push(path);
            }
        }
    }
    let mut paths = Vec::new();
    walk(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/generated"),
        &mut paths,
    );
    paths.sort();
    let fixtures: Vec<_> = paths
        .into_iter()
        .map(|path| {
            let data = fs::read(&path).unwrap();
            (path, data)
        })
        .filter(|(_, data)| matches!(clean_bytes(data, &Options::default()), Ok(Some(_))))
        .collect();
    assert!(fixtures.len() >= 10, "fixtures missing?");
    fixtures
}

fn clean(data: &[u8], options: &Options) -> Cleaned {
    clean_bytes(data, options).unwrap().unwrap()
}

fn option_sets() -> [Options; 3] {
    [
        Options::default(),
        Options {
            keep_color_profile: true,
            ..Options::default()
        },
        Options {
            strip_orientation: true,
            ..Options::default()
        },
    ]
}

#[test]
fn cleaning_is_deterministic() {
    for (path, data) in cleanable_fixtures() {
        for options in option_sets() {
            assert_eq!(
                clean(&data, &options),
                clean(&data, &options),
                "{}",
                path.display()
            );
        }
    }
}

#[test]
fn cleaning_a_cleaned_file_changes_nothing() {
    for (path, data) in cleanable_fixtures() {
        for options in option_sets() {
            let once = clean(&data, &options).data;
            let twice = clean(&once, &options).data;
            assert_eq!(once, twice, "{}", path.display());
        }
    }
}

#[test]
fn cleaned_files_pass_verification() {
    for (path, data) in cleanable_fixtures() {
        for options in option_sets() {
            let cleaned = clean(&data, &options).data;
            let (_, findings) = inspect_bytes(&cleaned, &options).unwrap();
            let remaining: Vec<_> = findings
                .iter()
                .filter(|f| !f.disposition.is_acceptable_after_cleaning())
                .collect();
            assert!(remaining.is_empty(), "{}: {remaining:?}", path.display());
        }
    }
}

#[test]
fn cleaning_reports_what_inspect_reports() {
    for (path, data) in cleanable_fixtures() {
        for options in option_sets() {
            let (_, inspected) = inspect_bytes(&data, &options).unwrap();
            assert_eq!(
                clean(&data, &options).findings,
                inspected,
                "{}",
                path.display()
            );
        }
    }
}

#[test]
fn cleaning_never_changes_the_pixels() {
    for (path, data) in cleanable_fixtures() {
        let cleaned = clean(&data, &Options::default()).data;
        let before = image::load_from_memory(&data)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
            .to_rgba8();
        let after = image::load_from_memory(&cleaned)
            .unwrap_or_else(|e| panic!("{} (cleaned): {e}", path.display()))
            .to_rgba8();
        assert!(before == after, "{}: pixels changed", path.display());
    }
}
