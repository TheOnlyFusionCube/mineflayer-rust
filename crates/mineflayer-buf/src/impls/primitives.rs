use std::io::{self, Cursor, Read, Write};

use byteorder::{BE, ReadBytesExt, WriteBytesExt};
use tracing::warn;

use crate::{BufReadError, McBuf, McBufVar};

impl McBuf for () {
    fn mc_read(_buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        Ok(())
    }
    fn mc_write(&self, _buf: &mut impl Write) -> io::Result<()> {
        Ok(())
    }
}

impl McBuf for i32 {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        Ok(buf.read_i32::<BE>()?)
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        buf.write_i32::<BE>(*self)
    }
}

impl McBufVar for i32 {
    /// Read a single varint from the reader and return the value
    fn mc_read_var(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        // fast varint impl based on https://github.com/luojia65/mc-varint/blob/master/src/lib.rs#L67
        let mut buffer = [0];
        let mut ans = 0;
        for i in 0..5 {
            buf.read_exact(&mut buffer)?;
            ans |= ((buffer[0] & 0b0111_1111) as i32) << (7 * i);
            if buffer[0] & 0b1000_0000 == 0 {
                break;
            }
        }
        Ok(ans)
    }

    fn mc_write_var(&self, buf: &mut impl Write) -> io::Result<()> {
        let mut buffer = [0];
        let mut value = *self;
        if value == 0 {
            buf.write_all(&buffer)?;
        }
        while value != 0 {
            buffer[0] = (value & 0b0111_1111) as u8;
            value = (value >> 7) & (i32::MAX >> 6);
            if value != 0 {
                buffer[0] |= 0b1000_0000;
            }
            buf.write_all(&buffer)?;
        }
        Ok(())
    }
}

impl McBufVar for i64 {
    fn mc_read_var(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        let mut buffer = [0];
        let mut ans = 0;
        for i in 0..10 {
            buf.read_exact(&mut buffer)
                .map_err(|_| BufReadError::InvalidVarLong)?;
            ans |= ((buffer[0] & 0b0111_1111) as i64) << (7 * i);
            if buffer[0] & 0b1000_0000 == 0 {
                break;
            }
        }
        Ok(ans)
    }

    fn mc_write_var(&self, buf: &mut impl Write) -> io::Result<()> {
        let mut buffer = [0];
        let mut value = *self;
        if value == 0 {
            buf.write_all(&buffer).unwrap();
        }
        while value != 0 {
            buffer[0] = (value & 0b0111_1111) as u8;
            value = (value >> 7) & (i64::MAX >> 6);
            if value != 0 {
                buffer[0] |= 0b1000_0000;
            }
            buf.write_all(&buffer)?;
        }
        Ok(())
    }
}

impl McBufVar for u64 {
    fn mc_read_var(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        i64::mc_read_var(buf).map(|i| i as u64)
    }
    fn mc_write_var(&self, buf: &mut impl Write) -> io::Result<()> {
        i64::mc_write_var(&(*self as i64), buf)
    }
}

impl McBuf for u32 {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        Ok(i32::mc_read(buf)? as u32)
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        i32::mc_write(&(*self as i32), buf)
    }
}

impl McBufVar for u32 {
    fn mc_read_var(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        Ok(i32::mc_read_var(buf)? as u32)
    }
    fn mc_write_var(&self, buf: &mut impl Write) -> io::Result<()> {
        i32::mc_write_var(&(*self as i32), buf)
    }
}

impl McBuf for u16 {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        i16::mc_read(buf).map(|i| i as u16)
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        i16::mc_write(&(*self as i16), buf)
    }
}

impl McBuf for i16 {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        Ok(buf.read_i16::<BE>()?)
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        buf.write_i16::<BE>(*self)
    }
}

impl McBufVar for u16 {
    fn mc_read_var(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        Ok(i32::mc_read_var(buf)? as u16)
    }
    fn mc_write_var(&self, buf: &mut impl Write) -> io::Result<()> {
        i32::mc_write_var(&(*self as i32), buf)
    }
}

impl McBuf for i64 {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        Ok(buf.read_i64::<BE>()?)
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        buf.write_i64::<BE>(*self)
    }
}

impl McBuf for u64 {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        i64::mc_read(buf).map(|i| i as u64)
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        buf.write_u64::<BE>(*self)
    }
}

impl McBuf for bool {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        let byte = u8::mc_read(buf)?;
        if byte > 1 {
            warn!("Boolean value was not 0 or 1, but {byte}");
        }
        Ok(byte != 0)
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        let byte = u8::from(*self);
        byte.mc_write(buf)
    }
}

impl McBuf for u8 {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        Ok(buf.read_u8()?)
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        buf.write_u8(*self)
    }
}

impl McBuf for i8 {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        u8::mc_read(buf).map(|i| i as i8)
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        (*self as u8).mc_write(buf)
    }
}

impl McBuf for f32 {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        Ok(buf.read_f32::<BE>()?)
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        buf.write_f32::<BE>(*self)
    }
}

impl McBuf for f64 {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        Ok(buf.read_f64::<BE>()?)
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        buf.write_f64::<BE>(*self)
    }
}
