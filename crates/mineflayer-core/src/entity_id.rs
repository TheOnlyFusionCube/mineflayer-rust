use std::{
    fmt::{self, Display},
    hash::{Hash, Hasher},
    io::{self, Cursor},
};

use derive_more::{Deref, DerefMut};
use mineflayer_buf::{BufReadError, McBuf, McBufVar};

// note: this is here instead of in mineflayer-entity because mineflayer-world
// depends on it. and this isn't in mineflayer-world so mineflayer-protocol
// doesn't need to depend on mineflayer-world.

/// An entity ID used by Minecraft.
///
/// These IDs are picked by the server. Some server software (like Bungeecord)
/// may pick entity IDs per-player, so you should avoid relying on them for
/// identifying IDs (especially if you're using a shared world -- i.e. a swarm).
///
/// You might find [`Entity`] more useful, since that's an ID decided by us that
/// is likely to be correct across shared worlds. You could also use the
/// `EntityUuid` from `mineflayer_entity`, that one is unlikely to change even
/// across server restarts.
///
/// This serializes as a i32. Usually it's a VarInt in the protocol, but not
/// always. If you do need it to serialize as a VarInt, make sure to use use the
/// `#[var]` attribute.
///
/// [`Entity`]: bevy_ecs::entity::Entity
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "bevy_ecs", derive(bevy_ecs::component::Component))]
#[derive(Clone, Copy, Debug, Default, Deref, DerefMut, Eq, PartialEq)]
pub struct MinecraftEntityId(pub i32);

impl Hash for MinecraftEntityId {
    fn hash<H: Hasher>(&self, hasher: &mut H) {
        hasher.write_i32(self.0);
    }
}
impl nohash_hasher::IsEnabled for MinecraftEntityId {}

// we can't have the default be #[var] because mojang doesn't use varints for
// entities sometimes :(
impl McBuf for MinecraftEntityId {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        i32::mc_read(buf).map(MinecraftEntityId)
    }
    fn mc_write(&self, buf: &mut impl io::Write) -> io::Result<()> {
        i32::mc_write(&self.0, buf)
    }
}
impl McBufVar for MinecraftEntityId {
    fn mc_read_var(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        i32::mc_read_var(buf).map(MinecraftEntityId)
    }
    fn mc_write_var(&self, buf: &mut impl io::Write) -> io::Result<()> {
        i32::mc_write_var(&self.0, buf)
    }
}
impl Display for MinecraftEntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "eid({})", self.0)
    }
}
impl From<i32> for MinecraftEntityId {
    fn from(id: i32) -> Self {
        Self(id)
    }
}
impl From<u32> for MinecraftEntityId {
    fn from(id: u32) -> Self {
        Self(id as i32)
    }
}
