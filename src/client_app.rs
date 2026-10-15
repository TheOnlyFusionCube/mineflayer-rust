//! The client app the port joins through.
//!
//! [`app`] mirrors azalea's own `StartClientOpts::new` bootstrap — the same
//! default plugin groups, in the same order — plus the port's
//! [`crate::interact::DigAbortPlugin`]. It exists because `Client::join` /
//! `join_with_proxy` build the app internally and offer no plugin hook, and
//! the dig-abort reconciler must be inside the tick that materializes a
//! pending mining start.
//!
//! Contract (re-check when the azalea pin moves): the upstream source is
//! `azalea/src/client_impl/mod.rs::StartClientOpts::new` at rev `ddef371`
//! (`DefaultPlugins`, `DefaultBotPlugins`, `DefaultSwarmPlugins`, then
//! `start_ecs_runner`). The runner start, proxy pairing, and event-channel
//! wiring stay in `bot.rs`; this function owns app construction only.

pub(crate) fn app() -> azalea::app::App {
    let mut app = azalea::app::App::new();
    app.add_plugins((
        azalea::DefaultPlugins,
        azalea::bot::DefaultBotPlugins,
        azalea::swarm::DefaultSwarmPlugins,
    ));
    app.add_plugins(crate::interact::DigAbortPlugin);
    app
}

#[cfg(test)]
mod tests {
    use azalea::{BlockPos, core::direction::Direction, mining::MiningQueued};

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
        world.run_schedule(azalea::core::tick::GameTick);
        assert!(
            world.get::<MiningQueued>(entity).is_none(),
            "the production GameTick must cancel an aborted dig's queued mine"
        );
    }
}
