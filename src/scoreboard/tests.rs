
use mineflayer_engine::protocol::packets::game::{
    c_boss_event::{AddOperation, BossBarColor, BossBarOverlay, Operation, Properties, Style},
    c_set_objective::Method,
    c_set_player_team::{CollisionRule, Method as TeamMethod, NameTagVisibility, Parameters},
};

use super::*;

fn params() -> Parameters {
    Parameters {
        display_name: "The Team".into(),
        player_prefix: "[P]".into(),
        player_suffix: "[S]".into(),
        nametag_visibility: NameTagVisibility::HideForOwnTeam,
        collision_rule: CollisionRule::PushOtherTeams,
        color: ChatFormatting::Red,
        options: 0x03,
    }
}

#[test]
fn objective_lifecycle() {
    let mut s = ScoreboardState::default();

    // add
    let events = handle(
        &mut s,
        &ClientboundGamePacket::SetObjective(
            mineflayer_engine::protocol::packets::game::c_set_objective::ClientboundSetObjective {
                objective_name: "obj".into(),
                method: Method::Add {
                    display_name: "My Objective".into(),
                    render_type: mineflayer_engine::core::objectives::ObjectiveCriteria::Integer,
                    number_format: mineflayer_engine::chat::numbers::NumberFormat::Blank,
                },
            },
        ),
    )
    .unwrap();
    assert!(
        matches!(&events[..], [BotEvent::ScoreboardCreated(sb)] if sb.name == "obj" && sb.title.to_string() == "My Objective")
    );
    assert_eq!(s.scoreboards["obj"].title.to_string(), "My Objective");

    // change title
    let events = handle(
        &mut s,
        &ClientboundGamePacket::SetObjective(
            mineflayer_engine::protocol::packets::game::c_set_objective::ClientboundSetObjective {
                objective_name: "obj".into(),
                method: Method::Change {
                    display_name: "Renamed".into(),
                    render_type: mineflayer_engine::core::objectives::ObjectiveCriteria::Integer,
                    number_format: mineflayer_engine::chat::numbers::NumberFormat::Blank,
                },
            },
        ),
    )
    .unwrap();
    assert!(
        matches!(&events[..], [BotEvent::ScoreboardTitleChanged(sb)] if sb.title.to_string() == "Renamed")
    );

    // score add + sorted items
    let events = handle(
        &mut s,
        &ClientboundGamePacket::SetScore(
            mineflayer_engine::protocol::packets::game::c_set_score::ClientboundSetScore {
                owner: "alice".into(),
                objective_name: "obj".into(),
                score: 7,
                display: None,
                number_format: None,
            },
        ),
    )
    .unwrap();
    assert!(
        matches!(&events[..], [BotEvent::ScoreUpdated { item, .. }] if item.name == "alice" && item.value == 7)
    );
    handle(
        &mut s,
        &ClientboundGamePacket::SetScore(
            mineflayer_engine::protocol::packets::game::c_set_score::ClientboundSetScore {
                owner: "bob".into(),
                objective_name: "obj".into(),
                score: 9,
                display: None,
                number_format: None,
            },
        ),
    );
    assert_eq!(s.scoreboards["obj"].items()[0].name, "bob");

    // reset by owner across scoreboards (objective_name None)
    let events = handle(
        &mut s,
        &ClientboundGamePacket::ResetScore(
            mineflayer_engine::protocol::packets::game::c_reset_score::ClientboundResetScore {
                owner: "bob".into(),
                objective_name: None,
            },
        ),
    )
    .unwrap();
    assert!(matches!(&events[..], [BotEvent::ScoreRemoved { item, .. }] if item.name == "bob"));
    assert!(!s.scoreboards["obj"].items_map().contains_key("bob"));

    // display slot binding
    let events = handle(
            &mut s,
            &ClientboundGamePacket::SetDisplayObjective(
                mineflayer_engine::protocol::packets::game::c_set_display_objective::ClientboundSetDisplayObjective {
                    slot: DisplaySlot::Sidebar,
                    objective_name: "obj".into(),
                },
            ),
        )
        .unwrap();
    assert!(
        matches!(&events[..], [BotEvent::ScoreboardPosition { slot: ScoreboardSlot::Sidebar, scoreboard, .. }] if scoreboard.name == "obj")
    );

    // remove objective clears its display slots too
    let events = handle(
        &mut s,
        &ClientboundGamePacket::SetObjective(
            mineflayer_engine::protocol::packets::game::c_set_objective::ClientboundSetObjective {
                objective_name: "obj".into(),
                method: Method::Remove,
            },
        ),
    )
    .unwrap();
    assert!(matches!(&events[..], [BotEvent::ScoreboardDeleted(sb)] if sb.name == "obj"));
    assert!(s.scoreboards.is_empty());
    assert!(s.display_slots.is_empty());
}

#[test]
fn display_slot_previous_board_and_unknown_bind() {
    let mut s = ScoreboardState::default();
    let objective = |name: &str| {
        ClientboundGamePacket::SetObjective(
            mineflayer_engine::protocol::packets::game::c_set_objective::ClientboundSetObjective {
                objective_name: name.into(),
                method: Method::Add {
                    display_name: name.into(),
                    render_type: mineflayer_engine::core::objectives::ObjectiveCriteria::Integer,
                    number_format: mineflayer_engine::chat::numbers::NumberFormat::Blank,
                },
            },
        )
    };
    let display = |name: &str| {
        ClientboundGamePacket::SetDisplayObjective(
                mineflayer_engine::protocol::packets::game::c_set_display_objective::ClientboundSetDisplayObjective {
                    slot: DisplaySlot::Sidebar,
                    objective_name: name.into(),
                },
            )
    };

    // a display packet for an unknown objective neither binds nor emits
    let events = handle(&mut s, &display("ghost")).unwrap();
    assert!(events.is_empty());
    assert!(s.display_slots.is_empty());

    handle(&mut s, &objective("first"));
    let events = handle(&mut s, &display("first")).unwrap();
    assert!(matches!(
        &events[..],
        [BotEvent::ScoreboardPosition {
            previous_board: None,
            ..
        }]
    ));

    // rebinding the slot reports the objective it replaced
    handle(&mut s, &objective("second"));
    let events = handle(&mut s, &display("second")).unwrap();
    assert!(
        matches!(&events[..], [BotEvent::ScoreboardPosition { previous_board: Some(prev), scoreboard, .. }] if prev.name == "first" && scoreboard.name == "second")
    );
}

#[test]
fn negative_scores_unwrap_and_sort_last() {
    let mut s = ScoreboardState::default();
    handle(
        &mut s,
        &ClientboundGamePacket::SetObjective(
            mineflayer_engine::protocol::packets::game::c_set_objective::ClientboundSetObjective {
                objective_name: "obj".into(),
                method: Method::Add {
                    display_name: "x".into(),
                    render_type: mineflayer_engine::core::objectives::ObjectiveCriteria::Integer,
                    number_format: mineflayer_engine::chat::numbers::NumberFormat::Blank,
                },
            },
        ),
    );
    let set_score = |owner: &str, score: u32| {
        ClientboundGamePacket::SetScore(
            mineflayer_engine::protocol::packets::game::c_set_score::ClientboundSetScore {
                owner: owner.into(),
                objective_name: "obj".into(),
                score,
                display: None,
                number_format: None,
            },
        )
    };
    // a wire -1 decodes through the engine's u32 as 0xFFFFFFFF
    let events = handle(&mut s, &set_score("neg", u32::MAX)).unwrap();
    assert!(
        matches!(&events[..], [BotEvent::ScoreUpdated { item, .. }] if item.value == -1),
        "wire -1 must surface as -1, not wrapped u32::MAX"
    );
    handle(&mut s, &set_score("pos", 5));
    handle(&mut s, &set_score("zero", 0));
    // descending signed order: 5, 0, -1 — the wrapped score must
    // not sort above the positives
    let items = s.scoreboards["obj"].items();
    let order: Vec<(&str, i32)> = items.iter().map(|i| (i.name.as_str(), i.value)).collect();
    assert_eq!(order, [("pos", 5), ("zero", 0), ("neg", -1)]);
}

#[test]
fn unknown_objective_update_is_error_event() {
    let mut s = ScoreboardState::default();
    let events = handle(
        &mut s,
        &ClientboundGamePacket::SetObjective(
            mineflayer_engine::protocol::packets::game::c_set_objective::ClientboundSetObjective {
                objective_name: "ghost".into(),
                method: Method::Change {
                    display_name: "x".into(),
                    render_type: mineflayer_engine::core::objectives::ObjectiveCriteria::Integer,
                    number_format: mineflayer_engine::chat::numbers::NumberFormat::Blank,
                },
            },
        ),
    )
    .unwrap();
    assert!(matches!(&events[..], [BotEvent::Error(e)] if e.contains("ghost")));
}

#[test]
fn team_lifecycle() {
    let mut s = ScoreboardState::default();

    let events = handle(
            &mut s,
            &ClientboundGamePacket::SetPlayerTeam(
                mineflayer_engine::protocol::packets::game::c_set_player_team::ClientboundSetPlayerTeam {
                    name: "t".into(),
                    method: TeamMethod::Add((params(), vec!["alice".into(), "bob".into()])),
                },
            ),
        )
        .unwrap();
    assert!(
        matches!(&events[..], [BotEvent::TeamCreated(t)] if t.name == "t" && t.members.len() == 2 && t.friendly_fire && t.see_friendly_invisibles && t.color == ChatFormatting::Red)
    );
    assert_eq!(s.team_map["alice"], "t");

    // join + leave
    let events = handle(
            &mut s,
            &ClientboundGamePacket::SetPlayerTeam(
                mineflayer_engine::protocol::packets::game::c_set_player_team::ClientboundSetPlayerTeam {
                    name: "t".into(),
                    method: TeamMethod::Join(vec!["carol".into()]),
                },
            ),
        )
        .unwrap();
    assert!(matches!(&events[..], [BotEvent::TeamMemberAdded(t)] if t.members.contains("carol")));
    assert_eq!(s.team_map["carol"], "t");
    let events = handle(
            &mut s,
            &ClientboundGamePacket::SetPlayerTeam(
                mineflayer_engine::protocol::packets::game::c_set_player_team::ClientboundSetPlayerTeam {
                    name: "t".into(),
                    method: TeamMethod::Leave(vec!["alice".into()]),
                },
            ),
        )
        .unwrap();
    assert!(
        matches!(&events[..], [BotEvent::TeamMemberRemoved(t)] if !t.members.contains("alice"))
    );
    assert!(!s.team_map.contains_key("alice"));

    // change
    let mut p2 = params();
    p2.options = 0;
    let events = handle(
            &mut s,
            &ClientboundGamePacket::SetPlayerTeam(
                mineflayer_engine::protocol::packets::game::c_set_player_team::ClientboundSetPlayerTeam {
                    name: "t".into(),
                    method: TeamMethod::Change(p2),
                },
            ),
        )
        .unwrap();
    assert!(
        matches!(&events[..], [BotEvent::TeamUpdated(t)] if !t.friendly_fire && !t.see_friendly_invisibles)
    );

    // member display name renders prefix + member + suffix
    let team = s.teams["t"].clone();
    assert_eq!(team.member_display_name("bob").to_string(), "[P]bob[S]");

    // remove unmaps every member
    let events = handle(
            &mut s,
            &ClientboundGamePacket::SetPlayerTeam(
                mineflayer_engine::protocol::packets::game::c_set_player_team::ClientboundSetPlayerTeam {
                    name: "t".into(),
                    method: TeamMethod::Remove,
                },
            ),
        )
        .unwrap();
    assert!(matches!(&events[..], [BotEvent::TeamRemoved(t)] if t.name == "t"));
    assert!(s.team_map.is_empty() && s.teams.is_empty());
}

#[test]
fn boss_bar_lifecycle() {
    let mut s = ScoreboardState::default();
    // `uuid` isn't in our dep set: build the packet id by decoding one
    // from the wire — McBuf gives us a `Uuid` without naming the type.
    let id =
        mineflayer_engine::buf::McBuf::mc_read(&mut std::io::Cursor::new(&[0u8; 16][..])).unwrap();
    let id_string = "00000000-0000-0000-0000-000000000000".to_string();
    let packet = |operation| {
        ClientboundGamePacket::BossEvent(
            mineflayer_engine::protocol::packets::game::c_boss_event::ClientboundBossEvent {
                id,
                operation,
            },
        )
    };

    let events = handle(
        &mut s,
        &packet(Operation::Add(AddOperation {
            name: "Ender Dragon".into(),
            progress: 1.0,
            style: Style {
                color: BossBarColor::Purple,
                overlay: BossBarOverlay::Notched10,
            },
            properties: Properties {
                darken_screen: true,
                play_music: true,
                create_world_fog: false,
            },
        })),
    )
    .unwrap();
    assert!(
        matches!(&events[..], [BotEvent::BossBarCreated(b)] if b.entity_uuid == id_string && b.dividers == 10 && b.color == BossBarColor::Purple && b.darken_sky && b.dragon_bar && !b.create_fog)
    );

    let events = handle(&mut s, &packet(Operation::UpdateProgress(0.4))).unwrap();
    assert!(
        matches!(&events[..], [BotEvent::BossBarUpdated(b)] if (b.health - 0.4).abs() < f32::EPSILON)
    );

    let events = handle(
        &mut s,
        &packet(Operation::UpdateStyle(Style {
            color: BossBarColor::White,
            overlay: BossBarOverlay::Progress,
        })),
    )
    .unwrap();
    assert!(
        matches!(&events[..], [BotEvent::BossBarUpdated(b)] if b.dividers == 0 && b.color == BossBarColor::White)
    );

    let events = handle(&mut s, &packet(Operation::Remove)).unwrap();
    assert!(matches!(&events[..], [BotEvent::BossBarDeleted(b)] if b.entity_uuid == id_string));
    assert!(s.boss_bars.is_empty());
}
/// Login/respawn clear objectives, teams, and the member map — but not
/// boss bars, like upstream's `login` handler. The clear belongs to
/// the scoreboard domain (it owns the state) and runs through the same
/// `observe` entry the bridge uses.
#[test]
fn login_clears_the_domain_state_through_observe() {
    use mineflayer_engine::{
        core::game_type::{GameMode, OptionalGameType},
        protocol::packets::{common::CommonPlayerSpawnInfo, game::c_login::ClientboundLogin},
    };

    // seed the state the domain owns
    let state = Mutex::new(ScoreboardState {
        scoreboards: [(
            "o".to_string(),
            ScoreBoard {
                name: "o".into(),
                title: "t".into(),
                items_map: Default::default(),
            },
        )]
        .into(),
        display_slots: [(ScoreboardSlot::Sidebar, "o".to_string())].into(),
        teams: [(
            "t".to_string(),
            Team {
                name: "t".into(),
                display_name: "d".into(),
                friendly_fire: false,
                see_friendly_invisibles: false,
                nametag_visibility: NameTagVisibility::Always,
                collision_rule: CollisionRule::Always,
                color: ChatFormatting::Red,
                prefix: "".into(),
                suffix: "".into(),
                members: Default::default(),
            },
        )]
        .into(),
        team_map: [("m".to_string(), "t".to_string())].into(),
        boss_bars: [(
            "b".to_string(),
            BossBar {
                entity_uuid: "b".into(),
                title: "t".into(),
                health: 1.0,
                dividers: 0,
                color: BossBarColor::Purple,
                darken_sky: false,
                dragon_bar: false,
                create_fog: false,
            },
        )]
        .into(),
    });

    let login = ClientboundGamePacket::Login(ClientboundLogin {
        player_id: mineflayer_engine::core::entity_id::MinecraftEntityId(0),
        hardcore: false,
        levels: vec![],
        max_players: 20,
        chunk_radius: 2,
        simulation_distance: 2,
        reduced_debug_info: false,
        show_death_screen: true,
        do_limited_crafting: false,
        common: CommonPlayerSpawnInfo {
            dimension_type: mineflayer_engine::registry::DataRegistry::new_raw(0),
            dimension: mineflayer_engine::Identifier::new("minecraft:overworld"),
            seed: 0,
            game_type: GameMode::Survival,
            previous_game_type: OptionalGameType(None),
            is_debug: false,
            is_flat: false,
            last_death_location: None,
            portal_cooldown: 0,
            sea_level: 63,
        },
        enforces_secure_chat: false,
    });
    let events = observe(&state, &Event::Packet(std::sync::Arc::new(login)));
    assert!(events.is_empty());
    let state = state.lock();
    assert!(state.scoreboards.is_empty() && state.display_slots.is_empty());
    assert!(state.teams.is_empty() && state.team_map.is_empty());
    assert_eq!(state.boss_bars.len(), 1, "boss bars survive login");
}
