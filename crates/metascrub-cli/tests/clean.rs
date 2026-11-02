//! `clean` and `verify` on real files, checked against exiftool as an
//! independent oracle. Locally the exiftool tests are skipped when it isn't
//! installed; CI sets METASCRUB_REQUIRE_EXIFTOOL so they can't be skipped there.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // In tests, a panic is a test failure.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture(relative: &str) -> PathBuf {
    root().join("fixtures/generated").join(relative)
}

fn metascrub(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_metascrub"))
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

fn clean_all_into(out: &Path) -> Vec<PathBuf> {
    let mut fixtures = Vec::new();
    for dir in ["jpeg", "png", "webp", "office"] {
        for entry in fs::read_dir(fixture(dir)).unwrap() {
            fixtures.push(entry.unwrap().path());
        }
    }
    fixtures.sort();
    let mut args = vec![
        "clean".as_ref(),
        "-q".as_ref(),
        "-o".as_ref(),
        out.as_os_str(),
    ];
    args.extend(fixtures.iter().map(|p| p.as_os_str()));
    metascrub(&args);
    fixtures
}

/// `Group:Tag` names exiftool reports, ignoring values.
fn exiftool_tags(path: &Path) -> Option<BTreeSet<String>> {
    let output = Command::new("exiftool")
        .args(["-G1", "-a", "-u", "-e", "-s", "-q"])
        .arg(path)
        .output();
    let output = match output {
        Ok(output) => output,
        Err(_) if std::env::var_os("METASCRUB_REQUIRE_EXIFTOOL").is_none() => return None,
        Err(e) => panic!("exiftool is required but couldn't run: {e}"),
    };
    let text = String::from_utf8_lossy(&output.stdout);
    Some(
        text.lines()
            .filter_map(|line| {
                let mut words = line.split_whitespace();
                Some(format!("{}{}", words.next()?, words.next()?))
            })
            .collect(),
    )
}

/// Tags exiftool may still see after cleaning, beyond those of the
/// metadata-free base image: only what MetaScrub deliberately keeps.
/// `[RIFF]` tags are the WebP canvas size, feature flags and animation
/// settings, which the extended format needs.
fn allowed_extras(name: &str) -> &'static [&'static str] {
    const ORIENTATION: &[&str] = &["[File]ExifByteOrder", "[IFD0]Orientation"];
    const ICC: &[&str] = &["[ICC-header]", "[ICC_Profile]", "[PNG]ProfileName"];
    const WEBP_ORIENTATION: &[&str] = &["[RIFF]", "[File]ExifByteOrder", "[IFD0]Orientation"];
    const WEBP_ICC: &[&str] = &["[RIFF]", "[ICC-header]", "[ICC_Profile]"];
    match name {
        "camera.jpg" | "exif.png" => ORIENTATION,
        "icc-standard.jpg" | "icc.png" => ICC,
        "camera.webp" => WEBP_ORIENTATION,
        "icc.webp" => WEBP_ICC,
        _ if name.ends_with(".webp") => &["[RIFF]"],
        _ => &[],
    }
}

#[test]
fn exiftool_finds_nothing_left_but_what_is_kept() {
    let Some(base_jpeg) = exiftool_tags(&fixture("jpeg/clean.jpg")) else {
        eprintln!("exiftool not installed; skipping");
        return;
    };
    let base_png = exiftool_tags(&fixture("png/clean.png")).unwrap();
    let base_webp = exiftool_tags(&fixture("webp/clean.webp")).unwrap();
    // Every Office file shows its ZIP structure: entry names, sizes, dates.
    let base_office = exiftool_tags(&fixture("office/clean.docx")).unwrap();
    // Sanity check that the oracle sees what we planted.
    let original = exiftool_tags(&fixture("jpeg/camera.jpg")).unwrap();
    assert!(
        original.iter().any(|t| t.starts_with("[GPS]")),
        "{original:?}"
    );

    let out = tempfile::tempdir().unwrap();
    clean_all_into(out.path());
    let mut checked = 0;
    for entry in fs::read_dir(out.path()).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let base = if name.ends_with(".docx") || name.ends_with(".xlsx") {
            &base_office
        } else if name.ends_with(".webp") {
            &base_webp
        } else if name.ends_with(".png") && name != "jpeg-named.png" {
            &base_png
        } else {
            &base_jpeg
        };
        let extras: Vec<_> = exiftool_tags(&path)
            .unwrap()
            .difference(base)
            .filter(|tag| !allowed_extras(&name).iter().any(|a| tag.starts_with(a)))
            .cloned()
            .collect();
        assert!(extras.is_empty(), "{name}: exiftool still finds {extras:?}");
        checked += 1;
    }
    assert!(checked >= 20, "only {checked} files were cleaned");
}

/// Cleaned Office files must still open: LibreOffice converts each to PDF.
/// Skipped when LibreOffice isn't installed, unless
/// METASCRUB_REQUIRE_LIBREOFFICE is set (as in CI on Linux).
#[test]
fn cleaned_office_files_still_open() {
    let required = std::env::var_os("METASCRUB_REQUIRE_LIBREOFFICE").is_some_and(|v| !v.is_empty());
    if Command::new("soffice").arg("--version").output().is_err() {
        assert!(
            !required,
            "LibreOffice (soffice) is required but couldn't run"
        );
        eprintln!("LibreOffice not installed; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let mut args = vec![
        "clean".as_ref(),
        "-q".as_ref(),
        "-o".as_ref(),
        out.as_os_str(),
    ];
    let originals: Vec<PathBuf> = [
        "office/clean.docx",
        "office/report.docx",
        "office/budget.xlsx",
    ]
    .iter()
    .map(|f| fixture(f))
    .collect();
    args.extend(originals.iter().map(|p| p.as_os_str()));
    assert_eq!(metascrub(&args).status.code(), Some(0));

    let pdfs = dir.path().join("pdf");
    let profile = format!(
        "-env:UserInstallation=file://{}",
        dir.path().join("profile").display()
    );
    for entry in fs::read_dir(&out).unwrap() {
        let path = entry.unwrap().path();
        let status = Command::new("soffice")
            .args([
                profile.as_str(),
                "--headless",
                "--convert-to",
                "pdf",
                "--outdir",
            ])
            .arg(&pdfs)
            .arg(&path)
            .output()
            .unwrap();
        let pdf = pdfs.join(path.with_extension("pdf").file_name().unwrap());
        let size = fs::metadata(&pdf).map(|m| m.len()).unwrap_or(0);
        assert!(size > 1000, "{} didn't open: {status:?}", path.display());
    }
}

#[test]
fn originals_are_never_modified() {
    let dir = tempfile::tempdir().unwrap();
    let copies: Vec<PathBuf> = ["jpeg/camera.jpg", "png/text-chunks.png", "other/notes.txt"]
        .iter()
        .map(|relative| {
            let copy = dir.path().join(Path::new(relative).file_name().unwrap());
            fs::copy(fixture(relative), &copy).unwrap();
            copy
        })
        .collect();
    let before: Vec<_> = copies.iter().map(|p| fs::read(p).unwrap()).collect();

    let out = dir.path().join("out");
    let mut args = vec!["clean".as_ref(), "-o".as_ref(), out.as_os_str()];
    args.extend(copies.iter().map(|p| p.as_os_str()));
    metascrub(&args);
    let mut next_to: Vec<_> = vec!["clean".as_ref()];
    next_to.extend(copies.iter().map(|p| p.as_os_str()));
    metascrub(&next_to);

    let after: Vec<_> = copies.iter().map(|p| fs::read(p).unwrap()).collect();
    assert!(before == after);
}

#[test]
fn verify_exits_4_until_the_file_is_cleaned() {
    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("camera.jpg");
    fs::copy(fixture("jpeg/camera.jpg"), &original).unwrap();

    let verify = |path: &Path| {
        metascrub(&["verify".as_ref(), path.as_os_str()])
            .status
            .code()
    };
    assert_eq!(verify(&original), Some(4));
    assert_eq!(
        metascrub(&["clean".as_ref(), original.as_os_str()])
            .status
            .code(),
        Some(0)
    );
    assert_eq!(verify(&dir.path().join("camera.clean.jpg")), Some(0));
}

#[test]
fn output_and_in_place_conflict() {
    let output = metascrub(&[
        "clean".as_ref(),
        "-o".as_ref(),
        "out".as_ref(),
        "--in-place".as_ref(),
        fixture("jpeg/camera.jpg").as_os_str(),
    ]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn in_place_cleans_the_original() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("photo.png");
    fs::copy(fixture("png/text-chunks.png"), &file).unwrap();
    let output = metascrub(&["clean".as_ref(), "--in-place".as_ref(), file.as_os_str()]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        metascrub(&["verify".as_ref(), file.as_os_str()])
            .status
            .code(),
        Some(0)
    );
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn dry_run_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("camera.jpg");
    fs::copy(fixture("jpeg/camera.jpg"), &file).unwrap();
    let output = metascrub(&["clean".as_ref(), "--dry-run".as_ref(), file.as_os_str()]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}
