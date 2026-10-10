//! Movement, physics, and control states — mineflayer's physics.js surface:
//! `bot.setControlState`, `bot.look`/`lookAt`, the `move`/`forcedMove`/
//! `mount`/`dismount`/`abilities` events, and the `bot.physics`/
//! `bot.abilities` properties.
//!
//! Upstream keeps seven booleans in `bot.controlState`; azalea already keeps
//! the equivalent inputs on the local player's [`PhysicsState`] (walk
//! direction, sprint/crouch intents) and `Jumping` components. We read and
//! write those components directly — the ECS stays the single source of
//! truth, so `control_states()` also reflects movement started by azalea's
//! pathfinder. All access goes through `try_query_self`/`get_component`:
//! azalea's `set_jumping`/`set_crouching`/`set_direction`/`jumping`
//! convenience wrappers panic before the client entity exists, while
//! upstream's plain booleans can be set any time.

use std::f64::consts::PI;

use azalea::{
    Event, PhysicsState, Vec3, WalkDirection,
    entity::{Jumping, LookDirection, PlayerAbilities, Position, dimensions::EntityDimensions},
    protocol::packets::game::ClientboundGamePacket,
};

use crate::{Bot, BotEvent, Result};

/// A `bot.setControlState` key — one of the seven held inputs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Control {
    Forward,
    Back,
    Left,
    Right,
    Jump,
    Sprint,
    Sneak,
}

/// `bot.controlState` — a snapshot of the seven held inputs.
///
/// Upstream exposes a live accessor whose setters call `setControlState`;
/// ours is a plain struct snapshot. Call [`Bot::set_control_state`] to
/// change a state.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ControlState {
    pub forward: bool,
    pub back: bool,
    pub left: bool,
    pub right: bool,
    pub jump: bool,
    pub sprint: bool,
    pub sneak: bool,
}

impl ControlState {
    fn get(&self, control: Control) -> bool {
        match control {
            Control::Forward => self.forward,
            Control::Back => self.back,
            Control::Left => self.left,
            Control::Right => self.right,
            Control::Jump => self.jump,
            Control::Sprint => self.sprint,
            Control::Sneak => self.sneak,
        }
    }
}

/// `bot.physics` — the physics knobs upstream exposes as mutable.
///
/// Upstream returns the live prismarine-physics object: plugins read
/// `bot.physics.gravity` and also mutate it (e.g. `gravity = 0` to fly).
/// azalea simulates the same vanilla constants internally but offers no
/// mutation seam, so this is a read-only snapshot of the vanilla values
/// for the targeted version — a documented `diverges` (api-parity.md).
/// Methods on upstream's physics object (`simulatePlayer`, …) have no
/// public analog and are not mirrored.
#[derive(Clone, Copy, Debug)]
pub struct PhysicsSettings {
    /// blocks/tick²
    pub gravity: f64,
    /// air drag per tick (multiplier on horizontal velocity)
    pub airdrag: f64,
    /// radians the head can turn per tick
    pub yaw_speed: f64,
    pub pitch_speed: f64,
    /// base movement speed attribute value
    pub player_speed: f64,
    /// sprint speed multiplier
    pub sprint_speed: f64,
    /// sneak speed multiplier
    pub sneak_speed: f64,
    /// max step-up without jumping
    pub step_height: f64,
    /// velocities below this are zeroed
    pub negligeable_velocity: f64,
    pub soulsand_speed: f64,
    pub honeyblock_speed: f64,
    pub honeyblock_jump_speed: f64,
    pub ladder_max_speed: f64,
    pub ladder_climb_speed: f64,
    /// half of the player's bounding-box width (the "apothem")
    pub player_half_width: f64,
    pub player_height: f64,
    pub water_inertia: f64,
    pub lava_inertia: f64,
    pub liquid_acceleration: f64,
    pub airborne_inertia: f64,
    pub airborne_acceleration: f64,
    pub default_slipperiness: f64,
    pub out_of_liquid_impulse: f64,
    pub fluid_jump_threshold: f64,
    /// ticks before autojump can retrigger (upstream's `jumpTicks` ceiling)
    pub autojump_cooldown: u32,
    /// liquid gravity as a proportion of `gravity` (1.13+ feature set)
    pub water_gravity: f64,
    pub lava_gravity: f64,
    pub slow_falling: f64,
    /// radians under which a horizontal collision doesn't break a sprint
    pub minor_collision_angle: f64,
    pub bubble_column_surface_drag: ColumnDrag,
    pub bubble_column_drag: ColumnDrag,
}

/// The `{down, maxDown, up, maxUp}` drag quadruplet prismarine-physics uses
/// for bubble columns.
#[derive(Clone, Copy, Debug)]
pub struct ColumnDrag {
    pub down: f64,
    pub max_down: f64,
    pub up: f64,
    pub max_up: f64,
}

impl PhysicsSettings {
    /// The vanilla constants prismarine-physics ships for the targeted
    /// version (`proportionalLiquidGravity` → water = gravity/16,
    /// lava = gravity/4).
    pub const VANILLA: Self = Self {
        gravity: 0.08,
        airdrag: 0.98,
        yaw_speed: 3.0,
        pitch_speed: 3.0,
        player_speed: 0.1,
        sprint_speed: 0.3,
        sneak_speed: 0.3,
        step_height: 0.6,
        negligeable_velocity: 0.003,
        soulsand_speed: 0.4,
        honeyblock_speed: 0.4,
        honeyblock_jump_speed: 0.4,
        ladder_max_speed: 0.15,
        ladder_climb_speed: 0.2,
        player_half_width: 0.3,
        player_height: 1.8,
        water_inertia: 0.8,
        lava_inertia: 0.5,
        liquid_acceleration: 0.02,
        airborne_inertia: 0.91,
        airborne_acceleration: 0.02,
        default_slipperiness: 0.6,
        out_of_liquid_impulse: 0.3,
        fluid_jump_threshold: 0.4,
        autojump_cooldown: 10,
        water_gravity: 0.005,
        lava_gravity: 0.02,
        slow_falling: 0.125,
        minor_collision_angle: 0.13962634,
        bubble_column_surface_drag: ColumnDrag {
            down: 0.03,
            max_down: -0.9,
            up: 0.1,
            max_up: 1.8,
        },
        bubble_column_drag: ColumnDrag {
            down: 0.03,
            max_down: -0.3,
            up: 0.06,
            max_up: 0.7,
        },
    };
}

/// Decompose a [`WalkDirection`] into `(forward, back, left, right)` held
/// keys — the same encoding azalea's `DirectionStates` uses in newer
/// versions (held pairs can't occur in a `WalkDirection`).
fn direction_bools(d: WalkDirection) -> (bool, bool, bool, bool) {
    match d {
        WalkDirection::None => (false, false, false, false),
        WalkDirection::Forward => (true, false, false, false),
        WalkDirection::Backward => (false, true, false, false),
        WalkDirection::Left => (false, false, true, false),
        WalkDirection::Right => (false, false, false, true),
        WalkDirection::ForwardRight => (true, false, false, true),
        WalkDirection::ForwardLeft => (true, false, true, false),
        WalkDirection::BackwardRight => (false, true, false, true),
        WalkDirection::BackwardLeft => (false, true, true, false),
    }
}

/// Compose four held directions into a [`WalkDirection`]. Opposing keys on
/// an axis cancel (held W+S, held A+D), like vanilla and azalea's
/// `DirectionStates` conversion.
fn walk_direction_of(forward: bool, back: bool, left: bool, right: bool) -> WalkDirection {
    match (
        forward && !back,
        back && !forward,
        left && !right,
        right && !left,
    ) {
        (true, false, false, false) => WalkDirection::Forward,
        (true, false, true, false) => WalkDirection::ForwardLeft,
        (true, false, false, true) => WalkDirection::ForwardRight,
        (false, true, false, false) => WalkDirection::Backward,
        (false, true, true, false) => WalkDirection::BackwardLeft,
        (false, true, false, true) => WalkDirection::BackwardRight,
        (false, false, true, false) => WalkDirection::Left,
        (false, false, false, true) => WalkDirection::Right,
        _ => WalkDirection::None,
    }
}

/// Apply one direction control to a [`WalkDirection`], preserving the other
/// three inputs.
fn walk_direction_with(current: WalkDirection, control: Control, state: bool) -> WalkDirection {
    let (mut f, mut b, mut l, mut r) = direction_bools(current);
    match control {
        Control::Forward => f = state,
        Control::Back => b = state,
        Control::Left => l = state,
        Control::Right => r = state,
        _ => return current,
    }
    walk_direction_of(f, b, l, r)
}

/// mineflayer `toNotchianYaw` — upstream yaw (radians) → protocol degrees.
fn to_notchian_yaw(yaw: f64) -> f32 {
    (PI - yaw).to_degrees() as f32
}

/// mineflayer `toNotchianPitch` — upstream pitch (radians) → protocol degrees.
fn to_notchian_pitch(pitch: f64) -> f32 {
    (-pitch).to_degrees() as f32
}

/// mineflayer `lookAt` trigonometry — the `(yaw, pitch)` in upstream radians
/// that faces `target` from `eye`. The same formula azalea's
/// `direction_looking_at` borrowed from upstream, kept in radians.
fn yaw_pitch_of(eye: Vec3, target: Vec3) -> (f64, f64) {
    let delta = target - eye;
    let yaw = (-delta.x).atan2(-delta.z);
    let ground = (delta.x * delta.x + delta.z * delta.z).sqrt();
    let pitch = delta.y.atan2(ground);
    (yaw, pitch)
}

/// Standing eye height, matching vanilla `EntityDimensions` for a player.
const DEFAULT_EYE_HEIGHT: f64 = 1.62;

impl Bot {
    /// `bot.setControlState(control, state)` — hold or release one input.
    ///
    /// Forward/back/left/right compose into azalea's `move_direction` (hold
    /// two for diagonals, opposing pairs cancel — same as upstream's key
    /// model). `Sprint` sets the sprint intent flag, not the direction —
    /// `Client::walk`/`Client::sprint` would clobber the other state, so
    /// the components are written directly. Sets are silent no-ops before
    /// the client entity exists (upstream keeps a plain bool that applies
    /// on spawn; the ECS has nowhere to hold it yet).
    pub fn set_control_state(&self, control: Control, state: bool) {
        match control {
            Control::Forward | Control::Back | Control::Left | Control::Right => {
                let _ = self.client.try_query_self::<&mut PhysicsState, _>(|mut s| {
                    s.move_direction = walk_direction_with(s.move_direction, control, state);
                });
            }
            // Upstream's jump is a held state like the others; azalea's
            // `jump()` is the one-tick variant, `set_jumping` the held one.
            Control::Jump => {
                let _ = self
                    .client
                    .try_query_self::<&mut Jumping, _>(|mut j| j.0 = state);
            }
            Control::Sprint => {
                let _ = self.client.try_query_self::<&mut PhysicsState, _>(|mut s| {
                    s.trying_to_sprint = state;
                });
            }
            Control::Sneak => {
                let _ = self.client.try_query_self::<&mut PhysicsState, _>(|mut s| {
                    s.trying_to_crouch = state;
                });
            }
        }
    }

    /// `bot.getControlState(control)` — whether the input is held.
    pub fn control_state(&self, control: Control) -> bool {
        self.control_states().get(control)
    }

    /// `bot.controlState` — a snapshot of all seven held inputs.
    pub fn control_states(&self) -> ControlState {
        let physics = self
            .client
            .get_component::<PhysicsState>()
            .map(|s| s.clone())
            .unwrap_or_default();
        let (forward, back, left, right) = direction_bools(physics.move_direction);
        ControlState {
            forward,
            back,
            left,
            right,
            jump: self.jump_queued(),
            sprint: physics.trying_to_sprint,
            sneak: physics.trying_to_crouch,
        }
    }

    /// `bot.clearControlStates()` — release every input.
    pub fn clear_control_states(&self) {
        for control in [
            Control::Forward,
            Control::Back,
            Control::Left,
            Control::Right,
            Control::Jump,
            Control::Sprint,
            Control::Sneak,
        ] {
            self.set_control_state(control, false);
        }
    }

    /// `bot.look(yaw, pitch, force)` — face `yaw`/`pitch` (radians, upstream
    /// convention: `yaw = atan2(-dx, -dz)` to a target, pitch ±π/2).
    ///
    /// Upstream eases toward the target at `yawSpeed`/`pitchSpeed` per tick
    /// and resolves when the sent rotation catches up; azalea snaps the
    /// direction immediately (sensitivity-quantized, like a vanilla mouse),
    /// so this resolves after the tick that carries it to the server.
    /// `force` skips that wait, matching upstream's early return.
    pub async fn look(&self, yaw: f64, pitch: f64, force: bool) -> Result<()> {
        let _ = self
            .client
            .try_query_self::<&mut LookDirection, _>(|mut ld| {
                ld.update(LookDirection::new(
                    to_notchian_yaw(yaw),
                    to_notchian_pitch(pitch),
                ));
            });
        if !force {
            self.wait_for_ticks(1).await?;
        }
        Ok(())
    }

    /// `bot.lookAt(point, force)` — face `point` from the bot's eye.
    pub async fn look_at(&self, point: Vec3, force: bool) -> Result<()> {
        let eye_height = self
            .client
            .get_component::<EntityDimensions>()
            .map(|d| f64::from(d.eye_height))
            .unwrap_or(DEFAULT_EYE_HEIGHT);
        let (yaw, pitch) = yaw_pitch_of(self.position().up(eye_height), point);
        self.look(yaw, pitch, force).await
    }

    /// `bot.physics` — read-only snapshot of the vanilla physics constants
    /// (see [`PhysicsSettings`]; upstream's knobs are mutable, ours can't
    /// be — `diverges`).
    pub fn physics(&self) -> PhysicsSettings {
        PhysicsSettings::VANILLA
    }

    /// `bot.physicsEnabled` — always `true`; azalea's physics runs
    /// unconditionally, so upstream's toggle can't be offered (`diverges`).
    pub fn physics_enabled(&self) -> bool {
        true
    }

    /// `bot.abilities` — the abilities the server last granted (flying,
    /// instabreak, …). All-false default before the first abilities packet.
    pub fn abilities(&self) -> PlayerAbilities {
        self.client
            .get_component::<PlayerAbilities>()
            .map(|a| a.clone())
            .unwrap_or_default()
    }

    /// `bot.jumpQueued` (undocumented upstream) — whether the bot will try
    /// to jump next tick. Upstream's flag is a queued one-shot cleared on
    /// the next physics step; ours reads the held jump input — the honest
    /// azalea analog (`diverges`).
    pub fn jump_queued(&self) -> bool {
        self.client
            .get_component::<Jumping>()
            .map(|j| j.0)
            .unwrap_or(false)
    }
}

/// Bridge-side bookkeeping for the movement events azalea doesn't lift
/// into `Event`s: `move`, `forcedMove`, `abilities`. (`mount`/`dismount`
/// are decoded by the entity tracker — see `entity::EntityTracker`.)
#[derive(Default)]
pub(crate) struct MovementTracker {
    /// Position + look direction the last `Move` was diffed against;
    /// `None` until the first tick with a `Position` component.
    last_pose: Option<(Vec3, LookDirection)>,
}

/// Called by the bridge for every azalea [`Event`], after `translate` —
/// emits the movement `BotEvent`s azalea keeps internal.
pub(crate) fn bridge_events(
    client: &azalea::Client,
    tracker: &mut MovementTracker,
    event: &Event,
) -> Vec<BotEvent> {
    match event {
        // `move` — upstream fires whenever a movement/look packet is sent;
        // azalea sends one when the tick's position or rotation changed,
        // so a per-tick component diff is the honest equivalent.
        Event::Tick => {
            let Some(pos) = client.get_component::<Position>().map(|p| **p) else {
                return vec![];
            };
            let dir = client
                .get_component::<LookDirection>()
                .map(|d| *d)
                .unwrap_or_default();
            let old = tracker.last_pose.replace((pos, dir));
            match old {
                Some((old_pos, old_dir)) if old_pos != pos || old_dir != dir => {
                    vec![BotEvent::Move { old_pos }]
                }
                _ => vec![],
            }
        }
        Event::Packet(packet) => packet_events(client, tracker, packet),
        _ => vec![],
    }
}

/// Movement-relevant clientbound packets → `BotEvent`s. azalea handles
/// `PlayerPosition`/`PlayerAbilities` internally, keeping them opaque to
/// `Event` consumers — decode them here like upstream's
/// `position`/`abilities` handlers do. (`SetPassengers` is owned by the
/// entity tracker.)
fn packet_events(
    _client: &azalea::Client,
    _tracker: &mut MovementTracker,
    packet: &ClientboundGamePacket,
) -> Vec<BotEvent> {
    match packet {
        ClientboundGamePacket::PlayerPosition(_) => vec![BotEvent::ForcedMove],
        ClientboundGamePacket::PlayerAbilities(p) => {
            vec![BotEvent::Abilities(PlayerAbilities::from(p))]
        }
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use azalea::protocol::packets::game::c_player_abilities::{
        ClientboundPlayerAbilities, PlayerAbilitiesFlags,
    };

    use super::*;

    #[test]
    fn notchian_conversion_matches_upstream() {
        // upstream: toNotchianYaw = toDegrees(PI - yaw); yaw 0 faces -Z
        // (north) -> notchian 180, yaw -PI/2 (east, +X) -> 270.
        assert_eq!(to_notchian_yaw(0.0), 180.0);
        assert_eq!(to_notchian_yaw(-PI / 2.0), 270.0);
        assert_eq!(to_notchian_yaw(PI), 0.0);
        // upstream: toNotchianPitch = toDegrees(-pitch); +PI/2 is up -> -90.
        assert_eq!(to_notchian_pitch(PI / 2.0), -90.0);
        assert_eq!(to_notchian_pitch(-PI / 2.0), 90.0);
        assert_eq!(to_notchian_pitch(0.0), 0.0);
    }

    #[test]
    fn yaw_pitch_of_known_vectors() {
        let eye = Vec3::new(0.0, 62.0, 0.0);
        // Due east (+X): atan2(-1, 0) = -PI/2, level pitch.
        let (yaw, pitch) = yaw_pitch_of(eye, Vec3::new(1.0, 62.0, 0.0));
        assert!((yaw + PI / 2.0).abs() < 1e-9);
        assert_eq!(pitch, 0.0);
        // Due south (+Z): atan2(-0, -1) = -PI (same angle as PI).
        let (yaw, _) = yaw_pitch_of(eye, Vec3::new(0.0, 62.0, 1.0));
        assert_eq!(yaw.abs(), PI);
        // Straight up: pitch +PI/2; 45° below the horizon: -PI/4.
        let (_, pitch) = yaw_pitch_of(eye, Vec3::new(0.0, 63.0, 0.0));
        assert!((pitch - PI / 2.0).abs() < 1e-9);
        let (_, pitch) = yaw_pitch_of(eye, Vec3::new(1.0, 61.0, 0.0));
        assert!((pitch + PI / 4.0).abs() < 1e-9);
    }

    #[test]
    fn walk_direction_composes_held_keys() {
        let mut dir = WalkDirection::None;
        dir = walk_direction_with(dir, Control::Forward, true);
        assert_eq!(dir, WalkDirection::Forward);
        dir = walk_direction_with(dir, Control::Right, true);
        assert_eq!(dir, WalkDirection::ForwardRight);
        dir = walk_direction_with(dir, Control::Forward, false);
        assert_eq!(dir, WalkDirection::Right);
        // Opposing keys cancel, like vanilla's held-W+held-S.
        dir = walk_direction_with(dir, Control::Left, true);
        assert_eq!(dir, WalkDirection::None);
        // forward+back both held cancels; a WalkDirection can't record the
        // contradictory pair, so releasing one key stays None (upstream's
        // independent bools would resume the other — see api-parity.md).
        dir = walk_direction_with(WalkDirection::None, Control::Forward, true);
        dir = walk_direction_with(dir, Control::Back, true);
        assert_eq!(dir, WalkDirection::None);
        dir = walk_direction_with(dir, Control::Forward, false);
        assert_eq!(dir, WalkDirection::None);
        // back+right held -> diagonal.
        dir = walk_direction_with(WalkDirection::None, Control::Back, true);
        dir = walk_direction_with(dir, Control::Right, true);
        assert_eq!(dir, WalkDirection::BackwardRight);
    }

    #[test]
    fn non_direction_controls_leave_direction_alone() {
        for control in [Control::Jump, Control::Sprint, Control::Sneak] {
            assert_eq!(
                walk_direction_with(WalkDirection::ForwardLeft, control, true),
                WalkDirection::ForwardLeft
            );
        }
    }

    #[test]
    fn abilities_packet_decodes() {
        let p = ClientboundPlayerAbilities {
            flags: PlayerAbilitiesFlags {
                invulnerable: true,
                flying: true,
                can_fly: true,
                instant_break: true,
            },
            flying_speed: 0.1,
            walking_speed: 0.2,
        };
        let a = PlayerAbilities::from(&p);
        assert!(a.flying && a.can_fly && a.invulnerable && a.instant_break);
        assert_eq!(a.flying_speed, 0.1);
        assert_eq!(a.walking_speed, 0.2);
    }
}
