//! `mcdata.*` — the minecraft-data lookups upstream exposes via `bot.registry`
//! (`mcdata.blocks`, `items`, `biomes`, `entities`, …).
//!
//! The engine compiles the built-in registries into closed enums and carries the
//! dynamic ones in from the server's `registry_data` login payload, so the
//! honest equivalents are typed handles rather than lookup tables.
//!
//! - `mcdata.blocks` / `items` / `entities` → [`BlockKind`] / [`ItemKind`] /
//!   [`EntityKind`]. [`Registry::from_u32`]/[`Registry::to_u32`] are upstream's
//!   id index; `to_str`/`Display`/`FromStr`/`TryFrom<&Identifier>` the name
//!   lookups. Per-kind data upstream stores on each record (hardness,
//!   diggable, stack size, …) lives elsewhere: block behavior on
//!   `mineflayer_engine::block::BlockTrait::behavior` (see [`crate::world::Block`]), item
//!   data on `mineflayer_engine::inventory::ItemStack` data components.
//! - `mcdata.biomes` → [`Biome`], a protocol-id handle into the server's
//!   `worldgen/biome` registry (biomes are data-driven since 1.18 — the
//!   server sends them, so no compile-time table exists). Resolve the name
//!   with `Bot::client().resolve_registry_key(&biome)` (-> [`BiomeKey`]);
//!   element data (temperature, downfall, effects) arrives as NBT under
//!   `RegistryHolder::extra` on [`Bot::registry`](crate::Bot::registry).
//! - `mcdata.materials` — not ported. minecraft-data's material →
//!   item-id → multiplier table models a pre-1.20.5 protocol era; in mc26
//!   dig speed is per-item `minecraft:tool` data-component rules over block
//!   tag sets, which `mineflayer_engine::entity::mining::get_mine_progress` already
//!   reads.
//! - `mcdata.recipes` / `mcdata.instruments` — see [`crate::craft`] and
//!   [`crate::misc::Instrument`]; both are tracked in `docs/api-parity.md`.

pub use mineflayer_engine::registry::{
    DataRegistry, Registry,
    builtin::{BlockKind, EntityKind, ItemKind},
    data::{Biome, BiomeKey},
    identifier::Identifier,
};

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    #[test]
    fn kind_registries_round_trip_ids_and_names() {
        // `mcdata.blocks`/`items`/`entities` upstream: indexed by id, looked
        // up by name. The kind enums do both.
        let stone = BlockKind::from_u32(1).unwrap();
        assert_eq!(stone, BlockKind::Stone);
        assert_eq!(stone.to_str(), "minecraft:stone");
        assert_eq!(stone.to_u32(), 1);
        assert_eq!(BlockKind::from_str("stone").unwrap(), stone);

        let apple = ItemKind::from_str("apple").unwrap();
        assert_eq!(apple.to_str(), "minecraft:apple");

        let zombie = EntityKind::from_str("zombie").unwrap();
        assert_eq!(zombie, EntityKind::Zombie);
        assert_eq!(EntityKind::try_from(zombie.to_u32()).unwrap(), zombie);

        // out-of-range ids fail instead of transmuting
        assert!(BlockKind::from_u32(u32::MAX).is_none());
        assert!(BlockKind::from_str("not_a_block").is_err());
    }

    #[test]
    fn biome_is_a_server_registry_handle() {
        // `mcdata.biomes`/`Biome` upstream: biome records by id. The engine's
        // `Biome` is the protocol-id handle; `BiomeKey` is the known-key
        // enum resolved through the server's registry.
        let biome = Biome::new_raw(3);
        assert_eq!(biome.protocol_id(), 3);
        let key = BiomeKey::from(Identifier::new("minecraft:plains"));
        assert_eq!(key, BiomeKey::Plains);
    }
}
