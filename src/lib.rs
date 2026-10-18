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
mod op;
mod plugins;

pub mod chat;
pub mod craft;
pub mod entity;
pub mod interact;
pub mod inventory;
pub mod mcdata;
pub mod misc;
pub mod movement;
pub mod pathfinder;
pub mod plugin;
pub mod scoreboard;
pub mod state;
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

#[cfg(any(test, feature = "test-util"))]
pub mod test_util;

/// The README's Rust blocks, compiled as doctests.
#[doc = include_str!("../README.md")]
#[cfg(doctest)]
pub struct ReadmeDoctests;

/// The porting guide's Rust blocks, compiled as doctests.
#[doc = include_str!("../docs/porting.md")]
#[cfg(doctest)]
pub struct PortingDoctests;
