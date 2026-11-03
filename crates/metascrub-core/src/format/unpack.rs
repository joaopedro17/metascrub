//! Files inside archives. Each is unpacked on its own into a spool (memory,
//! or a temporary file once it's large), then cleaned with the same rules as
//! a file on disk, or kept as it is when MetaScrub can't read it.

use std::cell::Cell;
use std::io::{self, ErrorKind, Read, Seek, SeekFrom, Write};

use tempfile::SpooledTempFile;

use super::{Depth, FormatError, SNIFF_LEN, detect, handler};
use crate::limits::{
    MAX_ARCHIVE_UNPACKED_BYTES, MAX_ZIP_RATIO, SPOOL_IN_MEMORY_BYTES, ZIP_RATIO_CHECK_ABOVE,
};
use crate::model::{Disposition, Finding, FindingKind, Location, Value};
use crate::options::Options;

pub(super) type Spool = SpooledTempFile;

pub(super) fn spool() -> Spool {
    tempfile::spooled_tempfile(SPOOL_IN_MEMORY_BYTES)
}

thread_local! {
    static LEFT: Cell<Option<u64>> = const { Cell::new(None) };
}

/// What the archives in one file may unpack to, together: an archive inside
/// an archive draws from the same allowance, so nesting can't multiply it.
/// Each file is read on one thread, so a thread-local count works.
pub(super) struct Allowance {
    outermost: bool,
}

impl Allowance {
    pub(super) fn start() -> Self {
        let outermost = LEFT.with(|left| {
            let unset = left.get().is_none();
            if unset {
                left.set(Some(MAX_ARCHIVE_UNPACKED_BYTES));
            }
            unset
        });
        Self { outermost }
    }
}

impl Drop for Allowance {
    fn drop(&mut self) {
        if self.outermost {
            LEFT.with(|left| left.set(None));
        }
    }
}

fn spend(bytes: u64) -> Result<(), FormatError> {
    LEFT.with(|left| {
        let available = left.get().unwrap_or(MAX_ARCHIVE_UNPACKED_BYTES);
        match available.checked_sub(bytes) {
            Some(rest) => {
                left.set(Some(rest));
                Ok(())
            }
            None => Err(FormatError::TooLarge {
                limit: MAX_ARCHIVE_UNPACKED_BYTES,
            }),
        }
    })
}

/// Unpacks one file from an archive into a spool, within the allowance and,
/// when its compressed size is known, the compression ratio limit.
pub(super) fn unpack(reader: &mut dyn Read, compressed: Option<u64>) -> Result<Spool, FormatError> {
    let mut spool = spool();
    let mut buffer = vec![0; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(read_error(e)),
        };
        total += read as u64;
        spend(read as u64)?;
        if let Some(compressed) = compressed
            && total > ZIP_RATIO_CHECK_ABOVE
            && total / compressed.max(1) > MAX_ZIP_RATIO
        {
            return Err(FormatError::TooLarge {
                limit: compressed.saturating_mul(MAX_ZIP_RATIO),
            });
        }
        spool.write_all(buffer.get(..read).unwrap_or_default())?;
    }
    spool.seek(SeekFrom::Start(0))?;
    Ok(spool)
}

/// Reading an archive fails with I/O errors both when the disk fails and
/// when the data is damaged; only the first is an I/O problem.
pub(super) fn read_error(error: io::Error) -> FormatError {
    match error.kind() {
        ErrorKind::InvalidData
        | ErrorKind::InvalidInput
        | ErrorKind::UnexpectedEof
        | ErrorKind::Other => FormatError::malformed(error.to_string()),
        _ => FormatError::Io(error),
    }
}

/// One file from an archive after inspecting or cleaning it.
#[derive(Debug)]
pub(super) struct Unpacked {
    /// Located inside the file's name.
    pub(super) findings: Vec<Finding>,
    /// What to store: the cleaned file, or the original when it's kept.
    pub(super) content: Spool,
}

/// Inspects the file named `name` in an archive, or cleans it when `clean`.
/// A file MetaScrub can't read is kept as it is, with a warning: an unknown
/// type, or a damaged, encrypted, too large or too deeply nested file.
/// Errors only when the disk fails.
pub(super) fn entry(
    name: &str,
    mut data: Spool,
    options: &Options,
    clean: bool,
) -> Result<Unpacked, FormatError> {
    let read = read_entry(&mut data, options, clean);
    data.seek(SeekFrom::Start(0))?;
    Ok(match read {
        Ok(Some((findings, cleaned))) => Unpacked {
            findings: findings.into_iter().map(|f| f.within(name)).collect(),
            content: cleaned.unwrap_or(data),
        },
        Ok(None) | Err(_) => Unpacked {
            findings: vec![
                Finding::new(
                    FindingKind::UnreadFile,
                    Location::new([name]),
                    Disposition::Warn,
                )
                .with_value(Value::Text(name.to_owned())),
            ],
            content: data,
        },
    })
}

/// `None` when MetaScrub can't read this type of file.
#[allow(clippy::type_complexity)]
fn read_entry(
    data: &mut Spool,
    options: &Options,
    clean: bool,
) -> Result<Option<(Vec<Finding>, Option<Spool>)>, FormatError> {
    let _depth = Depth::enter()?;
    let mut head = Vec::with_capacity(SNIFF_LEN);
    Read::by_ref(data)
        .take(SNIFF_LEN as u64)
        .read_to_end(&mut head)?;
    data.seek(SeekFrom::Start(0))?;
    let Some(handler) = detect(&head, data)?.and_then(handler) else {
        return Ok(None);
    };
    if !clean {
        return Ok(Some((handler.inspect(data, options)?, None)));
    }
    let mut cleaned = spool();
    let findings = handler.clean(data, &mut cleaned, options)?;
    cleaned.seek(SeekFrom::Start(0))?;
    Ok(Some((findings, Some(cleaned))))
}

/// Permissions inside cleaned archives (requirements §13.10): folders and
/// programs 755, everything else 644.
pub(super) fn mode(original: Option<u32>, folder: bool) -> u32 {
    if folder || original.is_some_and(|mode| mode & 0o111 != 0) {
        0o755
    } else {
        0o644
    }
}

/// `1730563200` → `2024-11-02 16:00:00` (UTC).
pub(super) fn utc(seconds: u64) -> String {
    let days = seconds / 86_400;
    let time = seconds % 86_400;
    // Days since 1970-01-01 to a calendar date (Howard Hinnant's algorithm).
    let shifted = days + 719_468;
    let era = shifted / 146_097;
    let day_of_era = shifted % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        time / 3600,
        time % 3600 / 60,
        time % 60
    )
}

/// Copies a spool to the archive being written.
pub(super) fn copy_out(content: &mut Spool, out: &mut dyn Write) -> Result<u64, FormatError> {
    content.seek(SeekFrom::Start(0))?;
    Ok(io::copy(content, out)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_unix_times() {
        assert_eq!(utc(0), "1970-01-01 00:00:00");
        assert_eq!(utc(315_532_800), "1980-01-01 00:00:00");
        assert_eq!(utc(1_730_563_200), "2024-11-02 16:00:00");
        assert_eq!(utc(951_782_400), "2000-02-29 00:00:00");
    }

    #[test]
    fn nested_archives_share_one_allowance() {
        let _outer = Allowance::start();
        spend(MAX_ARCHIVE_UNPACKED_BYTES - 10).unwrap();
        {
            let _inner = Allowance::start();
            assert!(spend(11).is_err());
            spend(10).unwrap();
        }
        assert!(spend(1).is_err());
    }

    #[test]
    fn the_allowance_is_renewed_for_the_next_file() {
        {
            let _first = Allowance::start();
            spend(MAX_ARCHIVE_UNPACKED_BYTES).unwrap();
        }
        let _second = Allowance::start();
        spend(1).unwrap();
    }

    #[test]
    fn unpacking_stops_at_the_compression_ratio() {
        let zeros = vec![0u8; 2 * 1024 * 1024];
        let _allowance = Allowance::start();
        assert!(matches!(
            unpack(&mut zeros.as_slice(), Some(1000)),
            Err(FormatError::TooLarge { .. })
        ));
        let mut spool = unpack(&mut zeros.as_slice(), None).unwrap();
        assert_eq!(spool.seek(SeekFrom::End(0)).unwrap(), zeros.len() as u64);
    }

    #[test]
    fn unreadable_files_are_kept_as_they_are() {
        let mut data = spool();
        data.write_all(b"just some text").unwrap();
        let unpacked = entry("notes.txt", data, &Options::default(), true).unwrap();
        assert_eq!(unpacked.findings.len(), 1);
        assert_eq!(unpacked.findings[0].kind, FindingKind::UnreadFile);
        let mut content = String::new();
        let mut kept = unpacked.content;
        kept.read_to_string(&mut content).unwrap();
        assert_eq!(content, "just some text");
    }

    #[test]
    fn permissions_are_neutral() {
        assert_eq!(mode(Some(0o100600), false), 0o644);
        assert_eq!(mode(Some(0o100700), false), 0o755);
        assert_eq!(mode(None, false), 0o644);
        assert_eq!(mode(None, true), 0o755);
    }
}
