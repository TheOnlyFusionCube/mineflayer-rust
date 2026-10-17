use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use derive_more::Deref;
use mineflayer_core::tick::GameTick;
use tokio::sync::broadcast;

use crate::Client;

/// A plugin that makes the [`UpdateBroadcast`] and [`TickBroadcast`] resources
/// available.
pub struct TickBroadcastPlugin;
impl Plugin for TickBroadcastPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(TickBroadcast(broadcast::channel(1).0))
            .insert_resource(UpdateBroadcast(broadcast::channel(1).0))
            .add_systems(
                GameTick,
                send_tick_broadcast.after(mineflayer_client::tick_counter::increment_counter),
            )
            .add_systems(Update, send_update_broadcast);
    }
}

/// A resource that contains a [`broadcast::Sender`] that will be sent every
/// Minecraft tick (see [`GameTick`]).
///
/// Also see [`Client::wait_ticks`] and [`Client::get_tick_broadcaster`].
///
/// ```
/// use mineflayer_engine::tick_broadcast::TickBroadcast;
/// async fn example(tick_broadcast: &TickBroadcast) {
///     let mut receiver = tick_broadcast.subscribe();
///
///     while receiver.recv().await.is_ok() {
///         // do something
///     }
/// }
/// ```
#[derive(Deref, Resource)]
pub struct TickBroadcast(broadcast::Sender<()>);

/// A resource that contains a [`broadcast::Sender`] that will be sent every
/// The engine ECS `Update`.
///
/// Also see [`TickBroadcast`].
#[derive(Deref, Resource)]
pub struct UpdateBroadcast(broadcast::Sender<()>);

pub fn send_tick_broadcast(tick_broadcast: ResMut<TickBroadcast>) {
    let _ = tick_broadcast.0.send(());
}
pub fn send_update_broadcast(update_broadcast: ResMut<UpdateBroadcast>) {
    let _ = update_broadcast.0.send(());
}

impl Client {
    /// Returns a Receiver that receives a message every game tick.
    ///
    /// This is useful if you want to efficiently loop until a certain condition
    /// is met.
    ///
    /// ```
    /// # use mineflayer_engine::prelude::*;
    /// # use mineflayer_engine::container::WaitingForInventoryOpen;
    /// # async fn example(bot: &mut mineflayer_engine::Client) {
    /// let mut ticks = bot.get_tick_broadcaster();
    /// while ticks.recv().await.is_ok() {
    ///     let ecs = bot.ecs.read();
    ///     if ecs.get::<WaitingForInventoryOpen>(bot.entity).is_none() {
    ///         break;
    ///     }
    /// }
    /// # }
    /// ```
    pub fn get_tick_broadcaster(&self) -> tokio::sync::broadcast::Receiver<()> {
        let ecs = self.ecs.read();
        let tick_broadcast = ecs.resource::<TickBroadcast>();
        tick_broadcast.subscribe()
    }

    /// Returns a Receiver that receives a message every ECS Update.
    ///
    /// ECS Updates happen at least at the frequency of game ticks, usually
    /// faster.
    ///
    /// This is useful if you're sending an ECS event and want to make sure it's
    /// been handled before continuing.
    pub fn get_update_broadcaster(&self) -> tokio::sync::broadcast::Receiver<()> {
        let ecs = self.ecs.read();
        let update_broadcast = ecs.resource::<UpdateBroadcast>();
        update_broadcast.subscribe()
    }
}
