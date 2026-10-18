//! The mineflayer plugin analogs, written on [`mineflayer::plugin`] only:
//! the public API a third-party plugin has.
//!
//! | Upstream package | Module | Bot method |
//! |---|---|---|
//! | mineflayer-tool | [`tool`] | [`ToolExt::tool`] |
//! | mineflayer-auto-eat | [`auto_eat`] | [`AutoEatExt::auto_eat`] |
//! | mineflayer-collectblock | [`collect`] | [`CollectExt::collect_block`] |
//! | mineflayer-pvp | [`pvp`] | [`PvpExt::pvp`] |
//!
//! Each plugin is a cargo feature (all on by default). Import the
//! extension traits with `use mineflayer_plugins::prelude::*;`. Each
//! plugin's events are its own type; subscribe with
//! [`Bot::plugin_events`](mineflayer::Bot::plugin_events).

#[cfg(feature = "auto-eat")]
pub mod auto_eat;
#[cfg(feature = "collect")]
pub mod collect;
#[cfg(feature = "pvp")]
pub mod pvp;
#[cfg(feature = "tool")]
pub mod tool;

#[cfg(feature = "auto-eat")]
pub use auto_eat::AutoEatExt;
#[cfg(feature = "collect")]
pub use collect::CollectExt;
#[cfg(feature = "pvp")]
pub use pvp::PvpExt;
#[cfg(feature = "tool")]
pub use tool::ToolExt;

/// The extension traits that add the plugin methods to `Bot`.
pub mod prelude {
    #[cfg(feature = "auto-eat")]
    pub use crate::auto_eat::AutoEatExt;
    #[cfg(feature = "collect")]
    pub use crate::collect::CollectExt;
    #[cfg(feature = "pvp")]
    pub use crate::pvp::PvpExt;
    #[cfg(feature = "tool")]
    pub use crate::tool::ToolExt;
}
