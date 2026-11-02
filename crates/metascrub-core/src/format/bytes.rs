use super::FormatError;

/// Reads big-endian values from a byte slice. Every read is bounds-checked
/// and fails with [`FormatError::Malformed`] instead of panicking.
#[derive(Debug, Clone)]
pub(crate) struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub(crate) fn remaining(&self) -> &'a [u8] {
        self.data.get(self.pos..).unwrap_or_default()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.remaining().is_empty()
    }

    pub(crate) fn take(&mut self, len: usize) -> Result<&'a [u8], FormatError> {
        let bytes = self
            .pos
            .checked_add(len)
            .and_then(|end| self.data.get(self.pos..end))
            .ok_or_else(|| FormatError::malformed("unexpected end of data"))?;
        self.pos += len;
        Ok(bytes)
    }

    pub(crate) fn skip(&mut self, len: usize) -> Result<(), FormatError> {
        self.take(len).map(|_| ())
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N], FormatError> {
        let bytes = self.take(N)?;
        bytes
            .try_into()
            .map_err(|_| FormatError::malformed("unexpected end of data"))
    }

    pub(crate) fn u8(&mut self) -> Result<u8, FormatError> {
        Ok(u8::from_be_bytes(self.array()?))
    }

    pub(crate) fn u16_be(&mut self) -> Result<u16, FormatError> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    pub(crate) fn u32_be(&mut self) -> Result<u32, FormatError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    pub(crate) fn u32_le(&mut self) -> Result<u32, FormatError> {
        Ok(u32::from_le_bytes(self.array()?))
    }
}

/// Splits at the first NUL byte, dropping it.
pub(crate) fn split_nul(data: &[u8]) -> Option<(&[u8], &[u8])> {
    let nul = data.iter().position(|&b| b == 0)?;
    Some((data.get(..nul)?, data.get(nul + 1..)?))
}

/// Latin-1 maps every byte to the Unicode code point of the same value.
pub(crate) fn latin1(data: &[u8]) -> String {
    data.iter().map(|&b| char::from(b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_big_endian_values() {
        let mut cursor = Cursor::new(&[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07]);
        assert_eq!(cursor.u8().unwrap(), 0x01);
        assert_eq!(cursor.u16_be().unwrap(), 0x0203);
        assert_eq!(cursor.u32_be().unwrap(), 0x0405_0607);
        assert!(cursor.is_empty());
    }

    #[test]
    fn reading_past_the_end_is_an_error_not_a_panic() {
        let mut cursor = Cursor::new(&[0x01]);
        assert!(cursor.u16_be().is_err());
        assert!(cursor.take(usize::MAX).is_err());
        assert_eq!(cursor.u8().unwrap(), 0x01);
    }

    #[test]
    fn splits_at_first_nul() {
        assert_eq!(
            split_nul(b"Author\0Jane\0Doe"),
            Some((&b"Author"[..], &b"Jane\0Doe"[..]))
        );
        assert_eq!(split_nul(b"no nul"), None);
    }

    #[test]
    fn decodes_latin1() {
        assert_eq!(latin1(b"caf\xe9"), "café");
    }
}
