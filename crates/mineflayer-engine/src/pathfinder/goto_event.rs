use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use bevy_ecs::prelude::*;

use crate::pathfinder::{
    astar::PathfinderTimeout,
    goals::Goal,
    moves::{self, SuccessorsFn},
};

/// Send this event to start pathfinding to the given goal.
///
/// Also see [`PathfinderClientExt::goto`].
///
/// This event is read by [`goto_listener`]
///
/// [`goto_listener`]: crate::pathfinder::goto_listener
/// [`PathfinderClientExt::goto`]: crate::pathfinder::PathfinderClientExt::goto
#[derive(Message)]
pub struct GotoEvent {
    /// The local bot entity that will do the pathfinding and execute the path.
    pub entity: Entity,
    pub goal: Arc<dyn Goal>,
    pub opts: PathfinderOpts,
    /// Which goto this is. Each [`PathEvent`] of the goto carries it; a
    /// repath of the goto keeps it.
    ///
    /// [`PathEvent`]: crate::events::PathEvent
    pub id: GotoId,
}

impl GotoEvent {
    /// A new goto, with a new [`GotoId`].
    pub fn new(entity: Entity, goal: impl Goal + 'static, opts: PathfinderOpts) -> Self {
        Self {
            entity,
            goal: Arc::new(goal),
            opts,
            id: GotoId::next(),
        }
    }
}

/// The identifier of one goto, unique in the process.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct GotoId(pub u64);

impl GotoId {
    /// A new identifier. `GotoId(0)` is never returned.
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// Configuration options that the pathfinder will use when calculating and
/// executing a path.
///
/// This can be passed into [`Client::goto_with_opts`] or
/// [`Client::start_goto_with_opts`].
///
/// ```
/// # use mineflayer_engine::pathfinder::{moves, PathfinderOpts};
/// // example config to disallow mining blocks and to not do parkour
/// let opts = PathfinderOpts::new()
///     .allow_mining(false)
///     .successors_fn(moves::basic::basic_move);
/// ```
///
/// [`Client::goto_with_opts`]: super::PathfinderClientExt::goto_with_opts
/// [`Client::start_goto_with_opts`]: super::PathfinderClientExt::start_goto_with_opts
#[derive(Clone, Debug)]
pub struct PathfinderOpts {
    pub(crate) successors_fn: SuccessorsFn,
    pub(crate) allow_mining: bool,
    pub(crate) retry_on_no_path: bool,
    pub(crate) min_timeout: PathfinderTimeout,
    pub(crate) max_timeout: PathfinderTimeout,
    pub(crate) total_timeout: Option<Duration>,
    pub(crate) allow_sprint: bool,
}

impl PathfinderOpts {
    pub const fn new() -> Self {
        Self {
            successors_fn: moves::default_move,
            allow_mining: true,
            retry_on_no_path: true,
            min_timeout: PathfinderTimeout::Time(Duration::from_secs(1)),
            max_timeout: PathfinderTimeout::Time(Duration::from_secs(5)),
            total_timeout: None,
            allow_sprint: true,
        }
    }
    /// Set the function that's used for checking what moves are possible.
    ///
    /// Defaults to [`moves::default_move`].
    pub fn successors_fn(mut self, successors_fn: SuccessorsFn) -> Self {
        self.successors_fn = successors_fn;
        self
    }
    /// Set whether the bot is allowed to break blocks while pathfinding.
    ///
    /// Defaults to `true`.
    pub fn allow_mining(mut self, allow_mining: bool) -> Self {
        self.allow_mining = allow_mining;
        self
    }
    /// Whether we should recalculate the path when the pathfinder timed out and
    /// there's no partial path to try.
    ///
    /// Defaults to `true`.
    pub fn retry_on_no_path(mut self, retry_on_no_path: bool) -> Self {
        self.retry_on_no_path = retry_on_no_path;
        self
    }
    /// The minimum amount of time that should pass before the A* pathfinder
    /// function can return a timeout if it finds a path that seems good enough.
    /// It may take up to [`Self::max_timeout`] if it can't immediately find
    /// a usable path.
    ///
    /// Defaults to `PathfinderTimeout::Time(Duration::from_secs(1))`.
    ///
    /// Also see [`PathfinderTimeout::Nodes`]
    pub fn min_timeout(mut self, min_timeout: impl Into<PathfinderTimeout>) -> Self {
        self.min_timeout = min_timeout.into();
        self
    }
    /// The absolute maximum amount of time that the pathfinder function can
    /// take to find a path.
    ///
    /// If it takes this long, it means no usable path was found (so it might be
    /// impossible).
    ///
    /// Defaults to `PathfinderTimeout::Time(Duration::from_secs(5))`.
    pub fn max_timeout(mut self, max_timeout: impl Into<PathfinderTimeout>) -> Self {
        self.max_timeout = max_timeout.into();
        self
    }
    /// The search time one goto may use while it finds only partial paths.
    /// Each search has its own [`Self::max_timeout`]; this bounds their sum.
    /// When the sum reaches it, the goto gets one
    /// [`PathEvent::Timeout`](crate::events::PathEvent::Timeout) and keeps
    /// its goal. A complete path sets the sum to zero.
    ///
    /// Defaults to `None` (no total limit).
    pub fn total_timeout(mut self, total_timeout: Duration) -> Self {
        self.total_timeout = Some(total_timeout);
        self
    }
    /// Whether the moves may sprint. With `false` they walk where they
    /// would sprint.
    ///
    /// Defaults to `true`.
    pub fn allow_sprint(mut self, allow_sprint: bool) -> Self {
        self.allow_sprint = allow_sprint;
        self
    }
}
impl Default for PathfinderOpts {
    fn default() -> Self {
        Self::new()
    }
}
