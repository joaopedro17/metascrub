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
    for dir in ["jpeg", "png", "webp", "office", "pdf", "audio"] {
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
        // The form is kept (its values are only reported).
        "form.pdf" => &["[PDF]HasXFA"],
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
    // A PDF shows its version and page count.
    let base_pdf = exiftool_tags(&fixture("pdf/clean.pdf")).unwrap();
    // An MP3 shows its stream: version, bitrate, channels.
    let base_mp3 = exiftool_tags(&fixture("audio/clean.mp3")).unwrap();
    let base_flac = exiftool_tags(&fixture("audio/clean.flac")).unwrap();
    let base_ogg = exiftool_tags(&fixture("audio/clean.ogg")).unwrap();
    let base_opus = exiftool_tags(&fixture("audio/clean.opus")).unwrap();
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
        } else if name.ends_with(".pdf") {
            &base_pdf
        } else if name.ends_with(".mp3") {
            &base_mp3
        } else if name.ends_with(".flac") {
            &base_flac
        } else if name.ends_with(".ogg") {
            &base_ogg
        } else if name.ends_with(".opus") {
            &base_opus
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
    let originals: Vec<PathBuf> = [
        "office/clean.docx",
        "office/report.docx",
        "office/budget.xlsx",
        "office/comments.docx",
        "office/comments.xlsx",
        "office/tracked-changes.docx",
        "office/linked.xlsx",
    ]
    .iter()
    .map(|f| fixture(f))
    .collect();
    let profile = format!(
        "-env:UserInstallation=file://{}",
        dir.path().join("profile").display()
    );
    // With comments removed (the default) and kept anonymized, and tracked
    // changes accepted and rejected.
    for (mode, extra) in [
        ("removed", ["--accept-changes"].as_slice()),
        ("kept", ["--keep=comments", "--reject-changes"].as_slice()),
    ] {
        let out = dir.path().join(mode);
        let mut args = vec![
            "clean".as_ref(),
            "-q".as_ref(),
            "-o".as_ref(),
            out.as_os_str(),
        ];
        args.extend(extra.iter().map(std::ffi::OsStr::new));
        args.extend(originals.iter().map(|p| p.as_os_str()));
        assert_eq!(metascrub(&args).status.code(), Some(0), "{mode}");

        let pdfs = dir.path().join("pdf").join(mode);
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
}

/// Cleaned PDFs must be valid (`qpdf --check`) and every page must still
/// render (`pdftoppm`). Skipped when the tools aren't installed, unless
/// METASCRUB_REQUIRE_PDF_TOOLS is set (as in CI on Linux).
#[test]
fn cleaned_pdfs_are_valid_and_render() {
    let required = std::env::var_os("METASCRUB_REQUIRE_PDF_TOOLS").is_some_and(|v| !v.is_empty());
    let installed = ["qpdf", "pdftoppm"]
        .iter()
        .all(|tool| Command::new(tool).arg("--version").output().is_ok());
    if !installed {
        assert!(!required, "qpdf and pdftoppm are required but couldn't run");
        eprintln!("qpdf or pdftoppm not installed; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let originals: Vec<PathBuf> = fs::read_dir(fixture("pdf"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| !p.ends_with("encrypted.pdf"))
        .collect();
    for (mode, extra) in [
        ("removed", [].as_slice()),
        ("kept", ["--keep=comments", "--keep=attachments"].as_slice()),
    ] {
        let out = dir.path().join(mode);
        let mut args = vec![
            "clean".as_ref(),
            "-q".as_ref(),
            "-o".as_ref(),
            out.as_os_str(),
        ];
        args.extend(extra.iter().map(std::ffi::OsStr::new));
        args.extend(originals.iter().map(|p| p.as_os_str()));
        assert_eq!(metascrub(&args).status.code(), Some(0), "{mode}");

        let mut checked = 0;
        for entry in fs::read_dir(&out).unwrap() {
            let path = entry.unwrap().path();
            let check = Command::new("qpdf")
                .arg("--check")
                .arg(&path)
                .output()
                .unwrap();
            let report = String::from_utf8_lossy(&check.stdout);
            assert!(
                check.status.success() && !report.contains("WARNING"),
                "{} isn't valid: {report}",
                path.display()
            );
            let image = dir.path().join("page");
            let render = Command::new("pdftoppm")
                .args(["-r", "20", "-png", "-singlefile"])
                .arg(&path)
                .arg(&image)
                .output()
                .unwrap();
            assert!(
                render.status.success(),
                "{} didn't render: {render:?}",
                path.display()
            );
            assert!(fs::metadata(image.with_extension("png")).unwrap().len() > 100);
            checked += 1;
        }
        assert_eq!(checked, originals.len(), "{mode}");
    }
}

#[test]
fn tracked_changes_need_a_decision() {
    let dir = tempfile::tempdir().unwrap();
    let original = fixture("office/tracked-changes.docx");
    let clean = |extra: &[&str]| {
        let out = dir
            .path()
            .join(extra.first().unwrap_or(&"none").trim_start_matches('-'));
        let mut args = vec!["clean".as_ref(), "-o".as_ref(), out.as_os_str()];
        args.extend(extra.iter().map(std::ffi::OsStr::new));
        args.push(original.as_os_str());
        (metascrub(&args), out)
    };

    let (output, out) = clean(&[]);
    assert_eq!(output.status.code(), Some(3));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("needs your decision"), "{text}");
    assert!(text.contains("--accept-changes"), "{text}");
    // Nothing was written, so nothing reads as already removed or failed.
    assert!(text.contains("— will be removed"), "{text}");
    assert!(!text.contains("— removed"), "{text}");
    assert!(
        text.contains("0 files cleaned, 1 needs your decision"),
        "{text}"
    );
    assert!(!out.exists());

    for (flag, kept, gone) in [
        ("--accept-changes", "€25,000", "€40,000"),
        ("--reject-changes", "€40,000", "€25,000"),
    ] {
        let (output, out) = clean(&[flag]);
        assert_eq!(output.status.code(), Some(0), "{flag}");
        let cleaned = out.join("tracked-changes.docx");
        let verify = metascrub(&["verify".as_ref(), cleaned.as_os_str()]);
        assert_eq!(verify.status.code(), Some(0), "{flag}");
        let mut zip = zip::ZipArchive::new(fs::File::open(&cleaned).unwrap()).unwrap();
        let mut document = String::new();
        std::io::Read::read_to_string(
            &mut zip.by_name("word/document.xml").unwrap(),
            &mut document,
        )
        .unwrap();
        assert!(
            document.contains(kept) && !document.contains(gone),
            "{flag}: {document}"
        );
        assert!(!document.contains("Jane Doe") && !document.contains("John Roe"));
    }

    let both = metascrub(&[
        "clean".as_ref(),
        "--accept-changes".as_ref(),
        "--reject-changes".as_ref(),
        original.as_os_str(),
    ]);
    assert_eq!(both.status.code(), Some(2));
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
    let output = metascrub(&[
        "verify".as_ref(),
        dir.path().join("camera.clean.jpg").as_os_str(),
    ]);
    assert_eq!(output.status.code(), Some(0));
    // No hints about values or options for a report that lists nothing.
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(text.lines().filter(|l| !l.is_empty()).count(), 2, "{text}");
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

/// Cleaned archives must still extract with the system's own tools, with
/// their files intact and the junk gone. Skipped when `unzip` or `tar`
/// isn't installed, unless METASCRUB_REQUIRE_ARCHIVE_TOOLS is set (as in CI
/// on Linux).
#[test]
fn cleaned_archives_still_extract() {
    let required =
        std::env::var_os("METASCRUB_REQUIRE_ARCHIVE_TOOLS").is_some_and(|v| !v.is_empty());
    let installed = ["unzip", "tar"]
        .iter()
        .all(|tool| Command::new(tool).arg("--help").output().is_ok());
    if !installed {
        assert!(!required, "unzip and tar are required but couldn't run");
        eprintln!("unzip or tar not installed; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let names = ["photos.zip", "photos.tar", "photos.tar.gz"];
    let mut args = vec![
        "clean".as_ref(),
        "-q".as_ref(),
        "-o".as_ref(),
        out.as_os_str(),
    ];
    let originals: Vec<_> = names
        .iter()
        .map(|n| fixture(&format!("archive/{n}")))
        .collect();
    args.extend(originals.iter().map(|p| p.as_os_str()));
    assert_eq!(metascrub(&args).status.code(), Some(0));

    for name in names {
        let archive = out.join(name);
        let into = dir.path().join(format!("{name}.extracted"));
        fs::create_dir_all(&into).unwrap();
        let extract = if name.ends_with(".zip") {
            Command::new("unzip")
                .arg("-q")
                .arg(&archive)
                .arg("-d")
                .arg(&into)
                .output()
        } else {
            Command::new("tar")
                .arg("-xf")
                .arg(&archive)
                .arg("-C")
                .arg(&into)
                .output()
        }
        .unwrap();
        assert!(extract.status.success(), "{name}: {extract:?}");

        let holiday = into.join("Holiday");
        assert_eq!(
            fs::read_to_string(holiday.join("notes.txt")).unwrap(),
            "Remember the charger.\n",
            "{name}"
        );
        for file in ["IMG_20260314_092652.jpg", "itinerary.docx", "upload.sh"] {
            assert!(holiday.join(file).is_file(), "{name}: {file}");
        }
        assert!(!into.join("__MACOSX").exists(), "{name}");
        assert!(!holiday.join(".DS_Store").exists(), "{name}");
        assert!(!holiday.join("._notes.txt").exists(), "{name}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |file: &str| {
                fs::metadata(holiday.join(file))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777
            };
            assert_eq!(mode("upload.sh") & 0o100, 0o100, "{name}: still a program");
            let link = fs::read_link(holiday.join("latest.jpg")).unwrap();
            assert_eq!(link, Path::new("IMG_20260314_092652.jpg"), "{name}");
        }
    }
    let verify = metascrub(&["verify".as_ref(), "-r".as_ref(), out.as_os_str()]);
    assert_eq!(verify.status.code(), Some(0), "{verify:?}");
}

fn media_fixtures() -> Vec<PathBuf> {
    let mut files: Vec<_> = fs::read_dir(fixture("media"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    files.sort();
    files
}

fn clean_media_into(out: &Path) -> Vec<PathBuf> {
    let originals = media_fixtures();
    let mut args = vec![
        "clean".as_ref(),
        "-q".as_ref(),
        "-o".as_ref(),
        out.as_os_str(),
    ];
    args.extend(originals.iter().map(|p| p.as_os_str()));
    let output = metascrub(&args);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    originals
}

/// Cleaned videos and music must still play to the end, and photos still
/// decode, with nothing moved: only trailing data makes a file shorter.
/// Skipped when ffmpeg or heif-convert isn't installed, unless
/// METASCRUB_REQUIRE_MEDIA_TOOLS is set (as in CI on Linux).
#[test]
fn cleaned_media_still_play() {
    let required = std::env::var_os("METASCRUB_REQUIRE_MEDIA_TOOLS").is_some_and(|v| !v.is_empty());
    let installed = ["ffmpeg", "heif-convert"]
        .iter()
        .all(|tool| Command::new(tool).arg("-version").output().is_ok());
    if !installed {
        assert!(
            !required,
            "ffmpeg and heif-convert are required but couldn't run"
        );
        eprintln!("ffmpeg or heif-convert not installed; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let originals = clean_media_into(&out);
    assert!(originals.len() >= 8);
    for original in originals {
        let name = original.file_name().unwrap();
        let cleaned = out.join(name);
        let name = name.to_string_lossy();
        let shorter = if name == "trailing.mp4" { 24 } else { 0 };
        assert_eq!(
            fs::metadata(&cleaned).unwrap().len() + shorter,
            fs::metadata(&original).unwrap().len(),
            "{name}"
        );
        let decoded = if name.ends_with(".heic") || name.ends_with(".avif") {
            Command::new("heif-convert")
                .arg(&cleaned)
                .arg(dir.path().join(format!("{name}.png")))
                .output()
        } else {
            Command::new("ffmpeg")
                .args(["-v", "error", "-xerror", "-i"])
                .arg(&cleaned)
                .args(["-f", "null", "-"])
                .output()
        }
        .unwrap();
        assert!(
            decoded.status.success() && decoded.stderr.is_empty(),
            "{name}: {}",
            String::from_utf8_lossy(&decoded.stderr)
        );
    }
    let verify = metascrub(&["verify".as_ref(), "-r".as_ref(), out.as_os_str()]);
    assert_eq!(verify.status.code(), Some(0), "{verify:?}");
}

/// Cleaned music must sound exactly the same: ffmpeg decodes the original
/// and the cleaned file to the same samples. Skipped like the test above.
#[test]
fn cleaned_music_sounds_the_same() {
    let required = std::env::var_os("METASCRUB_REQUIRE_MEDIA_TOOLS").is_some_and(|v| !v.is_empty());
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        assert!(!required, "ffmpeg is required but couldn't run");
        eprintln!("ffmpeg not installed; skipping");
        return;
    }
    let samples = |path: &Path| {
        let output = Command::new("ffmpeg")
            .args(["-v", "quiet", "-i"])
            .arg(path)
            .args(["-map", "0:a", "-f", "s16le", "-"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", path.display());
        output.stdout
    };
    let out = tempfile::tempdir().unwrap();
    let mut checked = 0;
    for entry in fs::read_dir(fixture("audio")).unwrap() {
        let original = entry.unwrap().path();
        let name = original.file_name().unwrap();
        let output = metascrub(&[
            "clean".as_ref(),
            "-q".as_ref(),
            "-o".as_ref(),
            out.path().as_os_str(),
            original.as_os_str(),
        ]);
        let cleaned = out.path().join(name);
        if !cleaned.exists() {
            // Refused files (data hidden between frames) are never written.
            assert_ne!(output.status.code(), Some(0), "{}", original.display());
            continue;
        }
        let original_samples = samples(&original);
        assert!(!original_samples.is_empty());
        assert!(
            samples(&cleaned) == original_samples,
            "{} sounds different after cleaning",
            name.to_string_lossy()
        );
        checked += 1;
    }
    assert!(checked >= 13, "only {checked} music files were checked");
}

/// What exiftool, reading with its own parser, finds in a video or photo
/// beyond its structure (sizes, codecs, tracks), zeroed dates and the
/// orientation MetaScrub keeps.
fn exiftool_media_leftovers(path: &Path) -> Vec<String> {
    const GROUPS: &[&str] = &[
        "[Keys]",
        "[ItemList]",
        "[UserData]",
        "[GPS]",
        "[XMP",
        "[ExifIFD]",
        "[IFD1]",
        "[Composite]GPS",
    ];
    const TAGS: &[&str] = &["VendorID", "Encoder", "Software", "Make", "Model", "Artist"];
    let output = Command::new("exiftool")
        .args(["-G1", "-a", "-u", "-s", "-q"])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| {
            let (tag, value) = line.split_once(':').unwrap_or((line, ""));
            let tag: String = tag.split_whitespace().collect();
            let value = value.trim();
            let name = tag.rsplit(']').next().unwrap_or_default();
            let named = GROUPS.iter().any(|g| tag.starts_with(g))
                || TAGS.contains(&name)
                || (name == "CompressorName" && !value.is_empty());
            let dated = tag.contains("Date")
                && !tag.starts_with("[System]")
                && value != "0000:00:00 00:00:00";
            named || dated
        })
        .map(str::to_owned)
        .collect()
}

#[test]
fn exiftool_finds_no_media_metadata() {
    if exiftool_tags(&fixture("jpeg/clean.jpg")).is_none() {
        eprintln!("exiftool not installed; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    for original in clean_media_into(dir.path()) {
        let name = original.file_name().unwrap();
        let found = exiftool_media_leftovers(&original);
        let clean = name.to_string_lossy().starts_with("clean.") || name == "trailing.mp4";
        assert_eq!(
            found.is_empty(),
            clean,
            "{name:?}: the oracle sees {found:?}"
        );
        let left = exiftool_media_leftovers(&dir.path().join(name));
        assert!(left.is_empty(), "{name:?}: exiftool still finds {left:?}");
    }
}
