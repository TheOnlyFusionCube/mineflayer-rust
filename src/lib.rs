//! `mineflayer` is a Rust port of the [mineflayer](https://github.com/PrismarineJS/mineflayer)
//! Minecraft bot API. It exposes mineflayer's familiar surface — a `Bot` handle,
//! `bot.on(...)`-style events, and per-domain methods (`chat`, `world`,
//! `inventory`, `pathfinder`, …) — on top of the [`azalea`] crate, which
//! provides the protocol, ECS world state, physics, and pathfinding
//! underneath. The unit of truth for API coverage is `docs/api-parity.md`;
//! every public mineflayer API is tracked there as `pending`, `ported`,
//! `diverges`, or `wontfix`.
//!
//! Domain modules — each an `impl Bot` block mirroring a mineflayer docs
//! section: [`chat`] (chat/whisper/patterns/tablist), [`world`] (blocks,
//! raycasts, signs, time, weather), [`entity`] (entities, players, combat),
//! [`movement`] (control states, look, physics reads), [`interact`]
//! (dig/place/activate), [`pathfinder`] (goals and movements), [`inventory`]
//! (items, equip, transfer, openers), [`windows`] (`Window`/`Item`),
//! [`window_types`] (furnace/enchant/anvil/villager/book/command block),
//! [`craft`] (recipe book + crafting), [`misc`] (settings, bed, fish,
//! creative, resource packs), [`scoreboard`] (objectives, teams, boss bars),
//! [`state`] (health/food/experience/dimension reads). Runnable ports of
//! upstream's examples live in `examples/` (`echo`, `digger`, `chest`).

mod bot;
mod bridge;
mod error;

pub mod chat;
pub mod craft;
pub mod entity;
pub mod interact;
pub mod inventory;
pub mod misc;
pub mod movement;
pub mod pathfinder;
pub mod scoreboard;
pub mod state;
pub mod window_types;
pub mod windows;
pub mod world;

pub use bot::{Auth, Bot, BotBuilder, BotEvent};
pub use error::{Error, Result};
pub use movement::{Control, ControlState, PhysicsSettings};

pub use chat::{
    ChatPattern, ChatPatternKey, ChatPatternMatch, ChatPatternOpts, MessageMatcher,
    MessagePosition, TabList,
};
// ItemKind/MenuKind/DataComponentPatch appear in the public inventory API
// (toss/transfer/deposit/Window::menu_kind) — re-exported like `BlockPos`.
pub use azalea::{
    inventory::{DataComponentPatch, ItemStack},
    registry::builtin::{ItemKind, MenuKind},
};
pub use craft::{DeltaEntry, Ingredient, Recipe, RecipeItem, RecipeResult};
pub use inventory::{ContainerTarget, EquipDest, QUICK_BAR_START, TransferOpts};
pub use misc::{
    ChatVisibility, Creative, Instrument, ModelCustomization, Particle, ParticleStatus, Settings,
    TitleKind,
};
pub use pathfinder::{Goal, Movements, Pathfinder};
pub use scoreboard::{BossBar, ScoreBoard, ScoreItem, ScoreboardSlot, Team};
pub use window_types::{
    Anvil, CommandBlockMode, CommandBlockOpts, EnchantmentOption, EnchantmentTable,
    ExpectedEnchant, Furnace, Villager, VillagerTrade,
};
pub use windows::{Chest, Dispenser, Item, Window};
pub use world::{
    Block, BlockMatcher, DEFAULT_CURSOR_DISTANCE, DEFAULT_ENTITY_CURSOR_DISTANCE, FindBlocksOpts,
    Game, Time, WorldRef,
};

pub use interact::{DigAbortReason, DigFace, ForceLook};

#[cfg(test)]
pub(crate) mod test_util {
    /// A `Client` whose own entity is already despawned — the state packets
    /// still in flight during teardown race into.
    pub(crate) fn despawned_client() -> azalea::Client {
        let mut app = azalea::app::App::new();
        // `Client::new` wants `Arc<RwLock<World>>`; `start_ecs_runner` hands
        // one back without naming parking_lot (not a direct dependency).
        let (ecs, _run_systems, _exit_rx) = azalea::start_ecs_runner(app.main_mut());
        let entity = {
            let mut ecs = ecs.write();
            let entity = ecs.spawn_empty().id();
            ecs.despawn(entity);
            entity
        };
        azalea::Client::new(entity, ecs)
    }
}
