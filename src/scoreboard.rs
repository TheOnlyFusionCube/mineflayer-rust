//! Scoreboard objectives, display slots, teams, and boss bars — the
//! `bot.scoreboards` / `bot.scoreboard` / `bot.teams` / `bot.teamMap` /
//! `bot.bossBars` surface.
//!
//! The engine stores none of this; everything here is facade-kept state on
//! `ScoreboardState`, fed by `c_set_objective` / `c_set_score` /
//! `c_reset_score` / `c_set_display_objective` / `c_set_player_team` /
//! `c_boss_event` packets decoded in `handle`.

use std::collections::{HashMap, HashSet};

use mineflayer_engine::chat::style::ChatFormatting;
use mineflayer_engine::{
    Event, FormattedText,
    protocol::packets::game::{
        ClientboundGamePacket,
        c_boss_event::{self, BossBarColor, BossBarOverlay},
        c_reset_score,
        c_set_display_objective::{self, DisplaySlot},
        c_set_objective, c_set_player_team, c_set_score,
    },
};

use mineflayer_engine::ecs::{self as bevy_ecs, prelude::Component};

use crate::{Bot, BotEvent};

/// Scoreboard/team/boss-bar state — `bot.scoreboards`, `bot.displaySlots`
/// (`bot.scoreboard(slot)`), `bot.teams`, `bot.teamMap`, `bot.bossBars`.
///
/// Upstream keeps these in JS objects owned by the scoreboard plugin. Here
/// it is a component on the client entity that the bridge writes and `Bot`
/// reads; the domain owns its whole lifecycle: packet decode and the
/// login/respawn clear.
#[derive(Component, Default)]
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
    pub boss_bars: HashMap<uuid::Uuid, BossBar>,
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

/// The bridge observer: decode a packet into scoreboard state and events.
/// Login and respawn clear the state and emit nothing.
pub(crate) fn observe(state: &mut ScoreboardState, event: &Event) -> Vec<BotEvent> {
    let Event::Packet(packet) = event else {
        return Vec::new();
    };
    if matches!(
        &**packet,
        ClientboundGamePacket::Login(_) | ClientboundGamePacket::Respawn(_)
    ) {
        state.reset_on_login();
        return Vec::new();
    }
    handle(state, packet).unwrap_or_default()
}

/// One entry on a [`ScoreBoard`] — the `itemsMap` values (`{name, value}`,
/// plus the optional custom display name the packet can carry).
#[derive(Clone, Debug, PartialEq)]
pub struct ScoreItem {
    pub name: String,
    /// The signed score — objectives legitimately go negative (the wire
    /// is a signed VarInt; the engine decodes it as `u32` and we un-wrap).
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
pub use mineflayer_engine::protocol::packets::game::c_set_player_team::CollisionRule;
/// `team.nameTagVisibility` (`mineflayer-engine`'s packet enum, which is the domain type).
pub use mineflayer_engine::protocol::packets::game::c_set_player_team::NameTagVisibility;

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
        use mineflayer_engine::chat::{
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
    /// `bossBar.entityUUID` — the packet's bar id (upstream exposes the
    /// hyphenated string, which `to_string` gives).
    pub entity_uuid: uuid::Uuid,
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
    fn from_add(id: uuid::Uuid, add: c_boss_event::AddOperation) -> Self {
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
        self.scoreboard_state(|m| m.scoreboards.clone())
            .unwrap_or_default()
    }

    /// `bot.scoreboard` — the objective currently bound to a display slot
    /// (`ScoreBoard.positions` upstream; `bot.scoreboard('sidebar')` is
    /// `bot.scoreboard(ScoreboardSlot::Sidebar)` here).
    pub fn scoreboard(&self, slot: ScoreboardSlot) -> Option<ScoreBoard> {
        self.scoreboard_state(|m| {
            let name = m.display_slots.get(&slot)?;
            m.scoreboards.get(name).cloned()
        })
        .flatten()
    }

    /// `bot.teams` — every known team, keyed by name. A snapshot.
    pub fn teams(&self) -> HashMap<String, Team> {
        self.scoreboard_state(|m| m.teams.clone())
            .unwrap_or_default()
    }

    /// `bot.teamMap` — member name → team, keyed like upstream. A snapshot.
    pub fn team_map(&self) -> HashMap<String, Team> {
        self.scoreboard_state(|m| {
            m.team_map
                .iter()
                .filter_map(|(member, team)| m.teams.get(team).map(|t| (member.clone(), t.clone())))
                .collect()
        })
        .unwrap_or_default()
    }

    /// `bot.bossBars` — every active boss bar. A snapshot.
    pub fn boss_bars(&self) -> Vec<BossBar> {
        self.scoreboard_state(|m| m.boss_bars.values().cloned().collect())
            .unwrap_or_default()
    }

    fn scoreboard_state<R>(&self, f: impl FnOnce(&ScoreboardState) -> R) -> Option<R> {
        crate::bot::with_component(&self.client, f)
    }
}

/// Apply one clientbound packet to the scoreboard/team/bossbar state.
/// Returns the `BotEvent`s to emit, or `None` when the packet isn't ours.
pub(crate) fn handle(
    state: &mut ScoreboardState,
    packet: &ClientboundGamePacket,
) -> Option<Vec<BotEvent>> {
    Some(match packet {
        ClientboundGamePacket::SetObjective(p) => objective(state, p),
        ClientboundGamePacket::SetScore(p) => set_score(state, p),
        ClientboundGamePacket::ResetScore(p) => reset_score(state, p),
        ClientboundGamePacket::SetDisplayObjective(p) => display_objective(state, p),
        ClientboundGamePacket::SetPlayerTeam(p) => team(state, p),
        ClientboundGamePacket::BossEvent(p) => boss_bar(state, p),
        _ => return None,
    })
}

fn objective(
    state: &mut ScoreboardState,
    p: &c_set_objective::ClientboundSetObjective,
) -> Vec<BotEvent> {
    match &p.method {
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
                return vec![BotEvent::Error(format!(
                    "Received update for unknown objective {}",
                    p.objective_name
                ))];
            };
            scoreboard.set_title(display_name.clone());
            vec![BotEvent::ScoreboardTitleChanged(scoreboard.clone())]
        }
    }
}

fn set_score(state: &mut ScoreboardState, p: &c_set_score::ClientboundSetScore) -> Vec<BotEvent> {
    let Some(scoreboard) = state.scoreboards.get_mut(&p.objective_name) else {
        return vec![];
    };
    let item = ScoreItem {
        name: p.owner.clone(),
        // the wire field is a signed VarInt the engine reads as u32 —
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

fn reset_score(
    state: &mut ScoreboardState,
    p: &c_reset_score::ClientboundResetScore,
) -> Vec<BotEvent> {
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
        return vec![];
    };
    let Some(item) = scoreboard.remove(&p.owner) else {
        return vec![];
    };
    vec![BotEvent::ScoreRemoved {
        scoreboard: scoreboard.clone(),
        item,
    }]
}

fn display_objective(
    state: &mut ScoreboardState,
    p: &c_set_display_objective::ClientboundSetDisplayObjective,
) -> Vec<BotEvent> {
    let slot = ScoreboardSlot::from(p.slot);
    // upstream binds a slot only when the objective is already
    // known — a display packet for a never-created objective
    // leaves the slot empty there (no retroactive resolution)
    let Some(scoreboard) = state.scoreboards.get(&p.objective_name) else {
        return vec![];
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

fn team(
    state: &mut ScoreboardState,
    p: &c_set_player_team::ClientboundSetPlayerTeam,
) -> Vec<BotEvent> {
    match &p.method {
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
                return vec![];
            };
            for member in &team.members {
                state.team_map.remove(member);
            }
            vec![BotEvent::TeamRemoved(team)]
        }
        c_set_player_team::Method::Change(params) => {
            let Some(team) = state.teams.get_mut(&p.name) else {
                return vec![];
            };
            team.update(params.clone());
            vec![BotEvent::TeamUpdated(team.clone())]
        }
        c_set_player_team::Method::Join(players) => {
            let Some(team) = state.teams.get_mut(&p.name) else {
                return vec![];
            };
            for player in players {
                team.members.insert(player.clone());
                state.team_map.insert(player.clone(), p.name.clone());
            }
            vec![BotEvent::TeamMemberAdded(team.clone())]
        }
        c_set_player_team::Method::Leave(players) => {
            let Some(team) = state.teams.get_mut(&p.name) else {
                return vec![];
            };
            for player in players {
                team.members.remove(player);
                state.team_map.remove(player);
            }
            vec![BotEvent::TeamMemberRemoved(team.clone())]
        }
    }
}

fn boss_bar(state: &mut ScoreboardState, p: &c_boss_event::ClientboundBossEvent) -> Vec<BotEvent> {
    match &p.operation {
        c_boss_event::Operation::Add(add) => {
            let bar = BossBar::from_add(p.id, add.clone());
            state.boss_bars.insert(p.id, bar.clone());
            vec![BotEvent::BossBarCreated(bar)]
        }
        c_boss_event::Operation::Remove => match state.boss_bars.remove(&p.id) {
            Some(bar) => vec![BotEvent::BossBarDeleted(bar)],
            None => vec![],
        },
        c_boss_event::Operation::UpdateProgress(progress) => {
            let Some(bar) = state.boss_bars.get_mut(&p.id) else {
                return vec![];
            };
            bar.health = *progress;
            vec![BotEvent::BossBarUpdated(bar.clone())]
        }
        c_boss_event::Operation::UpdateName(name) => {
            let Some(bar) = state.boss_bars.get_mut(&p.id) else {
                return vec![];
            };
            bar.title = name.clone();
            vec![BotEvent::BossBarUpdated(bar.clone())]
        }
        c_boss_event::Operation::UpdateStyle(style) => {
            let Some(bar) = state.boss_bars.get_mut(&p.id) else {
                return vec![];
            };
            bar.dividers = overlay_notches(style.overlay);
            bar.color = style.color;
            vec![BotEvent::BossBarUpdated(bar.clone())]
        }
        c_boss_event::Operation::UpdateProperties(props) => {
            let Some(bar) = state.boss_bars.get_mut(&p.id) else {
                return vec![];
            };
            bar.darken_sky = props.darken_screen;
            bar.dragon_bar = props.play_music;
            bar.create_fog = props.create_world_fog;
            vec![BotEvent::BossBarUpdated(bar.clone())]
        }
    }
}

#[cfg(test)]
mod tests;
