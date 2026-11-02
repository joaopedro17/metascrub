//! Writing output files: never over an existing file unless replacing in
//! place was asked for, and never half-written. Everything is written to a
//! temporary file in the destination folder first, then renamed.

use std::fs::{self, File, FileTimes, Permissions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use tempfile::NamedTempFile;

use crate::format::FormatError;

/// 1980-01-01T00:00:00Z, the neutral date given to output files (requirements §13.3).
const NEUTRAL_TIME: Duration = Duration::from_secs(315_532_800);

const MAX_NAME_ATTEMPTS: u32 = 10_000;

#[derive(Debug, Clone)]
pub(crate) struct Settings {
    /// Copied from the original file, so the output has the same access rights.
    pub(crate) permissions: Permissions,
    pub(crate) neutral_times: bool,
}

/// Writes a new file at `target`, or `name-2.ext`, `name-3.ext`, … when that
/// name is taken. Returns the path written.
pub(crate) fn write_new(
    target: &Path,
    settings: &Settings,
    write: impl FnOnce(&mut dyn Write) -> Result<(), FormatError>,
) -> Result<PathBuf, FormatError> {
    let dir = parent(target);
    fs::create_dir_all(dir)?;
    let mut temp = prepare(dir, settings, write)?;
    for n in 1..=MAX_NAME_ATTEMPTS {
        let candidate = numbered(target, n);
        match temp.persist_noclobber(&candidate) {
            Ok(_) => return Ok(candidate),
            Err(e) if e.error.kind() == io::ErrorKind::AlreadyExists => temp = e.file,
            Err(e) => return Err(e.error.into()),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free file name for the output",
    )
    .into())
}

/// Replaces `original` atomically: readers see either the old file or the new one.
pub(crate) fn replace(
    original: &Path,
    settings: &Settings,
    write: impl FnOnce(&mut dyn Write) -> Result<(), FormatError>,
) -> Result<(), FormatError> {
    let temp = prepare(parent(original), settings, write)?;
    temp.persist(original).map_err(|e| e.error)?;
    Ok(())
}

fn prepare(
    dir: &Path,
    settings: &Settings,
    write: impl FnOnce(&mut dyn Write) -> Result<(), FormatError>,
) -> Result<NamedTempFile, FormatError> {
    let mut temp = tempfile::Builder::new()
        .prefix(".metascrub-")
        .suffix(".tmp")
        .tempfile_in(dir)?;
    {
        let mut out = BufWriter::new(temp.as_file_mut());
        write(&mut out)?;
        out.flush()?;
    }
    fs::set_permissions(temp.path(), settings.permissions.clone())?;
    // Last, so no later write changes the modification time again.
    if settings.neutral_times {
        set_neutral_times(temp.as_file())?;
    }
    Ok(temp)
}

fn set_neutral_times(file: &File) -> io::Result<()> {
    let time = UNIX_EPOCH + NEUTRAL_TIME;
    let times = FileTimes::new().set_accessed(time).set_modified(time);
    #[cfg(windows)]
    let times = {
        use std::os::windows::fs::FileTimesExt;
        times.set_created(time)
    };
    file.set_times(times)
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

/// `photo.clean.jpg` → `photo.clean-2.jpg` for n = 2.
fn numbered(target: &Path, n: u32) -> PathBuf {
    if n == 1 {
        return target.to_owned();
    }
    let mut name = target.file_stem().unwrap_or_default().to_os_string();
    name.push(format!("-{n}"));
    if let Some(extension) = target.extension() {
        name.push(".");
        name.push(extension);
    }
    target.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn settings(dir: &Path) -> Settings {
        let probe = dir.join("probe");
        fs::write(&probe, b"").unwrap();
        Settings {
            permissions: fs::metadata(&probe).unwrap().permissions(),
            neutral_times: true,
        }
    }

    fn write_text(text: &'static str) -> impl FnOnce(&mut dyn Write) -> Result<(), FormatError> {
        move |out| Ok(out.write_all(text.as_bytes())?)
    }

    #[test]
    fn numbers_names_before_the_extension() {
        assert_eq!(
            numbered(Path::new("a/photo.clean.jpg"), 2),
            Path::new("a/photo.clean-2.jpg")
        );
        assert_eq!(numbered(Path::new("notes"), 3), Path::new("notes-3"));
        assert_eq!(numbered(Path::new("x.png"), 1), Path::new("x.png"));
    }

    #[test]
    fn never_overwrites_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("photo.clean.jpg");
        fs::write(&target, "existing").unwrap();

        let written = write_new(&target, &settings(dir.path()), write_text("new")).unwrap();
        assert_eq!(written, dir.path().join("photo.clean-2.jpg"));
        assert_eq!(fs::read_to_string(&target).unwrap(), "existing");
        assert_eq!(fs::read_to_string(&written).unwrap(), "new");
    }

    #[test]
    fn creates_missing_output_folders() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out/nested/photo.jpg");
        let written = write_new(&target, &settings(dir.path()), write_text("x")).unwrap();
        assert_eq!(written, target);
    }

    #[test]
    fn a_failed_write_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let settings = settings(dir.path());
        let target = dir.path().join("photo.jpg");
        let result = write_new(&target, &settings, |_| {
            Err(FormatError::malformed("broken input"))
        });
        assert!(result.is_err());
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["probe"]);
    }

    #[test]
    fn output_files_get_the_neutral_date() {
        let dir = tempfile::tempdir().unwrap();
        let written = write_new(
            &dir.path().join("photo.jpg"),
            &settings(dir.path()),
            write_text("x"),
        )
        .unwrap();
        let modified = fs::metadata(written).unwrap().modified().unwrap();
        assert_eq!(modified, SystemTime::UNIX_EPOCH + NEUTRAL_TIME);
    }

    #[test]
    fn replace_swaps_the_content_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("photo.jpg");
        fs::write(&original, "old").unwrap();
        replace(&original, &settings(dir.path()), write_text("new")).unwrap();
        assert_eq!(fs::read_to_string(&original).unwrap(), "new");
    }

    #[cfg(unix)]
    #[test]
    fn keeps_the_original_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let settings = Settings {
            permissions: Permissions::from_mode(0o640),
            neutral_times: false,
        };
        let written = write_new(&dir.path().join("photo.jpg"), &settings, write_text("x")).unwrap();
        let mode = fs::metadata(written).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o640);
    }
}
