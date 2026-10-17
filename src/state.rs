//! Bot state reads — the "Health / food / experience / oxygen" and
//! `bot.game.*` properties.
//!
//! All of these read ECS components that the engine removes on disconnect, so
//! they degrade to defaults/None instead of panicking like the raw
//! `mineflayer_engine::Client` accessors do.

use mineflayer_engine::{
    core::game_type::GameMode,
    entity::metadata::{AirSupply, Health},
    local_player::{Experience, Hunger, LocalGameMode},
    world::WorldName,
};

use crate::Bot;

impl Bot {
    /// `bot.isAlive` — alive and in the world. With the engine's always-on
    /// auto-respawn this is rarely `false` for long; watch
    /// [`crate::BotEvent::Death`] if you need the moment itself.
    pub fn is_alive(&self) -> bool {
        self.client.is_alive()
    }

    /// `bot.health` — typically `0..=20`. `0.0` when the component is gone
    /// (dead, or after disconnect).
    pub fn health(&self) -> f32 {
        self.client
            .get_component::<Health>()
            .map(|h| h.0)
            .unwrap_or(0.0)
    }

    /// `bot.food` — the hunger bar, `0..=20`. Defaults to `20` when the
    /// component is absent.
    pub fn food(&self) -> u32 {
        self.hunger().food
    }

    /// `bot.foodSaturation`.
    pub fn food_saturation(&self) -> f32 {
        self.hunger().saturation
    }

    /// `bot.oxygenLevel` — `0..=20`. The engine tracks air in ticks (`0..=300`),
    /// converted with `round(air / 15)` exactly like upstream.
    pub fn oxygen_level(&self) -> u8 {
        let air = self
            .client
            .get_component::<AirSupply>()
            .map(|a| a.0)
            .unwrap_or(300);
        (air.clamp(0, 300) as f32 / 15.0).round() as u8
    }

    /// `bot.experience` — the engine's `{ progress, level, total }`; upstream's
    /// `experience.points` is `total` here.
    pub fn experience(&self) -> Experience {
        self.client
            .get_component::<Experience>()
            .map(|e| (*e).clone())
            .unwrap_or_default()
    }

    /// `bot.game.gameMode` — `None` before the login packet arrives.
    pub fn game_mode(&self) -> Option<GameMode> {
        self.client
            .get_component::<LocalGameMode>()
            .map(|g| g.current)
    }

    /// `bot.game.dimension` — the world/dimension identifier (e.g.
    /// `minecraft:overworld`); `None` before login.
    pub fn dimension(&self) -> Option<WorldName> {
        self.client
            .get_component::<WorldName>()
            .map(|w| (*w).clone())
    }

    fn hunger(&self) -> Hunger {
        self.client
            .get_component::<Hunger>()
            .map(|h| (*h).clone())
            .unwrap_or_default()
    }
}
