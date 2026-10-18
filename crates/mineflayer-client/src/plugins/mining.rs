use bevy_app::{App, Plugin, Update};
use bevy_ecs::prelude::*;
use derive_more::{Deref, DerefMut};
use mineflayer_block::{BlockState, BlockTrait, fluid_state::FluidState};
use mineflayer_core::{
    direction::Direction, game_type::GameMode, position::BlockPos, tick::GameTick,
};
use mineflayer_entity::{
    ActiveEffects, Attributes, FluidOnEyes, Physics, PlayerAbilities, Position,
    inventory::Inventory, mining::get_mine_progress,
};
use mineflayer_inventory::ItemStack;
use mineflayer_physics::{PhysicsSystems, collision::BlockWithShape};
use mineflayer_protocol::packets::game::s_player_action::{self, ServerboundPlayerAction};
use mineflayer_registry::builtin::{BlockKind, ItemKind};
use mineflayer_world::{WorldName, Worlds};
use tracing::{debug, trace, warn};

use crate::{
    interact::{
        BlockStatePredictionHandler, SwingArmEvent, can_use_game_master_blocks,
        check_is_interaction_restricted, pick::HitResultComponent,
    },
    inventory::InventorySystems,
    local_player::{LocalGameMode, PermissionLevel, WorldHolder},
    movement::MoveEventsSystems,
    packet::game::SendGamePacketEvent,
};

/// A plugin that allows clients to break blocks in the world.
pub struct MiningPlugin;
impl Plugin for MiningPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<StartMiningBlockEvent>()
            .add_message::<StopMiningBlockEvent>()
            .add_message::<MiningFinished>()
            .add_message::<MineBlockProgressEvent>()
            .add_message::<AttackBlockEvent>()
            .add_systems(
                GameTick,
                (
                    update_mining_component,
                    handle_auto_mine,
                    handle_mining_queued,
                    decrement_mine_delay,
                    continue_mining_block,
                )
                    .chain()
                    .before(PhysicsSystems)
                    .before(super::movement::send_position)
                    .before(super::interact::handle_start_use_item_queued)
                    .after(mineflayer_entity::update_fluid_on_eyes)
                    .in_set(MiningSystems),
            )
            .add_systems(
                Update,
                (
                    handle_start_mining_block_event,
                    handle_stop_mining_block_event,
                )
                    .chain()
                    .in_set(MiningSystems)
                    .after(InventorySystems)
                    .after(MoveEventsSystems)
                    .after(crate::interact::pick::update_hit_result_component)
                    .after(crate::attack::handle_attack_event),
            )
            .add_observer(handle_finish_mining_block_observer)
            .add_observer(handle_cancel_mining_event);
    }
}

/// The Bevy system set for things related to mining.
#[derive(Clone, Debug, Eq, Hash, PartialEq, SystemSet)]
pub struct MiningSystems;

/// A component that simulates the client holding down left click to mine the
/// block that it's facing, but this only interacts with blocks and not
/// entities.
#[derive(Component)]
pub struct LeftClickMine;

#[allow(clippy::type_complexity)]
fn handle_auto_mine(
    mut query: Query<
        (
            &HitResultComponent,
            Entity,
            Option<&Mining>,
            &Inventory,
            &MineBlockPos,
            &MineItem,
        ),
        With<LeftClickMine>,
    >,
    mut start_mining_block_event: MessageWriter<StartMiningBlockEvent>,
    mut stop_mining_block_event: MessageWriter<StopMiningBlockEvent>,
) {
    for (
        hit_result_component,
        entity,
        mining,
        inventory,
        current_mining_pos,
        current_mining_item,
    ) in &mut query.iter_mut()
    {
        let block_pos = hit_result_component
            .as_block_hit_result_if_not_miss()
            .map(|b| b.block_pos);

        // start mining if we're looking at a block and we're not already mining
        // it
        if let Some(block_pos) = block_pos
            && (mining.is_none()
                || !is_same_mining_target(
                    block_pos,
                    inventory,
                    current_mining_pos,
                    current_mining_item,
                ))
        {
            start_mining_block_event.write(StartMiningBlockEvent {
                entity,
                position: block_pos,
                force: true,
                id: MiningId::next(),
            });
        } else if mining.is_some() && hit_result_component.miss() {
            stop_mining_block_event.write(StopMiningBlockEvent { entity });
        }
    }
}

/// Information about the block we're currently mining.
///
/// This is only present if we're currently mining a block.
#[derive(Clone, Component, Debug)]
pub struct Mining {
    pub pos: BlockPos,
    pub dir: Direction,
    /// See [`MiningQueued::force`].
    pub force: bool,
    /// The start this mining came from.
    pub id: MiningId,
}

/// The identifier of one [`StartMiningBlockEvent`], unique in the process.
/// [`MiningFinished`] names it.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct MiningId(pub u64);

impl MiningId {
    /// A new identifier. `MiningId(0)` is never returned.
    pub fn next() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }
}

/// How a mining start ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MiningOutcome {
    /// The client finished the block and predicted it as broken.
    Broken,
    /// A stop, a cancel, or a newer start ended it.
    Cancelled,
    /// It ended without a break for another reason: the interaction was
    /// not allowed, the bot stopped looking at the block, or the block
    /// was gone.
    Interrupted,
}

/// Sent once for each [`StartMiningBlockEvent`] when its mining ends.
#[derive(Clone, Copy, Debug, Message)]
pub struct MiningFinished {
    pub entity: Entity,
    pub id: MiningId,
    pub outcome: MiningOutcome,
}

/// Start mining the block at the given position.
///
/// If we're looking at the block then the correct direction will be used,
/// otherwise it'll be [`Direction::Down`].
#[derive(Debug, Message)]
pub struct StartMiningBlockEvent {
    pub entity: Entity,
    pub position: BlockPos,
    /// Whether we should ignore blocks that are blocking the view of this
    /// block.
    ///
    /// Most of the time, you'll want to set this to true as it'll make the
    /// behavior more predictable. If it's set to false, then it might fail or
    /// it might mine blocks other than the one at `position` (which may be
    /// preferable if you're trying to act like vanilla).
    pub force: bool,
    /// Names this start in its [`MiningFinished`].
    pub id: MiningId,
}
fn handle_start_mining_block_event(
    mut commands: Commands,
    mut events: MessageReader<StartMiningBlockEvent>,
    mut finished: MessageWriter<MiningFinished>,
    mut query: Query<(
        &HitResultComponent,
        Option<&MiningCancelledBefore>,
        Option<&MiningQueued>,
    )>,
) {
    // a start queued in this run replaces the one queued before it
    let mut queued_now = std::collections::HashMap::new();
    for (event, message_id) in events.read_with_id() {
        trace!("{event:?}");
        let (hit_result, cancelled_before, queued) = query.get_mut(event.entity).unwrap();
        let mut end = |id, outcome| {
            finished.write(MiningFinished {
                entity: event.entity,
                id,
                outcome,
            });
        };
        if cancelled_before.is_some_and(|c| message_id.id < c.0) {
            end(event.id, MiningOutcome::Cancelled);
            continue;
        }
        let replaced = queued_now
            .get(&event.entity)
            .copied()
            .or(queued.map(|q| q.id));
        if event.force {
            let direction = if let Some(block_hit_result) =
                hit_result.as_block_hit_result_if_not_miss()
                && block_hit_result.block_pos == event.position
            {
                // we're looking at the block
                block_hit_result.direction
            } else {
                debug!(
                    "Got StartMiningBlockEvent but we're not looking at the block ({hit_result:?}.block_pos != {:?}). Picking an arbitrary direction instead.",
                    event.position
                );
                // we're not looking at the block, arbitrary direction
                Direction::Down
            };
            if let Some(replaced) = replaced {
                end(replaced, MiningOutcome::Cancelled);
            }
            queued_now.insert(event.entity, event.id);
            commands.entity(event.entity).insert(MiningQueued {
                position: event.position,
                direction,
                force: true,
                id: event.id,
            });
        } else {
            // let block_hit_result =
            // hit_result.as_block_hit_result_if_not_miss();
            // let direction = block_hit_result.map_or(Direction::Down, |b|
            // b.direction);
            if let Some(block_hit_result) = hit_result.as_block_hit_result_if_not_miss()
                && block_hit_result.block_pos == event.position
            {
                if let Some(replaced) = replaced {
                    end(replaced, MiningOutcome::Cancelled);
                }
                queued_now.insert(event.entity, event.id);
                commands.entity(event.entity).insert(MiningQueued {
                    position: event.position,
                    direction: block_hit_result.direction,
                    force: false,
                    id: event.id,
                });
            } else {
                end(event.id, MiningOutcome::Interrupted);
                warn!(
                    "Got StartMiningBlockEvent with force=false but we're not looking at the block ({hit_result:?}.block_pos != {:?}). You should've looked at the block before trying to mine with force=false.",
                    event.position
                );
            };
        }
    }
}

/// Present on entities when they're going to start mining a block next tick.
#[derive(Clone, Component, Debug)]
pub struct MiningQueued {
    pub position: BlockPos,
    pub direction: Direction,
    /// Whether we should mine the block regardless of whether it's reachable.
    pub force: bool,
    /// The start this came from.
    pub id: MiningId,
}
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn handle_mining_queued(
    mut commands: Commands,
    mut attack_block_events: MessageWriter<AttackBlockEvent>,
    mut mine_block_progress_events: MessageWriter<MineBlockProgressEvent>,
    mut finished: MessageWriter<MiningFinished>,
    query: Query<(
        Entity,
        &MiningQueued,
        &WorldHolder,
        &LocalGameMode,
        &Inventory,
        &ActiveEffects,
        &FluidOnEyes,
        &Physics,
        &Attributes,
        Option<&mut Mining>,
        &mut BlockStatePredictionHandler,
        (
            &mut MineDelay,
            &mut MineProgress,
            &mut MineTicks,
            &mut MineItem,
            &mut MineBlockPos,
        ),
    )>,
) {
    for (
        entity,
        mining_queued,
        world_holder,
        game_mode,
        inventory,
        active_effects,
        fluid_on_eyes,
        physics,
        attributes,
        mut mining,
        mut sequence_number,
        (
            mut mine_delay,
            mut mine_progress,
            mut mine_ticks,
            mut current_mining_item,
            mut current_mining_pos,
        ),
    ) in query
    {
        trace!("handle_mining_queued {mining_queued:?}");
        commands.entity(entity).remove::<MiningQueued>();

        let world = world_holder.shared.read();
        if check_is_interaction_restricted(
            &world,
            mining_queued.position,
            &game_mode.current,
            inventory,
        ) {
            finished.write(MiningFinished {
                entity,
                id: mining_queued.id,
                outcome: MiningOutcome::Interrupted,
            });
            continue;
        }
        // TODO (when world border is implemented): vanilla ignores if the block
        // is outside of the worldborder

        if let Some(mining) = &mut mining {
            // this matters if we were previously mining a block without force
            if mining_queued.force {
                mining.force = true;
            }
            // a start for the block being mined takes the mining over
            if mining.id != mining_queued.id
                && is_same_mining_target(
                    mining_queued.position,
                    inventory,
                    &current_mining_pos,
                    &current_mining_item,
                )
            {
                finished.write(MiningFinished {
                    entity,
                    id: mining.id,
                    outcome: MiningOutcome::Cancelled,
                });
                mining.id = mining_queued.id;
            }
        }

        if game_mode.current == GameMode::Creative {
            // In creative mode, first send START_DESTROY_BLOCK packet then
            // immediately finish mining
            commands.trigger(SendGamePacketEvent::new(
                entity,
                ServerboundPlayerAction {
                    action: s_player_action::Action::StartDestroyBlock,
                    pos: mining_queued.position,
                    direction: mining_queued.direction,
                    seq: sequence_number.start_predicting(),
                },
            ));
            commands.trigger(FinishMiningBlockEvent {
                entity,
                position: mining_queued.position,
                id: mining_queued.id,
            });
            **mine_delay = 5;
            commands.trigger(SwingArmEvent { entity });
        } else if mining.is_none()
            || !is_same_mining_target(
                mining_queued.position,
                inventory,
                &current_mining_pos,
                &current_mining_item,
            )
        {
            if let Some(mining) = &mining {
                finished.write(MiningFinished {
                    entity,
                    id: mining.id,
                    outcome: MiningOutcome::Cancelled,
                });
                // send a packet to stop mining since we just changed target
                commands.trigger(SendGamePacketEvent::new(
                    entity,
                    ServerboundPlayerAction {
                        action: s_player_action::Action::AbortDestroyBlock,
                        pos: current_mining_pos
                            .expect("IsMining is true so MineBlockPos must be present"),
                        direction: mining_queued.direction,
                        seq: 0,
                    },
                ));
            }

            let target_block_state = world
                .get_block_state(mining_queued.position)
                .unwrap_or_default();

            // we can't break blocks if they don't have a bounding box
            let block_is_solid = !target_block_state.outline_shape().is_empty();

            if block_is_solid && **mine_progress == 0. {
                // interact with the block (like note block left click) here
                attack_block_events.write(AttackBlockEvent {
                    entity,
                    position: mining_queued.position,
                });
            }

            let block = Box::<dyn BlockTrait>::from(target_block_state);

            let held_item = inventory.held_item();

            if block_is_solid
                && get_mine_progress(
                    block.as_ref(),
                    held_item,
                    fluid_on_eyes,
                    physics,
                    attributes,
                    active_effects,
                ) >= 1.
            {
                // block was broken instantly (instamined)
                commands.trigger(FinishMiningBlockEvent {
                    entity,
                    position: mining_queued.position,
                    id: mining_queued.id,
                });
            } else {
                let mining = Mining {
                    pos: mining_queued.position,
                    dir: mining_queued.direction,
                    force: mining_queued.force,
                    id: mining_queued.id,
                };
                trace!("inserting mining component {mining:?} for entity {entity:?}");
                commands.entity(entity).insert(mining);
                **current_mining_pos = Some(mining_queued.position);
                **current_mining_item = held_item.clone();
                **mine_progress = 0.;
                **mine_ticks = 0.;
                mine_block_progress_events.write(MineBlockProgressEvent {
                    entity,
                    position: mining_queued.position,
                    destroy_stage: mine_progress.destroy_stage(),
                });
            }

            commands.trigger(SendGamePacketEvent::new(
                entity,
                ServerboundPlayerAction {
                    action: s_player_action::Action::StartDestroyBlock,
                    pos: mining_queued.position,
                    direction: mining_queued.direction,
                    seq: sequence_number.start_predicting(),
                },
            ));
            commands.trigger(SwingArmEvent { entity });
            // another swing packet gets sent in the same tick in
            // continue_mining_block, vanilla does this too
        }
    }
}

#[derive(Message)]
pub struct MineBlockProgressEvent {
    pub entity: Entity,
    pub position: BlockPos,
    pub destroy_stage: Option<u32>,
}

/// A player left clicked on a block, used for stuff like interacting with note
/// blocks.
#[derive(Message)]
pub struct AttackBlockEvent {
    pub entity: Entity,
    pub position: BlockPos,
}

/// Returns whether the block and item are still the same as when we started
/// mining.
fn is_same_mining_target(
    target_block: BlockPos,
    inventory: &Inventory,
    current_mining_pos: &MineBlockPos,
    current_mining_item: &MineItem,
) -> bool {
    let held_item = inventory.held_item();
    Some(target_block) == current_mining_pos.0 && held_item == &current_mining_item.0
}

/// A component bundle for players that can mine blocks.
#[derive(Bundle, Clone, Default)]
pub struct MineBundle {
    pub delay: MineDelay,
    pub progress: MineProgress,
    pub ticks: MineTicks,
    pub mining_pos: MineBlockPos,
    pub mine_item: MineItem,
}

/// A component that counts down until we start mining the next block.
#[derive(Clone, Component, Debug, Default, Deref, DerefMut)]
pub struct MineDelay(pub u32);

/// A component that stores the progress of the current mining operation.
///
/// This is a value between 0 and 1.
#[derive(Clone, Component, Debug, Default, Deref, DerefMut)]
pub struct MineProgress(pub f32);

impl MineProgress {
    pub fn destroy_stage(&self) -> Option<u32> {
        if self.0 > 0. {
            Some((self.0 * 10.) as u32)
        } else {
            None
        }
    }
}

/// A component that stores the number of ticks that we've been mining the same
/// block for.
///
/// This is a float despite the fact that it should only ever be a round number.
#[derive(Clone, Component, Debug, Default, Deref, DerefMut)]
pub struct MineTicks(pub f32);

/// A component that stores the position of the block we're currently mining.
#[derive(Clone, Component, Debug, Default, Deref, DerefMut)]
pub struct MineBlockPos(pub Option<BlockPos>);

/// A component that contains the item we're currently using to mine, or
/// [`ItemStack::Empty`] if nothing is being mined.
#[derive(Clone, Component, Debug, Default, Deref, DerefMut)]
pub struct MineItem(pub ItemStack);

/// A trigger that's sent when we completed mining a block.
#[derive(EntityEvent)]
pub struct FinishMiningBlockEvent {
    pub entity: Entity,
    pub position: BlockPos,
    /// The start the mining came from.
    pub id: MiningId,
}

pub fn handle_finish_mining_block_observer(
    finish_mining_block: On<FinishMiningBlockEvent>,
    mut finished: MessageWriter<MiningFinished>,
    mut query: Query<(
        &WorldName,
        &LocalGameMode,
        &Inventory,
        &PlayerAbilities,
        &PermissionLevel,
        &Position,
        &mut BlockStatePredictionHandler,
    )>,
    worlds: Res<Worlds>,
) {
    let event = finish_mining_block.event();
    let broke = predict_broken_block(event, &mut query, &worlds);
    finished.write(MiningFinished {
        entity: event.entity,
        id: event.id,
        outcome: if broke {
            MiningOutcome::Broken
        } else {
            MiningOutcome::Interrupted
        },
    });
}

/// Predict the block of a finished mining as broken; `false` when the
/// break is not allowed or there is nothing to break.
#[allow(clippy::type_complexity)]
fn predict_broken_block(
    event: &FinishMiningBlockEvent,
    query: &mut Query<(
        &WorldName,
        &LocalGameMode,
        &Inventory,
        &PlayerAbilities,
        &PermissionLevel,
        &Position,
        &mut BlockStatePredictionHandler,
    )>,
    worlds: &Worlds,
) -> bool {
    let (
        world_name,
        game_mode,
        inventory,
        abilities,
        permission_level,
        player_pos,
        mut prediction_handler,
    ) = query.get_mut(event.entity).unwrap();
    let world_lock = worlds.get(world_name).unwrap();
    let world = world_lock.read();
    if check_is_interaction_restricted(&world, event.position, &game_mode.current, inventory) {
        return false;
    }

    if game_mode.current == GameMode::Creative {
        let held_item = inventory.held_item().kind();
        if matches!(held_item, ItemKind::Trident | ItemKind::DebugStick)
            || mineflayer_registry::tags::items::SWORDS.contains(&held_item)
        {
            return false;
        }
    }

    let Some(block_state) = world.get_block_state(event.position) else {
        return false;
    };

    let registry_block = block_state.as_block_kind();
    if !can_use_game_master_blocks(abilities, permission_level)
        && matches!(
            registry_block,
            BlockKind::CommandBlock | BlockKind::StructureBlock
        )
    {
        return false;
    }
    if block_state == BlockState::AIR {
        return false;
    }

    // when we break a waterlogged block we want to keep the water there
    let fluid_state = FluidState::from(block_state);
    let block_state_for_fluid = BlockState::from(fluid_state);
    let old_state = world
        .set_block_state(event.position, block_state_for_fluid)
        .unwrap_or_default();
    prediction_handler.retain_known_server_state(event.position, old_state, **player_pos);
    true
}

/// Abort mining a block.
#[derive(Message)]
pub struct StopMiningBlockEvent {
    pub entity: Entity,
}
pub fn handle_stop_mining_block_event(
    mut events: MessageReader<StopMiningBlockEvent>,
    mut commands: Commands,
    mut mine_block_progress_events: MessageWriter<MineBlockProgressEvent>,
    mut finished: MessageWriter<MiningFinished>,
    mut query: Query<(&MineBlockPos, &mut MineProgress, Option<&Mining>)>,
) {
    for event in events.read() {
        let (mine_block_pos, mut mine_progress, mining) = query.get_mut(event.entity).unwrap();

        let mine_block_pos =
            mine_block_pos.expect("IsMining is true so MineBlockPos must be present");
        if let Some(mining) = mining {
            finished.write(MiningFinished {
                entity: event.entity,
                id: mining.id,
                outcome: MiningOutcome::Cancelled,
            });
        }
        abort_mining(
            &mut commands,
            &mut mine_block_progress_events,
            event.entity,
            mine_block_pos,
        );
        **mine_progress = 0.;
    }
}

/// Cancel mining now: drop a queued start, including a
/// [`StartMiningBlockEvent`] written before this event and not read yet,
/// and abort the block being mined, if any. Starts written after this
/// event are not affected.
///
/// Trigger it on the world. Unlike [`StopMiningBlockEvent`], it is ordered
/// against the start messages: the cutoff is taken when it is triggered,
/// and the rest applies at the next command flush.
#[derive(EntityEvent)]
pub struct CancelMiningEvent {
    pub entity: Entity,
}

/// Start messages with an id below this were written before the last
/// [`CancelMiningEvent`]; the start handler skips them.
#[derive(Component)]
struct MiningCancelledBefore(usize);

fn handle_cancel_mining_event(
    event: On<CancelMiningEvent>,
    mut commands: Commands,
    starts: Res<Messages<StartMiningBlockEvent>>,
    mut mine_block_progress_events: MessageWriter<MineBlockProgressEvent>,
    mut finished: MessageWriter<MiningFinished>,
    mut query: Query<(
        Option<&Mining>,
        Option<&MiningQueued>,
        &MineBlockPos,
        &mut MineProgress,
    )>,
) {
    let entity = event.entity;
    if let Ok((mining, queued, ..)) = query.get(entity) {
        for id in [mining.map(|m| m.id), queued.map(|q| q.id)]
            .into_iter()
            .flatten()
        {
            finished.write(MiningFinished {
                entity,
                id,
                outcome: MiningOutcome::Cancelled,
            });
        }
    }
    commands
        .entity(entity)
        .remove::<MiningQueued>()
        .insert(MiningCancelledBefore(
            starts.oldest_message_count() + starts.len(),
        ));
    let Ok((mining, _, mine_block_pos, mut mine_progress)) = query.get_mut(entity) else {
        return;
    };
    if mining.is_some()
        && let Some(pos) = **mine_block_pos
    {
        abort_mining(&mut commands, &mut mine_block_progress_events, entity, pos);
        **mine_progress = 0.;
    }
}

fn abort_mining(
    commands: &mut Commands,
    mine_block_progress_events: &mut MessageWriter<MineBlockProgressEvent>,
    entity: Entity,
    pos: BlockPos,
) {
    commands.trigger(SendGamePacketEvent::new(
        entity,
        ServerboundPlayerAction {
            action: s_player_action::Action::AbortDestroyBlock,
            pos,
            direction: Direction::Down,
            seq: 0,
        },
    ));
    commands.entity(entity).remove::<Mining>();
    mine_block_progress_events.write(MineBlockProgressEvent {
        entity,
        position: pos,
        destroy_stage: None,
    });
}

pub fn decrement_mine_delay(mut query: Query<&mut MineDelay>) {
    for mut mine_delay in &mut query {
        if **mine_delay > 0 {
            **mine_delay -= 1;
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn continue_mining_block(
    mut query: Query<(
        Entity,
        &WorldName,
        &LocalGameMode,
        &Inventory,
        &MineBlockPos,
        &MineItem,
        &ActiveEffects,
        &FluidOnEyes,
        &Physics,
        &Attributes,
        &Mining,
        &mut MineDelay,
        &mut MineProgress,
        &mut MineTicks,
        &mut BlockStatePredictionHandler,
    )>,
    mut commands: Commands,
    mut mine_block_progress_events: MessageWriter<MineBlockProgressEvent>,
    mut finished: MessageWriter<MiningFinished>,
    worlds: Res<Worlds>,
) {
    for (
        entity,
        world_name,
        game_mode,
        inventory,
        current_mining_pos,
        current_mining_item,
        active_effects,
        fluid_on_eyes,
        physics,
        attributes,
        mining,
        mut mine_delay,
        mut mine_progress,
        mut mine_ticks,
        mut prediction_handler,
    ) in query.iter_mut()
    {
        if game_mode.current == GameMode::Creative {
            // TODO: worldborder check
            **mine_delay = 5;
            commands.trigger(SendGamePacketEvent::new(
                entity,
                ServerboundPlayerAction {
                    action: s_player_action::Action::StartDestroyBlock,
                    pos: mining.pos,
                    direction: mining.dir,
                    seq: prediction_handler.start_predicting(),
                },
            ));
            commands.trigger(FinishMiningBlockEvent {
                entity,
                position: mining.pos,
                id: mining.id,
            });
            commands.trigger(SwingArmEvent { entity });
        } else if mining.force
            || is_same_mining_target(
                mining.pos,
                inventory,
                current_mining_pos,
                current_mining_item,
            )
        {
            trace!("continue mining block at {:?}", mining.pos);
            let world_lock = worlds.get(world_name).unwrap();
            let world = world_lock.read();
            let target_block_state = world.get_block_state(mining.pos).unwrap_or_default();

            trace!("target_block_state: {target_block_state:?}");

            if target_block_state.is_air() {
                commands.entity(entity).remove::<Mining>();
                finished.write(MiningFinished {
                    entity,
                    id: mining.id,
                    outcome: MiningOutcome::Interrupted,
                });
                continue;
            }
            let block = Box::<dyn BlockTrait>::from(target_block_state);
            **mine_progress += get_mine_progress(
                block.as_ref(),
                current_mining_item,
                fluid_on_eyes,
                physics,
                attributes,
                active_effects,
            );

            if **mine_ticks % 4. == 0. {
                // vanilla makes a mining sound here
            }
            **mine_ticks += 1.;

            if **mine_progress >= 1. {
                // MiningQueued is removed in case we were doing an infinite
                // loop that repeatedly inserts MiningQueued
                commands.entity(entity).remove::<(Mining, MiningQueued)>();
                trace!("finished mining block at {:?}", mining.pos);
                commands.trigger(FinishMiningBlockEvent {
                    entity,
                    position: mining.pos,
                    id: mining.id,
                });
                commands.trigger(SendGamePacketEvent::new(
                    entity,
                    ServerboundPlayerAction {
                        action: s_player_action::Action::StopDestroyBlock,
                        pos: mining.pos,
                        direction: mining.dir,
                        seq: prediction_handler.start_predicting(),
                    },
                ));
                **mine_progress = 0.;
                **mine_ticks = 0.;
                **mine_delay = 5;
            }

            mine_block_progress_events.write(MineBlockProgressEvent {
                entity,
                position: mining.pos,
                destroy_stage: mine_progress.destroy_stage(),
            });
            commands.trigger(SwingArmEvent { entity });
        } else {
            trace!("switching mining target to {:?}", mining.pos);
            commands.entity(entity).insert(MiningQueued {
                position: mining.pos,
                direction: mining.dir,
                force: false,
                id: mining.id,
            });
        }
    }
}

pub fn update_mining_component(
    mut commands: Commands,
    mut finished: MessageWriter<MiningFinished>,
    mut query: Query<(Entity, &mut Mining, &HitResultComponent)>,
) {
    for (entity, mut mining, hit_result_component) in &mut query.iter_mut() {
        if let Some(block_hit_result) = hit_result_component.as_block_hit_result_if_not_miss() {
            if mining.force && block_hit_result.block_pos != mining.pos {
                continue;
            }

            if mining.pos != block_hit_result.block_pos {
                debug!(
                    "Updating Mining::pos from {:?} to {:?}",
                    mining.pos, block_hit_result.block_pos
                );
                mining.pos = block_hit_result.block_pos;
            }
            mining.dir = block_hit_result.direction;
        } else {
            if mining.force {
                continue;
            }

            debug!("Removing mining component because we're no longer looking at the block");
            commands.entity(entity).remove::<Mining>();
            finished.write(MiningFinished {
                entity,
                id: mining.id,
                outcome: MiningOutcome::Interrupted,
            });
        }
    }
}
