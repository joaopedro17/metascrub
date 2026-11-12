//! Runs the real binary on the fixtures and compares its output with the
//! expected files in `tests/golden/`. After an intended change, regenerate them:
//! `METASCRUB_BLESS=1 cargo test -p metascrub --test golden`

#![allow(clippy::unwrap_used)] // In tests, a panic is a test failure.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every generated fixture, as workspace-relative paths with `/` separators
/// so the output is identical on every OS.
fn all_fixtures() -> Vec<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                let relative = path.strip_prefix(root).unwrap();
                let parts: Vec<_> = relative
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                out.push(parts.join("/"));
            }
        }
    }
    let root = workspace_root();
    let mut fixtures = Vec::new();
    walk(&root.join("fixtures/generated"), &root, &mut fixtures);
    fixtures.sort();
    fixtures
}

fn run(args: &[String]) -> (i32, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_metascrub"))
        .args(args)
        .current_dir(workspace_root())
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8(output.stdout).unwrap(),
    )
}

fn check(name: &str, args: &[&str], with_all_fixtures: bool) {
    let mut args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    if with_all_fixtures {
        args.extend(all_fixtures());
    }
    let (code, stdout) = run(&args);
    let actual = format!("$ metascrub {}\nexit: {code}\n\n{stdout}", args.join(" "));

    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.txt"));
    if std::env::var_os("METASCRUB_BLESS").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = fs::read_to_string(&path).unwrap_or_default();
    assert!(
        expected == actual,
        "output of `{name}` changed; if intended, re-run with METASCRUB_BLESS=1\n\n\
         --- expected\n{expected}\n--- actual\n{actual}"
    );
}

#[test]
fn inspect_all_fixtures_human() {
    check("inspect-all", &["inspect"], true);
}

#[test]
fn inspect_all_fixtures_verbose_with_values() {
    check(
        "inspect-all-verbose",
        &["inspect", "-v", "--show-values"],
        true,
    );
}

#[test]
fn inspect_all_fixtures_json() {
    check(
        "inspect-all-json",
        &["inspect", "--json", "--show-values"],
        true,
    );
}

#[test]
fn json_hides_values_by_default() {
    check(
        "inspect-camera-json",
        &["inspect", "--json", "fixtures/generated/jpeg/camera.jpg"],
        false,
    );
}

#[test]
fn quiet_shows_only_problems_and_summary() {
    check("inspect-all-quiet", &["inspect", "-q"], true);
}

#[test]
fn clean_dry_run_all_fixtures() {
    check("clean-dry-run-all", &["clean", "--dry-run"], true);
}

#[test]
fn verify_all_fixtures() {
    check("verify-all", &["verify"], true);
}

#[test]
fn checksums_when_asked() {
    check(
        "inspect-camera-checksums",
        &[
            "inspect",
            "--checksums",
            "fixtures/generated/jpeg/camera.jpg",
            "fixtures/generated/other/notes.txt",
        ],
        false,
    );
    check(
        "inspect-camera-checksums-json",
        &[
            "inspect",
            "--json",
            "--checksums",
            "fixtures/generated/jpeg/camera.jpg",
        ],
        false,
    );
}
