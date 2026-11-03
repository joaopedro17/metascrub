//! Alternate data streams. Listing them needs `FindFirstStreamW`, which the
//! standard library doesn't offer: this is the only place MetaScrub calls
//! the operating system directly. Reading and deleting a stream works with
//! ordinary file operations on `path:name`.
#![allow(unsafe_code)]

use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{
    ERROR_HANDLE_EOF, ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    FindClose, FindFirstStreamW, FindNextStreamW, FindStreamInfoStandard, WIN32_FIND_STREAM_DATA,
};

use super::MAX_VALUE_BYTES;

/// Closes the search when dropped, also on early returns.
struct Search(HANDLE);

impl Drop for Search {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful `FindFirstStreamW` and is
        // closed only here.
        unsafe { FindClose(self.0) };
    }
}

/// The named streams of `path` and their sizes. The unnamed stream is the
/// file's content.
pub(super) fn names(path: &Path) -> io::Result<Vec<OsString>> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut data = WIN32_FIND_STREAM_DATA::default();
    // SAFETY: `wide` is NUL-terminated and outlives the call; `data` is the
    // structure `FindStreamInfoStandard` fills.
    let handle = unsafe {
        FindFirstStreamW(
            wide.as_ptr(),
            FindStreamInfoStandard,
            (&raw mut data).cast(),
            0,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        let error = io::Error::last_os_error();
        return match error.raw_os_error().and_then(|c| u32::try_from(c).ok()) {
            // No streams at all (a folder), or a file system without them (FAT).
            Some(
                ERROR_HANDLE_EOF
                | ERROR_INVALID_FUNCTION
                | ERROR_INVALID_PARAMETER
                | ERROR_NOT_SUPPORTED,
            ) => Ok(Vec::new()),
            _ => Err(error),
        };
    }
    let search = Search(handle);
    let mut names = Vec::new();
    loop {
        names.extend(stream_name(&data));
        // SAFETY: the handle is open (owned by `search`) and `data` is as above.
        if unsafe { FindNextStreamW(search.0, (&raw mut data).cast()) } == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == i32::try_from(ERROR_HANDLE_EOF).ok() {
                return Ok(names);
            }
            return Err(error);
        }
    }
}

/// `:Zone.Identifier:$DATA` → `Zone.Identifier`; `::$DATA` (the content) → none.
fn stream_name(data: &WIN32_FIND_STREAM_DATA) -> Option<OsString> {
    const SUFFIX: &[u16] = &[0x3A, 0x24, 0x44, 0x41, 0x54, 0x41]; // ":$DATA"
    let end = data
        .cStreamName
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(data.cStreamName.len());
    let name = data
        .cStreamName
        .get(..end)?
        .strip_prefix(&[0x3A])?
        .strip_suffix(SUFFIX)?;
    (!name.is_empty()).then(|| OsString::from_wide(name))
}

fn stream_path(path: &Path, name: &OsStr) -> PathBuf {
    let mut full = path.as_os_str().to_owned();
    full.push(":");
    full.push(name);
    PathBuf::from(full)
}

pub(super) fn read(path: &Path, name: &OsStr) -> Option<Vec<u8>> {
    let mut value = Vec::new();
    File::open(stream_path(path, name))
        .ok()?
        .take(MAX_VALUE_BYTES)
        .read_to_end(&mut value)
        .ok()?;
    Some(value)
}

pub(super) fn remove(path: &Path, name: &OsStr) -> io::Result<()> {
    fs::remove_file(stream_path(path, name))
}

#[cfg(test)]
pub(super) fn mark_downloaded(path: &Path, url: &str) -> bool {
    let zone = format!("[ZoneTransfer]\r\nZoneId=3\r\nHostUrl={url}\r\n");
    fs::write(stream_path(path, OsStr::new("Zone.Identifier")), zone).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsmeta::{list, strip};
    use crate::model::{FindingKind, Value};

    const ZONE: &[u8] = b"[ZoneTransfer]\r\nZoneId=3\r\nHostUrl=https://example.com/photo.jpg\r\n";

    fn file_with_streams(dir: &Path) -> PathBuf {
        let file = dir.join("photo.jpg");
        fs::write(&file, "x").unwrap();
        fs::write(stream_path(&file, OsStr::new("Zone.Identifier")), ZONE).unwrap();
        fs::write(stream_path(&file, OsStr::new("custom")), "anything").unwrap();
        file
    }

    #[test]
    fn finds_and_removes_named_streams() {
        let dir = tempfile::tempdir().unwrap();
        let file = file_with_streams(dir.path());
        let findings: Vec<_> = list(&file)
            .unwrap()
            .into_iter()
            .flat_map(|a| a.findings)
            .collect();
        let kinds: Vec<_> = findings.iter().map(|f| f.kind).collect();
        assert!(kinds.contains(&FindingKind::DownloadUrl), "{findings:?}");
        assert!(findings.iter().any(|f| f.kind == FindingKind::FileAttribute
            && f.value == Some(Value::Text("custom".into()))));

        assert_eq!(strip(&file).unwrap().len(), 2);
        assert!(list(&file).unwrap().is_empty());
        assert_eq!(fs::read_to_string(&file).unwrap(), "x");
    }

    #[test]
    fn folders_have_streams_too() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(stream_path(dir.path(), OsStr::new("custom")), "x").unwrap();
        assert_eq!(strip(dir.path()).unwrap().len(), 1);
        assert!(list(dir.path()).unwrap().is_empty());
    }
}
