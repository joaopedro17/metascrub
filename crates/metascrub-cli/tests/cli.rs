#![allow(clippy::unwrap_used)] // In tests, a panic is a test failure.

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
fn folders_are_rejected_as_invalid_usage() {
    let output = metascrub(&["inspect", "fixtures/generated"]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("is a folder"), "{stderr}");
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
