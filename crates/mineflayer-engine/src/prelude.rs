//! The engine prelude. Things that are necessary for a bare-bones bot are
//! re-exported here.

pub use bevy_app::AppExit;
pub use mineflayer_client::account::Account;
pub use mineflayer_core::tick::GameTick;

// this is necessary to make the macros that reference bevy_ecs work
pub use crate::ecs as bevy_ecs;
pub use crate::{
    Client, ClientBuilder, Event,
    ecs::{component::Component, resource::Resource},
    pathfinder::PathfinderClientExt,
};
