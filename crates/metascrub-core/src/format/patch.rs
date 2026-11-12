//! Writing a file as a copy of its input with some ranges overwritten, for
//! formats cleaned in place (TIFF, Matroska): nothing moves, so every
//! offset inside the file stays valid.

use std::io::{self, Read, SeekFrom, Write};

use super::{FormatError, ReadSeek};

#[derive(Debug)]
pub(super) enum Fill {
    Bytes(Vec<u8>),
    Zeros(u64),
}

impl Fill {
    fn len(&self) -> u64 {
        match self {
            Self::Bytes(bytes) => bytes.len() as u64,
            Self::Zeros(len) => *len,
        }
    }
}

/// Copies `input` up to `end`, with each `(offset, fill)` written over the
/// original bytes. Patches must not overlap.
pub(super) fn write(
    input: &mut dyn ReadSeek,
    output: &mut dyn Write,
    mut patches: Vec<(u64, Fill)>,
    end: u64,
) -> Result<(), FormatError> {
    patches.sort_by_key(|(at, _)| *at);
    input.seek(SeekFrom::Start(0))?;
    let mut at = 0;
    for (start, fill) in patches {
        let len = fill.len();
        if start < at || start.saturating_add(len) > end {
            return Err(FormatError::malformed("metadata in overlapping places"));
        }
        copy(input, output, start - at)?;
        match fill {
            Fill::Bytes(bytes) => output.write_all(&bytes)?,
            Fill::Zeros(len) => {
                io::copy(&mut io::repeat(0).take(len), output)?;
            }
        }
        input.seek(SeekFrom::Start(start + len))?;
        at = start + len;
    }
    copy(input, output, end - at)
}

fn copy(input: &mut dyn ReadSeek, output: &mut dyn Write, len: u64) -> Result<(), FormatError> {
    if io::copy(&mut (&mut *input).take(len), output)? != len {
        return Err(FormatError::malformed("the file ends early"));
    }
    Ok(())
}
