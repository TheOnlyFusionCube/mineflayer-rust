use std::{
    io,
    io::{Cursor, Write},
};

use mineflayer_buf::{McBuf, BufReadError};
use mineflayer_core::position::BlockPos;
use mineflayer_protocol_macros::ServerboundGamePacket;
use mineflayer_registry::identifier::Identifier;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundSetJigsawBlock {
    pub pos: BlockPos,
    pub name: Identifier,
    pub target: Identifier,
    pub pool: Identifier,
    pub final_state: String,
    pub joint: String,
    #[var]
    pub selection_priority: i32,
    #[var]
    pub placement_priority: i32,
}

pub enum JointType {
    Rollable,
    Aligned,
}

impl McBuf for JointType {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        let name = String::mc_read(buf)?;
        match name.as_str() {
            "rollable" => Ok(JointType::Rollable),
            "aligned" => Ok(JointType::Aligned),
            _ => Err(BufReadError::UnexpectedStringEnumVariant { id: name }),
        }
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        match self {
            JointType::Rollable => "rollable".to_owned().mc_write(buf)?,
            JointType::Aligned => "aligned".to_owned().mc_write(buf)?,
        };
        Ok(())
    }
}
