use std::{
    io,
    io::{Cursor, Write},
};

use mineflayer_buf::{McBuf, McBufVar, BufReadError};
use mineflayer_chat::FormattedText;
use mineflayer_core::bitset::FixedBitSet;
use mineflayer_protocol_macros::ClientboundGamePacket;
use uuid::Uuid;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundBossEvent {
    pub id: Uuid,
    pub operation: Operation,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Operation {
    Add(AddOperation),
    Remove,
    UpdateProgress(f32),
    UpdateName(FormattedText),
    UpdateStyle(Style),
    UpdateProperties(Properties),
}

impl McBuf for Operation {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        let operation_id = u32::mc_read_var(buf)?;
        Ok(match operation_id {
            0 => Operation::Add(AddOperation::mc_read(buf)?),
            1 => Operation::Remove,
            2 => Operation::UpdateProgress(f32::mc_read(buf)?),
            3 => Operation::UpdateName(FormattedText::mc_read(buf)?),
            4 => Operation::UpdateStyle(Style::mc_read(buf)?),
            5 => Operation::UpdateProperties(Properties::mc_read(buf)?),
            _ => {
                return Err(BufReadError::UnexpectedEnumVariant {
                    id: operation_id as i32,
                });
            }
        })
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        match self {
            Operation::Add(add) => {
                0u32.mc_write_var(buf)?;
                add.mc_write(buf)?;
            }
            Operation::Remove => {
                1u32.mc_write_var(buf)?;
            }
            Operation::UpdateProgress(progress) => {
                2u32.mc_write_var(buf)?;
                progress.mc_write(buf)?;
            }
            Operation::UpdateName(name) => {
                3u32.mc_write_var(buf)?;
                name.mc_write(buf)?;
            }
            Operation::UpdateStyle(style) => {
                4u32.mc_write_var(buf)?;
                style.mc_write(buf)?;
            }
            Operation::UpdateProperties(properties) => {
                5u32.mc_write_var(buf)?;
                properties.mc_write(buf)?;
            }
        }
        Ok(())
    }
}

#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct AddOperation {
    pub name: FormattedText,
    pub progress: f32,
    pub style: Style,
    pub properties: Properties,
}

#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct Style {
    pub color: BossBarColor,
    pub overlay: BossBarOverlay,
}

#[derive(McBuf, Clone, Copy, Debug, PartialEq)]
pub enum BossBarColor {
    Pink = 0,
    Blue = 1,
    Red = 2,
    Green = 3,
    Yellow = 4,
    Purple = 5,
    White = 6,
}

#[derive(McBuf, Clone, Copy, Debug, PartialEq)]
pub enum BossBarOverlay {
    Progress = 0,
    Notched6 = 1,
    Notched10 = 2,
    Notched12 = 3,
    Notched20 = 4,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Properties {
    pub darken_screen: bool,
    pub play_music: bool,
    pub create_world_fog: bool,
}

impl McBuf for Properties {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        let set = FixedBitSet::<3>::mc_read(buf)?;
        Ok(Self {
            darken_screen: set.index(0),
            play_music: set.index(1),
            create_world_fog: set.index(2),
        })
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        let mut set = FixedBitSet::<3>::new();
        if self.darken_screen {
            set.set(0);
        }
        if self.play_music {
            set.set(1);
        }
        if self.create_world_fog {
            set.set(2);
        }
        set.mc_write(buf)?;
        Ok(())
    }
}
