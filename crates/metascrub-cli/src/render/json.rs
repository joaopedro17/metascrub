//! JSON output. Bump `SCHEMA_VERSION` on any breaking change to its shape.

use std::path::Path;

use metascrub_core::model::{FileReport, RunReport};
use serde_json::{Value, json};

use super::Settings;

const SCHEMA_VERSION: u32 = 1;

pub fn render(report: &RunReport, command: &str, settings: &Settings) -> Value {
    json!({
        "schema_version": SCHEMA_VERSION,
        "command": command,
        "values_shown": settings.show_values,
        "files": report.files.iter().map(|f| file(f, settings)).collect::<Vec<_>>(),
        "summary": report.summary(),
    })
}

fn file(file: &FileReport, settings: &Settings) -> Value {
    let findings: Vec<Value> = file
        .findings
        .iter()
        .map(|finding| {
            let mut value = serde_json::to_value(finding).unwrap_or(Value::Null);
            if !settings.show_values
                && let Some(object) = value.as_object_mut()
            {
                object.remove("value");
            }
            value
        })
        .collect();
    json!({
        "path": path(&file.path),
        "format": file.format,
        "outcome": file.outcome,
        "findings": findings,
        "output": file.output.as_deref().map(path),
    })
}

/// Paths that aren't valid Unicode are kept losslessly as raw hex
/// (bytes on Unix, UTF-16 code units on Windows).
fn path(path: &Path) -> Value {
    match path.to_str() {
        Some(text) => json!(text),
        None => json!({
            "lossy": path.to_string_lossy(),
            "raw_hex": raw_hex(path),
        }),
    }
}

#[cfg(unix)]
fn raw_hex(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str()
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(windows)]
fn raw_hex(path: &Path) -> String {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .map(|unit| format!("{unit:04x}"))
        .collect()
}

#[cfg(not(any(unix, windows)))]
fn raw_hex(_path: &Path) -> String {
    String::new()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn non_unicode_paths_are_kept_losslessly() {
        let raw = Path::new(OsStr::from_bytes(b"photo-\xff.jpg"));
        assert_eq!(
            path(raw),
            json!({ "lossy": "photo-\u{FFFD}.jpg", "raw_hex": "70686f746f2dff2e6a7067" })
        );
        assert_eq!(path(Path::new("photo.jpg")), json!("photo.jpg"));
    }
}
