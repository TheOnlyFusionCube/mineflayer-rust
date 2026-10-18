//! The port's ECS side: the Bevy plugins every bot's app runs beside the
//! engine's, and the plugins a user adds with [`BotBuilder::plugin`].
//!
//! The bridge stays the one sender of `BotEvent`s; it runs between engine
//! passes, in engine-event order. A port plugin records what the bridge
//! cannot read after a pass, such as the state a block update replaced,
//! into a component that the bridge then reads.
//!
//! [`BotBuilder::plugin`]: crate::BotBuilder::plugin

use std::sync::Arc;

use mineflayer_engine::{
    ClientPlugins,
    accept_resource_packs::AcceptResourcePacksPlugin,
    app::{Plugin, PluginGroup, PluginGroupBuilder},
    auto_respawn::AutoRespawnPlugin,
};

/// Adds one user plugin to the app; shared so a cloned builder can build
/// the app again.
pub(crate) type AddPlugin = Arc<dyn Fn(PluginGroupBuilder) -> PluginGroupBuilder + Send + Sync>;

pub(crate) fn add_plugin(plugin: impl Plugin + Clone) -> AddPlugin {
    Arc::new(move |group| group.add(plugin.clone()))
}

/// The engine's [`ClientPlugins`], the port's plugins, and the user's.
#[derive(Clone)]
pub(crate) struct BotPlugins {
    /// `options.respawn`: `false` leaves out the engine's auto-respawn.
    pub(crate) respawn: bool,
    /// `false` leaves out the engine's accept-every-resource-pack plugin.
    pub(crate) accept_resource_packs: bool,
    pub(crate) user: Vec<AddPlugin>,
}

impl Default for BotPlugins {
    fn default() -> Self {
        Self {
            respawn: true,
            accept_resource_packs: true,
            user: Vec::new(),
        }
    }
}

impl std::fmt::Debug for BotPlugins {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BotPlugins")
            .field("respawn", &self.respawn)
            .field("accept_resource_packs", &self.accept_resource_packs)
            .field("user", &self.user.len())
            .finish()
    }
}

impl PluginGroup for BotPlugins {
    fn build(self) -> PluginGroupBuilder {
        let mut group = PluginGroupBuilder::start::<Self>()
            .add_group(ClientPlugins)
            .add(crate::world::WorldPlugin);
        if !self.respawn {
            group = group.disable::<AutoRespawnPlugin>();
        }
        if !self.accept_resource_packs {
            group = group.disable::<AcceptResourcePacksPlugin>();
        }
        self.user.iter().fold(group, |group, add| add(group))
    }
}

#[cfg(test)]
mod tests;
