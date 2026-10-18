//! `mineflayer` is a Rust port of the [mineflayer](https://github.com/PrismarineJS/mineflayer)
//! Minecraft bot API. It exposes mineflayer's familiar surface — a `Bot` handle,
//! `bot.on(...)`-style events, and per-domain methods (`chat`, `world`,
//! `inventory`, `pathfinder`, …) — on top of the in-tree `mineflayer-engine`
//! crate ([`engine`]), which provides the protocol, ECS world state, physics,
//! and pathfinding underneath. The unit of truth for API coverage is `docs/api-parity.md`;
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

pub use bot::{Auth, Bot, BotBuilder, BotEvent, WaitEnd};
pub use error::{Error, Result};
pub use movement::{Control, ControlState, PhysicsSettings};

pub use chat::{
    ChatMessage, ChatPattern, ChatPatternKey, ChatPatternMatch, ChatPatternOpts, MessageMatcher,
    MessagePosition, TabList,
};
/// The client engine under the port (protocol, ECS world, physics,
/// pathfinding). Re-exported so a dependent crate names engine types through
/// this crate (`mineflayer::engine::...`) and always gets the same version.
pub use mineflayer_engine as engine;
// Engine value types that appear in this crate's public signatures — users
// should not need their own engine dependency to call them.
// `ItemKind`/`MenuKind`/`DataComponentPatch` appear in the public
// inventory API (toss/transfer/deposit/Window::menu_kind).
pub use craft::{DeltaEntry, Ingredient, Recipe, RecipeItem, RecipeResult};
pub use entity::{Painting, Player, ProfileKeys};
pub use inventory::{ContainerTarget, EquipDest, QUICK_BAR_START, TransferOpts};
pub use mineflayer_engine::inventory::operations::ClickOperation;
/// `BotBuilder::proxy` — the engine's SOCKS5 proxy type (`options.agent`-analog).
pub use mineflayer_engine::protocol::connect::Proxy;
pub use mineflayer_engine::{
    BlockPos, EntityRef, Vec3,
    inventory::{DataComponentPatch, ItemStack},
    registry::{
        builtin::{BlockKind, ItemKind, MenuKind},
        data::Biome,
    },
};
pub use misc::{
    ChatVisibility, Creative, HumanoidArm, Instrument, ModelCustomization, Particle,
    ParticleStatus, Settings, TitleKind, ViewDistance,
};
pub use pathfinder::{Goal, Movements, PathError, PathResult, PathStatus, Pathfinder};
pub use scoreboard::{BossBar, ScoreBoard, ScoreItem, ScoreboardSlot, Team};
/// Player UUIDs in the public API ([`Bot::player_by_uuid`]).
pub use uuid::Uuid;
pub use window_types::{
    Anvil, CommandBlockMode, CommandBlockOpts, EnchantmentOption, EnchantmentTable,
    ExpectedEnchant, Furnace, Villager, VillagerTrade,
};
pub use windows::{Chest, Dispenser, Item, Window};
pub use world::{
    Block, BlockMatcher, BoundingBox, DEFAULT_CURSOR_DISTANCE, DEFAULT_ENTITY_CURSOR_DISTANCE,
    FindBlocksOpts, Game, Location, Time, WorldRef,
};

pub use interact::{DigAbortReason, DigFace, ForceLook};
pub use vec3::Vec3Ext;

// — ecosystem-plugin analogs (U12): mineflayer-collectblock, -tool, -pvp, -auto-eat —
pub use auto_eat::{AutoEat, AutoEatOpts, EatOpts, FoodData, FoodPriority, FoodSelection};
pub use collect::{CollectBlock, CollectOpts, CollectTarget, Collected, ItemFilter};
pub use pvp::{AttackRate, Pvp};
pub use tool::{EquipOpts, Tool};

/// The common imports for a bot: the handle, its events, the error type,
/// and the engine value types that appear in signatures — so examples and
/// dependent crates never name the `mineflayer-engine` crate themselves.
pub mod prelude {
    pub use crate::bot::{Auth, Bot, BotBuilder, BotEvent};
    pub use crate::error::{Error, Result};
    pub use crate::movement::{Control, ControlState};
    pub use crate::pathfinder::{Goal, Movements};
    pub use crate::windows::{Item, Window};
    pub use crate::{
        BlockKind, BlockPos, EntityRef, EquipDest, FindBlocksOpts, ItemKind, ItemStack, Vec3,
    };
}

#[cfg(test)]
pub(crate) mod test_util {
    use std::ops::Deref as _;

    /// A `Client` whose own entity is already despawned — the state packets
    /// still in flight during teardown race into.
    pub(crate) fn despawned_client() -> mineflayer_engine::Client {
        let mut app = mineflayer_engine::app::App::new();
        // `Client::new` wants `Arc<RwLock<World>>`; `start_ecs_runner` hands
        // one back without naming the lock type.
        let (ecs, _run_systems, _exit_rx) = mineflayer_engine::start_ecs_runner(app.main_mut());
        let entity = {
            let mut ecs = ecs.write();
            let entity = ecs.spawn_empty().id();
            ecs.despawn(entity);
            entity
        };
        mineflayer_engine::Client::new(entity, ecs)
    }

    /// A `Client` whose own entity is alive and carries `bundle` — for
    /// tests that need components present (plugin state, pathfinder
    /// facade, inventory). No systems run, so ECS events are queued but
    /// never processed and tick broadcasters may not exist.
    pub(crate) fn live_entity_client(
        bundle: impl mineflayer_engine::ecs::bundle::Bundle,
    ) -> mineflayer_engine::Client {
        let mut app = mineflayer_engine::app::App::new();
        let (ecs, _run_systems, _exit_rx) = mineflayer_engine::start_ecs_runner(app.main_mut());
        let entity = ecs.write().spawn(bundle).id();
        mineflayer_engine::Client::new(entity, ecs)
    }

    /// A `Client` whose ECS carries a `TickBroadcast` — tick-driven waits
    /// park on `get_tick_broadcaster()` instead of panicking. No systems
    /// run, so ticks only arrive when the test sends them via
    /// [`tick_sender`].
    pub(crate) fn tick_client(
        bundle: impl mineflayer_engine::ecs::bundle::Bundle,
    ) -> mineflayer_engine::Client {
        let mut app = mineflayer_engine::app::App::new();
        app.add_plugins(mineflayer_engine::tick_broadcast::TickBroadcastPlugin);
        let (ecs, _run_systems, _exit_rx) = mineflayer_engine::start_ecs_runner(app.main_mut());
        let entity = ecs.write().spawn(bundle).id();
        mineflayer_engine::Client::new(entity, ecs)
    }

    /// The `TickBroadcast` sender — capacity 1, so two sends while a
    /// receiver is parked make its next `recv` report `Lagged(1)`, the
    /// routine scheduling stall the tick drivers must survive.
    pub(crate) fn tick_sender(
        client: &mineflayer_engine::Client,
    ) -> tokio::sync::broadcast::Sender<()> {
        client
            .ecs
            .read()
            .resource::<mineflayer_engine::tick_broadcast::TickBroadcast>()
            .deref()
            .clone()
    }

    /// A `Bot` over a prebuilt `Client` — the unit-test scaffold: no event
    /// bridge (so `Bot::events()` yields a silent stream), and a shutdown
    /// receiver that stays pending (the sender is leaked on purpose: a
    /// scaffold bot has no bridge to own it).
    pub(crate) fn bot(client: mineflayer_engine::Client) -> crate::Bot {
        let (events, _) = tokio::sync::broadcast::channel(crate::bot::EVENT_CHANNEL_CAPACITY);
        let (alive, _) = tokio::sync::watch::channel(());
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(());
        std::mem::forget(shutdown_tx);
        crate::Bot {
            client,
            bootstrap_events: std::sync::Arc::new(parking_lot::Mutex::new(Some(
                events.subscribe(),
            ))),
            _alive: Some(alive),
            shutdown: shutdown_rx,
            shared: std::sync::Arc::new(crate::bot::BotShared::new(events)),
            chat_length_limit: crate::chat::CHAT_LENGTH_LIMIT,
        }
    }

    /// A `Bot` whose `shutdown` receiver is wired to the returned sender —
    /// drop the sender to play the bridge's exit (disconnect), like
    /// `bridge::spawn` does.
    pub(crate) fn bot_with_shutdown(
        client: mineflayer_engine::Client,
    ) -> (crate::Bot, tokio::sync::watch::Sender<()>) {
        let mut bot = bot(client);
        let (tx, rx) = tokio::sync::watch::channel(());
        bot.shutdown = rx;
        (bot, tx)
    }
}
