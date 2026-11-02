//! Turns the paths the user gave into the list of files to process, in a
//! fixed order, with each output name chosen before anything runs in parallel.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::model::{Failure, FailureKind, Outcome, SkipReason};
use crate::output;
use crate::pipeline::{Command, OutputMode, Request};

#[derive(Debug)]
pub(crate) struct Job {
    pub(crate) path: PathBuf,
    /// Where `clean` writes, already free of collisions. `None` in place.
    pub(crate) target: Option<PathBuf>,
    /// Known without opening the file (folder errors, already-cleaned names).
    pub(crate) outcome: Option<Outcome>,
}

/// A file found in the input, with its path relative to the output folder.
#[derive(Debug)]
struct Found {
    path: PathBuf,
    relative: PathBuf,
    error: Option<Failure>,
}

pub(crate) fn plan(request: &Request) -> Vec<Job> {
    let output_dir = match &request.output {
        OutputMode::Directory(dir) => fs::canonicalize(dir).ok(),
        _ => None,
    };
    let mut found = Vec::new();
    for path in &request.paths {
        if request.recursive && is_folder(path) {
            walk(path, output_dir.as_deref(), &mut found);
        } else {
            found.push(Found {
                path: path.clone(),
                relative: PathBuf::from(file_name(path)),
                error: None,
            });
        }
    }

    let mut taken = HashSet::new();
    found
        .into_iter()
        .map(|found| {
            let outcome = match found.error {
                Some(failure) => Some(Outcome::Failed(failure)),
                None if request.command == Command::Clean && looks_cleaned(&found.path) => {
                    Some(Outcome::Skipped(SkipReason::AlreadyCleaned))
                }
                None => None,
            };
            let target = match (&request.command, &request.output, &outcome) {
                (Command::Clean, OutputMode::NextToOriginal, None) => {
                    Some(output::reserve(&cleaned_name(&found.path), &mut taken))
                }
                (Command::Clean, OutputMode::Directory(dir), None) => {
                    Some(output::reserve(&dir.join(&found.relative), &mut taken))
                }
                _ => None,
            };
            Job {
                path: found.path,
                target,
                outcome,
            }
        })
        .collect()
}

/// Symbolic links to folders are not followed, like links inside folders.
fn is_folder(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

/// Everything inside `root` except folders, sorted by name. Symbolic links and
/// special files are listed too, so the report says why they were skipped.
fn walk(root: &Path, output_dir: Option<&Path>, found: &mut Vec<Found>) {
    // `photos` is mirrored as `out/photos/…`; `.` has no name, so its
    // contents go straight into `out/`.
    let base = PathBuf::from(root.file_name().unwrap_or_default());
    let walker = WalkDir::new(root)
        .min_depth(1)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| {
            !(entry.file_type().is_dir()
                && output_dir.is_some_and(|out| {
                    fs::canonicalize(entry.path()).is_ok_and(|path| path == out)
                }))
        });
    for entry in walker {
        match entry {
            Ok(entry) if entry.file_type().is_dir() => {}
            Ok(entry) => {
                let relative = entry.path().strip_prefix(root).unwrap_or(entry.path());
                found.push(Found {
                    relative: base.join(relative),
                    path: entry.into_path(),
                    error: None,
                });
            }
            Err(error) => {
                let path = error.path().unwrap_or(root).to_owned();
                found.push(Found {
                    relative: PathBuf::new(),
                    path,
                    error: Some(Failure {
                        kind: FailureKind::Io,
                        detail: error.to_string(),
                    }),
                });
            }
        }
    }
}

/// `photo.jpg` → `photo.clean.jpg`, `notes` → `notes.clean`.
fn cleaned_name(path: &Path) -> PathBuf {
    let mut name: OsString = path.file_stem().unwrap_or_default().to_owned();
    name.push(".clean");
    if let Some(extension) = path.extension() {
        name.push(".");
        name.push(extension);
    }
    path.with_file_name(name)
}

/// Names MetaScrub gives its own output: `photo.clean.jpg`, `photo.clean-2.jpg`,
/// `notes.clean`.
fn looks_cleaned(path: &Path) -> bool {
    let ends_cleaned = |name: &OsStr| {
        let name = name.to_string_lossy();
        let Some((_, suffix)) = name.rsplit_once(".clean") else {
            return false;
        };
        suffix.is_empty()
            || suffix
                .strip_prefix('-')
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    };
    path.file_name().is_some_and(ends_cleaned) || path.file_stem().is_some_and(ends_cleaned)
}

fn file_name(path: &Path) -> &OsStr {
    path.file_name().unwrap_or(OsStr::new("output"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(command: Command, output: OutputMode, paths: &[&Path]) -> Request {
        Request {
            output,
            recursive: true,
            ..Request::new(command, paths.iter().map(|p| p.to_path_buf()).collect())
        }
    }

    fn touch(path: PathBuf) -> PathBuf {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "x").unwrap();
        path
    }

    #[test]
    fn names_the_output_next_to_the_original() {
        assert_eq!(
            cleaned_name(Path::new("a/photo.jpg")),
            Path::new("a/photo.clean.jpg")
        );
        assert_eq!(cleaned_name(Path::new("notes")), Path::new("notes.clean"));
    }

    #[test]
    fn recognizes_its_own_output_names() {
        for name in [
            "a.clean.jpg",
            "a.clean-2.jpg",
            "notes.clean",
            "notes.clean-12",
        ] {
            assert!(looks_cleaned(Path::new(name)), "{name}");
        }
        for name in [
            "a.jpg",
            "a.cleaner.jpg",
            "a.clean-.jpg",
            "a.clean-x.jpg",
            "clean.jpg",
        ] {
            assert!(!looks_cleaned(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn walks_folders_in_name_order_and_mirrors_them() {
        let dir = tempfile::tempdir().unwrap();
        let photos = dir.path().join("photos");
        let b = touch(photos.join("b.jpg"));
        let a = touch(photos.join("2024/a.jpg"));
        let out = dir.path().join("out");

        let jobs = plan(&request(
            Command::Clean,
            OutputMode::Directory(out.clone()),
            &[&photos],
        ));
        let paths: Vec<_> = jobs.iter().map(|j| j.path.clone()).collect();
        assert_eq!(paths, [a, b]);
        assert_eq!(jobs[0].target, Some(out.join("photos/2024/a.jpg")));
        assert_eq!(jobs[1].target, Some(out.join("photos/b.jpg")));
    }

    #[test]
    fn folders_without_recursion_are_left_to_the_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path().join("a.jpg"));
        let jobs = plan(&Request::new(Command::Inspect, vec![dir.path().to_owned()]));
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].path, dir.path());
    }

    #[test]
    fn skips_the_output_folder_inside_the_source() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path().join("a.jpg"));
        touch(dir.path().join("out/dir/a.jpg"));
        let out = dir.path().join("out/dir");
        let jobs = plan(&request(
            Command::Clean,
            OutputMode::Directory(out),
            &[dir.path()],
        ));
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].path, dir.path().join("a.jpg"));
    }

    #[test]
    fn skips_already_cleaned_files_only_when_cleaning() {
        let dir = tempfile::tempdir().unwrap();
        let cleaned = touch(dir.path().join("a.clean.jpg"));
        let paths = [cleaned.as_path()];

        let clean = plan(&request(Command::Clean, OutputMode::NextToOriginal, &paths));
        assert_eq!(
            clean[0].outcome,
            Some(Outcome::Skipped(SkipReason::AlreadyCleaned))
        );
        assert_eq!(clean[0].target, None);

        let verify = plan(&request(
            Command::Verify,
            OutputMode::NextToOriginal,
            &paths,
        ));
        assert_eq!(verify[0].outcome, None);
    }

    #[test]
    fn numbers_files_that_would_land_on_the_same_name() {
        let dir = tempfile::tempdir().unwrap();
        let first = touch(dir.path().join("x/a.jpg"));
        let second = touch(dir.path().join("y/a.jpg"));
        let out = dir.path().join("out");
        let jobs = plan(&request(
            Command::Clean,
            OutputMode::Directory(out.clone()),
            &[&first, &second],
        ));
        assert_eq!(jobs[0].target, Some(out.join("a.jpg")));
        assert_eq!(jobs[1].target, Some(out.join("a-2.jpg")));
    }

    #[test]
    fn writes_in_place_without_a_target() {
        let dir = tempfile::tempdir().unwrap();
        let file = touch(dir.path().join("a.jpg"));
        let jobs = plan(&request(Command::Clean, OutputMode::InPlace, &[&file]));
        assert_eq!(jobs[0].target, None);
    }

    #[cfg(unix)]
    #[test]
    fn lists_symbolic_links_without_following_them() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        touch(elsewhere.path().join("secret.jpg"));
        std::os::unix::fs::symlink(elsewhere.path(), dir.path().join("link")).unwrap();
        let jobs = plan(&request(
            Command::Inspect,
            OutputMode::NextToOriginal,
            &[dir.path()],
        ));
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].path, dir.path().join("link"));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_folders_are_reported_and_the_rest_continues() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked");
        touch(locked.join("a.jpg"));
        touch(dir.path().join("b.jpg"));
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        // Root can read anything, so the error can't be produced.
        if fs::read_dir(&locked).is_ok() {
            return;
        }
        let jobs = plan(&request(
            Command::Inspect,
            OutputMode::NextToOriginal,
            &[dir.path()],
        ));
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].path, dir.path().join("b.jpg"));
        assert_eq!(jobs[1].path, locked);
        assert!(matches!(
            &jobs[1].outcome,
            Some(Outcome::Failed(f)) if f.kind == FailureKind::Io
        ));
    }
}
