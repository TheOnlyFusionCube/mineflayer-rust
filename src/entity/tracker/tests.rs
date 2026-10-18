use std::sync::Arc;

use mineflayer_engine::protocol::packets::game::c_animate::ClientboundAnimate;

use super::*;

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
fn forget_cleans_vehicle_and_passenger_maps() {
    let id = |i: i32| MinecraftEntityId(i);
    // mount: self appears in a vehicle's passenger list
    let mut tracker = EntityTracker::default();
    assert!(tracker.passengers_of.is_empty());
    // can't call set_passengers (needs a Client); exercise the diff logic
    // through the maps directly
    tracker.passengers_of.insert(id(1), vec![id(5)]);
    tracker.vehicle_of.insert(id(5), id(1));
    assert_eq!(tracker.vehicle_of.get(&id(5)), Some(&id(1)));
    // removal cleans the maps
    tracker.forget(Some(id(5)), id(1));
    assert!(!tracker.vehicle_of.contains_key(&id(5)));
    // removing a passenger cleans its vehicle link
    tracker.passengers_of.insert(id(2), vec![id(6)]);
    tracker.vehicle_of.insert(id(6), id(2));
    tracker.forget(None, id(6));
    assert!(tracker.passengers_of.get(&id(2)).unwrap().is_empty());
}

/// A dimension change keeps only the bot's own entries.
#[test]
fn respawn_forgets_other_dimension_entities() {
    use mineflayer_engine::{
        core::game_type::{GameMode, OptionalGameType},
        protocol::packets::{common::CommonPlayerSpawnInfo, game::c_respawn::ClientboundRespawn},
        registry::{DataRegistry as _, data::DimensionKind},
    };
    let me = MinecraftEntityId(7);
    let other = MinecraftEntityId(8);
    let client = crate::test_util::live_entity_client(me);
    let mut tracker = EntityTracker::default();
    for id in [me, other] {
        tracker.equipment.insert(id, HashMap::new());
        tracker.attributes.insert(id, HashMap::new());
        tracker.effects.insert(id, HashMap::new());
        tracker.meta.insert(id, EntityMeta::default());
    }
    tracker.vehicle_of.insert(other, me);
    tracker.passengers_of.insert(me, vec![other]);
    tracker.seen_fireworks.insert(other);
    tracker.difficulty = Some(Difficulty::Hard);

    let respawn = ClientboundGamePacket::Respawn(ClientboundRespawn {
        common: CommonPlayerSpawnInfo {
            dimension_type: DimensionKind::new_raw(0),
            dimension: mineflayer_engine::Identifier::new("minecraft:the_nether"),
            seed: 0,
            game_type: GameMode::Survival,
            previous_game_type: OptionalGameType(None),
            is_debug: false,
            is_flat: false,
            last_death_location: None,
            portal_cooldown: 0,
            sea_level: 32,
        },
        data_to_keep: 0,
    });
    assert!(tracker.handle_packet(&client, &respawn).is_empty());

    for map_len in [
        tracker.equipment.len(),
        tracker.attributes.len(),
        tracker.effects.len(),
        tracker.meta.len(),
    ] {
        assert_eq!(map_len, 1, "only the bot's own entry stays");
    }
    assert!(tracker.equipment.contains_key(&me));
    assert!(tracker.vehicle_of.is_empty() && tracker.passengers_of.is_empty());
    assert!(tracker.seen_fireworks.is_empty());
    assert_eq!(tracker.difficulty, Some(Difficulty::Hard));
}

#[test]
fn packet_after_self_despawn_emits_nothing() {
    let client = crate::test_util::despawned_client();
    let tracker = RwLock::new(EntityTracker::default());
    // teardown packets race the despawn of our own entity; the id
    // lookup must miss quietly instead of panicking.
    let packet = ClientboundGamePacket::Animate(ClientboundAnimate {
        id: MinecraftEntityId(1),
        action: AnimationAction::SwingMainHand,
    });
    assert!(observe_event(&tracker, &client, &Event::Packet(Arc::new(packet))).is_empty());
}

#[test]
fn crouch_and_sleep_require_the_metadata_item() {
    use mineflayer_engine::{
        BlockPos,
        entity::{EntityMetadataItems, metadata::AbstractEntityShiftKeyDown},
        protocol::packets::game::c_set_entity_data::ClientboundSetEntityData,
    };

    fn packet(items: Vec<EntityDataItem>) -> ClientboundGamePacket {
        ClientboundGamePacket::SetEntityData(ClientboundSetEntityData {
            id: MinecraftEntityId(7),
            packed_items: EntityMetadataItems(items),
        })
    }
    let flags = |b: u8| EntityDataItem {
        index: 0,
        value: EntityDataValue::Byte(b),
    };
    let air = EntityDataItem {
        index: 1,
        value: EntityDataValue::Int(300),
    };

    let client = crate::test_util::live_entity_client(());
    // the entity IS crouched component-side — the regression this test
    // pins is that packets without shared_flags still emit nothing
    let e = client
        .ecs
        .write()
        .spawn((MinecraftEntityId(7), AbstractEntityShiftKeyDown(true)))
        .id();
    let mut tracker = EntityTracker::default();
    tracker.known.insert(MinecraftEntityId(7), e);

    // unrelated metadata while crouched: EntityUpdate only, no Crouch
    let out = tracker.handle_packet(&client, &packet(vec![air.clone()]));
    assert!(
        matches!(&out[..], [BotEvent::EntityUpdate(_)]),
        "expected EntityUpdate only, got {out:?}"
    );
    // shared_flags 0x02: crouch fires, and refires per flag packet
    // (upstream emits unconditionally while the bit is set)
    for _ in 0..2 {
        let out = tracker.handle_packet(&client, &packet(vec![flags(0x02)]));
        assert!(
            matches!(
                &out[..],
                [BotEvent::EntityUpdate(_), BotEvent::EntityCrouch(_)]
            ),
            "expected Update+Crouch, got {out:?}"
        );
    }
    // still crouched component-side, but this packet carries no flags:
    // nothing crouch-related may emit
    let out = tracker.handle_packet(&client, &packet(vec![air]));
    assert!(
        matches!(&out[..], [BotEvent::EntityUpdate(_)]),
        "expected EntityUpdate only, got {out:?}"
    );
    // flags with the bit clear: the release edge fires once
    let out = tracker.handle_packet(&client, &packet(vec![flags(0x00)]));
    assert!(
        matches!(
            &out[..],
            [BotEvent::EntityUpdate(_), BotEvent::EntityUncrouch(_)]
        ),
        "expected Update+Uncrouch, got {out:?}"
    );
    let out = tracker.handle_packet(&client, &packet(vec![flags(0x00)]));
    assert!(
        matches!(&out[..], [BotEvent::EntityUpdate(_)]),
        "expected EntityUpdate only, got {out:?}"
    );

    // sleep fires per packet carrying pose=Sleeping or a Some
    // sleeping_pos — never on unrelated metadata
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
    let out = tracker.handle_packet(&client, &packet(vec![sleeping_pose]));
    assert!(
        matches!(
            &out[..],
            [BotEvent::EntityUpdate(_), BotEvent::EntitySleep(_)]
        ),
        "expected Update+Sleep, got {out:?}"
    );
    let out = tracker.handle_packet(&client, &packet(vec![bed_pos]));
    assert!(
        matches!(
            &out[..],
            [BotEvent::EntityUpdate(_), BotEvent::EntitySleep(_)]
        ),
        "expected Update+Sleep, got {out:?}"
    );
    // an absent sleeping_pos (waking) carries no sleep signal
    let out = tracker.handle_packet(&client, &packet(vec![no_bed]));
    assert!(
        matches!(&out[..], [BotEvent::EntityUpdate(_)]),
        "expected EntityUpdate only, got {out:?}"
    );
}

#[test]
fn elytra_flew_is_flag_packet_gated_and_edged() {
    use mineflayer_engine::{
        entity::EntityMetadataItems,
        protocol::packets::game::c_set_entity_data::ClientboundSetEntityData,
    };

    fn packet(items: Vec<EntityDataItem>) -> ClientboundGamePacket {
        ClientboundGamePacket::SetEntityData(ClientboundSetEntityData {
            id: MinecraftEntityId(7),
            packed_items: EntityMetadataItems(items),
        })
    }
    let flags = |b: u8| EntityDataItem {
        index: 0,
        value: EntityDataValue::Byte(b),
    };

    let client = crate::test_util::live_entity_client(());
    let e = client.ecs.write().spawn(MinecraftEntityId(7)).id();
    let mut tracker = EntityTracker::default();
    tracker.known.insert(MinecraftEntityId(7), e);

    // fall-flying bit: fires on the false->true edge only
    let out = tracker.handle_packet(&client, &packet(vec![flags(0x80)]));
    assert!(
        matches!(
            &out[..],
            [BotEvent::EntityUpdate(_), BotEvent::EntityElytraFlew(_)]
        ),
        "expected Update+ElytraFlew, got {out:?}"
    );
    let out = tracker.handle_packet(&client, &packet(vec![flags(0x80)]));
    assert!(
        matches!(&out[..], [BotEvent::EntityUpdate(_)]),
        "expected EntityUpdate only, got {out:?}"
    );
    // bit clears then sets again: a fresh edge refires
    tracker.handle_packet(&client, &packet(vec![flags(0x00)]));
    let out = tracker.handle_packet(&client, &packet(vec![flags(0x80)]));
    assert!(
        matches!(
            &out[..],
            [BotEvent::EntityUpdate(_), BotEvent::EntityElytraFlew(_)]
        ),
        "expected Update+ElytraFlew, got {out:?}"
    );
}

#[test]
fn profile_keys_tracked_from_player_info_update() {
    use mineflayer_engine::{
        auth::game_profile::GameProfile,
        protocol::packets::game::{
            c_player_info_remove::ClientboundPlayerInfoRemove,
            c_player_info_update::{ActionEnumSet, ClientboundPlayerInfoUpdate, PlayerInfoEntry},
            s_chat_session_update::{ProfilePublicKeyData, RemoteChatSessionData},
        },
    };

    let client = crate::test_util::despawned_client();
    let mut tracker = EntityTracker::default();
    let uuid = uuid::Uuid::from_u128(0x1234);
    tracker.handle_packet(
        &client,
        &ClientboundGamePacket::PlayerInfoUpdate(ClientboundPlayerInfoUpdate {
            actions: ActionEnumSet {
                add_player: true,
                initialize_chat: true,
                update_game_mode: true,
                update_listed: true,
                update_latency: true,
                update_display_name: false,
                update_list_order: false,
                update_hat: false,
            },
            entries: vec![PlayerInfoEntry {
                profile: GameProfile::new(uuid, "faye".into()),
                chat_session: Some(RemoteChatSessionData {
                    session_id: uuid::Uuid::from_u128(0xbeef),
                    profile_public_key: ProfilePublicKeyData {
                        expires_at: 0,
                        key: vec![1, 2, 3],
                        key_signature: vec![4, 5],
                    },
                }),
                ..Default::default()
            }],
        }),
    );
    assert_eq!(
        tracker.chat_sessions.get(&uuid),
        Some(&ProfileKeys {
            public_key: vec![1, 2, 3],
            signature: vec![4, 5],
        })
    );

    // a re-add without a chat session clears the old key
    tracker.handle_packet(
        &client,
        &ClientboundGamePacket::PlayerInfoUpdate(ClientboundPlayerInfoUpdate {
            actions: ActionEnumSet {
                add_player: true,
                initialize_chat: false,
                update_game_mode: false,
                update_listed: false,
                update_latency: false,
                update_display_name: false,
                update_list_order: false,
                update_hat: false,
            },
            entries: vec![PlayerInfoEntry {
                profile: GameProfile::new(uuid, "faye".into()),
                ..Default::default()
            }],
        }),
    );
    assert!(tracker.chat_sessions.is_empty());

    // re-add the key, then PlayerInfoRemove clears it
    tracker.chat_sessions.insert(
        uuid,
        ProfileKeys {
            public_key: vec![1],
            signature: vec![2],
        },
    );
    tracker.handle_packet(
        &client,
        &ClientboundGamePacket::PlayerInfoRemove(ClientboundPlayerInfoRemove {
            profile_ids: vec![uuid],
        }),
    );
    assert!(tracker.chat_sessions.is_empty());
}
