//! Path events: translate the engine's [`Event::Pathfinder`] stream into
//! `path_update`/`goal_reached`/`path_stop`, and step follow and `dynamic`
//! goals on each tick.
//!
//! Only the events of the goto the facade goal runs as count: an event of
//! a replaced or stopped goto is stale. A search that ends with no path is
//! a failure, reported as its status (`noPath`/`timeout`), never as a
//! `path_stop`. The engine's `Timeout` keeps the goal (upstream's surviving
//! `stateGoal`); it settles the waiting `goto`s only.

use super::*;

/// Called by the bridge for every engine [`Event`].
pub(crate) fn bridge_events(client: &mineflayer_engine::Client, event: &Event) -> Vec<BotEvent> {
    match event {
        Event::Tick => step(client),
        Event::Pathfinder(event) => path_event(client, event),
        _ => Vec::new(),
    }
}

/// Run `f` on the facade when its goal still runs as `goto`.
fn if_current<R>(
    client: &mineflayer_engine::Client,
    goto: GotoId,
    f: impl FnOnce(&mut PathfinderFacade) -> R,
) -> Option<R> {
    with_component_mut::<PathfinderFacade, _>(client, |facade| {
        (facade.goto == Some(goto)).then(|| f(facade))
    })
    .flatten()
}

fn path_event(client: &mineflayer_engine::Client, event: &PathEvent) -> Vec<BotEvent> {
    let (PathEvent::PathFound { goto, .. }
    | PathEvent::Timeout { goto, .. }
    | PathEvent::GoalReached { goto }
    | PathEvent::Stopped { goto }) = event;
    let Some((goal, dynamic, think_timeout)) = with_component::<PathfinderFacade, _>(client, |f| {
        (f.goto == Some(*goto))
            .then(|| f.goal.clone().map(|g| (g, f.dynamic, f.think_timeout)))
            .flatten()
    })
    .flatten() else {
        return Vec::new();
    };
    match event {
        PathEvent::PathFound {
            path,
            is_partial: false,
            ..
        } => vec![BotEvent::PathUpdate {
            path: path.clone(),
            is_partial: false,
            status: PathStatus::Success,
        }],
        PathEvent::PathFound { path, .. } if !path.is_empty() => vec![BotEvent::PathUpdate {
            path: path.clone(),
            is_partial: true,
            status: PathStatus::Partial,
        }],
        PathEvent::PathFound { elapsed, .. } => {
            // no route, or the search used its whole budget: told apart
            // by duration, like upstream's `results.status`
            let (error, status) = if *elapsed >= think_timeout {
                (PathError::Timeout, PathStatus::Timeout)
            } else {
                (PathError::NoPath, PathStatus::NoPath)
            };
            let failed = if_current(client, *goto, |f| {
                f.goto = None;
                f.goal = None;
                f.settle(Err(error));
            });
            if failed.is_none() {
                return Vec::new();
            }
            client.stop_pathfinding();
            vec![BotEvent::PathUpdate {
                path: Vec::new(),
                is_partial: true,
                status,
            }]
        }
        PathEvent::Timeout { path, .. } => {
            if_current(client, *goto, |f| f.settle(Err(PathError::Timeout)));
            vec![BotEvent::PathUpdate {
                path: path.clone(),
                is_partial: true,
                status: PathStatus::Timeout,
            }]
        }
        PathEvent::GoalReached { .. } if dynamic => {
            // a dynamic goal never completes: it waits at the target, and
            // `step` sends it again when it stops holding
            if_current(client, *goto, |f| f.goto = None);
            Vec::new()
        }
        PathEvent::GoalReached { .. } => {
            // a follow step reaches the target's old block; the target
            // may have moved since, and then the next step follows it
            if matches!(goal, Goal::Follow { .. }) && !goal.is_end(client, client.position()) {
                if_current(client, *goto, |f| {
                    f.goto = None;
                    f.follow_block = None;
                });
                return Vec::new();
            }
            if_current(client, *goto, |f| {
                f.goto = None;
                f.settle(Ok(()));
                f.goal.take()
            })
            .flatten()
            .map(|goal| vec![BotEvent::GoalReached { goal }])
            .unwrap_or_default()
        }
        PathEvent::Stopped { .. } => {
            // a stop from inside the engine, such as a world change; a stop
            // the port sent cleared `goto` first
            if_current(client, *goto, |f| {
                f.goto = None;
                f.settle(Err(PathError::PathStopped));
                f.goal.take()
            })
            .flatten()
            .map(|goal| vec![BotEvent::PathStop { goal: Some(goal) }])
            .unwrap_or_default()
        }
    }
}

/// Send a follow goal again when its target moves to another block, and
/// a `dynamic` goal when it stops holding. A follow target that despawns
/// ends the goal, as upstream's `isValid()` failure does.
fn step(client: &mineflayer_engine::Client) -> Vec<BotEvent> {
    let Some((goal, goto, dynamic, follow_block)) =
        with_component::<PathfinderFacade, _>(client, |f| {
            f.goal
                .clone()
                .map(|g| (g, f.goto, f.dynamic, f.follow_block))
        })
        .flatten()
    else {
        return Vec::new();
    };
    let still_ours =
        |f: &PathfinderFacade| f.goto == goto && f.goal.as_ref().is_some_and(|g| g == &goal);
    let idle_unmet = || goto.is_none() && !goal.is_end(client, client.position());
    let next: Option<(Arc<dyn EngineGoal>, Option<BlockPos>)> = match &goal {
        Goal::Follow { entity, range } => {
            let Some(target) = entity.get_component::<Position>().map(|p| **p) else {
                let stopped = with_component_mut::<PathfinderFacade, _>(client, |f| {
                    still_ours(f).then(|| {
                        f.goto = None;
                        f.settle(Err(PathError::PathStopped));
                        f.goal.take()
                    })
                })
                .flatten()
                .flatten();
                let Some(goal) = stopped else {
                    return Vec::new();
                };
                client.stop_pathfinding();
                return vec![BotEvent::PathStop { goal: Some(goal) }];
            };
            let block = BlockPos::from(target);
            (follow_block != Some(block) || idle_unmet()).then(|| {
                (
                    Arc::new(goals::RadiusGoal::new(target, *range)) as Arc<dyn EngineGoal>,
                    Some(block),
                )
            })
        }
        _ if dynamic && idle_unmet() => goal.engine_goal(client).map(|g| (g, None)),
        _ => None,
    };
    if let Some((engine_goal, block)) = next {
        update_facade(client, |f| {
            if !still_ours(f) {
                return ((), None);
            }
            f.follow_block = block;
            ((), Some(f.goto_event(client.entity, engine_goal)))
        });
    }
    Vec::new()
}

#[cfg(test)]
mod tests;
