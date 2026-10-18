use mineflayer_engine::{
    app::{App, Plugin, Update},
    ecs::{self as bevy_ecs, prelude::*},
    protocol::packets::game::{
        ClientboundResourcePackPush, ClientboundSetHealth, ServerboundGamePacket,
    },
};

use crate::{BotBuilder, test_util::sim::BotSim};

fn sent_after_death(builder: BotBuilder) -> Vec<ServerboundGamePacket> {
    let mut sim = BotSim::spawned(builder);
    sim.sent();
    sim.receive(ClientboundSetHealth {
        health: 0.,
        food: 20,
        saturation: 5.,
    });
    sim.tick();
    sim.tick();
    sim.sent()
}

fn is_respawn(p: &ServerboundGamePacket) -> bool {
    matches!(p, ServerboundGamePacket::ClientCommand(_))
}

#[test]
fn the_bot_respawns_unless_respawn_is_off() {
    assert!(
        sent_after_death(BotBuilder::default())
            .iter()
            .any(is_respawn)
    );
    assert!(
        !sent_after_death(BotBuilder::default().respawn(false))
            .iter()
            .any(is_respawn)
    );
}

fn sent_after_pack(builder: BotBuilder) -> Vec<ServerboundGamePacket> {
    let mut sim = BotSim::spawned(builder);
    sim.sent();
    sim.receive(ClientboundResourcePackPush {
        id: uuid::Uuid::from_u128(5),
        url: "https://packs.invalid/p.zip".into(),
        hash: String::new(),
        required: false,
        prompt: None,
    });
    sim.tick();
    sim.tick();
    sim.sent()
}

fn is_pack_answer(p: &ServerboundGamePacket) -> bool {
    matches!(p, ServerboundGamePacket::ResourcePack(_))
}

#[test]
fn packs_are_accepted_unless_accept_resource_packs_is_off() {
    assert!(
        sent_after_pack(BotBuilder::default())
            .iter()
            .any(is_pack_answer)
    );
    assert!(
        !sent_after_pack(BotBuilder::default().accept_resource_packs(false))
            .iter()
            .any(is_pack_answer)
    );
}

/// With the engine's answer off, the offer still arrives as an event,
/// and a denial is the only answer that goes out.
#[test]
fn a_denial_is_the_only_answer_when_accept_is_off() {
    use mineflayer_engine::protocol::packets::game::s_resource_pack::Action;

    let mut sim = BotSim::spawned(BotBuilder::default().accept_resource_packs(false));
    sim.events();
    sim.sent();
    sim.receive(ClientboundResourcePackPush {
        id: uuid::Uuid::from_u128(5),
        url: "https://packs.invalid/p.zip".into(),
        hash: String::new(),
        required: false,
        prompt: None,
    });
    sim.tick();
    let offered = sim.events().into_iter().any(|e| {
        matches!(e, crate::BotEvent::ResourcePack { uuid, .. } if uuid == uuid::Uuid::from_u128(5))
    });
    assert!(offered, "the offer is an event");
    sim.bot.deny_resource_pack().unwrap();
    sim.tick();
    let answers: Vec<Action> = sim
        .sent()
        .into_iter()
        .filter_map(|p| match p {
            ServerboundGamePacket::ResourcePack(p) => Some(p.action),
            _ => None,
        })
        .collect();
    assert_eq!(answers, [Action::Declined]);
}

#[derive(Component, Default)]
struct Ticks(u32);

#[derive(Clone)]
struct CountUpdates;

impl Plugin for CountUpdates {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, |mut q: Query<&mut Ticks>| {
            for mut t in &mut q {
                t.0 += 1;
            }
        });
    }
}

#[test]
fn a_user_plugin_runs_in_the_bot_app() {
    let mut sim = BotSim::spawned(BotBuilder::default().plugin(CountUpdates));
    let entity = sim.bot.client.entity;
    sim.bot
        .client
        .ecs
        .write()
        .entity_mut(entity)
        .insert(Ticks(0));
    sim.tick();
    sim.tick();
    assert_eq!(
        crate::bot::with_component::<Ticks, _>(&sim.bot.client, |t| t.0),
        Some(2)
    );
}
