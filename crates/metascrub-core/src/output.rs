//! Writing output files: never over an existing file unless replacing in
//! place was asked for, and never half-written. Everything is written to a
//! temporary file in the destination folder first, then renamed.

use std::collections::HashSet;
use std::fs::{self, File, FileTimes, OpenOptions, Permissions};
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use tempfile::{NamedTempFile, PersistError};

use crate::format::FormatError;

/// 1980-01-01T00:00:00Z, the neutral date given to output files (requirements §13.3).
pub(crate) const NEUTRAL_TIME: Duration = Duration::from_secs(315_532_800);

const MAX_NAME_ATTEMPTS: u32 = 10_000;

#[derive(Debug, Clone)]
pub(crate) struct Settings {
    /// Copied from the original file, so the output has the same access rights.
    pub(crate) permissions: Permissions,
    pub(crate) neutral_times: bool,
}

/// An output being written: a temporary file in the folder it will end up
/// in. Dropped before it's finished, it disappears, and so do the folders
/// created for it if nothing else was written into them.
#[derive(Debug)]
pub(crate) struct Draft {
    temp: Option<NamedTempFile>,
    created: Vec<PathBuf>,
}

impl Draft {
    /// Creates the folder of `destination` if needed.
    pub(crate) fn new(destination: &Path) -> Result<Self, FormatError> {
        let dir = parent(destination);
        let created: Vec<PathBuf> = dir
            .ancestors()
            .take_while(|d| !d.as_os_str().is_empty() && !d.exists())
            .map(Path::to_owned)
            .collect();
        // Another output's unfinished draft may remove a shared new folder
        // just after it was created here; then it's created again.
        let mut attempts = 0;
        let temp = loop {
            fs::create_dir_all(dir)?;
            match tempfile::Builder::new()
                .prefix(".metascrub-")
                .suffix(".tmp")
                .tempfile_in(dir)
            {
                Err(e) if e.kind() == io::ErrorKind::NotFound && attempts < 3 => attempts += 1,
                other => break other?,
            }
        };
        Ok(Self {
            temp: Some(temp),
            created,
        })
    }

    pub(crate) fn file(&mut self) -> &mut File {
        match &mut self.temp {
            Some(temp) => temp.as_file_mut(),
            // Only `seal` takes it, consuming the draft.
            None => unreachable!("a draft keeps its file until it's finished"),
        }
    }

    /// Becomes a new file at `target`, or `name-2.ext`, `name-3.ext`, … when
    /// that name is taken. Returns the path written.
    pub(crate) fn finish_new(
        self,
        target: &Path,
        settings: &Settings,
    ) -> Result<PathBuf, FormatError> {
        let temp = self.seal(settings)?;
        place(temp, target, settings, |temp, path| {
            temp.persist_noclobber(path).map(drop)
        })
    }

    /// Replaces `original` atomically: readers see either the old file or the new one.
    pub(crate) fn finish_replace(
        self,
        original: &Path,
        settings: &Settings,
    ) -> Result<(), FormatError> {
        let temp = self.seal(settings)?;
        temp.persist(original).map_err(|e| e.error)?;
        Ok(())
    }

    fn seal(mut self, settings: &Settings) -> Result<NamedTempFile, FormatError> {
        self.created.clear();
        let temp = self
            .temp
            .take()
            .ok_or_else(|| io::Error::other("the draft was already finished"))?;
        fs::set_permissions(temp.path(), settings.permissions.clone())?;
        // Last, so no later write changes the modification time again.
        if settings.neutral_times {
            set_neutral_times(temp.as_file())?;
        }
        Ok(temp)
    }
}

impl Drop for Draft {
    fn drop(&mut self) {
        // The temporary file first, so its folder can be empty.
        drop(self.temp.take());
        // Innermost first; a folder something else was written into stays.
        for dir in &self.created {
            if fs::remove_dir(dir).is_err() {
                break;
            }
        }
    }
}

/// Writes a new file at `target` (see [`Draft::finish_new`]).
pub(crate) fn write_new(
    target: &Path,
    settings: &Settings,
    write: impl FnOnce(&mut dyn Write) -> Result<(), FormatError>,
) -> Result<PathBuf, FormatError> {
    let mut draft = Draft::new(target)?;
    write_into(&mut draft, write)?;
    draft.finish_new(target, settings)
}

/// Gives `temp` the first free name, with `rename`, which must never
/// replace an existing file. Some file systems (FAT, network drives) can't
/// do that; there the content is copied into a file created only if the
/// name is free, which is as safe but not atomic.
fn place(
    mut temp: NamedTempFile,
    target: &Path,
    settings: &Settings,
    rename: impl Fn(NamedTempFile, &Path) -> Result<(), PersistError>,
) -> Result<PathBuf, FormatError> {
    let mut copy = false;
    for n in 1..=MAX_NAME_ATTEMPTS {
        let candidate = numbered(target, n);
        if !copy {
            match rename(temp, &candidate) {
                Ok(()) => return Ok(candidate),
                Err(e) if e.error.kind() == io::ErrorKind::AlreadyExists => {
                    temp = e.file;
                    continue;
                }
                Err(e) => {
                    temp = e.file;
                    copy = true;
                }
            }
        }
        match copy_new(&mut temp, &candidate, settings) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free file name for the output",
    )
    .into())
}

/// Copies `temp` into `path` if no file has that name yet. On failure the
/// partial copy is removed.
fn copy_new(temp: &mut NamedTempFile, path: &Path, settings: &Settings) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let mut copy = || {
        let source = temp.as_file_mut();
        source.seek(SeekFrom::Start(0))?;
        io::copy(source, &mut file)?;
        fs::set_permissions(path, settings.permissions.clone())?;
        if settings.neutral_times {
            set_neutral_times(&file)?;
        }
        Ok(())
    };
    let result = copy();
    if result.is_err() {
        drop(file);
        let _ = fs::remove_file(path);
    }
    result
}

/// The first of `target`, `name-2.ext`, … that is neither taken by another
/// file of this run nor already on disk. Choosing names before the parallel
/// part of a run keeps the numbering the same from one run to the next.
pub(crate) fn reserve(target: &Path, taken: &mut HashSet<PathBuf>) -> PathBuf {
    let free = (1..=MAX_NAME_ATTEMPTS)
        .map(|n| numbered(target, n))
        .find(|c| !taken.contains(c) && fs::symlink_metadata(c).is_err())
        // `write_new` then fails with a clear error instead.
        .unwrap_or_else(|| target.to_owned());
    taken.insert(free.clone());
    free
}

fn write_into(
    draft: &mut Draft,
    write: impl FnOnce(&mut dyn Write) -> Result<(), FormatError>,
) -> Result<(), FormatError> {
    let mut out = BufWriter::new(draft.file());
    write(&mut out)?;
    out.flush()?;
    Ok(())
}

/// Gives an existing file or folder the neutral date, changing nothing else.
pub(crate) fn set_neutral_times_at(path: &Path) -> io::Result<()> {
    set_neutral_times(&open_for_times(path)?)
}

#[cfg(windows)]
fn open_for_times(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_WRITE_ATTRIBUTES,
    };
    // Only the right to change dates; "backup semantics" lets folders open too.
    fs::OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

/// The owner may change the dates through any open file, folders included.
#[cfg(not(windows))]
fn open_for_times(path: &Path) -> io::Result<File> {
    File::open(path)
}

fn set_neutral_times(file: &File) -> io::Result<()> {
    let time = UNIX_EPOCH + NEUTRAL_TIME;
    let times = FileTimes::new().set_accessed(time).set_modified(time);
    #[cfg(windows)]
    let times = {
        use std::os::windows::fs::FileTimesExt;
        times.set_created(time)
    };
    #[cfg(target_os = "macos")]
    let times = {
        use std::os::macos::fs::FileTimesExt;
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
    fn reserves_names_free_in_the_run_and_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.jpg"), "existing").unwrap();
        let mut taken = HashSet::new();
        let target = dir.path().join("a.jpg");
        assert_eq!(reserve(&target, &mut taken), dir.path().join("a-2.jpg"));
        assert_eq!(reserve(&target, &mut taken), dir.path().join("a-3.jpg"));
        let other = dir.path().join("b.jpg");
        assert_eq!(reserve(&other, &mut taken), other);
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
    fn existing_files_and_folders_get_the_neutral_date() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        fs::write(&file, "x").unwrap();
        for path in [file.as_path(), dir.path()] {
            set_neutral_times_at(path).unwrap();
            let modified = fs::metadata(path).unwrap().modified().unwrap();
            assert_eq!(modified, SystemTime::UNIX_EPOCH + NEUTRAL_TIME);
        }
        assert_eq!(fs::read_to_string(&file).unwrap(), "x");
    }

    /// A file system that can't rename without replacing.
    fn no_safe_rename(temp: NamedTempFile, _: &Path) -> Result<(), PersistError> {
        Err(PersistError {
            error: io::Error::from(io::ErrorKind::Unsupported),
            file: temp,
        })
    }

    #[test]
    fn copies_where_the_file_system_cannot_rename_safely() {
        let dir = tempfile::tempdir().unwrap();
        let settings = settings(dir.path());
        let target = dir.path().join("photo.clean.jpg");
        fs::write(&target, "existing").unwrap();
        let mut draft = Draft::new(&target).unwrap();
        write_into(&mut draft, write_text("new")).unwrap();
        let temp = draft.seal(&settings).unwrap();

        let written = place(temp, &target, &settings, no_safe_rename).unwrap();
        assert_eq!(written, dir.path().join("photo.clean-2.jpg"));
        assert_eq!(fs::read_to_string(&target).unwrap(), "existing");
        assert_eq!(fs::read_to_string(&written).unwrap(), "new");
        let modified = fs::metadata(&written).unwrap().modified().unwrap();
        assert_eq!(modified, SystemTime::UNIX_EPOCH + NEUTRAL_TIME);
        let mut names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names, ["photo.clean-2.jpg", "photo.clean.jpg", "probe"]);
    }

    #[test]
    fn an_unfinished_draft_leaves_no_folders_behind() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out/photos/photo.jpg");
        let draft = Draft::new(&target).unwrap();
        assert!(dir.path().join("out/photos").is_dir());
        drop(draft);
        assert!(!dir.path().join("out").exists());

        // A folder something else was written into stays.
        let draft = Draft::new(&target).unwrap();
        fs::write(dir.path().join("out/other.jpg"), "x").unwrap();
        drop(draft);
        assert!(!dir.path().join("out/photos").exists());
        assert!(dir.path().join("out/other.jpg").exists());
    }

    #[test]
    fn replace_swaps_the_content_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("photo.jpg");
        fs::write(&original, "old").unwrap();
        let mut draft = Draft::new(&original).unwrap();
        write_into(&mut draft, write_text("new")).unwrap();
        draft
            .finish_replace(&original, &settings(dir.path()))
            .unwrap();
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
