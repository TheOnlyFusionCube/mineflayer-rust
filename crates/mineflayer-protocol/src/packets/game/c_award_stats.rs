use std::collections::HashMap;

use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_registry::builtin::{BlockKind, CustomStat, EntityKind, ItemKind};

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundAwardStats {
    #[var]
    pub stats: HashMap<Stat, i32>,
}

#[derive(McBuf, Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Stat {
    Mined(BlockKind),
    Crafted(ItemKind),
    Used(ItemKind),
    Broken(ItemKind),
    PickedUp(ItemKind),
    Dropped(ItemKind),
    Killed(EntityKind),
    KilledBy(EntityKind),
    Custom(CustomStat),
}
