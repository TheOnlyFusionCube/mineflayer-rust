use std::{
    collections::HashMap,
    hash::Hash,
    io::{self, Cursor, Write},
    sync::Arc,
};

use indexmap::IndexMap;

use crate::{
    BufReadError, MAX_STRING_LENGTH, McBuf, McBufLimited, McBufVar, UnsizedByteArray, read_bytes,
    read_utf_with_len, write_utf_with_len,
};

impl McBuf for UnsizedByteArray {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        // read to end of the buffer
        let data = buf.get_ref()[buf.position() as usize..].to_vec();
        buf.set_position((buf.position()) + data.len() as u64);
        Ok(UnsizedByteArray(data))
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        buf.write_all(self)
    }
}

macro_rules! impl_for_map_type {
    ($ty: ident) => {
        impl<K: McBuf + Eq + Hash, V: McBuf> McBuf for $ty<K, V> {
            fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
                let length = i32::mc_read_var(buf)? as usize;
                let mut contents = Self::with_capacity(usize::min(length, 65536));
                for _ in 0..length {
                    contents.insert(K::mc_read(buf)?, V::mc_read(buf)?);
                }
                Ok(contents)
            }
            fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
                u32::mc_write_var(&(self.len() as u32), buf)?;
                for (key, value) in self {
                    key.mc_write(buf)?;
                    value.mc_write(buf)?;
                }

                Ok(())
            }
        }
        impl<K: McBuf + Eq + Hash, V: McBufVar> McBufVar for $ty<K, V> {
            fn mc_read_var(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
                let length = i32::mc_read_var(buf)? as usize;
                let mut contents = Self::with_capacity(usize::min(length, 65536));
                for _ in 0..length {
                    contents.insert(K::mc_read(buf)?, V::mc_read_var(buf)?);
                }
                Ok(contents)
            }
            fn mc_write_var(&self, buf: &mut impl Write) -> io::Result<()> {
                u32::mc_write_var(&(self.len() as u32), buf)?;
                for (key, value) in self {
                    key.mc_write(buf)?;
                    value.mc_write_var(buf)?;
                }

                Ok(())
            }
        }
    };
}

impl_for_map_type!(HashMap);
impl_for_map_type!(IndexMap);

macro_rules! impl_for_list_type {
    ($ty: ty) => {
        impl<T: McBuf> McBuf for $ty {
            default fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
                let length = u32::mc_read_var(buf)? as usize;
                // we limit the capacity to not get exploited into allocating a bunch
                let mut contents = Vec::with_capacity(usize::min(length, 65536));
                for _ in 0..length {
                    contents.push(T::mc_read(buf)?);
                }
                Ok(contents.into())
            }
            default fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
                (self.len() as u32).mc_write_var(buf)?;
                for item in self.iter() {
                    T::mc_write(item, buf)?;
                }
                Ok(())
            }
        }
        impl<T: McBufVar> McBufVar for $ty {
            fn mc_read_var(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
                let length = i32::mc_read_var(buf)? as usize;
                let mut contents = Vec::with_capacity(usize::min(length, 65536));
                for _ in 0..length {
                    contents.push(T::mc_read_var(buf)?);
                }
                Ok(contents.into())
            }
            fn mc_write_var(&self, buf: &mut impl Write) -> io::Result<()> {
                (self.len() as u32).mc_write_var(buf)?;
                for item in self.iter() {
                    T::mc_write_var(item, buf)?;
                }
                Ok(())
            }
        }
        impl<T: McBuf> McBufLimited for $ty {
            fn mc_read_limited(buf: &mut Cursor<&[u8]>, limit: u32) -> Result<Self, BufReadError> {
                let length = u32::mc_read_var(buf)?;
                if length > limit {
                    return Err(BufReadError::VecLengthTooLong {
                        length: length as u32,
                        max_length: limit as u32,
                    });
                }

                let mut contents = Vec::with_capacity(u32::min(length, 65536) as usize);
                for _ in 0..length {
                    contents.push(T::mc_read(buf)?);
                }
                Ok(contents.into())
            }
        }
    };
}

impl_for_list_type!(Vec<T>);
impl_for_list_type!(Box<[T]>);
// `Arc<[T]>` is deliberately not implemented here, because converting a
// `Vec<T>` to `Arc<[T]>` results in allocations (since `Arc` stores the
// counters on the heap) and we'd like to avoid that. for us it's typically
// better to do `Arc<Box<[T]>>`.

impl McBuf for Vec<u8> {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        let length = i32::mc_read_var(buf)? as usize;
        read_bytes(buf, length).map(|b| b.to_vec())
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        (self.len() as u32).mc_write_var(buf)?;
        buf.write_all(self)
    }
}

impl McBuf for String {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        read_utf_with_len(buf, MAX_STRING_LENGTH).map(Into::into)
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        write_utf_with_len(buf, self, MAX_STRING_LENGTH)
    }
}
impl McBufLimited for String {
    fn mc_read_limited(buf: &mut Cursor<&[u8]>, limit: u32) -> Result<Self, BufReadError> {
        read_utf_with_len(buf, limit).map(Into::into)
    }
}

impl McBuf for Box<str> {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        read_utf_with_len(buf, MAX_STRING_LENGTH).map(Into::into)
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        write_utf_with_len(buf, self, MAX_STRING_LENGTH)
    }
}

impl McBufLimited for Box<str> {
    fn mc_read_limited(buf: &mut Cursor<&[u8]>, limit: u32) -> Result<Self, BufReadError> {
        String::mc_read_limited(buf, limit).map(Into::into)
    }
}

impl<T: McBuf> McBuf for Option<T> {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        let present = bool::mc_read(buf)?;
        Ok(if present {
            Some(T::mc_read(buf)?)
        } else {
            None
        })
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        if let Some(s) = self {
            true.mc_write(buf)?;
            s.mc_write(buf)?;
        } else {
            false.mc_write(buf)?;
        };
        Ok(())
    }
}

impl<T: McBufVar> McBufVar for Option<T> {
    fn mc_read_var(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        let present = bool::mc_read(buf)?;
        Ok(if present {
            Some(T::mc_read_var(buf)?)
        } else {
            None
        })
    }
    fn mc_write_var(&self, buf: &mut impl Write) -> io::Result<()> {
        if let Some(s) = self {
            true.mc_write(buf)?;
            s.mc_write_var(buf)?;
        } else {
            false.mc_write(buf)?;
        };
        Ok(())
    }
}
impl<T: McBufLimited> McBufLimited for Option<T> {
    fn mc_read_limited(buf: &mut Cursor<&[u8]>, limit: u32) -> Result<Self, BufReadError> {
        let present = bool::mc_read(buf)?;
        Ok(if present {
            Some(T::mc_read_limited(buf, limit)?)
        } else {
            None
        })
    }
}

impl<T: McBuf, const N: usize> McBuf for [T; N] {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        let mut contents = Vec::with_capacity(N);
        for _ in 0..N {
            contents.push(T::mc_read(buf)?);
        }
        Ok(contents
            .try_into()
            .unwrap_or_else(|_| unreachable!("The vec is the same size as the array")))
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        for i in self {
            i.mc_write(buf)?;
        }
        Ok(())
    }
}

impl McBuf for simdnbt::owned::NbtTag {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        Ok(simdnbt::owned::read_tag(buf).map_err(simdnbt::Error::from)?)
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        let mut data = Vec::new();
        self.write(&mut data);
        buf.write_all(&data)
    }
}

impl McBuf for simdnbt::owned::NbtCompound {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        match simdnbt::owned::read_tag(buf).map_err(simdnbt::Error::from)? {
            simdnbt::owned::NbtTag::Compound(compound) => Ok(compound),
            _ => Err(BufReadError::Custom("Expected compound tag".to_owned())),
        }
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        let mut data = Vec::new();
        simdnbt::owned::NbtTag::Compound(self.clone()).write(&mut data);
        buf.write_all(&data)
    }
}

impl McBuf for simdnbt::owned::Nbt {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        Ok(simdnbt::owned::read_unnamed(buf)?)
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        let mut data = Vec::new();
        self.write_unnamed(&mut data);
        buf.write_all(&data)
    }
}

impl<T> McBuf for Box<T>
where
    T: McBuf,
{
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        T::mc_read(buf).map(Box::new)
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        T::mc_write(&**self, buf)
    }
}

impl<A: McBuf, B: McBuf> McBuf for (A, B) {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        Ok((A::mc_read(buf)?, B::mc_read(buf)?))
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        self.0.mc_write(buf)?;
        self.1.mc_write(buf)
    }
}

impl<T: McBuf> McBuf for Arc<T> {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        Ok(Arc::new(T::mc_read(buf)?))
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        T::mc_write(&**self, buf)
    }
}
