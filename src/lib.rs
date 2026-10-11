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

pub mod auto_eat;
pub mod chat;
pub mod collect;
pub mod craft;
pub mod entity;
pub mod interact;
pub mod inventory;
pub mod mcdata;
pub mod misc;
pub mod movement;
pub mod pathfinder;
pub mod pvp;
pub mod scoreboard;
pub mod state;
pub mod tool;
pub mod vec3;
pub mod window_types;
pub mod windows;
pub mod world;

pub use bot::{Auth, Bot, BotBuilder, BotEvent};
pub use error::{Error, Result};
pub use movement::{Control, ControlState, PhysicsSettings};

pub use chat::{
    ChatMessage, ChatPattern, ChatPatternKey, ChatPatternMatch, ChatPatternOpts, MessageMatcher,
    MessagePosition, TabList,
};
// ItemKind/MenuKind/DataComponentPatch appear in the public inventory API
// (toss/transfer/deposit/Window::menu_kind) — re-exported like `BlockPos`.
/// `BotBuilder::proxy` — azalea's SOCKS5 proxy type (`options.agent`-analog).
pub use azalea::protocol::connect::Proxy;
pub use azalea::{
    Vec3,
    inventory::{DataComponentPatch, ItemStack},
    registry::{
        builtin::{ItemKind, MenuKind},
        data::Biome,
    },
};
pub use craft::{DeltaEntry, Ingredient, Recipe, RecipeItem, RecipeResult};
pub use entity::{Painting, Player, ProfileKeys};
pub use inventory::{ContainerTarget, EquipDest, QUICK_BAR_START, TransferOpts};
pub use misc::{
    ChatVisibility, Creative, HumanoidArm, Instrument, ModelCustomization, Particle,
    ParticleStatus, Settings, TitleKind,
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
    Game, Location, Time, WorldRef,
};

pub use interact::{DigAbortReason, DigFace, ForceLook};
pub use vec3::Vec3Ext;

// — ecosystem-plugin analogs (U12): mineflayer-collectblock, -tool, -pvp, -auto-eat —
pub use auto_eat::{AutoEat, AutoEatOpts, EatOpts, FoodData, FoodPriority, FoodSelection};
pub use collect::{CollectBlock, CollectOpts, CollectTarget, Collected, ItemFilter};
pub use pvp::{AttackRate, Pvp};
pub use tool::{EquipOpts, Tool};

#[cfg(test)]
pub(crate) mod test_util {
    use std::ops::Deref as _;

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

    /// A `Client` whose own entity is alive and carries `bundle` — for
    /// tests that need components present (plugin state, pathfinder
    /// facade, inventory). No systems run, so ECS events are queued but
    /// never processed and tick broadcasters may not exist.
    pub(crate) fn live_entity_client(bundle: impl azalea::ecs::bundle::Bundle) -> azalea::Client {
        let mut app = azalea::app::App::new();
        let (ecs, _run_systems, _exit_rx) = azalea::start_ecs_runner(app.main_mut());
        let entity = ecs.write().spawn(bundle).id();
        azalea::Client::new(entity, ecs)
    }

    /// A `Client` whose ECS carries a `TickBroadcast` — tick-driven waits
    /// park on `get_tick_broadcaster()` instead of panicking. No systems
    /// run, so ticks only arrive when the test sends them via
    /// [`tick_sender`].
    pub(crate) fn tick_client(bundle: impl azalea::ecs::bundle::Bundle) -> azalea::Client {
        let mut app = azalea::app::App::new();
        app.add_plugins(azalea::tick_broadcast::TickBroadcastPlugin);
        let (ecs, _run_systems, _exit_rx) = azalea::start_ecs_runner(app.main_mut());
        let entity = ecs.write().spawn(bundle).id();
        azalea::Client::new(entity, ecs)
    }

    /// The `TickBroadcast` sender — capacity 1, so two sends while a
    /// receiver is parked make its next `recv` report `Lagged(1)`, the
    /// routine scheduling stall the tick drivers must survive.
    pub(crate) fn tick_sender(client: &azalea::Client) -> tokio::sync::broadcast::Sender<()> {
        client
            .ecs
            .read()
            .resource::<azalea::tick_broadcast::TickBroadcast>()
            .deref()
            .clone()
    }

    /// A `Bot` whose `shutdown` receiver is wired to the returned sender —
    /// drop the sender to play the bridge's exit (disconnect), like
    /// `bridge::spawn` does.
    pub(crate) fn bot_with_shutdown(
        client: azalea::Client,
    ) -> (crate::Bot, tokio::sync::watch::Sender<()>) {
        let mut bot = crate::Bot::new(client);
        let (tx, rx) = tokio::sync::watch::channel(());
        bot.shutdown = rx;
        (bot, tx)
    }
}
