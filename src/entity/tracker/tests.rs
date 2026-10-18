//! The entity packets through the real engine and the real bridge.

use mineflayer_engine::{
    BlockPos,
    entity::EntityMetadataItems,
    protocol::packets::{
        Packet,
        game::{
            ClientboundAddEntity, ClientboundRemoveEntities, ClientboundRemoveMobEffect,
            ClientboundSetEntityData, ClientboundSetEquipment, ClientboundSetPassengers,
            ClientboundTakeItemEntity, ClientboundUpdateMobEffect,
            c_player_info_remove::ClientboundPlayerInfoRemove,
            c_player_info_update::{ActionEnumSet, ClientboundPlayerInfoUpdate, PlayerInfoEntry},
            c_set_equipment::EquipmentSlots,
            s_chat_session_update::{ProfilePublicKeyData, RemoteChatSessionData},
        },
    },
    test_utils::simulation::make_basic_add_entity,
};

use super::*;
use crate::test_util::sim::{BotSim, names};

/// The bot's own entity id in `default_login_packet`.
const ME: MinecraftEntityId = MinecraftEntityId(0);

fn sim() -> BotSim {
    let mut sim = BotSim::new();
    sim.events();
    sim
}

/// The entity events of one pass that reads `packets`.
fn step(sim: &mut BotSim, packets: Vec<ClientboundGamePacket>) -> Vec<BotEvent> {
    for packet in packets {
        sim.receive(packet);
    }
    sim.tick();
    sim.events()
        .into_iter()
        .filter(|e| {
            names(std::slice::from_ref(e))[0].starts_with("Entity")
                || matches!(
                    e,
                    BotEvent::Mounted { .. }
                        | BotEvent::Dismounted { .. }
                        | BotEvent::PlayerCollect { .. }
                        | BotEvent::ItemDrop(_)
                )
        })
        .collect()
}

fn add(kind: EntityKind, id: i32) -> ClientboundGamePacket {
    ClientboundAddEntity {
        uuid: uuid::Uuid::from_u128(1000 + id as u128),
        ..make_basic_add_entity(kind, id, (0.5, 64., 0.5))
    }
    .into_variant()
}

fn remove(id: i32) -> ClientboundGamePacket {
    ClientboundRemoveEntities {
        entity_ids: vec![MinecraftEntityId(id)],
    }
    .into_variant()
}

fn metadata(id: i32, items: Vec<EntityDataItem>) -> ClientboundGamePacket {
    ClientboundSetEntityData {
        id: MinecraftEntityId(id),
        packed_items: EntityMetadataItems(items),
    }
    .into_variant()
}

fn flags(b: u8) -> EntityDataItem {
    EntityDataItem {
        index: 0,
        value: EntityDataValue::Byte(b),
    }
}

fn air() -> EntityDataItem {
    EntityDataItem {
        index: 1,
        value: EntityDataValue::Int(300),
    }
}

fn spawned(kind: EntityKind, id: i32) -> BotSim {
    let mut sim = sim();
    step(&mut sim, vec![add(kind, id)]);
    sim
}

#[track_caller]
fn assert_names(events: &[BotEvent], want: &[&str]) {
    assert_eq!(names(events), want, "{events:?}");
}

#[test]
fn entity_status_table_matches_upstream() {
    // upstream's entityStatusEvents mapping, verbatim
    assert_eq!(entity_status_kind(2), Some(StatusKind::Hurt));
    assert_eq!(entity_status_kind(3), Some(StatusKind::Dead));
    assert_eq!(entity_status_kind(6), Some(StatusKind::Taming));
    assert_eq!(entity_status_kind(7), Some(StatusKind::Tamed));
    assert_eq!(entity_status_kind(8), Some(StatusKind::ShakingOffWater));
    assert_eq!(entity_status_kind(10), Some(StatusKind::EatingGrass));
    assert_eq!(entity_status_kind(55), Some(StatusKind::HandSwap));
    assert_eq!(entity_status_kind(4), None);
    assert_eq!(entity_status_kind(0), None);
    assert_eq!(entity_status_kind(54), None);
}

#[test]
fn packet_after_self_despawn_emits_nothing() {
    let client = crate::test_util::despawned_client();
    // teardown packets race the despawn of our own entity; the id
    // lookup must miss quietly instead of panicking.
    let packet = ClientboundGamePacket::Animate(
        mineflayer_engine::protocol::packets::game::ClientboundAnimate {
            id: MinecraftEntityId(1),
            action: AnimationAction::SwingMainHand,
        },
    );
    assert!(observe_event(&client, &Event::Packet(std::sync::Arc::new(packet))).is_empty());
}

#[test]
fn crouch_and_sleep_require_the_metadata_item() {
    let mut sim = spawned(EntityKind::Zombie, 7);
    // shared_flags 0x02: crouch fires, and fires again per flag packet
    // (upstream emits while the bit is set)
    for _ in 0..2 {
        let out = step(&mut sim, vec![metadata(7, vec![flags(0x02)])]);
        assert_names(&out, &["EntityUpdate", "EntityCrouch"]);
    }
    // still crouched, but this packet carries no flags: nothing
    // crouch-related goes out
    let out = step(&mut sim, vec![metadata(7, vec![air()])]);
    assert_names(&out, &["EntityUpdate"]);
    // flags with the bit clear: the release edge fires once
    let out = step(&mut sim, vec![metadata(7, vec![flags(0x00)])]);
    assert_names(&out, &["EntityUpdate", "EntityUncrouch"]);
    let out = step(&mut sim, vec![metadata(7, vec![flags(0x00)])]);
    assert_names(&out, &["EntityUpdate"]);

    // two flag packets in one pass give both edges, in order
    let out = step(
        &mut sim,
        vec![
            metadata(7, vec![flags(0x02)]),
            metadata(7, vec![flags(0x00)]),
        ],
    );
    assert_names(
        &out,
        &[
            "EntityUpdate",
            "EntityCrouch",
            "EntityUpdate",
            "EntityUncrouch",
        ],
    );

    // sleep fires per packet that carries pose=Sleeping or a Some
    // sleeping_pos, never on other metadata
    let sleeping_pose = EntityDataItem {
        index: 6,
        value: EntityDataValue::Pose(Pose::Sleeping),
    };
    let bed_pos = EntityDataItem {
        index: 14,
        value: EntityDataValue::OptionalBlockPos(Some(BlockPos::new(1, 64, 1))),
    };
    let no_bed = EntityDataItem {
        index: 14,
        value: EntityDataValue::OptionalBlockPos(None),
    };
    let out = step(&mut sim, vec![metadata(7, vec![sleeping_pose])]);
    assert_names(&out, &["EntityUpdate", "EntitySleep"]);
    let out = step(&mut sim, vec![metadata(7, vec![bed_pos])]);
    assert_names(&out, &["EntityUpdate", "EntitySleep"]);
    let out = step(&mut sim, vec![metadata(7, vec![no_bed])]);
    assert_names(&out, &["EntityUpdate"]);
}

#[test]
fn elytra_flew_is_flag_packet_gated_and_edged() {
    let mut sim = spawned(EntityKind::Zombie, 7);
    let out = step(&mut sim, vec![metadata(7, vec![flags(0x80)])]);
    assert_names(&out, &["EntityUpdate", "EntityElytraFlew"]);
    let out = step(&mut sim, vec![metadata(7, vec![flags(0x80)])]);
    assert_names(&out, &["EntityUpdate"]);
    // the bit clears, then sets again: a new edge fires again
    step(&mut sim, vec![metadata(7, vec![flags(0x00)])]);
    let out = step(&mut sim, vec![metadata(7, vec![flags(0x80)])]);
    assert_names(&out, &["EntityUpdate", "EntityElytraFlew"]);
}

/// An item stack anywhere in the metadata gives `itemDrop`.
#[test]
fn item_metadata_gives_item_drop() {
    let mut sim = spawned(EntityKind::Item, 9);
    let out = step(
        &mut sim,
        vec![metadata(
            9,
            vec![EntityDataItem {
                index: 8,
                value: EntityDataValue::ItemStack(ItemStack::new(
                    mineflayer_engine::registry::builtin::ItemKind::Stone,
                    1,
                )),
            }],
        )],
    );
    assert_names(&out, &["EntityUpdate", "ItemDrop"]);
}

/// Packets for an entity that a later packet of the same pass removes
/// still resolve it, and `entityGone` carries its handle.
#[test]
fn an_entity_removed_later_in_the_pass_still_resolves() {
    let mut sim = spawned(EntityKind::Item, 9);
    let out = step(
        &mut sim,
        vec![
            ClientboundTakeItemEntity {
                item_id: 9,
                player_id: ME,
                amount: 1,
            }
            .into_variant(),
            remove(9),
        ],
    );
    let [
        BotEvent::PlayerCollect {
            collector: Some(_),
            collected: Some(collected),
        },
        BotEvent::EntityGone {
            entity: Some(gone),
            id: MinecraftEntityId(9),
        },
    ] = &out[..]
    else {
        panic!("{out:?}");
    };
    assert_eq!(collected.id(), gone.id());
}

/// Riding: `mount` when the bot joins a vehicle's passengers, `dismount`
/// when the vehicle is removed.
#[test]
fn removing_the_vehicle_dismounts() {
    let mut sim = spawned(EntityKind::Minecart, 5);
    let out = step(
        &mut sim,
        vec![
            ClientboundSetPassengers {
                vehicle: MinecraftEntityId(5),
                passengers: vec![ME],
            }
            .into_variant(),
        ],
    );
    assert!(
        matches!(&out[..], [BotEvent::Mounted { vehicle: Some(_) }]),
        "{out:?}"
    );
    assert!(sim.bot.vehicle().is_some());

    let out = step(&mut sim, vec![remove(5)]);
    assert_names(&out, &["EntityGone", "Dismounted"]);
    assert!(sim.bot.vehicle().is_none());
}

#[test]
fn equipment_and_effects_are_kept_per_entity() {
    use mineflayer_engine::{inventory::components::EquipmentSlot, registry::builtin::MobEffect};
    let mut sim = spawned(EntityKind::Zombie, 7);
    let sword = ItemStack::new(mineflayer_engine::registry::builtin::ItemKind::IronSword, 1);
    step(
        &mut sim,
        vec![
            ClientboundSetEquipment {
                entity_id: MinecraftEntityId(7),
                slots: EquipmentSlots {
                    slots: vec![(EquipmentSlot::Mainhand, sword.clone())],
                },
            }
            .into_variant(),
        ],
    );
    let zombie = sim.bot.entities()[&MinecraftEntityId(7)].clone();
    assert_eq!(
        sim.bot.equipment(&zombie).get(&EquipmentSlot::Mainhand),
        Some(&sword)
    );

    let data = MobEffectData {
        amplifier: 2,
        duration: 100,
        ..Default::default()
    };
    step(
        &mut sim,
        vec![
            ClientboundUpdateMobEffect {
                entity_id: MinecraftEntityId(7),
                mob_effect: MobEffect::Speed,
                data: data.clone(),
            }
            .into_variant(),
        ],
    );
    let end = |sim: &mut BotSim| {
        step(
            sim,
            vec![
                ClientboundRemoveMobEffect {
                    entity_id: MinecraftEntityId(7),
                    effect: MobEffect::Speed,
                }
                .into_variant(),
            ],
        )
    };
    let out = end(&mut sim);
    assert!(
        matches!(&out[..], [BotEvent::EntityEffectEnd { data: d, .. }] if d.amplifier == 2),
        "{out:?}"
    );
    // an effect that is not tracked ends with upstream's fallback data
    let out = end(&mut sim);
    assert!(
        matches!(&out[..], [BotEvent::EntityEffectEnd { data: d, .. }] if d.amplifier == -1 && d.duration == -1),
        "{out:?}"
    );
}

#[test]
fn profile_keys_tracked_from_player_info_update() {
    use mineflayer_engine::auth::game_profile::GameProfile;

    let mut sim = sim();
    let uuid = uuid::Uuid::from_u128(0x1234);
    let update = |chat_session: Option<RemoteChatSessionData>| ClientboundPlayerInfoUpdate {
        actions: ActionEnumSet {
            add_player: true,
            initialize_chat: chat_session.is_some(),
            update_game_mode: true,
            update_listed: true,
            update_latency: true,
            update_display_name: false,
            update_list_order: false,
            update_hat: false,
        },
        entries: vec![PlayerInfoEntry {
            profile: GameProfile::new(uuid, "faye".into()),
            chat_session,
            ..Default::default()
        }],
    };
    let session = RemoteChatSessionData {
        session_id: uuid::Uuid::from_u128(0xbeef),
        profile_public_key: ProfilePublicKeyData {
            expires_at: 0,
            key: vec![1, 2, 3],
            key_signature: vec![4, 5],
        },
    };
    let keys = |sim: &BotSim| {
        sim.bot
            .players()
            .get("faye")
            .and_then(|p| p.profile_keys.clone())
    };

    step(&mut sim, vec![update(Some(session.clone())).into_variant()]);
    assert_eq!(
        keys(&sim),
        Some(ProfileKeys {
            public_key: vec![1, 2, 3],
            signature: vec![4, 5],
        })
    );
    // a re-add without a chat session clears the key
    step(&mut sim, vec![update(None).into_variant()]);
    assert_eq!(keys(&sim), None);
    // PlayerInfoRemove clears it too
    step(&mut sim, vec![update(Some(session)).into_variant()]);
    step(
        &mut sim,
        vec![
            ClientboundPlayerInfoRemove {
                profile_ids: vec![uuid],
            }
            .into_variant(),
        ],
    );
    assert!(!sim.bot.players().contains_key("faye"));
}
