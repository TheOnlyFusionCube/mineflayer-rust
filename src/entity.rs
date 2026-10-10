//! Entity and player state — the U1 slice: `bot.entity`, `bot.position`,
//! `bot.players`. The rest of the entity domain lands with the entities unit.

use std::collections::HashMap;

use azalea::{entity::Position, player::PlayerInfo};

use crate::Bot;

impl Bot {
    /// `bot.entity` — the bot's own entity as an azalea [`azalea::EntityRef`],
    /// usable anywhere a world entity is expected.
    pub fn entity(&self) -> azalea::EntityRef {
        self.client.entity()
    }

    /// `bot.entity.position` convenience — the bot's feet position.
    /// `Vec3::ZERO` until the first position arrives (azalea's own default).
    pub fn position(&self) -> azalea::Vec3 {
        self.client
            .get_component::<Position>()
            .map(|p| **p)
            .unwrap_or_default()
    }

    /// `bot.players` — the tab list, keyed by username (upstream keys it by
    /// username; azalea's `tab_list` is keyed by UUID).
    pub fn players(&self) -> HashMap<String, PlayerInfo> {
        self.client
            .get_component::<azalea::local_player::TabList>()
            .map(|tab_list| {
                tab_list
                    .values()
                    .map(|info| (info.profile.name.clone(), info.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }
}
