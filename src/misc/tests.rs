use std::io::Cursor;

use mineflayer_engine::{
    protocol::packets::game::{
        c_clear_titles::ClientboundClearTitles,
        c_game_event::{ClientboundGameEvent, EventType},
        c_level_particles::ClientboundLevelParticles,
        c_resource_pack_push::ClientboundResourcePackPush,
        c_set_subtitle_text::ClientboundSetSubtitleText,
        c_set_title_text::ClientboundSetTitleText,
        c_set_titles_animation::ClientboundSetTitlesAnimation,
        c_sound::{ClientboundSound, SoundSource},
        c_sound_entity::ClientboundSoundEntity,
    },
    registry::Holder,
};

use super::*;

fn run(packet: ClientboundGamePacket) -> (MiscState, Vec<BotEvent>) {
    let mut s = MiscState::default();
    let events = handle_packet_state(&mut s, &packet, None);
    (s, events)
}

#[test]
fn title_packets() {
    let (_, e) = run(ClientboundGamePacket::SetTitleText(
        ClientboundSetTitleText {
            text: "Hello".into(),
        },
    ));
    assert!(
        matches!(&e[..], [BotEvent::Title { kind: TitleKind::Title, text }] if text == "Hello")
    );

    let (_, e) = run(ClientboundGamePacket::SetSubtitleText(
        ClientboundSetSubtitleText {
            text: "World".into(),
        },
    ));
    assert!(
        matches!(&e[..], [BotEvent::Title { kind: TitleKind::Subtitle, text }] if text == "World")
    );

    let (_, e) = run(ClientboundGamePacket::SetTitlesAnimation(
        ClientboundSetTitlesAnimation {
            fade_in: 10,
            stay: 70,
            fade_out: 20,
        },
    ));
    assert!(matches!(
        &e[..],
        [BotEvent::TitleTimes {
            fade_in: 10,
            stay: 70,
            fade_out: 20
        }]
    ));

    let (_, e) = run(ClientboundGamePacket::ClearTitles(ClientboundClearTitles {
        reset_times: true,
    }));
    assert!(matches!(&e[..], [BotEvent::TitleClear]));
}

#[test]
fn particle_event_fields() {
    let (_, e) = run(ClientboundGamePacket::LevelParticles(
        ClientboundLevelParticles {
            override_limiter: false,
            always_show: true,
            pos: Vec3::new(1.0, 2.0, 3.0),
            x_dist: 0.1,
            y_dist: 0.2,
            z_dist: 0.3,
            max_speed: 0.5,
            count: 6,
            particle: mineflayer_engine::entity::particle::Particle::Fishing,
        },
    ));
    let [BotEvent::ParticleSpawned(p)] = &e[..] else {
        panic!("expected one ParticleSpawned, got {e:?}")
    };
    assert_eq!(p.name, "minecraft:fishing");
    assert!(p.long_distance_render);
    assert_eq!(p.count, 6);
    assert_eq!(p.position, Vec3::new(1.0, 2.0, 3.0));
    // offsets go through f32, so compare with tolerance
    assert!(
        (p.offset.x - 0.1).abs() < 1e-6
            && (p.offset.y - 0.2).abs() < 1e-6
            && (p.offset.z - 0.3).abs() < 1e-6
    );
    assert_eq!(
        p.data,
        mineflayer_engine::entity::particle::Particle::Fishing
    );
    // the id round-trips through the wire encoding
    assert_eq!(p.id, ParticleKind::to_u32(&ParticleKind::Fishing));
}

#[test]
fn sound_event_named_and_custom() {
    // a registry reference yields the builtin name
    let (_, e) = run(ClientboundGamePacket::Sound(ClientboundSound {
        sound: Holder::Reference(SoundEvent::EntityFishingBobberSplash),
        source: SoundSource::Neutral,
        x: 80,
        y: 520,
        z: -160,
        volume: 1.0,
        pitch: 1.5,
        seed: 0,
    }));
    assert!(
        matches!(&e[..], [BotEvent::SoundHeard { name, pos, volume: 1.0, pitch: 1.5 }] if name == "minecraft:entity.fishing_bobber.splash" && *pos == Vec3::new(10.0, 65.0, -20.0))
    );

    // a direct custom sound carries its own identifier
    let (_, e) = run(ClientboundGamePacket::Sound(ClientboundSound {
        sound: Holder::Direct(mineflayer_engine::core::sound::CustomSound {
            sound_id: mineflayer_engine::Identifier::new("custom:thing"),
            range: None,
        }),
        source: SoundSource::Master,
        x: 0,
        y: 0,
        z: 0,
        volume: 1.0,
        pitch: 1.0,
        seed: 0,
    }));
    assert!(matches!(&e[..], [BotEvent::SoundHeard { name, .. }] if name == "custom:thing"));
}

#[test]
fn note_heard_instruments() {
    let packet = |instrument: u8| {
        ClientboundGamePacket::BlockEvent(
            mineflayer_engine::protocol::packets::game::c_block_event::ClientboundBlockEvent {
                pos: mineflayer_engine::BlockPos::new(1, 2, 3),
                action_id: instrument,
                action_parameter: 12,
                block: BlockKind::NoteBlock,
            },
        )
    };
    let (_, e) = run(packet(7));
    assert!(matches!(
        &e[..],
        [BotEvent::NoteHeard {
            instrument: Instrument::Guitar,
            pitch: 12,
            ..
        }]
    ));
    // out of range falls back to harp
    let (_, e) = run(packet(99));
    assert!(matches!(
        &e[..],
        [BotEvent::NoteHeard {
            instrument: Instrument::Harp,
            ..
        }]
    ));
    // non-note-block block events are not ours
    let (_, e) = run(ClientboundGamePacket::BlockEvent(
        mineflayer_engine::protocol::packets::game::c_block_event::ClientboundBlockEvent {
            pos: mineflayer_engine::BlockPos::new(0, 0, 0),
            action_id: 0,
            action_parameter: 0,
            block: BlockKind::Piston,
        },
    ));
    assert!(e.is_empty());
}

#[test]
fn game_event_spawn_reset() {
    let (_, e) = run(ClientboundGamePacket::GameEvent(ClientboundGameEvent {
        event: EventType::NoRespawnBlockAvailable,
        param: 0.0,
    }));
    assert!(matches!(&e[..], [BotEvent::SpawnReset]));

    // other game events aren't ours (time/weather is another unit's)
    let (_, e) = run(ClientboundGamePacket::GameEvent(ClientboundGameEvent {
        event: EventType::StartRaining,
        param: 0.0,
    }));
    assert!(e.is_empty());
}

#[test]
fn resource_pack_push_tracked() {
    let mut s = MiscState::default();
    let push = ClientboundResourcePackPush {
        id: mineflayer_engine::buf::McBuf::mc_read(&mut Cursor::new(&[0u8; 16][..])).unwrap(),
        url: "https://packs.example/x.zip".into(),
        hash: "deadbeef".into(),
        required: true,
        prompt: None,
    };
    let events = handle_packet_state(&mut s, &ClientboundGamePacket::ResourcePackPush(push), None);
    assert!(
        matches!(&events[..], [BotEvent::ResourcePack { uuid, url, hash, required }] if url == "https://packs.example/x.zip" && hash == "deadbeef" && *required && uuid.is_nil())
    );
    assert_eq!(s.latest_resource_pack.unwrap().hash, "deadbeef");
}

#[test]
fn settings_map_to_client_information() {
    let settings = Settings {
        view_distance: 5.into(),
        chat_visibility: ChatVisibility::System,
        main_hand: mineflayer_engine::entity::HumanoidArm::Left,
        ..Default::default()
    };
    let info = ClientInformation::from(&settings);
    assert_eq!(info.view_distance, 5);

    // upstream's named presets: far/normal/short/tiny → 12/10/8/6
    for (alias, chunks) in [("far", 12), ("normal", 10), ("short", 8), ("tiny", 6)] {
        let parsed: ViewDistance = alias.parse().unwrap();
        assert_eq!(parsed.chunks(), chunks);
        let info = ClientInformation::from(&Settings {
            view_distance: parsed,
            ..Default::default()
        });
        assert_eq!(info.view_distance, chunks);
    }
    assert_eq!("12".parse::<ViewDistance>().unwrap().chunks(), 12);
    assert!("far-ish".parse::<ViewDistance>().is_err());
    assert_eq!(info.chat_visibility, ChatVisibility::System);
    assert_eq!(info.main_hand, mineflayer_engine::entity::HumanoidArm::Left);
    assert_eq!(info.language, "en_us");
    // the skin-parts bitfield keeps all seven flags on by default
    let skin = info.model_customization;
    assert!(
        skin.cape
            && skin.jacket
            && skin.left_sleeve
            && skin.right_sleeve
            && skin.left_pants
            && skin.right_pants
            && skin.hat
    );
    // and a subset can be turned off per flag
    let info = ClientInformation::from(&Settings {
        model_customization: ModelCustomization {
            cape: false,
            jacket: false,
            left_sleeve: true,
            right_sleeve: false,
            left_pants: true,
            right_pants: false,
            hat: false,
        },
        ..Default::default()
    });
    let skin = info.model_customization;
    assert!(
        !skin.cape
            && !skin.jacket
            && skin.left_sleeve
            && !skin.right_sleeve
            && skin.left_pants
            && !skin.right_pants
            && !skin.hat
    );
    // the skin-parts subset round-trips back
    let back = Settings::from(info);
    let skin = back.model_customization;
    assert!(skin.left_sleeve && skin.left_pants && !skin.cape && !skin.hat);
    // and `difficulty` keeps its upstream default
    assert_eq!(back.difficulty, 2);
}

#[test]
fn sound_entity_after_self_despawn_uses_a_default_position() {
    let client = crate::test_util::despawned_client();
    // the sound position resolves through our own entity's index — once
    // it is despawned the lookup must miss, not panic; the event still
    // goes out (the reducer needs no ECS state).
    let out = handle_packet(
        &client,
        &ClientboundGamePacket::SoundEntity(ClientboundSoundEntity {
            sound: Holder::Reference(SoundEvent::EntityFishingBobberSplash),
            source: SoundSource::Neutral,
            id: MinecraftEntityId(1),
            volume: 1.0,
            pitch: 1.0,
            seed: 0,
        }),
    );
    assert!(matches!(
        &out[..],
        [BotEvent::SoundHeard { pos, .. }] if *pos == Vec3::ZERO
    ));
}

/// `fish()` must not park on disconnect: `End` is the in-band exit —
/// the `events` sender lives on `Bot`, so `Closed` can't be counted
/// on (the bridge's shutdown watch is the backstop for the
/// lagged-out case).
/// `fish()` follows the derived events: a bobber `EntitySpawn`,
/// then `EntityGone` for that id, rejects with upstream's
/// `Fishing cancelled` — no raw `Packet` event is needed.
#[tokio::test]
async fn fish_follows_derived_bobber_events() {
    let client = crate::test_util::live_entity_client((
        mineflayer_engine::world::WorldName(mineflayer_engine::Identifier::new(
            "minecraft:overworld",
        )),
        Position::new(Vec3::new(0., 64., 0.)),
    ));
    let bobber = client
        .ecs
        .write()
        .spawn((
            EntityKindComponent(EntityKind::FishingBobber),
            MinecraftEntityId(42),
        ))
        .id();
    let bobber = client.entity_ref_for(bobber);
    let bot = crate::test_util::bot(client);
    let bot2 = bot.clone();
    let fishing = tokio::spawn(async move { bot2.fish().await });
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        while !fishing.is_finished() {
            bot.emit_event(BotEvent::EntitySpawn(bobber.clone()));
            bot.emit_event(BotEvent::EntityGone {
                entity: None,
                id: MinecraftEntityId(42),
            });
            tokio::task::yield_now().await;
        }
        fishing.await.unwrap()
    })
    .await
    .expect("fish ignored the bobber events");
    assert!(
        matches!(&result, Err(Error::Cancelled(m)) if m == "Fishing cancelled"),
        "{result:?}"
    );
}

#[tokio::test]
async fn fish_exits_on_end() {
    let bot = crate::test_util::bot(crate::test_util::live_entity_client((
        mineflayer_engine::world::WorldName(mineflayer_engine::Identifier::new(
            "minecraft:overworld",
        )),
        Position::new(Vec3::new(0., 64., 0.)),
    )));
    let bot2 = bot.clone();
    let fishing = tokio::spawn(async move { bot2.fish().await });
    // `End` can land before the task subscribes — keep emitting until
    // one is seen
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        while !fishing.is_finished() {
            bot.emit_event(BotEvent::End("socketClosed".to_string()));
            tokio::task::yield_now().await;
        }
        fishing.await.unwrap()
    })
    .await
    .expect("fish ignored End");
    assert!(matches!(result, Err(Error::NotConnected)));
}
