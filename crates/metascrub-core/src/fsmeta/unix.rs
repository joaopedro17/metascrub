//! Extended attributes, never following symbolic links.

use std::ffi::{OsStr, OsString};
use std::io::{self, ErrorKind};
use std::path::Path;

use super::MAX_VALUE_BYTES;

/// On Linux only `user.*` attributes are the user's; the others belong to
/// the system (security labels, access lists). On macOS they all are.
fn is_user_attribute(name: &OsStr) -> bool {
    cfg!(target_os = "macos") || name.as_encoded_bytes().starts_with(b"user.")
}

pub(super) fn names(path: &Path) -> io::Result<Vec<OsString>> {
    match xattr::list(path) {
        Ok(names) => Ok(names.filter(|name| is_user_attribute(name)).collect()),
        // File systems without attributes (FAT, some network drives) have none.
        Err(e) if e.kind() == ErrorKind::Unsupported => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

pub(super) fn read(path: &Path, name: &OsStr) -> Option<Vec<u8>> {
    xattr::get(path, name)
        .ok()
        .flatten()
        .filter(|value| value.len() as u64 <= MAX_VALUE_BYTES)
}

pub(super) fn remove(path: &Path, name: &OsStr) -> io::Result<()> {
    xattr::remove(path, name)
}

#[cfg(test)]
pub(super) fn mark_downloaded(path: &Path, url: &str) -> bool {
    xattr::set(path, "user.xdg.origin.url", url.as_bytes()).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsmeta::{list, mark_downloaded, strip};
    use crate::model::FindingKind;

    /// Some file systems (like tmpfs on older kernels) don't allow user attributes.
    fn file_with_attributes(dir: &Path) -> Option<std::path::PathBuf> {
        let file = dir.join("photo.jpg");
        std::fs::write(&file, "x").unwrap();
        if !mark_downloaded(&file, "https://example.com/photo.jpg") {
            return None;
        }
        xattr::set(&file, "user.custom", b"anything").unwrap();
        Some(file)
    }

    #[test]
    fn finds_and_removes_user_attributes() {
        let dir = tempfile::tempdir().unwrap();
        let Some(file) = file_with_attributes(dir.path()) else {
            return;
        };
        let mut kinds: Vec<_> = list(&file)
            .unwrap()
            .into_iter()
            .flat_map(|a| a.findings)
            .map(|f| f.kind)
            .collect();
        kinds.sort_by_key(|k| format!("{k:?}"));
        assert_eq!(
            kinds,
            [FindingKind::DownloadUrl, FindingKind::FileAttribute]
        );

        assert_eq!(strip(&file).unwrap().len(), 2);
        assert!(list(&file).unwrap().is_empty());
    }

    #[test]
    fn symbolic_links_are_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let Some(file) = file_with_attributes(dir.path()) else {
            return;
        };
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        // Linux doesn't allow user attributes on links themselves.
        let _ = strip(&link);
        assert_eq!(list(&file).unwrap().len(), 2);
    }
}
