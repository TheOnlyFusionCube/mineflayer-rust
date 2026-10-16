//! Scoreboard objectives, display slots, teams, and boss bars — the
//! `bot.scoreboards` / `bot.scoreboard` / `bot.teams` / `bot.teamMap` /
//! `bot.bossBars` surface.
//!
//! azalea stores none of this; everything here is facade-kept state on
//! `ScoreboardState`, fed by `c_set_objective` / `c_set_score` /
//! `c_reset_score` / `c_set_display_objective` / `c_set_player_team` /
//! `c_boss_event` packets decoded in `handle`.

use std::collections::{HashMap, HashSet};

use azalea::chat::style::ChatFormatting;
use azalea::{
    FormattedText,
    protocol::packets::game::{
        ClientboundGamePacket,
        c_boss_event::{self, BossBarColor, BossBarOverlay},
        c_set_display_objective::DisplaySlot,
        c_set_objective, c_set_player_team,
    },
};

use parking_lot::Mutex;

use crate::{Bot, BotEvent};

/// Scoreboard/team/boss-bar state — `bot.scoreboards`, `bot.displaySlots`
/// (`bot.scoreboard(slot)`), `bot.teams`, `bot.teamMap`, `bot.bossBars`.
///
/// Upstream keeps these in JS objects owned by the scoreboard plugin. Only
/// the bridge writes this state and `Bot` reads it without any azalea
/// component, so it lives in `BotShared` (the storage rule there), not on
/// the client entity; the domain still owns its whole lifecycle: packet
/// decode and the login/respawn clear.
#[derive(Default)]
pub(crate) struct ScoreboardState {
    /// Objective name -> scoreboard.
    pub scoreboards: HashMap<String, ScoreBoard>,
    /// Display slot -> objective name.
    pub display_slots: HashMap<ScoreboardSlot, String>,
    /// Team name -> team.
    pub teams: HashMap<String, Team>,
    /// Member name -> team name (values are looked up in `teams`, so the
    /// two views can't drift).
    pub team_map: HashMap<String, String>,
    /// Bar uuid string -> bar.
    pub boss_bars: HashMap<String, BossBar>,
}

impl ScoreboardState {
    /// Objectives and teams don't survive a login (upstream clears them on
    /// the `login` packet; the objects emit no events). Boss bars are
    /// intentionally kept — upstream never clears them either.
    fn reset_on_login(&mut self) {
        self.scoreboards.clear();
        self.display_slots.clear();
        self.teams.clear();
        self.team_map.clear();
    }
}

/// Decode a packet into scoreboard state; `Some(events)` when the packet
/// belongs to this domain (login/respawn clear the state and emit nothing),
/// `None` when another domain owns it.
pub(crate) fn handle_for(
    state: &Mutex<ScoreboardState>,
    packet: &ClientboundGamePacket,
) -> Option<Vec<BotEvent>> {
    if matches!(
        packet,
        ClientboundGamePacket::Login(_) | ClientboundGamePacket::Respawn(_)
    ) {
        state.lock().reset_on_login();
        return Some(Vec::new());
    }
    handle(&mut state.lock(), packet)
}

/// One entry on a [`ScoreBoard`] — the `itemsMap` values (`{name, value}`,
/// plus the optional custom display name the packet can carry).
#[derive(Clone, Debug, PartialEq)]
pub struct ScoreItem {
    pub name: String,
    /// The signed score — objectives legitimately go negative (the wire
    /// is a signed VarInt; azalea decodes it as `u32` and we un-wrap).
    pub value: i32,
    /// `scoreboard_score.display` — a custom name shown instead of `name`.
    pub display: Option<FormattedText>,
}

impl ScoreItem {
    /// The `displayName` getter upstream puts on every `itemsMap` entry:
    /// the team-styled name ([`Team::member_display_name`]) when `name` is
    /// on a team, else the plain name. Upstream reads `bot.teamMap` live —
    /// pass [`Bot::team_map`] (or any retained member→team map).
    pub fn display_name(&self, team_map: &HashMap<String, Team>) -> FormattedText {
        match team_map.get(&self.name) {
            Some(team) => team.member_display_name(&self.name),
            None => FormattedText::from(self.name.clone()),
        }
    }
}

/// A scoreboard objective — the upstream `ScoreBoard` class.
#[derive(Clone, Debug, PartialEq)]
pub struct ScoreBoard {
    /// The objective name (the `bot.scoreboards` map key).
    pub name: String,
    /// The objective's display name (`ScoreBoard.title` upstream, kept as a
    /// chat component rather than pre-rendered text).
    pub title: FormattedText,
    pub(crate) items_map: HashMap<String, ScoreItem>,
}

impl ScoreBoard {
    /// `ScoreBoard.itemsMap`.
    pub fn items_map(&self) -> &HashMap<String, ScoreItem> {
        &self.items_map
    }
    /// `ScoreBoard.items` — entries sorted by descending score like upstream.
    pub fn items(&self) -> Vec<&ScoreItem> {
        let mut items: Vec<_> = self.items_map.values().collect();
        items.sort_by_key(|i| std::cmp::Reverse(i.value));
        items
    }
    /// `ScoreBoard.setTitle` — a client-side view mutation (no packet).
    pub fn set_title(&mut self, title: FormattedText) {
        self.title = title;
    }
    /// `ScoreBoard.add` — client-side view mutation.
    pub fn add(&mut self, name: String, value: i32) {
        self.items_map.insert(
            name.clone(),
            ScoreItem {
                name,
                value,
                display: None,
            },
        );
    }
    /// `ScoreBoard.remove` — client-side view mutation.
    pub fn remove(&mut self, name: &str) -> Option<ScoreItem> {
        self.items_map.remove(name)
    }
}

/// Where an objective is displayed — covers `ScoreBoard.positions` upstream
/// (`sidebar`, `list`, `belowName`, and the per-team-color sidebar slots).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScoreboardSlot {
    List,
    Sidebar,
    BelowName,
    /// `bot.scoreboard` slots 3..=18 upstream — the sidebar shown to members
    /// of a team with the given color.
    TeamSidebar(ChatFormatting),
}

impl From<DisplaySlot> for ScoreboardSlot {
    fn from(slot: DisplaySlot) -> Self {
        match slot {
            DisplaySlot::List => Self::List,
            DisplaySlot::Sidebar => Self::Sidebar,
            DisplaySlot::BelowName => Self::BelowName,
            // team sidebar slots are ordered like the 16 legacy chat colors
            team => Self::TeamSidebar(match team {
                DisplaySlot::TeamBlack => ChatFormatting::Black,
                DisplaySlot::TeamDarkBlue => ChatFormatting::DarkBlue,
                DisplaySlot::TeamDarkGreen => ChatFormatting::DarkGreen,
                DisplaySlot::TeamDarkAqua => ChatFormatting::DarkAqua,
                DisplaySlot::TeamDarkRed => ChatFormatting::DarkRed,
                DisplaySlot::TeamDarkPurple => ChatFormatting::DarkPurple,
                DisplaySlot::TeamGold => ChatFormatting::Gold,
                DisplaySlot::TeamGray => ChatFormatting::Gray,
                DisplaySlot::TeamDarkGray => ChatFormatting::DarkGray,
                DisplaySlot::TeamBlue => ChatFormatting::Blue,
                DisplaySlot::TeamGreen => ChatFormatting::Green,
                DisplaySlot::TeamAqua => ChatFormatting::Aqua,
                DisplaySlot::TeamRed => ChatFormatting::Red,
                DisplaySlot::TeamLightPurple => ChatFormatting::LightPurple,
                DisplaySlot::TeamYellow => ChatFormatting::Yellow,
                DisplaySlot::TeamWhite => ChatFormatting::White,
                _ => unreachable!("non-team DisplaySlot handled above"),
            }),
        }
    }
}

/// A scoreboard team — the upstream `Team` class.
#[derive(Clone, Debug, PartialEq)]
pub struct Team {
    /// The team key — the `bot.teams`/`bot.teamMap` map key (upstream's
    /// `team.team`; upstream's `team.name` is [`Self::display_name`] here).
    pub name: String,
    /// The team's display name (upstream's `team.name` chat component).
    pub display_name: FormattedText,
    /// `team.friendlyFire` — members can hit each other (options bit 0x01).
    pub friendly_fire: bool,
    /// Members are visible to each other when invisible (options bit 0x02).
    pub see_friendly_invisibles: bool,
    pub nametag_visibility: NameTagVisibility,
    pub collision_rule: CollisionRule,
    /// `team.color` — upstream's `colorString(formatting)` is the same
    /// enum's `.name()`; `ChatFormatting::Reset` means "no color".
    pub color: ChatFormatting,
    pub prefix: FormattedText,
    pub suffix: FormattedText,
    /// `team.members`.
    pub members: HashSet<String>,
}

/// `team.collisionRule`.
pub use azalea::protocol::packets::game::c_set_player_team::CollisionRule;
/// `team.nameTagVisibility` (`azalea`'s packet enum, which is the domain type).
pub use azalea::protocol::packets::game::c_set_player_team::NameTagVisibility;

impl Team {
    fn new(name: String, params: c_set_player_team::Parameters) -> Self {
        Self {
            name,
            display_name: params.display_name,
            friendly_fire: params.options & 0x01 != 0,
            see_friendly_invisibles: params.options & 0x02 != 0,
            nametag_visibility: params.nametag_visibility,
            collision_rule: params.collision_rule,
            color: params.color,
            prefix: params.player_prefix,
            suffix: params.player_suffix,
            members: HashSet::new(),
        }
    }

    fn update(&mut self, params: c_set_player_team::Parameters) {
        self.display_name = params.display_name;
        self.friendly_fire = params.options & 0x01 != 0;
        self.see_friendly_invisibles = params.options & 0x02 != 0;
        self.nametag_visibility = params.nametag_visibility;
        self.collision_rule = params.collision_rule;
        self.color = params.color;
        self.prefix = params.player_prefix;
        self.suffix = params.player_suffix;
    }

    /// `team.displayName(member)` — `prefix + colored member + suffix` as a
    /// chat component.
    pub fn member_display_name(&self, member: &str) -> FormattedText {
        use azalea::chat::{
            base_component::BaseComponent,
            style::{Style, TextColor},
            text_component::TextComponent,
        };
        let mut style = Style::default();
        style.color = TextColor::try_from(self.color).ok();
        let mut component = self.prefix.clone();
        component.get_base_mut().siblings.extend([
            FormattedText::Text(TextComponent {
                text: member.to_string(),
                base: BaseComponent {
                    siblings: Vec::new(),
                    style: Box::new(style),
                },
            }),
            self.suffix.clone(),
        ]);
        component
    }
}

/// A boss bar — the (undocumented) upstream `BossBar` class.
#[derive(Clone, Debug, PartialEq)]
pub struct BossBar {
    /// `bossBar.entityUUID` — the packet's bar id, as the hyphenated uuid
    /// string upstream exposes.
    pub entity_uuid: String,
    pub title: FormattedText,
    /// `bossBar.health` — the fill progress, `0.0..=1.0`.
    pub health: f32,
    /// `bossBar.dividers` — the notch count (0, 6, 10, 12, or 20).
    pub dividers: u32,
    /// `bossBar.color`.
    pub color: BossBarColor,
    /// `bossBar.shouldDarkenSky` (flag bit 0x01).
    pub darken_sky: bool,
    /// `bossBar.isDragonBar` (flag bit 0x02 — the bar also plays boss music).
    pub dragon_bar: bool,
    /// `bossBar.createFog` (flag bit 0x04).
    pub create_fog: bool,
}

impl BossBar {
    fn from_add(id: String, add: c_boss_event::AddOperation) -> Self {
        Self {
            entity_uuid: id,
            title: add.name,
            health: add.progress,
            dividers: overlay_notches(add.style.overlay),
            color: add.style.color,
            darken_sky: add.properties.darken_screen,
            dragon_bar: add.properties.play_music,
            create_fog: add.properties.create_world_fog,
        }
    }
}

/// `bossBar.dividers` — upstream maps the wire overlay to a notch count.
fn overlay_notches(overlay: BossBarOverlay) -> u32 {
    match overlay {
        BossBarOverlay::Progress => 0,
        BossBarOverlay::Notched6 => 6,
        BossBarOverlay::Notched10 => 10,
        BossBarOverlay::Notched12 => 12,
        BossBarOverlay::Notched20 => 20,
    }
}

impl Bot {
    /// `bot.scoreboards` — every known objective, keyed by name. A snapshot;
    /// upstream returns a live view.
    pub fn scoreboards(&self) -> HashMap<String, ScoreBoard> {
        {
            let m = self.shared.scoreboard.lock();
            m.scoreboards.clone()
        }
    }

    /// `bot.scoreboard` — the objective currently bound to a display slot
    /// (`ScoreBoard.positions` upstream; `bot.scoreboard('sidebar')` is
    /// `bot.scoreboard(ScoreboardSlot::Sidebar)` here).
    pub fn scoreboard(&self, slot: ScoreboardSlot) -> Option<ScoreBoard> {
        let m = self.shared.scoreboard.lock();
        let name = m.display_slots.get(&slot)?;
        m.scoreboards.get(name).cloned()
    }

    /// `bot.teams` — every known team, keyed by name. A snapshot.
    pub fn teams(&self) -> HashMap<String, Team> {
        {
            let m = self.shared.scoreboard.lock();
            m.teams.clone()
        }
    }

    /// `bot.teamMap` — member name → team, keyed like upstream. A snapshot.
    pub fn team_map(&self) -> HashMap<String, Team> {
        let m = self.shared.scoreboard.lock();
        m.team_map
            .iter()
            .filter_map(|(member, team)| m.teams.get(team).map(|t| (member.clone(), t.clone())))
            .collect()
    }

    /// `bot.bossBars` — every active boss bar. A snapshot.
    pub fn boss_bars(&self) -> Vec<BossBar> {
        {
            let m = self.shared.scoreboard.lock();
            m.boss_bars.values().cloned().collect()
        }
    }
}

/// Apply one clientbound packet to the scoreboard/team/bossbar state.
/// Returns the `BotEvent`s to emit, or `None` when the packet isn't ours.
pub(crate) fn handle(
    state: &mut ScoreboardState,
    packet: &ClientboundGamePacket,
) -> Option<Vec<BotEvent>> {
    Some(match packet {
        ClientboundGamePacket::SetObjective(p) => match &p.method {
            c_set_objective::Method::Add {
                display_name,
                render_type: _,
                number_format: _,
            } => {
                let name = p.objective_name.clone();
                let scoreboard = ScoreBoard {
                    name: name.clone(),
                    title: display_name.clone(),
                    items_map: HashMap::new(),
                };
                state.scoreboards.insert(name, scoreboard.clone());
                vec![BotEvent::ScoreboardCreated(scoreboard)]
            }
            c_set_objective::Method::Remove => {
                let mut events = Vec::new();
                if let Some(scoreboard) = state.scoreboards.remove(&p.objective_name) {
                    events.push(BotEvent::ScoreboardDeleted(scoreboard));
                }
                // display slots referencing the dead objective are dropped too
                state.display_slots.retain(|_, n| *n != p.objective_name);
                events
            }
            c_set_objective::Method::Change {
                display_name,
                render_type: _,
                number_format: _,
            } => {
                let Some(scoreboard) = state.scoreboards.get_mut(&p.objective_name) else {
                    // upstream emits 'error' for updates to unknown objectives
                    return Some(vec![BotEvent::Error(format!(
                        "Received update for unknown objective {}",
                        p.objective_name
                    ))]);
                };
                scoreboard.set_title(display_name.clone());
                vec![BotEvent::ScoreboardTitleChanged(scoreboard.clone())]
            }
        },
        ClientboundGamePacket::SetScore(p) => {
            let Some(scoreboard) = state.scoreboards.get_mut(&p.objective_name) else {
                return Some(vec![]);
            };
            let item = ScoreItem {
                name: p.owner.clone(),
                // the wire field is a signed VarInt azalea reads as u32 —
                // `as i32` un-wraps it, so -1 arrives as -1 not 0xFFFFFFFF
                value: p.score as i32,
                display: p.display.clone(),
            };
            scoreboard.items_map.insert(p.owner.clone(), item.clone());
            vec![BotEvent::ScoreUpdated {
                scoreboard: scoreboard.clone(),
                item,
            }]
        }
        ClientboundGamePacket::ResetScore(p) => {
            // `objective_name: None` means "remove this owner wherever it
            // appears" — upstream scans every scoreboard and removes the
            // first match.
            let scoreboard = match &p.objective_name {
                Some(name) => state.scoreboards.get_mut(name),
                None => state
                    .scoreboards
                    .values_mut()
                    .find(|sb| sb.items_map.contains_key(&p.owner)),
            };
            let Some(scoreboard) = scoreboard else {
                return Some(vec![]);
            };
            let Some(item) = scoreboard.remove(&p.owner) else {
                return Some(vec![]);
            };
            vec![BotEvent::ScoreRemoved {
                scoreboard: scoreboard.clone(),
                item,
            }]
        }
        ClientboundGamePacket::SetDisplayObjective(p) => {
            let slot = ScoreboardSlot::from(p.slot);
            // upstream binds a slot only when the objective is already
            // known — a display packet for a never-created objective
            // leaves the slot empty there (no retroactive resolution)
            let Some(scoreboard) = state.scoreboards.get(&p.objective_name) else {
                return Some(vec![]);
            };
            let previous_board = state
                .display_slots
                .get(&slot)
                .and_then(|name| state.scoreboards.get(name))
                .cloned();
            let scoreboard = scoreboard.clone();
            state.display_slots.insert(slot, p.objective_name.clone());
            vec![BotEvent::ScoreboardPosition {
                slot,
                scoreboard,
                previous_board,
            }]
        }
        ClientboundGamePacket::SetPlayerTeam(p) => match &p.method {
            c_set_player_team::Method::Add((params, players)) => {
                let mut team = Team::new(p.name.clone(), params.clone());
                for player in players {
                    team.members.insert(player.clone());
                    state.team_map.insert(player.clone(), p.name.clone());
                }
                state.teams.insert(p.name.clone(), team.clone());
                vec![BotEvent::TeamCreated(team)]
            }
            c_set_player_team::Method::Remove => {
                let Some(team) = state.teams.remove(&p.name) else {
                    return Some(vec![]);
                };
                for member in &team.members {
                    state.team_map.remove(member);
                }
                vec![BotEvent::TeamRemoved(team)]
            }
            c_set_player_team::Method::Change(params) => {
                let Some(team) = state.teams.get_mut(&p.name) else {
                    return Some(vec![]);
                };
                team.update(params.clone());
                vec![BotEvent::TeamUpdated(team.clone())]
            }
            c_set_player_team::Method::Join(players) => {
                let Some(team) = state.teams.get_mut(&p.name) else {
                    return Some(vec![]);
                };
                for player in players {
                    team.members.insert(player.clone());
                    state.team_map.insert(player.clone(), p.name.clone());
                }
                vec![BotEvent::TeamMemberAdded(team.clone())]
            }
            c_set_player_team::Method::Leave(players) => {
                let Some(team) = state.teams.get_mut(&p.name) else {
                    return Some(vec![]);
                };
                for player in players {
                    team.members.remove(player);
                    state.team_map.remove(player);
                }
                vec![BotEvent::TeamMemberRemoved(team.clone())]
            }
        },
        ClientboundGamePacket::BossEvent(p) => match &p.operation {
            c_boss_event::Operation::Add(add) => {
                let bar = BossBar::from_add(p.id.to_string(), add.clone());
                state.boss_bars.insert(p.id.to_string(), bar.clone());
                vec![BotEvent::BossBarCreated(bar)]
            }
            c_boss_event::Operation::Remove => match state.boss_bars.remove(&p.id.to_string()) {
                Some(bar) => vec![BotEvent::BossBarDeleted(bar)],
                None => vec![],
            },
            c_boss_event::Operation::UpdateProgress(progress) => {
                let Some(bar) = state.boss_bars.get_mut(&p.id.to_string()) else {
                    return Some(vec![]);
                };
                bar.health = *progress;
                vec![BotEvent::BossBarUpdated(bar.clone())]
            }
            c_boss_event::Operation::UpdateName(name) => {
                let Some(bar) = state.boss_bars.get_mut(&p.id.to_string()) else {
                    return Some(vec![]);
                };
                bar.title = name.clone();
                vec![BotEvent::BossBarUpdated(bar.clone())]
            }
            c_boss_event::Operation::UpdateStyle(style) => {
                let Some(bar) = state.boss_bars.get_mut(&p.id.to_string()) else {
                    return Some(vec![]);
                };
                bar.dividers = overlay_notches(style.overlay);
                bar.color = style.color;
                vec![BotEvent::BossBarUpdated(bar.clone())]
            }
            c_boss_event::Operation::UpdateProperties(props) => {
                let Some(bar) = state.boss_bars.get_mut(&p.id.to_string()) else {
                    return Some(vec![]);
                };
                bar.darken_sky = props.darken_screen;
                bar.dragon_bar = props.play_music;
                bar.create_fog = props.create_world_fog;
                vec![BotEvent::BossBarUpdated(bar.clone())]
            }
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use azalea::protocol::packets::game::{
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
                azalea::protocol::packets::game::c_set_objective::ClientboundSetObjective {
                    objective_name: "obj".into(),
                    method: Method::Add {
                        display_name: "My Objective".into(),
                        render_type: azalea::core::objectives::ObjectiveCriteria::Integer,
                        number_format: azalea::chat::numbers::NumberFormat::Blank,
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
                azalea::protocol::packets::game::c_set_objective::ClientboundSetObjective {
                    objective_name: "obj".into(),
                    method: Method::Change {
                        display_name: "Renamed".into(),
                        render_type: azalea::core::objectives::ObjectiveCriteria::Integer,
                        number_format: azalea::chat::numbers::NumberFormat::Blank,
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
                azalea::protocol::packets::game::c_set_score::ClientboundSetScore {
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
                azalea::protocol::packets::game::c_set_score::ClientboundSetScore {
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
                azalea::protocol::packets::game::c_reset_score::ClientboundResetScore {
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
                azalea::protocol::packets::game::c_set_display_objective::ClientboundSetDisplayObjective {
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
                azalea::protocol::packets::game::c_set_objective::ClientboundSetObjective {
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
                azalea::protocol::packets::game::c_set_objective::ClientboundSetObjective {
                    objective_name: name.into(),
                    method: Method::Add {
                        display_name: name.into(),
                        render_type: azalea::core::objectives::ObjectiveCriteria::Integer,
                        number_format: azalea::chat::numbers::NumberFormat::Blank,
                    },
                },
            )
        };
        let display = |name: &str| {
            ClientboundGamePacket::SetDisplayObjective(
                azalea::protocol::packets::game::c_set_display_objective::ClientboundSetDisplayObjective {
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
                azalea::protocol::packets::game::c_set_objective::ClientboundSetObjective {
                    objective_name: "obj".into(),
                    method: Method::Add {
                        display_name: "x".into(),
                        render_type: azalea::core::objectives::ObjectiveCriteria::Integer,
                        number_format: azalea::chat::numbers::NumberFormat::Blank,
                    },
                },
            ),
        );
        let set_score = |owner: &str, score: u32| {
            ClientboundGamePacket::SetScore(
                azalea::protocol::packets::game::c_set_score::ClientboundSetScore {
                    owner: owner.into(),
                    objective_name: "obj".into(),
                    score,
                    display: None,
                    number_format: None,
                },
            )
        };
        // a wire -1 decodes through azalea's u32 as 0xFFFFFFFF
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
                azalea::protocol::packets::game::c_set_objective::ClientboundSetObjective {
                    objective_name: "ghost".into(),
                    method: Method::Change {
                        display_name: "x".into(),
                        render_type: azalea::core::objectives::ObjectiveCriteria::Integer,
                        number_format: azalea::chat::numbers::NumberFormat::Blank,
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
                azalea::protocol::packets::game::c_set_player_team::ClientboundSetPlayerTeam {
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
                azalea::protocol::packets::game::c_set_player_team::ClientboundSetPlayerTeam {
                    name: "t".into(),
                    method: TeamMethod::Join(vec!["carol".into()]),
                },
            ),
        )
        .unwrap();
        assert!(
            matches!(&events[..], [BotEvent::TeamMemberAdded(t)] if t.members.contains("carol"))
        );
        assert_eq!(s.team_map["carol"], "t");
        let events = handle(
            &mut s,
            &ClientboundGamePacket::SetPlayerTeam(
                azalea::protocol::packets::game::c_set_player_team::ClientboundSetPlayerTeam {
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
                azalea::protocol::packets::game::c_set_player_team::ClientboundSetPlayerTeam {
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
                azalea::protocol::packets::game::c_set_player_team::ClientboundSetPlayerTeam {
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
        // from the wire — AzBuf gives us a `Uuid` without naming the type.
        let id =
            azalea::buf::AzBuf::azalea_read(&mut std::io::Cursor::new(&[0u8; 16][..])).unwrap();
        let id_string = "00000000-0000-0000-0000-000000000000".to_string();
        let packet = |operation| {
            ClientboundGamePacket::BossEvent(
                azalea::protocol::packets::game::c_boss_event::ClientboundBossEvent {
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
    /// `handle_for` entry the bridge uses.
    #[test]
    fn login_clears_the_domain_state_through_handle_for() {
        use azalea::{
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
            player_id: azalea::core::entity_id::MinecraftEntityId(0),
            hardcore: false,
            levels: vec![],
            max_players: 20,
            chunk_radius: 2,
            simulation_distance: 2,
            reduced_debug_info: false,
            show_death_screen: true,
            do_limited_crafting: false,
            common: CommonPlayerSpawnInfo {
                dimension_type: azalea::registry::DataRegistry::new_raw(0),
                dimension: azalea::Identifier::new("minecraft:overworld"),
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
        let events = handle_for(&state, &login);
        assert!(matches!(&events, Some(v) if v.is_empty()));
        let state = state.lock();
        assert!(state.scoreboards.is_empty() && state.display_slots.is_empty());
        assert!(state.teams.is_empty() && state.team_map.is_empty());
        assert_eq!(state.boss_bars.len(), 1, "boss bars survive login");
    }
}
