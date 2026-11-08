//! The diagnostic log: written only when asked, and never holding what
//! MetaScrub removes (requirements §14.6).

#![allow(clippy::unwrap_used)] // In tests, a panic is a test failure.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Every value `inspect --show-values` reports for `camera.jpg`.
const CAMERA_VALUES: &[&str] = &[
    "Fictional Camera Co.",
    "FC-100",
    "PhotoTool 2.1",
    "2026:03:14",
    "Jane Doe",
    "SN-0042-7781",
    "FC 24-70mm",
    "48.8584",
    "2.2945",
];

/// A folder with a copy of `camera.jpg` in `photos/`.
fn folder() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let photos = dir.path().join("photos");
    fs::create_dir(&photos).unwrap();
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/generated/jpeg/camera.jpg");
    fs::copy(fixture, photos.join("camera.jpg")).unwrap();
    (dir, photos)
}

fn metascrub(dir: &Path, args: &[&str], log_level: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_metascrub"));
    command
        .args(args)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .env_remove("METASCRUB_LOG");
    if let Some(level) = log_level {
        command.env("METASCRUB_LOG", level);
    }
    command.output().unwrap()
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn nothing_is_logged_unless_asked() {
    let (dir, photos) = folder();
    let output = metascrub(dir.path(), &["clean", "-r", "-q", "photos"], None);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(names(&photos), ["camera.clean.jpg", "camera.jpg"]);
    assert_eq!(names(dir.path()), ["photos"]);
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_log_holds_no_values_and_no_names() {
    let (dir, _) = folder();
    let output = metascrub(
        dir.path(),
        &[
            "clean", "-r", "-q", "photos", "-o", "out", "--log", "run.log",
        ],
        None,
    );
    assert_eq!(output.status.code(), Some(0));
    let log = fs::read_to_string(dir.path().join("run.log")).unwrap();
    assert!(log.contains("file 2: Cleaned [Jpeg"), "{log}");
    assert!(log.contains("self-check passed"), "{log}");
    for secret in CAMERA_VALUES.iter().chain(&["camera", "photos"]) {
        assert!(!log.contains(secret), "the log contains {secret:?}:\n{log}");
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Diagnostic log written to run.log"),
        "{stderr}"
    );
}

#[test]
fn names_only_with_show_values_and_values_never() {
    let (dir, _) = folder();
    let output = metascrub(
        dir.path(),
        &[
            "inspect",
            "-r",
            "-q",
            "--show-values",
            "photos",
            "--log",
            "run.log",
        ],
        None,
    );
    assert_eq!(output.status.code(), Some(0));
    let log = fs::read_to_string(dir.path().join("run.log")).unwrap();
    assert!(log.contains("camera.jpg"), "{log}");
    for secret in CAMERA_VALUES {
        assert!(!log.contains(secret), "the log contains {secret:?}:\n{log}");
    }
}

#[test]
fn an_existing_log_is_kept_and_the_log_itself_is_not_cleaned() {
    let (dir, photos) = folder();
    fs::write(photos.join("run.log"), "an earlier log").unwrap();
    let output = metascrub(
        dir.path(),
        &[
            "clean",
            "-r",
            "-q",
            "photos",
            "-o",
            "out",
            "--log",
            "photos/run.log",
        ],
        None,
    );
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        fs::read_to_string(photos.join("run.log")).unwrap(),
        "an earlier log"
    );
    assert!(photos.join("run-2.log").is_file());
    // The earlier log is an ordinary file, copied; the new one isn't.
    assert_eq!(
        names(&dir.path().join("out/photos")),
        ["camera.jpg", "run.log"]
    );
}

#[test]
fn the_environment_variable_prints_to_the_terminal() {
    let (dir, _) = folder();
    let output = metascrub(
        dir.path(),
        &["inspect", "-r", "-q", "photos"],
        Some("debug"),
    );
    assert_eq!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("DEBUG") && stderr.contains("file 2: Jpeg"),
        "{stderr}"
    );
    assert_eq!(names(dir.path()), ["photos"]);

    let output = metascrub(dir.path(), &["inspect", "photos/camera.jpg"], Some("loud"));
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("METASCRUB_LOG must be one of"), "{stderr}");
}
