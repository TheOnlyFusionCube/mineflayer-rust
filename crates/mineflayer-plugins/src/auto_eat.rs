//! The mineflayer-auto-eat plugin surface: `bot.auto_eat()` — a per-tick
//! food monitor plus a manual `eat`.
//!
//! Upstream (the `mineflayer-auto-eat` npm package) attaches an `EatUtil`
//! at `bot.autoEat`: `enableAuto()` hooks `physicsTick` and calls `eat()`
//! whenever `bot.food < opts.minHunger || bot.health < opts.minHealth`;
//! `eat()` equips the best-scoring non-banned food, `activateItem`s, waits
//! for the consume-finish signal (entity status 9 / held-stack change,
//! the same ones [`Bot::consume`] watches), then restores the previous
//! held item. Its `eatStart`/`eatFinish`/`eatFail` events ride a
//! per-plugin emitter; here they are [`AutoEatEvent`]s on the plugin's
//! own channel.
//!
//! The monitor driver is an operation: it loops on the ticks until
//! `disable_auto` or the disconnect ends it, tolerates `Lagged`, and exits
//! silently — upstream emits nothing when the plugin's listener dies.

use std::time::Duration;

use mineflayer::{
    Bot, EquipDest, Error, Item, Result,
    engine::{
        ecs::{self as bevy_ecs, prelude::Component},
        inventory::{components::Food, default_components::get_default_component},
        registry::builtin::ItemKind,
    },
    plugin::{Op, OpSlot},
};

/// The auto-eat plugin's events; subscribe with
/// [`Bot::plugin_events::<AutoEatEvent>`](Bot::plugin_events).
#[derive(Clone, Debug)]
pub enum AutoEatEvent {
    /// mineflayer-auto-eat `eatStart` — the bot started eating `food`
    /// (in the offhand when `offhand`). Upstream emits the sanitized
    /// opts; ours carries the resolved food and hand.
    Start {
        /// The stack being eaten.
        food: Item,
        /// Whether the offhand is doing the eating.
        offhand: bool,
        /// Whether the previously held item is re-equipped after the eat
        /// (upstream's sanitized `equipOldItem`).
        equip_old_item: bool,
    },
    /// mineflayer-auto-eat `eatFail` — the eat failed; `0` is the
    /// upstream `Error`'s message.
    Fail(String),
    /// mineflayer-auto-eat `eatFinish` — the eat attempt concluded.
    /// Upstream emits it in `finally`, so it follows `Fail` too: it is a
    /// "stopped eating" signal, not a success signal.
    Finish {
        /// The stack that was eaten (or attempted).
        food: Item,
        /// Whether the offhand was doing the eating.
        offhand: bool,
        /// Whether the previously held item was re-equipped.
        equip_old_item: bool,
    },
}

/// Upstream `mineflayer-auto-eat`'s default `bannedFood` — foods with
/// harmful or disorienting side effects in vanilla.
const DEFAULT_BANNED: &[&str] = &[
    "rotten_flesh",
    "pufferfish",
    "chorus_fruit",
    "poisonous_potato",
    "spider_eye",
];

/// `bot.autoEat.foodsByName[name]` analog — the food component the engine
/// registers per [`ItemKind`]. `nutrition` is upstream's `foodPoints`;
/// `saturation` is in points, so `effectiveQuality` is
/// `nutrition + saturation` and `saturationRatio` is
/// `saturation / nutrition`, matching minecraft-data.
pub type FoodData = Food;

/// Upstream `FoodPriority` — the `foodsByName` field the best-food sort
/// uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FoodPriority {
    /// `foodPoints` — most hunger restored (upstream default).
    #[default]
    FoodPoints,
    /// `saturation` — most saturation points restored.
    Saturation,
    /// `effectiveQuality` — `nutrition + saturation`.
    EffectiveQuality,
    /// `saturationRatio` — `saturation / nutrition` (minecraft-data's
    /// `saturation / foodPoints`).
    SaturationRatio,
}

impl FoodPriority {
    /// The upstream sort key for one food entry.
    fn score(self, food: &FoodData) -> f32 {
        let nutrition = food.nutrition as f32;
        match self {
            FoodPriority::FoodPoints => nutrition,
            FoodPriority::Saturation => food.saturation,
            FoodPriority::EffectiveQuality => nutrition + food.saturation,
            FoodPriority::SaturationRatio => food.saturation / nutrition,
        }
    }
}

/// `bot.autoEat.opts` — every field mirrors upstream's `IEatUtilOpts`
/// defaults.
#[derive(Clone, Debug)]
pub struct AutoEatOpts {
    /// Which `foodsByName` field the best-food pick sorts by (default
    /// [`FoodPriority::FoodPoints`]).
    pub priority: FoodPriority,
    /// `minHunger` — the monitor eats when `bot.food` drops below this
    /// (default 15).
    pub min_hunger: u32,
    /// `minHealth` — the monitor eats when `bot.health` drops below this
    /// even at full hunger (default 14; only `can_always_eat` foods like
    /// golden apples can actually complete then — upstream relies on the
    /// same).
    pub min_health: f32,
    /// `bannedFood` — item names (e.g. `"rotten_flesh"`) the auto-pick
    /// skips. Explicit [`EatOpts::food`] selections bypass it, like
    /// upstream.
    pub banned_food: Vec<String>,
    /// `returnToLastItem` — re-equip the previously held item after
    /// eating (default true).
    pub return_to_last_item: bool,
    /// `offhand` — eat from the offhand instead of the main hand
    /// (default false).
    pub offhand: bool,
    /// `eatingTimeout` — how long one eat waits for the finish signal
    /// before failing (default 3 s).
    pub eating_timeout: Duration,
    /// `strictErrors` — failed eats return `Err` when true, `Ok` when
    /// false ([`AutoEatEvent::Fail`] + [`AutoEatEvent::Finish`] are emitted
    /// either way, like upstream's catch/finally).
    pub strict_errors: bool,
}

impl Default for AutoEatOpts {
    fn default() -> Self {
        Self {
            priority: FoodPriority::default(),
            min_hunger: 15,
            min_health: 14.,
            banned_food: DEFAULT_BANNED.iter().map(|s| (*s).to_string()).collect(),
            return_to_last_item: true,
            offhand: false,
            eating_timeout: Duration::from_millis(3000),
            strict_errors: true,
        }
    }
}

/// Upstream `FoodSelection` — what [`EatOpts::food`] accepts. Upstream's
/// numeric (item id) form has no engine analog; [`ItemKind`] covers it.
#[derive(Clone, Debug)]
pub enum FoodSelection {
    /// A specific inventory stack (an [`Item`] read from
    /// [`Bot::inventory`]).
    Item(Item),
    /// First inventory stack of this kind.
    Kind(ItemKind),
    /// First inventory stack with this name (e.g. `"bread"`).
    Name(String),
}

impl From<Item> for FoodSelection {
    fn from(item: Item) -> Self {
        FoodSelection::Item(item)
    }
}
impl From<ItemKind> for FoodSelection {
    fn from(kind: ItemKind) -> Self {
        FoodSelection::Kind(kind)
    }
}
impl From<&str> for FoodSelection {
    fn from(name: &str) -> Self {
        FoodSelection::Name(name.to_string())
    }
}
impl From<String> for FoodSelection {
    fn from(name: String) -> Self {
        FoodSelection::Name(name)
    }
}

/// Upstream `EatOpts` — per-call overrides for [`AutoEat::eat`]. `None`
/// fields fall back to [`AutoEatOpts`].
#[derive(Clone, Debug, Default)]
pub struct EatOpts {
    /// Which food to eat; `None` auto-picks by `priority` like upstream.
    /// A selection that isn't in the inventory also falls back to the
    /// auto-pick (upstream `normalizeFoodChoice` does the same).
    pub food: Option<FoodSelection>,
    /// `offhand` override — defaults to [`AutoEatOpts::offhand`].
    pub offhand: Option<bool>,
    /// `equipOldItem` override — defaults to
    /// [`AutoEatOpts::return_to_last_item`].
    pub equip_old_item: Option<bool>,
    /// `priority` override — defaults to [`AutoEatOpts::priority`].
    pub priority: Option<FoodPriority>,
}

/// Facade state the engine has no slot for: the opts, the enabled/eating
/// flags, the in-flight eat (for `cancel_eat`), and the monitor driver —
/// operations, as [`crate::pvp`] keeps its driver.
/// Stored as an ECS component created on first use — rule 2 of the
/// storage rule on `bot::BotShared`.
#[derive(Component, Default)]
struct AutoEatState {
    opts: AutoEatOpts,
    /// `bot.autoEat.enabled` — whether the monitor driver should be
    /// running.
    enabled: bool,
    /// `bot.autoEat.isEating` — an eat is in flight.
    is_eating: bool,
    /// The eat [`AutoEat::cancel_eat`] reaches, while it waits.
    eat: OpSlot,
    /// The monitor driver; started by `enable_auto`.
    monitor: OpSlot,
}

/// The `bot.autoEat` namespace — a borrowed view onto the [`Bot`], like
/// [`crate::PvpExt::pvp`]. Upstream's plugin is always attached once loaded;
/// ours materializes its state on first use, so `bot.auto_eat()` needs no
/// `loadPlugin` step.
pub struct AutoEat<'a> {
    bot: &'a Bot,
}

/// Adds `auto_eat` to [`Bot`].
pub trait AutoEatExt {
    /// `bot.autoEat` — the mineflayer-auto-eat namespace (`eat`,
    /// `enable_auto`/`disable_auto`, `opts`/`set_opts`, `is_eating`).
    fn auto_eat(&self) -> AutoEat<'_>;
}

impl AutoEatExt for Bot {
    fn auto_eat(&self) -> AutoEat<'_> {
        AutoEat { bot: self }
    }
}

impl AutoEat<'_> {
    /// `bot.autoEat.opts` — a snapshot of the current options.
    pub fn opts(&self) -> AutoEatOpts {
        self.bot
            .state::<AutoEatState, _>(|s| s.opts.clone())
            .unwrap_or_default()
    }

    /// `bot.autoEat.setOpts(partial)` — replace the options. Read
    /// [`AutoEat::opts`], change fields, and write back for the
    /// partial-update form upstream's merge gives.
    pub fn set_opts(&self, opts: AutoEatOpts) {
        self.bot.state_mut::<AutoEatState, _>(|s| s.opts = opts);
    }

    /// `bot.autoEat.enabled` — whether the per-tick monitor is running.
    pub fn enabled(&self) -> bool {
        self.bot
            .state::<AutoEatState, _>(|s| s.enabled)
            .unwrap_or(false)
    }

    /// `bot.autoEat.isEating` — an eat is in flight (manual or
    /// monitor-driven).
    pub fn is_eating(&self) -> bool {
        self.bot
            .state::<AutoEatState, _>(|s| s.is_eating)
            .unwrap_or(false)
    }

    /// `bot.autoEat.foodsByName[name]` analog — the vanilla food data
    /// for `kind` (`nutrition`/`saturation`/`can_always_eat`), `None`
    /// for non-foods.
    pub fn food_data(kind: ItemKind) -> Option<FoodData> {
        get_default_component::<FoodData>(kind)
    }

    /// `bot.autoEat.enableAuto()` — start the per-tick monitor. While
    /// enabled, every game tick with `bot.food() < min_hunger ||
    /// bot.health() < min_health` runs one [`AutoEat::eat`]. Idempotent
    /// like upstream; the driver retires on [`AutoEat::disable_auto`] or
    /// disconnect.
    pub fn enable_auto(&self) {
        let started = self.bot.state_mut::<AutoEatState, _>(|s| {
            if s.enabled {
                return None;
            }
            s.enabled = true;
            Some(self.bot.start_op(&mut s.monitor))
        });
        if let Some(Some(op)) = started {
            tokio::spawn(drive(self.bot.driver(), op));
        }
    }

    /// `bot.autoEat.disableAuto()` — stop the monitor; an eat already in
    /// flight finishes normally, like upstream removing the listener.
    pub fn disable_auto(&self) {
        self.bot.state_mut::<AutoEatState, _>(|s| {
            s.enabled = false;
            s.monitor.cancel();
        });
    }

    /// `bot.autoEat.eat(opts)` — equip the best (or `opts.food`) food and
    /// eat it once. Errors `Already eating!` while an eat is in flight,
    /// and `"No food specified and couldn't find a choice in
    /// inventory!"` when there's nothing edible — both before any event,
    /// like upstream. After [`AutoEatEvent::Start`] the eat resolves on
    /// the consume-finish signal, fails on [`AutoEat::cancel_eat`], a
    /// superseding [`Bot::consume`], or `eating_timeout`; failures emit
    /// [`AutoEatEvent::Fail`] only when `strict_errors` is off — upstream's
    /// emit is dead code under the default `strictErrors: true` — while
    /// [`AutoEatEvent::Finish`] fires either way (upstream's `finally`),
    /// and the call returns `Err` under `strict_errors`.
    pub async fn eat(&self, opts: EatOpts) -> Result<()> {
        match self.bot.state_mut::<AutoEatState, _>(|s| {
            if s.is_eating {
                return false;
            }
            s.is_eating = true;
            true
        }) {
            None => return Err(Error::NotConnected),
            Some(false) => return Err(Error::InvalidState("Already eating!".into())),
            Some(true) => {}
        }
        let result = self.run_eat(opts).await;
        // upstream leaks _eating on the equip-failure throw; releasing it
        // on every path is strictly better
        self.bot
            .state_mut::<AutoEatState, _>(|s| s.is_eating = false);
        result
    }

    /// `bot.autoEat.cancelEat()` — cancel the in-flight eat (fails it
    /// with `Eating manually canceled!` and releases the item use), a
    /// no-op when nothing is eating.
    pub fn cancel_eat(&self) {
        let in_flight = self
            .bot
            .state_mut::<AutoEatState, _>(|s| s.eat.cancel())
            .unwrap_or(false);
        if !in_flight {
            return;
        }
        // upstream deactivateItem() in cancelEat
        self.bot.deactivate_item();
    }

    /// Everything between the `is_eating` claim and its release:
    /// resolve the food, equip it, activate, wait, restore, report.
    async fn run_eat(&self, opts: EatOpts) -> Result<()> {
        let bot = self.bot;
        let (offhand, equip_old, priority, timeout, strict) = bot
            .state::<AutoEatState, _>(|s| {
                (
                    opts.offhand.unwrap_or(s.opts.offhand),
                    opts.equip_old_item.unwrap_or(s.opts.return_to_last_item),
                    opts.priority.unwrap_or(s.opts.priority),
                    s.opts.eating_timeout,
                    s.opts.strict_errors,
                )
            })
            .unwrap_or((
                false,
                true,
                FoodPriority::default(),
                Duration::from_millis(3000),
                true,
            ));

        let Some(food) = self.resolve_food(opts.food.as_ref(), priority) else {
            return Err(Error::NotFound(
                "No food specified and couldn't find a choice in inventory!".into(),
            ));
        };

        let dest = if offhand {
            EquipDest::OffHand
        } else {
            EquipDest::Hand
        };
        let previous = self.hand_item(offhand);
        let switched = previous.as_ref().is_none_or(|p| p.slot != food.slot);
        if switched {
            bot.equip(&food, dest)
                .await
                .map_err(|e| Error::Failed(format!("Failed to equip: {}! ({e})", food.name())))?;
        }

        let outcome = self.use_food(&food, offhand, equip_old, timeout).await;

        // upstream's finally: restore the previous item first, then
        // report — eatFinish fires even on failure
        if equip_old
            && switched
            && let Some(previous) = previous
        {
            let _ = bot.equip(&previous, dest).await;
        }

        match outcome {
            Ok(()) => {
                bot.emit_plugin_event(AutoEatEvent::Finish {
                    food,
                    offhand,
                    equip_old_item: equip_old,
                });
                Ok(())
            }
            Err(e) => {
                // upstream's `emit('eatFail')` sits AFTER the strictErrors
                // throw in its catch — dead code under the default
                // `strictErrors: true`; only non-strict callers see it
                if !strict {
                    bot.emit_plugin_event(AutoEatEvent::Fail(e.to_string()));
                }
                bot.emit_plugin_event(AutoEatEvent::Finish {
                    food,
                    offhand,
                    equip_old_item: equip_old,
                });
                if strict { Err(e) } else { Ok(()) }
            }
        }
    }

    /// Use the food in hand and wait until the eat ends: finished,
    /// timed out, cancelled, or the held item switched.
    async fn use_food(
        &self,
        food: &Item,
        offhand: bool,
        equip_old: bool,
        timeout: Duration,
    ) -> Result<()> {
        let bot = self.bot;
        // `cancel_eat` reaches the eat from here on — upstream's
        // `_rejectionBinding` only exists during `buildEatingListener`, so
        // a `cancel_eat` before this point is a silent no-op there too
        let manual = bot
            .state_mut::<AutoEatState, _>(|s| bot.start_op(&mut s.eat))
            .ok_or(Error::NotConnected)?;

        // sanitize any prior use state before activating, like upstream
        bot.deactivate_item();
        bot.emit_plugin_event(AutoEatEvent::Start {
            food: food.clone(),
            offhand,
            equip_old_item: equip_old,
        });
        let end = tokio::select! {
            biased;
            _ = bot.disconnected() => Err(Error::NotConnected),
            _ = manual.cancelled() => {
                bot.cancel_consume();
                Err(Error::Cancelled("Eating manually canceled!".into()))
            }
            end = bot.consume_with(offhand, timeout) => end,
        };
        bot.state_mut::<AutoEatState, _>(|s| s.eat.finish(&manual));
        match end {
            Err(Error::Cancelled(reason)) if reason == "Eating manually canceled!" => {
                Err(Error::Cancelled(reason))
            }
            Err(Error::Cancelled(_)) => Err(Error::Cancelled(
                "Eating cancelled due to calling bot.consume() again".into(),
            )),
            Err(Error::Timeout) => Err(Error::Failed(format!(
                "Eating timed out with a time of {} milliseconds!",
                timeout.as_millis()
            ))),
            Err(e) => Err(e),
            // upstream's itemListener rejects when the slot's item changed
            // type mid-eat — an empty slot is a finished stack, not a
            // switch. (Upstream appends `\nItem: ${newItem}` — JS prints
            // "[object Object]", so the first line carries all the
            // information.)
            Ok(()) => match self.hand_item(offhand) {
                Some(held) if held.kind() != food.kind() => Err(Error::Failed(format!(
                    "Item switched early to: {}!",
                    held.name()
                ))),
                _ => Ok(()),
            },
        }
    }

    /// Upstream `findBestChoices(items, priority)`: the edible,
    /// non-banned items of `items`, sorted by `priority` descending.
    /// Ties keep the earlier slot (upstream's sort is stable).
    pub fn find_best_choices(&self, items: &[Item], priority: FoodPriority) -> Vec<Item> {
        // the read does not insert the component — fall back to the defaults
        // when no state component was ever inserted
        let banned = self
            .bot
            .state::<AutoEatState, _>(|s| s.opts.banned_food.clone())
            .unwrap_or_else(|| AutoEatOpts::default().banned_food);
        let mut choices: Vec<(Item, f32)> = items
            .iter()
            .filter(|i| !banned.iter().any(|b| b == i.name()))
            .filter_map(|i| Self::food_data(i.kind()).map(|f| (i.clone(), priority.score(&f))))
            .collect();
        choices.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        choices.into_iter().map(|(item, _)| item).collect()
    }

    /// Upstream `normalizeFoodChoice` + the `findBestChoices` fallback:
    /// an explicit selection resolves to an inventory stack (or is used
    /// as-is for [`FoodSelection::Item`]); otherwise the best-scoring
    /// non-banned food wins. `None` = nothing edible in inventory.
    fn resolve_food(
        &self,
        selection: Option<&FoodSelection>,
        priority: FoodPriority,
    ) -> Option<Item> {
        let chosen = match selection {
            Some(FoodSelection::Item(item)) => Some(item.clone()),
            Some(FoodSelection::Kind(kind)) => {
                self.food_items().into_iter().find(|i| i.kind() == *kind)
            }
            Some(FoodSelection::Name(name)) => {
                self.food_items().into_iter().find(|i| i.name() == name)
            }
            None => None,
        };
        chosen.or_else(|| self.best_food(priority))
    }

    /// The upstream `getAllItems` scan region for food purposes: the
    /// player-inventory region (9..45) plus the offhand slot 45 — armor
    /// slots can't hold food, so upstream's armor coverage adds nothing.
    fn food_items(&self) -> Vec<Item> {
        let inventory = self.bot.inventory();
        let mut items = inventory.inventory_items();
        if let Some(offhand) = inventory.slot(45) {
            items.push(offhand);
        }
        items
    }

    /// The item in the hand this eat uses — slot 45 for `offhand`, the
    /// held quickbar stack otherwise.
    fn hand_item(&self, offhand: bool) -> Option<Item> {
        if offhand {
            self.bot.inventory().slot(45)
        } else {
            self.bot.held_item()
        }
    }

    /// `find_best_choices` over the live inventory — upstream
    /// `findBestChoices(getAllItems())[0]`.
    fn best_food(&self, priority: FoodPriority) -> Option<Item> {
        self.find_best_choices(&self.food_items(), priority)
            .into_iter()
            .next()
    }
}

/// The monitor loop: one iteration per game tick, upstream's
/// `physicsTick` `statusCheck`. Owns no report — autoEat emits nothing
/// when the monitor ends, so a cancelled or disconnected driver exits
/// silently.
///
/// `op` ends it: `disable_auto` or the disconnect.
async fn drive(bot: Bot, op: Op) {
    let mut ticks = bot.ticks_until(&op);
    while ticks.next().await {
        // upstream statusCheck: `(bot.food < opts.minHunger ||
        // bot.health < opts.minHealth) && !isEating` → eat. `eat` gates
        // on is_eating itself, so only the thresholds are read here.
        let Some((min_hunger, min_health)) =
            bot.state::<AutoEatState, _>(|s| (s.opts.min_hunger, s.opts.min_health))
        else {
            continue;
        };
        if bot.food() < min_hunger || bot.health() < min_health {
            let _ = bot.auto_eat().eat(EatOpts::default()).await;
        }
    }
}

#[cfg(test)]
mod tests;
