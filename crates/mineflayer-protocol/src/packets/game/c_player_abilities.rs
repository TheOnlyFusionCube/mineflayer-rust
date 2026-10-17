use std::io::{self, Cursor, Write};

use mineflayer_buf::{McBuf, BufReadError};
use mineflayer_core::bitset::FixedBitSet;
use mineflayer_entity::PlayerAbilities;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundPlayerAbilities {
    pub flags: PlayerAbilitiesFlags,
    pub flying_speed: f32,
    /// Used for the fov
    pub walking_speed: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlayerAbilitiesFlags {
    pub invulnerable: bool,
    pub flying: bool,
    pub can_fly: bool,
    pub instant_break: bool,
}

impl McBuf for PlayerAbilitiesFlags {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        let set = FixedBitSet::<4>::mc_read(buf)?;
        Ok(PlayerAbilitiesFlags {
            invulnerable: set.index(0),
            flying: set.index(1),
            can_fly: set.index(2),
            instant_break: set.index(3),
        })
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        let mut set = FixedBitSet::<4>::new();
        if self.invulnerable {
            set.set(0);
        }
        if self.flying {
            set.set(1);
        }
        if self.can_fly {
            set.set(2);
        }
        if self.instant_break {
            set.set(3);
        }
        set.mc_write(buf)
    }
}

impl From<&ClientboundPlayerAbilities> for PlayerAbilities {
    fn from(packet: &ClientboundPlayerAbilities) -> Self {
        Self {
            invulnerable: packet.flags.invulnerable,
            flying: packet.flags.flying,
            can_fly: packet.flags.can_fly,
            instant_break: packet.flags.instant_break,
            flying_speed: packet.flying_speed,
            walking_speed: packet.walking_speed,
        }
    }
}
