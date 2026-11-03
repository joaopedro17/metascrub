//! File names that give something away: when a photo was taken
//! (`IMG_20261102_143512.jpg`), on which phone (`PXL_…`), or through which
//! app it was sent (`WhatsApp Image …`). `--rename` replaces them with
//! neutral names like `image-001.jpg`.

use std::ffi::OsStr;
use std::path::Path;

/// Names cameras, phones and apps give files. Compared in lower case.
const PREFIXES: &[&str] = &[
    "img_",
    "img-",
    "pxl_",
    "vid_",
    "vid-",
    "mvimg_",
    "pano_",
    "dsc_",
    "dscn",
    "dscf",
    "dsc0",
    "gopr",
    "screenshot",
    "screen shot",
    "screen recording",
    "screen_recording",
    "whatsapp",
    "signal-",
    "telegram",
    "photo_",
    "video_",
    "fb_img_",
    "snapchat-",
    "received_",
];

/// Whether `name` (a file name, extension included) reveals a date, a
/// device or an app.
pub(crate) fn leaks(name: &OsStr) -> bool {
    let stem = Path::new(name).file_stem().unwrap_or(name);
    let stem = stem.to_string_lossy().to_lowercase();
    PREFIXES.iter().any(|prefix| stem.starts_with(prefix)) || has_date(&stem)
}

/// A date as `20261102`, `2026-11-02` (or with `_` or `.`), possibly
/// followed by a time, or a Unix timestamp in seconds or milliseconds.
fn has_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        let run = bytes
            .get(start..)
            .unwrap_or_default()
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        if run == 0 {
            start += 1;
            continue;
        }
        let digits = bytes.get(start..start + run).unwrap_or_default();
        let rest = bytes.get(start + run..).unwrap_or_default();
        let found = match run {
            8 | 12 | 14 => is_date(digits),
            // Seconds since 1970, 2001–2033; or milliseconds.
            10 | 13 => digits.first() == Some(&b'1'),
            4 => separated_date(digits, rest),
            _ => false,
        };
        if found {
            return true;
        }
        start += run;
    }
    false
}

/// `YYYYMMDD` with a plausible year, month and day.
fn is_date(digits: &[u8]) -> bool {
    let number = |range: std::ops::Range<usize>| -> Option<u32> {
        std::str::from_utf8(digits.get(range)?).ok()?.parse().ok()
    };
    matches!(
        (number(0..4), number(4..6), number(6..8)),
        (Some(1990..=2099), Some(1..=12), Some(1..=31))
    )
}

/// `-MM-DD` after a year, with the same separator twice.
fn separated_date(year: &[u8], rest: &[u8]) -> bool {
    let [separator, m1, m2, again, d1, d2, ..] = *rest else {
        return false;
    };
    let date = [year, &[m1, m2, d1, d2]].concat();
    matches!(separator, b'-' | b'_' | b'.')
        && again == separator
        && rest.get(6).is_none_or(|b| !b.is_ascii_digit())
        && is_date(&date)
}

/// The neutral word for a file, from its extension.
pub(crate) fn kind(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "jpg" | "jpeg" | "jpe" | "jfif" | "png" | "webp" | "heic" | "heif" | "avif" | "gif"
        | "tif" | "tiff" | "bmp" | "dng" | "cr2" | "cr3" | "nef" | "arw" | "orf" | "rw2" => "image",
        "mp4" | "mov" | "m4v" | "avi" | "mkv" | "webm" | "3gp" => "video",
        "pdf" | "doc" | "docx" | "docm" | "dotx" | "xls" | "xlsx" | "xlsm" | "ppt" | "pptx"
        | "pptm" | "odt" | "ods" | "odp" | "rtf" | "txt" | "md" | "csv" => "document",
        _ => "file",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_names_that_leak() {
        for name in [
            "IMG_20261102_143512.jpg",
            "IMG_1234.JPG",
            "IMG-20261102-WA0001.jpg",
            "PXL_20261102_143512345.jpg",
            "VID_20261102_143512.mp4",
            "DSC_0042.NEF",
            "Screenshot 2026-11-02 at 14.35.12.png",
            "Screen Shot 2026-11-02 at 2.35.12 PM.png",
            "WhatsApp Image 2026-11-02 at 14.35.12.jpeg",
            "signal-2026-11-02-143512.jpg",
            "photo_2026-11-02_14-35-12.jpg",
            "report 2026-11-02.pdf",
            "scan_20261102.pdf",
            "holiday_2026.11.02.jpg",
            "1730563200000.jpg",
            "FB_IMG_1730563200000.jpg",
            "20261102143512.jpg",
        ] {
            assert!(leaks(OsStr::new(name)), "{name}");
        }
    }

    #[test]
    fn leaves_names_people_chose_alone() {
        for name in [
            "holiday.jpg",
            "report-final-v2.pdf",
            "budget 2026.xlsx",
            "invoice-12345678.pdf",
            "image-001.jpg",
            "IMG.jpg",
            "2026-13-45 notes.txt",
            "phone 0612345678.txt",
            "imagine.png",
        ] {
            assert!(!leaks(OsStr::new(name)), "{name}");
        }
    }

    #[test]
    fn names_the_kind_of_file_from_its_extension() {
        assert_eq!(kind(Path::new("a/IMG_1.JPG")), "image");
        assert_eq!(kind(Path::new("VID_1.mov")), "video");
        assert_eq!(kind(Path::new("scan 2026-11-02.pdf")), "document");
        assert_eq!(kind(Path::new("archive_20261102.zip")), "file");
        assert_eq!(kind(Path::new("20261102")), "file");
    }
}
