//! The client app the port joins through.
//!
//! [`app`] mirrors the engine's own `StartClientOpts::new` bootstrap — the same
//! default plugin groups, in the same order — plus the port's
//! [`crate::interact::DigAbortPlugin`]. It exists because `Client::join` /
//! `join_with_proxy` build the app internally and offer no plugin hook, and
//! the dig-abort reconciler must be inside the tick that materializes a
//! pending mining start.
//!
//! Contract (re-check when the engine changes): the source is
//! `crates/mineflayer-engine/src/client_impl/mod.rs::StartClientOpts::new`
//! (`DefaultPlugins`, `DefaultBotPlugins`, `DefaultSwarmPlugins`, then
//! `start_ecs_runner`). The runner start, proxy pairing, and event-channel
//! wiring stay in `bot.rs`; this function owns app construction only.

pub(crate) fn app() -> mineflayer_engine::app::App {
    let mut app = mineflayer_engine::app::App::new();
    app.add_plugins((
        mineflayer_engine::DefaultPlugins,
        mineflayer_engine::bot::DefaultBotPlugins,
        mineflayer_engine::swarm::DefaultSwarmPlugins,
    ));
    app.add_plugins(crate::interact::DigAbortPlugin);
    app
}

#[cfg(test)]
mod tests {
    use mineflayer_engine::{BlockPos, core::direction::Direction, mining::MiningQueued};

    /// D-log: the crate never installs a global subscriber. The engine's
    /// `log` feature adds a log plugin that does, and a dependent's own
    /// `tracing_subscriber::…::init()` after `connect` then panics.
    #[test]
    fn the_production_app_installs_no_global_subscriber() {
        let mut app = crate::client_app::app();
        app.finish();
        assert!(!tracing::dispatcher::has_been_set());
    }

    /// The integration guarantee the dig-abort design rests on: a pending
    /// start materialized in `Update` is cancelled by the reconciler inside
    /// `GameTick`, before the mining chain runs — through the *production*
    /// app (plugin presence + ordering), not a bare `World`.
    #[test]
    fn game_tick_cancels_an_aborted_dig_through_the_production_app() {
        use crate::interact::dig::InteractShared;

        let mut app = crate::client_app::app();
        app.finish();
        let pos = BlockPos::new(9, 64, 9);
        let mut shared = InteractShared::with_generation(1);
        shared.mark_aborted_engine_dig(pos, 1);
        let world = app.main_mut().world_mut();
        let entity = world
            .spawn((
                shared,
                MiningQueued {
                    position: pos,
                    direction: Direction::Down,
                    force: true,
                },
            ))
            .id();
        world.run_schedule(mineflayer_engine::core::tick::GameTick);
        assert!(
            world.get::<MiningQueued>(entity).is_none(),
            "the production GameTick must cancel an aborted dig's queued mine"
        );
    }
}
