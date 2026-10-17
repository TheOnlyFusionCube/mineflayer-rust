#![doc = include_str!("../README.md")]
#![feature(type_changing_struct_update)]

pub mod accept_resource_packs;
pub mod auto_reconnect;
pub mod auto_respawn;
pub mod auto_tool;
pub mod bot;
mod builder;
mod client_impl;
pub mod container;
mod entity_ref;
pub mod events;
mod join_opts;
pub mod nearest_entity;
pub mod pathfinder;
pub mod prelude;
pub mod swarm;
pub mod tick_broadcast;

pub use mineflayer_auth as auth;
pub use mineflayer_block as block;
#[doc(hidden)]
#[deprecated = "moved to `mineflayer_engine::block`"]
pub mod blocks {
    pub type BlockStates = mineflayer_block::BlockStates;
    pub type BlockState = mineflayer_block::BlockState;
    pub trait BlockTrait: mineflayer_block::BlockTrait {}
    // mineflayer_block has more items but rust doesn't mark them deprecated if
    // we `use mineflayer_block::*`, so hopefully the three types above are
    // enough for most users :(
}

pub use mineflayer_brigadier as brigadier;
pub use mineflayer_buf as buf;
pub use mineflayer_chat::FormattedText;
pub use mineflayer_client::*;
pub use mineflayer_core as core;
// these are re-exported on this level because they're very common
pub use mineflayer_core::position::{BlockPos, Vec3};
pub use mineflayer_entity as entity;
pub use mineflayer_physics as physics;
pub use mineflayer_protocol as protocol;
pub use mineflayer_registry as registry;
#[doc(hidden)]
#[deprecated(note = "renamed to `Identifier`.")]
pub type ResourceLocation = mineflayer_registry::identifier::Identifier;
pub use bevy_app as app;
pub use bevy_ecs as ecs;
use bevy_ecs::component::Component;
pub use builder::ClientBuilder;
use futures::future::BoxFuture;
pub use join_opts::JoinOpts;
pub use mineflayer_chat as chat;
pub use mineflayer_registry::identifier::Identifier;
pub use mineflayer_world as world;

pub use crate::{
    client_impl::{Client, StartClientOpts},
    entity_ref::EntityRef,
    events::Event,
};

pub type BoxHandleFn<S, R> = Box<dyn Fn(Client, Event, S) -> BoxFuture<'static, R> + Send>;
pub type HandleFn<S, Fut> = fn(Client, Event, S) -> Fut;

/// A marker that can be used in place of a State in [`ClientBuilder`] or
/// [`SwarmBuilder`].
///
/// You probably don't need to use this manually since the compiler will infer
/// it for you.
///
/// [`SwarmBuilder`]: swarm::SwarmBuilder
#[derive(Clone, Component, Default)]
pub struct NoState;
