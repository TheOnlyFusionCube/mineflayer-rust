//! The port's ECS side: the Bevy plugins every bot's app runs beside the
//! engine's.
//!
//! The bridge stays the one sender of `BotEvent`s; it runs between engine
//! passes, in engine-event order. A plugin here records what the bridge
//! cannot read after a pass, such as the state a block update replaced,
//! into a component that the bridge then reads.

use mineflayer_engine::{
    ClientPlugins,
    app::{PluginGroup, PluginGroupBuilder},
};

/// The engine's [`ClientPlugins`] and the port's plugins.
pub(crate) struct BotPlugins;

impl PluginGroup for BotPlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            .add_group(ClientPlugins)
            .add(crate::world::WorldPlugin)
    }
}
