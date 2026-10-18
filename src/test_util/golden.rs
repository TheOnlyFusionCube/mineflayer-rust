//! The golden event sequence of the bridge: a fixed packet script through
//! [`BotSim`], compared with `tests/golden/bridge_events.txt`.
//!
//! The event order is public behavior. A change that moves the bridge or
//! its reducers must keep this file unchanged, or say in its commit why
//! the order changed. `UPDATE_GOLDEN=1 cargo test golden` rewrites it.

use mineflayer_engine::{
    BlockPos,
    block::BlockState,
    chat::numbers::NumberFormat,
    chat::style::ChatFormatting,
    core::{
        delta::PositionDelta8,
        entity_id::MinecraftEntityId,
        objectives::ObjectiveCriteria,
        position::{ChunkPos, ChunkSectionBlockPos, ChunkSectionPos},
    },
    entity::{EntityDataItem, EntityDataValue, EntityMetadataItems},
    inventory::ItemStack,
    protocol::packets::game::{
        ClientboundBlockUpdate, ClientboundBossEvent, ClientboundContainerClose,
        ClientboundContainerSetContent, ClientboundContainerSetSlot, ClientboundMoveEntityPos,
        ClientboundOpenScreen, ClientboundPlayerCombatKill, ClientboundRemoveEntities,
        ClientboundSectionBlocksUpdate, ClientboundSetEntityData, ClientboundSetHealth,
        ClientboundSetObjective, ClientboundSetPlayerTeam, ClientboundSetScore,
        ClientboundSetTitleText,
        c_boss_event::{AddOperation, BossBarColor, BossBarOverlay, Operation, Properties, Style},
        c_section_blocks_update::BlockStateWithPosition,
        c_set_objective,
        c_set_player_team::{self, CollisionRule, NameTagVisibility, Parameters},
    },
    registry::{
        DataRegistry,
        builtin::{BlockKind, EntityKind, ItemKind, MenuKind},
    },
    test_utils::simulation::{
        make_basic_add_entity, make_basic_empty_chunk, make_basic_respawn_packet,
    },
};

use super::sim::BotSim;
use crate::BotEvent;

/// One line per event. Raw packets show only their kind, because their
/// `Debug` form is long and is the engine's, not the port's.
fn line(event: &BotEvent) -> String {
    match event {
        BotEvent::Packet(packet) => {
            let debug = format!("{packet:?}");
            let kind = debug.split('(').next().unwrap_or_default();
            format!("Packet({kind})")
        }
        other => format!("{other:?}"),
    }
}

fn run_script() -> String {
    let mut sim = BotSim::new();
    let mut out = String::new();
    let mut step = |sim: &mut BotSim, name: &str| {
        sim.tick();
        out.push_str(&format!("# {name}\n"));
        for event in sim.events() {
            out.push_str(&line(&event));
            out.push('\n');
        }
    };
    step(&mut sim, "login and the first chunk");

    sim.receive(make_basic_empty_chunk(ChunkPos::new(1, 0), 384 / 16));
    step(&mut sim, "a second chunk");

    sim.receive(ClientboundBlockUpdate {
        pos: BlockPos::new(1, 2, 3),
        block_state: BlockState::from(BlockKind::Stone),
    });
    step(&mut sim, "a block update");

    sim.receive(ClientboundSectionBlocksUpdate {
        section_pos: ChunkSectionPos { x: 1, y: 0, z: 0 },
        states: vec![
            BlockStateWithPosition {
                pos: ChunkSectionBlockPos { x: 1, y: 1, z: 1 },
                state: BlockState::from(BlockKind::Dirt),
            },
            BlockStateWithPosition {
                pos: ChunkSectionBlockPos { x: 2, y: 1, z: 1 },
                state: BlockState::from(BlockKind::OakPlanks),
            },
        ],
    });
    step(&mut sim, "a section update");

    sim.receive(make_basic_add_entity(
        EntityKind::Zombie,
        10,
        (2.5, 1.0, 2.5),
    ));
    step(&mut sim, "an entity spawn");

    sim.receive(ClientboundMoveEntityPos {
        entity_id: MinecraftEntityId(10),
        delta: PositionDelta8 {
            xa: 4096,
            ya: 0,
            za: 0,
        },
        on_ground: true,
    });
    step(&mut sim, "an entity move");

    sim.receive(ClientboundSetEntityData {
        id: MinecraftEntityId(10),
        packed_items: EntityMetadataItems(vec![EntityDataItem {
            index: 0,
            value: EntityDataValue::Byte(0x02),
        }]),
    });
    step(&mut sim, "entity metadata (crouch)");

    sim.receive(ClientboundRemoveEntities {
        entity_ids: vec![MinecraftEntityId(10)],
    });
    step(&mut sim, "an entity removal");

    sim.receive(ClientboundOpenScreen {
        container_id: 3,
        menu_type: MenuKind::Generic9x3,
        title: "Chest".into(),
    });
    step(&mut sim, "a window open");

    let mut items = vec![ItemStack::Empty; 27 + 36];
    items[0] = ItemStack::new(ItemKind::Stone, 5);
    sim.receive(ClientboundContainerSetContent {
        container_id: 3,
        state_id: 1,
        items,
        carried_item: ItemStack::Empty,
    });
    step(&mut sim, "a window fill");

    sim.receive(ClientboundContainerSetSlot {
        container_id: 3,
        state_id: 2,
        slot: 1,
        item_stack: ItemStack::new(ItemKind::Dirt, 3),
    });
    step(&mut sim, "a slot change");

    sim.receive(ClientboundContainerClose { container_id: 3 });
    step(&mut sim, "a window close");

    sim.receive(ClientboundSetObjective {
        objective_name: "kills".into(),
        method: c_set_objective::Method::Add {
            display_name: "Kills".into(),
            render_type: ObjectiveCriteria::Integer,
            number_format: NumberFormat::Blank,
        },
    });
    sim.receive(ClientboundSetScore {
        owner: "alice".into(),
        objective_name: "kills".into(),
        score: 3,
        display: None,
        number_format: None,
    });
    step(&mut sim, "a scoreboard objective and a score");

    sim.receive(ClientboundSetPlayerTeam {
        name: "red".into(),
        method: c_set_player_team::Method::Add((
            Parameters {
                display_name: "Red".into(),
                options: 0x01,
                nametag_visibility: NameTagVisibility::Always,
                collision_rule: CollisionRule::Always,
                color: ChatFormatting::Red,
                player_prefix: "".into(),
                player_suffix: "".into(),
            },
            vec!["alice".into()],
        )),
    });
    step(&mut sim, "a team");

    sim.receive(ClientboundBossEvent {
        id: uuid::Uuid::from_u128(7),
        operation: Operation::Add(AddOperation {
            name: "Boss".into(),
            progress: 0.5,
            style: Style {
                color: BossBarColor::Red,
                overlay: BossBarOverlay::Progress,
            },
            properties: Properties {
                darken_screen: false,
                play_music: false,
                create_world_fog: false,
            },
        }),
    });
    step(&mut sim, "a boss bar");

    sim.receive(ClientboundSetTitleText {
        text: "Welcome".into(),
    });
    step(&mut sim, "a title");

    sim.receive(ClientboundSetHealth {
        health: 0.0,
        food: 20,
        saturation: 5.0,
    });
    sim.receive(ClientboundPlayerCombatKill {
        player_id: MinecraftEntityId(0),
        message: "died".into(),
    });
    step(&mut sim, "a death");

    sim.receive(make_basic_respawn_packet(
        mineflayer_engine::registry::data::DimensionKind::new_raw(0),
        mineflayer_engine::Identifier::new("minecraft:overworld"),
    ));
    sim.receive(ClientboundSetHealth {
        health: 20.0,
        food: 20,
        saturation: 5.0,
    });
    step(&mut sim, "a respawn");

    out
}

#[test]
fn the_bridge_event_order_matches_the_golden_file() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/golden/bridge_events.txt"
    );
    let got = run_script();
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap()).unwrap();
        std::fs::write(path, &got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(path)
        .expect("tests/golden/bridge_events.txt is missing; run with UPDATE_GOLDEN=1");
    if got != want {
        let first = got
            .lines()
            .zip(want.lines())
            .position(|(g, w)| g != w)
            .unwrap_or(got.lines().count().min(want.lines().count()));
        panic!(
            "the bridge event order changed at line {}:\n  want: {}\n  got:  {}\n\
             (UPDATE_GOLDEN=1 rewrites the file; say why in the commit)",
            first + 1,
            want.lines().nth(first).unwrap_or("<end>"),
            got.lines().nth(first).unwrap_or("<end>"),
        );
    }
}

#[test]
fn the_script_is_deterministic() {
    assert_eq!(run_script(), run_script());
}
