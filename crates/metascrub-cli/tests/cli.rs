#![allow(clippy::unwrap_used, clippy::indexing_slicing)] // In tests, a panic is a test failure.

use std::path::Path;
use std::process::{Command, Output};

fn metascrub(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_metascrub"))
        .args(args)
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

#[test]
fn folders_need_recursive() {
    let output = metascrub(&["inspect", "fixtures/generated"]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Use -r"), "{stderr}");
}

#[test]
fn cleans_a_folder_recursively_into_a_mirrored_output() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/generated");
    let dir = tempfile::tempdir().unwrap();
    let photos = dir.path().join("photos");
    std::fs::create_dir_all(photos.join("sub")).unwrap();
    for (from, to) in [
        ("jpeg/camera.jpg", "camera.jpg"),
        ("png/text-chunks.png", "sub/text-chunks.png"),
        ("other/notes.txt", "notes.txt"),
    ] {
        std::fs::copy(fixtures.join(from), photos.join(to)).unwrap();
    }
    let out = dir.path().join("out");

    let run = |args: &[&std::ffi::OsStr]| {
        Command::new(env!("CARGO_BIN_EXE_metascrub"))
            .args(args)
            .env("NO_COLOR", "1")
            .output()
            .unwrap()
    };
    let clean = run(&[
        "clean".as_ref(),
        "-r".as_ref(),
        photos.as_ref(),
        "-o".as_ref(),
        out.as_ref(),
    ]);
    assert_eq!(clean.status.code(), Some(0), "{clean:?}");
    for name in ["camera.jpg", "sub/text-chunks.png", "notes.txt"] {
        assert!(out.join("photos").join(name).is_file(), "{name}");
    }
    let verify = run(&["verify".as_ref(), "-r".as_ref(), out.as_ref()]);
    assert_eq!(verify.status.code(), Some(0), "{verify:?}");
}

#[test]
fn verbose_and_quiet_conflict() {
    let output = metascrub(&["inspect", "-v", "-q", "fixtures/generated/jpeg/clean.jpg"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn all_files_failing_exits_with_3() {
    let output = metascrub(&["inspect", "fixtures/generated/jpeg/truncated.jpg"]);
    assert_eq!(output.status.code(), Some(3));
}

#[test]
fn some_files_failing_exits_with_1() {
    let output = metascrub(&[
        "inspect",
        "fixtures/generated/jpeg/truncated.jpg",
        "fixtures/generated/jpeg/clean.jpg",
    ]);
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn skipped_files_are_not_failures() {
    let output = metascrub(&["inspect", "fixtures/generated/other/notes.txt"]);
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn inspect_never_modifies_files() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/generated/jpeg/camera.jpg");
    let before = std::fs::read(&path).unwrap();
    metascrub(&["inspect", "fixtures/generated/jpeg/camera.jpg"]);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

/// A PNG whose Author value tries to set the terminal title and clear the screen.
fn hostile_png() -> Vec<u8> {
    let base = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/generated/png/clean.png"),
    )
    .unwrap();
    let body = b"tEXtAuthor\0\x1b]0;pwned\x07\x1b[2J\x1b[31mfake";
    let mut crc = flate2::Crc::new();
    crc.update(body);
    let mut chunk = ((body.len() - 4) as u32).to_be_bytes().to_vec();
    chunk.extend(body);
    chunk.extend(crc.sum().to_be_bytes());
    [&base[..33], &chunk, &base[33..]].concat()
}

#[test]
fn file_contents_cannot_send_terminal_control_sequences() {
    let dir = tempfile::tempdir().unwrap();
    let mut names = vec!["hostile.png".to_owned()];
    // File names can carry them too (not allowed on Windows).
    if cfg!(unix) {
        names.push("a\x1b[2Jb.png".to_owned());
    }
    for name in names {
        let path = dir.path().join(&name);
        std::fs::write(&path, hostile_png()).unwrap();
        // Force colors as if writing to a real terminal, so nothing is stripped for us.
        let output = Command::new(env!("CARGO_BIN_EXE_metascrub"))
            .args(["inspect", "-v", "--show-values"])
            .arg(&path)
            .env_remove("NO_COLOR")
            .env("CLICOLOR_FORCE", "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("fake"), "{stdout}");
        assert!(!stdout.contains("\x1b]0;"), "{name}: title sequence leaked");
        assert!(
            !stdout.contains("\x1b[2J"),
            "{name}: clear-screen sequence leaked"
        );
        assert!(!stdout.contains('\x07'), "{name}: bell leaked");
    }
}
