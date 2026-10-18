//! What a plugin crate needs from a [`Bot`], the Rust form of upstream's
//! `bot.loadPlugin(plugin)`.
//!
//! A plugin has two halves, and it can use either or both:
//!
//! - An ECS half: a Bevy plugin, added with
//!   [`BotBuilder::plugin`](crate::BotBuilder::plugin), whose systems run
//!   in the bot's engine schedule.
//! - An async half: methods on an extension trait of [`Bot`], built on the
//!   items here. Keep per-bot state in a component on the bot's entity
//!   ([`Bot::state`], [`Bot::state_mut`]); loop on the game ticks
//!   ([`Bot::ticks`]) from a task that holds a [`Bot::driver`] handle; run
//!   each long action as an [`Op`] in an [`OpSlot`], so that a new action
//!   or a disconnect ends the old one; and send the plugin's own events
//!   with [`Bot::emit_plugin_event`].
//!
//! ```no_run
//! use mineflayer::{Bot, plugin::OpSlot};
//! use mineflayer::engine::ecs::{self as bevy_ecs, component::Component};
//!
//! #[derive(Component, Default)]
//! struct Greeter {
//!     greetings: u32,
//!     op: OpSlot,
//! }
//!
//! #[derive(Clone, Debug)]
//! struct Greeted(u32);
//!
//! trait GreeterExt {
//!     async fn greet_every_tick(&self, times: u32);
//! }
//!
//! impl GreeterExt for Bot {
//!     async fn greet_every_tick(&self, times: u32) {
//!         let op = self
//!             .state_mut(|g: &mut Greeter| self.start_op(&mut g.op))
//!             .expect("the bot is connected");
//!         let mut ticks = self.ticks();
//!         for _ in 0..times {
//!             if op.is_cancelled() || !ticks.next().await {
//!                 return;
//!             }
//!             let n = self.state_mut(|g: &mut Greeter| {
//!                 g.greetings += 1;
//!                 g.greetings
//!             });
//!             self.emit_plugin_event(Greeted(n.unwrap_or_default()));
//!         }
//!         self.state_mut(|g: &mut Greeter| g.op.finish(&op));
//!     }
//! }
//! ```

use mineflayer_engine::ecs::{
    self as bevy_ecs,
    component::{Component, Mutable},
};
use tokio::sync::broadcast;

use crate::{
    Bot,
    bot::{with_component, with_component_mut},
};
pub use crate::{
    bot::TickStream,
    op::{Op, OpSlot},
};

/// How many events of one plugin event type a receiver can fall behind
/// before it gets `Lagged`.
const PLUGIN_EVENT_CAPACITY: usize = 1024;

/// The sender of one plugin event type, on the bot's entity.
#[derive(Component)]
struct PluginEvents<E: Send + Sync + 'static>(broadcast::Sender<E>);

impl<E: Clone + Send + Sync + 'static> Default for PluginEvents<E> {
    fn default() -> Self {
        Self(broadcast::channel(PLUGIN_EVENT_CAPACITY).0)
    }
}

impl Bot {
    /// `f` on the plugin state `T` on the bot's entity. `None` when the bot
    /// has no `T` or its entity is gone.
    ///
    /// `f` runs under the ECS lock: it must not call `Bot` methods that
    /// read the game state.
    pub fn state<T: Component, R>(&self, f: impl FnOnce(&T) -> R) -> Option<R> {
        with_component(&self.client, f)
    }

    /// `f` on the plugin state `T` on the bot's entity, created with its
    /// default on first use. `None` when the bot's entity is gone. The
    /// state goes away with the connection.
    ///
    /// `f` runs under the ECS lock, as for [`Self::state`].
    pub fn state_mut<T: Component<Mutability = Mutable> + Default, R>(
        &self,
        f: impl FnOnce(&mut T) -> R,
    ) -> Option<R> {
        with_component_mut(&self.client, f)
    }

    /// Start an operation in `slot`: cancel the one that runs there, and
    /// return the new one. A disconnect cancels it too.
    pub fn start_op(&self, slot: &mut OpSlot) -> Op {
        slot.start(&self.root)
    }

    /// The game ticks, until the connection ends.
    pub fn ticks(&self) -> TickStream {
        TickStream::new(&self.client, self.root.clone())
    }

    /// A handle for a background task. Unlike a clone, it does not keep
    /// the connection open when every other handle is dropped.
    pub fn driver(&self) -> Bot {
        self.driver_handle()
    }

    /// Subscribe to the plugin events of type `E`. A plugin defines its
    /// own event types, so they are not variants of
    /// [`BotEvent`](crate::BotEvent).
    pub fn plugin_events<E: Clone + Send + Sync + 'static>(&self) -> broadcast::Receiver<E> {
        with_component_mut::<PluginEvents<E>, _>(&self.client, |events| events.0.subscribe())
            .unwrap_or_else(|| broadcast::channel(1).1)
    }

    /// Send `event` to the subscribers of [`Self::plugin_events`] for `E`.
    pub fn emit_plugin_event<E: Clone + Send + Sync + 'static>(&self, event: E) {
        with_component::<PluginEvents<E>, _>(&self.client, |events| {
            let _ = events.0.send(event);
        });
    }
}
