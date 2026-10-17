use std::{
    io::{self, Cursor, Write},
    ops::Deref,
};

use indexmap::IndexMap;
use mineflayer_buf::{BufReadError, McBuf, McBufVar};
use mineflayer_registry::identifier::Identifier;

#[derive(Clone, Debug, PartialEq)]
pub struct TagMap(pub IndexMap<Identifier, Vec<Tags>>);

#[derive(Clone, Debug, PartialEq)]
pub struct Tags {
    pub name: Identifier,
    pub elements: Vec<i32>,
}

impl McBuf for TagMap {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        let length = u32::mc_read_var(buf)? as usize;
        let mut data = IndexMap::new();
        for _ in 0..length {
            let tag_type = Identifier::mc_read(buf)?;
            let tags_count = i32::mc_read_var(buf)? as usize;
            let mut tags_vec = Vec::new();
            for _ in 0..tags_count {
                let tags = Tags::mc_read(buf)?;
                tags_vec.push(tags);
            }
            data.insert(tag_type, tags_vec);
        }
        Ok(TagMap(data))
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        (self.len() as u32).mc_write_var(buf)?;
        for (k, v) in &self.0 {
            k.mc_write(buf)?;
            v.mc_write(buf)?;
        }
        Ok(())
    }
}
impl McBuf for Tags {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        let name = Identifier::mc_read(buf)?;
        let elements = Vec::<i32>::mc_read_var(buf)?;
        Ok(Tags { name, elements })
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        self.name.mc_write(buf)?;
        self.elements.mc_write_var(buf)?;
        Ok(())
    }
}

impl Deref for TagMap {
    type Target = IndexMap<Identifier, Vec<Tags>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
