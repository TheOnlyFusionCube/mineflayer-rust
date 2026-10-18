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
//! per-plugin emitter; here they are [`BotEvent`] tail variants like the
//! other plugin progress events.
//!
//! The monitor driver follows [`crate::pvp`]'s rules: it selects on the
//! tick broadcast and the bot's root token (the tick sender is a World
//! resource the driver's own `Client` keeps alive, so `Closed` is
//! unreachable and the token is the disconnect signal), tolerates `Lagged`, and exits
//! silently — upstream emits nothing when the plugin's listener dies.

use std::time::Duration;

use mineflayer_engine::{
    ecs::{self as bevy_ecs, prelude::Component},
    inventory::{components::Food, default_components::get_default_component},
    registry::builtin::ItemKind,
};

use crate::bot::{TickStream, with_component, with_component_mut};
use crate::{
    Bot, BotEvent, Error, Result,
    inventory::{EatEnd, EquipDest},
    op::{Op, OpSlot},
    windows::Item,
};

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
    /// false ([`BotEvent::EatFail`] + [`BotEvent::EatFinish`] are emitted
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
/// [`Bot::pvp`]. Upstream's plugin is always attached once loaded; ours
/// materializes its state on first use, so `bot.auto_eat()` needs no
/// `loadPlugin` step.
pub struct AutoEat<'a> {
    bot: &'a Bot,
}

impl Bot {
    /// `bot.autoEat` — the mineflayer-auto-eat namespace (`eat`,
    /// `enable_auto`/`disable_auto`, `opts`/`set_opts`, `is_eating`).
    pub fn auto_eat(&self) -> AutoEat<'_> {
        AutoEat { bot: self }
    }
}

impl AutoEat<'_> {
    /// `bot.autoEat.opts` — a snapshot of the current options.
    pub fn opts(&self) -> AutoEatOpts {
        with_component::<AutoEatState, _>(&self.bot.client, |s| s.opts.clone()).unwrap_or_default()
    }

    /// `bot.autoEat.setOpts(partial)` — replace the options. Read
    /// [`AutoEat::opts`], change fields, and write back for the
    /// partial-update form upstream's merge gives.
    pub fn set_opts(&self, opts: AutoEatOpts) {
        with_component_mut::<AutoEatState, _>(&self.bot.client, |s| s.opts = opts);
    }

    /// `bot.autoEat.enabled` — whether the per-tick monitor is running.
    pub fn enabled(&self) -> bool {
        with_component::<AutoEatState, _>(&self.bot.client, |s| s.enabled).unwrap_or(false)
    }

    /// `bot.autoEat.isEating` — an eat is in flight (manual or
    /// monitor-driven).
    pub fn is_eating(&self) -> bool {
        with_component::<AutoEatState, _>(&self.bot.client, |s| s.is_eating).unwrap_or(false)
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
        let started = with_component_mut::<AutoEatState, _>(&self.bot.client, |s| {
            if s.enabled {
                return None;
            }
            s.enabled = true;
            Some(s.monitor.start(&self.bot.root))
        });
        if let Some(Some(op)) = started {
            tokio::spawn(drive(self.bot.driver_handle(), op));
        }
    }

    /// `bot.autoEat.disableAuto()` — stop the monitor; an eat already in
    /// flight finishes normally, like upstream removing the listener.
    pub fn disable_auto(&self) {
        with_component_mut::<AutoEatState, _>(&self.bot.client, |s| {
            s.enabled = false;
            s.monitor.cancel();
        });
    }

    /// `bot.autoEat.eat(opts)` — equip the best (or `opts.food`) food and
    /// eat it once. Errors `Already eating!` while an eat is in flight,
    /// and `"No food specified and couldn't find a choice in
    /// inventory!"` when there's nothing edible — both before any event,
    /// like upstream. After [`BotEvent::EatStart`] the eat resolves on
    /// the consume-finish signal, fails on [`AutoEat::cancel_eat`], a
    /// superseding [`Bot::consume`], or `eating_timeout`; failures emit
    /// [`BotEvent::EatFail`] only when `strict_errors` is off — upstream's
    /// emit is dead code under the default `strictErrors: true` — while
    /// [`BotEvent::EatFinish`] fires either way (upstream's `finally`),
    /// and the call returns `Err` under `strict_errors`.
    pub async fn eat(&self, opts: EatOpts) -> Result<()> {
        match with_component_mut::<AutoEatState, _>(&self.bot.client, |s| {
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
        with_component_mut::<AutoEatState, _>(&self.bot.client, |s| s.is_eating = false);
        result
    }

    /// `bot.autoEat.cancelEat()` — cancel the in-flight eat (fails it
    /// with `Eating manually canceled!` and releases the item use), a
    /// no-op when nothing is eating.
    pub fn cancel_eat(&self) {
        let in_flight = with_component_mut::<AutoEatState, _>(&self.bot.client, |s| s.eat.cancel())
            .unwrap_or(false);
        if !in_flight {
            return;
        }
        // end the eat's wait the way a second consume() would; the eat
        // sees its own cancel and reports it as manual
        self.bot.windows(|t| t.eat.cancel());
        // upstream deactivateItem() in cancelEat
        self.bot.deactivate_item();
    }

    /// Everything between the `is_eating` claim and its release:
    /// resolve the food, equip it, activate, wait, restore, report.
    async fn run_eat(&self, opts: EatOpts) -> Result<()> {
        let bot = self.bot;
        let (offhand, equip_old, priority, timeout, strict) =
            with_component::<AutoEatState, _>(&bot.client, |s| {
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
                bot.emit_event(BotEvent::EatFinish {
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
                    bot.emit_event(BotEvent::EatFail(e.to_string()));
                }
                bot.emit_event(BotEvent::EatFinish {
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
        // start the eat before activating, like `Bot::consume` — a finish
        // packet landing between the use_item write and the wait can't be
        // missed
        let (op, on_finish) = bot.start_eat().ok_or(Error::NotConnected)?;
        // `cancel_eat` reaches the eat from here on — upstream's
        // `_rejectionBinding` only exists during `buildEatingListener`, so
        // a `cancel_eat` before this point is a silent no-op there too
        let manual = with_component_mut::<AutoEatState, _>(&bot.client, |s| s.eat.start(&bot.root))
            .ok_or(Error::NotConnected)?;

        // sanitize any prior use state before activating, like upstream
        bot.deactivate_item();
        bot.activate_item(offhand);
        bot.emit_event(BotEvent::EatStart {
            food: food.clone(),
            offhand,
            equip_old_item: equip_old,
        });

        let end = bot.wait_eat(&op, on_finish, timeout).await;
        with_component_mut::<AutoEatState, _>(&bot.client, |s| s.eat.finish(&manual));
        match end {
            EatEnd::Superseded if manual.is_cancelled() => {
                Err(Error::Cancelled("Eating manually canceled!".into()))
            }
            EatEnd::Superseded => Err(Error::Cancelled(
                "Eating cancelled due to calling bot.consume() again".into(),
            )),
            EatEnd::Timeout => Err(Error::Failed(format!(
                "Eating timed out with a time of {} milliseconds!",
                timeout.as_millis()
            ))),
            EatEnd::Disconnected => Err(Error::NotConnected),
            // upstream's itemListener rejects when the slot's item changed
            // type mid-eat — an empty slot is a finished stack, not a
            // switch. (Upstream appends `\nItem: ${newItem}` — JS prints
            // "[object Object]", so the first line carries all the
            // information.)
            EatEnd::Finished => match self.hand_item(offhand) {
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
        let banned =
            with_component::<AutoEatState, _>(&self.bot.client, |s| s.opts.banned_food.clone())
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
    let mut ticks = TickStream::new(&bot.client, op.token.clone());
    while ticks.next().await {
        // upstream statusCheck: `(bot.food < opts.minHunger ||
        // bot.health < opts.minHealth) && !isEating` → eat. `eat` gates
        // on is_eating itself, so only the thresholds are read here.
        let Some((min_hunger, min_health)) = with_component::<AutoEatState, _>(&bot.client, |s| {
            (s.opts.min_hunger, s.opts.min_health)
        }) else {
            continue;
        };
        if bot.food() < min_hunger || bot.health() < min_health {
            let _ = bot.auto_eat().eat(EatOpts::default()).await;
        }
    }
}

#[cfg(test)]
mod tests;
