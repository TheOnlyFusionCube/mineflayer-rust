//! `mineflayer` is a Rust port of the [mineflayer](https://github.com/PrismarineJS/mineflayer)
//! Minecraft bot API. It exposes mineflayer's familiar surface — a `Bot` handle,
//! `bot.on(...)`-style events, and per-domain methods (`chat`, `world`,
//! `inventory`, `pathfinder`, …) — on top of the [`azalea`] crate, which
//! provides the protocol, ECS world state, physics, and pathfinding
//! underneath. The unit of truth for API coverage is `docs/api-parity.md`;
//! every public mineflayer API is tracked there as `pending`, `ported`,
//! `diverges`, or `wontfix`.

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
pub mod windows;
pub mod world;

pub use bot::{Auth, Bot, BotBuilder, BotEvent};
pub use error::{Error, Result};
pub use misc::{
    ChatVisibility, Creative, Instrument, ModelCustomization, Particle, ParticleStatus, Settings,
    TitleKind,
};
pub use scoreboard::{BossBar, ScoreBoard, ScoreItem, ScoreboardSlot, Team};
