//! Block interaction — digging, placing, and activating blocks plus
//! placing entities (`lib/plugins/digging.js`, `place_block.js`,
//! `generic_place.js`, `place_entity.js`, `inventory.js#activateBlock`).
//!
//! azalea owns the wire mechanics: `Client::mine`-equivalent start +
//! `Mining`/`MiningQueued` components, `ServerboundUseItemOn` for block
//! interactions, and the mine-progress formula behind `dig_time`. This module
//! layers the mineflayer contract on top:
//!
//! - `dig` looks first, aborts any previous dig, then resolves when the block
//!   becomes air and rejects with `DiggingAborted` otherwise.
//! - `place_block`/`place_entity` go through the upstream
//!   `_placeBlockWithOptions`/`_genericPlace` flow and wait for a
//!   `blockUpdate` on the reference *and* destination blocks before
//!   resolving, then emit `blockPlaced`/`entityPlaced`.
//! - `activate_block` sends the right-click interaction (which doubles as a
//!   placement trigger, exactly like upstream).
//! - `InteractShared` holds the bookkeeping upstream keeps in JS-side
//!   state (`lastDigTime`, abort coordination); azalea's `Mining` component
//!   is the `targetDigBlock`/`targetDigFace` source of truth.

pub(crate) mod dig;
mod place;

pub(crate) use dig::{DigAbortPlugin, InteractShared};
pub use dig::{DigAbortReason, DigFace};
pub use place::{PlaceHalf, PlaceOpts};

/// Look-direction policy for `dig`/`place_entity`/`look_at` callers.
/// `bot.dig`'s `forceLook` argument (`lib/plugins/digging.js`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ForceLook {
    /// Default — look at the block and wait one tick for the look to land.
    #[default]
    Look,
    /// `true` — look and don't yield before starting the dig.
    Force,
    /// `'ignore'` — skip looking entirely.
    Ignore,
}
