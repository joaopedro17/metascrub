//! What the file system keeps around files: download addresses, folder
//! dates. Tests that need extended attributes are skipped where the
//! temporary folder doesn't allow them, unless METASCRUB_REQUIRE_FS_ATTRIBUTES
//! is set (as in CI).
#![allow(clippy::unwrap_used, clippy::indexing_slicing)] // In tests, a panic is a test failure.

use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

const URL: &str = "https://example.com/notes.txt";

fn metascrub(args: &[&OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_metascrub"))
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Records `URL` the way a browser on this system does.
fn mark_downloaded(path: &Path) -> bool {
    #[cfg(unix)]
    let marked = xattr::set(path, "user.xdg.origin.url", URL.as_bytes()).is_ok();
    #[cfg(windows)]
    let marked = {
        let mut stream = path.as_os_str().to_owned();
        stream.push(":Zone.Identifier");
        fs::write(
            stream,
            format!("[ZoneTransfer]\r\nZoneId=3\r\nHostUrl={URL}\r\n"),
        )
        .is_ok()
    };
    let required =
        std::env::var_os("METASCRUB_REQUIRE_FS_ATTRIBUTES").is_some_and(|v| !v.is_empty());
    assert!(
        marked || !required,
        "this file system doesn't allow extended attributes"
    );
    marked
}

#[test]
fn download_addresses_are_reported_and_removed_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let notes = dir.path().join("notes.txt");
    fs::write(&notes, "hello").unwrap();
    if !mark_downloaded(&notes) {
        return;
    }

    let inspect = metascrub(&["inspect".as_ref(), "--show-values".as_ref(), notes.as_ref()]);
    let output = text(&inspect);
    assert!(
        output.contains("The web address it was downloaded from"),
        "{output}"
    );
    assert!(output.contains(URL), "{output}");

    let next_to = metascrub(&["clean".as_ref(), notes.as_ref()]);
    assert!(
        text(&next_to).contains("With --in-place or --output"),
        "{next_to:?}"
    );

    let verify = metascrub(&["verify".as_ref(), notes.as_ref()]);
    assert_eq!(verify.status.code(), Some(4), "{verify:?}");

    let clean = metascrub(&["clean".as_ref(), "--in-place".as_ref(), notes.as_ref()]);
    assert_eq!(clean.status.code(), Some(0), "{clean:?}");
    let verify = metascrub(&["verify".as_ref(), notes.as_ref()]);
    assert_eq!(verify.status.code(), Some(0), "{verify:?}");
    assert_eq!(fs::read_to_string(&notes).unwrap(), "hello");
}

#[test]
fn remove_junk_needs_a_mode_that_may_change_folders() {
    let dir = tempfile::tempdir().unwrap();
    let output = metascrub(&[
        "clean".as_ref(),
        "-r".as_ref(),
        "--remove-junk".as_ref(),
        dir.path().as_ref(),
    ]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--in-place"), "{stderr}");
}

#[test]
fn junk_files_are_reported_and_removed_when_asked() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("photos");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join(".DS_Store"), "Bud1").unwrap();
    fs::write(folder.join("Thumbs.db"), "x").unwrap();

    let inspect = metascrub(&["inspect".as_ref(), "-r".as_ref(), folder.as_ref()]);
    assert!(text(&inspect).contains("A junk file"), "{inspect:?}");
    assert!(
        text(&inspect).contains("left alone: --remove-junk removes it"),
        "{inspect:?}"
    );

    let clean = metascrub(&[
        "clean".as_ref(),
        "-r".as_ref(),
        "--in-place".as_ref(),
        "--remove-junk".as_ref(),
        folder.as_ref(),
    ]);
    assert_eq!(clean.status.code(), Some(0), "{clean:?}");
    assert!(text(&clean).contains("2 junk files removed"), "{clean:?}");
    assert_eq!(fs::read_dir(&folder).unwrap().count(), 0);
}

#[test]
fn leaking_names_are_replaced_with_rename() {
    let dir = tempfile::tempdir().unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/generated");
    let photo = dir.path().join("IMG_20261102_143512.jpg");
    fs::copy(fixtures.join("jpeg/camera.jpg"), &photo).unwrap();

    let clean = metascrub(&["clean".as_ref(), photo.as_ref()]);
    assert!(
        text(&clean).contains("left alone: --rename replaces it"),
        "{clean:?}"
    );

    let out = dir.path().join("out");
    let clean = metascrub(&[
        "clean".as_ref(),
        "--rename".as_ref(),
        "-o".as_ref(),
        out.as_ref(),
        photo.as_ref(),
    ]);
    assert_eq!(clean.status.code(), Some(0), "{clean:?}");
    assert!(out.join("image-001.jpg").is_file(), "{clean:?}");
    assert!(photo.is_file(), "the original stays");
}
