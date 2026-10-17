use std::{
    fmt::Debug,
    io::{self, Cursor, Write},
};

use mineflayer_buf::{McBuf, McBufLimited, McBufVar};

/// Used for written books.
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Filterable<T> {
    pub raw: T,
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub filtered: Option<T>,
}

impl<T: McBuf> McBuf for Filterable<T> {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, mineflayer_buf::BufReadError> {
        let raw = McBuf::mc_read(buf)?;
        let filtered = McBuf::mc_read(buf)?;
        Ok(Self { raw, filtered })
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        self.raw.mc_write(buf)?;
        self.filtered.mc_write(buf)?;
        Ok(())
    }
}
impl<T: McBufLimited> McBufLimited for Filterable<T> {
    fn mc_read_limited(
        buf: &mut Cursor<&[u8]>,
        limit: u32,
    ) -> Result<Self, mineflayer_buf::BufReadError> {
        let raw = McBufLimited::mc_read_limited(buf, limit)?;
        let filtered = McBufLimited::mc_read_limited(buf, limit)?;
        Ok(Self { raw, filtered })
    }
}
impl<T: McBufVar> McBufVar for Filterable<T> {
    fn mc_read_var(buf: &mut Cursor<&[u8]>) -> Result<Self, mineflayer_buf::BufReadError> {
        let raw = McBufVar::mc_read_var(buf)?;
        let filtered = McBufVar::mc_read_var(buf)?;
        Ok(Self { raw, filtered })
    }
    fn mc_write_var(&self, buf: &mut impl Write) -> io::Result<()> {
        self.raw.mc_write_var(buf)?;
        self.filtered.mc_write_var(buf)?;
        Ok(())
    }
}

impl<T: Clone> Clone for Filterable<T> {
    fn clone(&self) -> Self {
        Self {
            raw: self.raw.clone(),
            filtered: self.filtered.clone(),
        }
    }
}
impl<T: PartialEq> PartialEq for Filterable<T> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw && self.filtered == other.filtered
    }
}
impl<T: Debug> Debug for Filterable<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Filterable")
            .field("raw", &self.raw)
            .field("filtered", &self.filtered)
            .finish()
    }
}
