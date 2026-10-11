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
//! tick broadcast and `bot.shutdown` (the tick sender is a World resource
//! the driver's own `Client` keeps alive, so `Closed` is unreachable and
//! shutdown is the disconnect signal), tolerates `Lagged`, and exits
//! silently — upstream emits nothing when the plugin's listener dies.

use std::{
    sync::{
        Arc,
        atomic::{self, AtomicBool},
    },
    time::Duration,
};

use azalea::{
    ecs::{self as bevy_ecs, prelude::Component},
    inventory::{components::Food, default_components::get_default_component},
    registry::builtin::ItemKind,
};

use crate::{Bot, BotEvent, Error, Result, inventory::EquipDest, windows::Item};

/// Upstream `mineflayer-auto-eat`'s default `bannedFood` — foods with
/// harmful or disorienting side effects in vanilla.
const DEFAULT_BANNED: &[&str] = &[
    "rotten_flesh",
    "pufferfish",
    "chorus_fruit",
    "poisonous_potato",
    "spider_eye",
];

/// `bot.autoEat.foodsByName[name]` analog — the food component azalea
/// registers per [`ItemKind`]. `nutrition` is upstream's `foodPoints`;
/// `saturation` is in points, so `effectiveQuality` is
/// `nutrition + saturation` and `saturationRatio` is
/// `saturation / (2 * nutrition)`, matching minecraft-data.
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
    /// `saturationRatio` — `saturation / (2 * nutrition)`.
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
            FoodPriority::SaturationRatio => food.saturation / (2. * nutrition),
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
/// numeric (item id) form has no azalea analog; [`ItemKind`] covers it.
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

/// Facade state azalea has no slot for: the opts, the enabled/eating
/// flags, the in-flight eat's cancel flag, and the monitor driver's
/// cancel flag — the same shape [`crate::pvp`] uses for its driver.
#[derive(Component, Default)]
struct AutoEatState {
    opts: AutoEatOpts,
    /// `bot.autoEat.enabled` — whether the monitor driver should be
    /// running.
    enabled: bool,
    /// `bot.autoEat.isEating` — an eat is in flight.
    is_eating: bool,
    /// Set per eat for [`AutoEat::cancel_eat`]; taken when it ends.
    eat_cancel: Option<Arc<AtomicBool>>,
    /// Retires the monitor driver; replaced on every `enable_auto`.
    driver: Option<Arc<AtomicBool>>,
}

/// Read from the [`AutoEatState`] component. `None` when it or the
/// entity doesn't exist.
fn auto_eat_state<R>(client: &azalea::Client, f: impl FnOnce(&AutoEatState) -> R) -> Option<R> {
    let ecs = client.ecs.read();
    let entity = ecs.get_entity(client.entity).ok()?;
    entity.get::<AutoEatState>().map(f)
}

/// Run `f` against [`AutoEatState`], inserting a default one first.
/// `None` when the client entity is gone.
fn auto_eat_state_mut<R>(
    client: &azalea::Client,
    f: impl FnOnce(&mut AutoEatState) -> R,
) -> Option<R> {
    let mut ecs = client.ecs.write();
    let mut entity = ecs.get_entity_mut(client.entity).ok()?;
    if entity.get::<AutoEatState>().is_none() {
        entity.insert(AutoEatState::default());
    }
    entity.get_mut::<AutoEatState>().map(|mut c| f(&mut c))
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
        auto_eat_state(&self.bot.client, |s| s.opts.clone()).unwrap_or_default()
    }

    /// `bot.autoEat.setOpts(partial)` — replace the options. Read
    /// [`AutoEat::opts`], change fields, and write back for the
    /// partial-update form upstream's merge gives.
    pub fn set_opts(&self, opts: AutoEatOpts) {
        auto_eat_state_mut(&self.bot.client, |s| s.opts = opts);
    }

    /// `bot.autoEat.enabled` — whether the per-tick monitor is running.
    pub fn enabled(&self) -> bool {
        auto_eat_state(&self.bot.client, |s| s.enabled).unwrap_or(false)
    }

    /// `bot.autoEat.isEating` — an eat is in flight (manual or
    /// monitor-driven).
    pub fn is_eating(&self) -> bool {
        auto_eat_state(&self.bot.client, |s| s.is_eating).unwrap_or(false)
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
        let spawned = auto_eat_state_mut(&self.bot.client, |s| {
            if s.enabled {
                return None;
            }
            s.enabled = true;
            let cancel = Arc::new(AtomicBool::new(false));
            // a stale driver (shouldn't exist while disabled) gets
            // retired so only one monitor ever runs
            if let Some(old) = s.driver.replace(cancel.clone()) {
                old.store(true, atomic::Ordering::Relaxed);
            }
            Some(cancel)
        });
        if let Some(Some(cancel)) = spawned {
            tokio::spawn(drive(self.bot.clone(), cancel, self.bot.shutdown.clone()));
        }
    }

    /// `bot.autoEat.disableAuto()` — stop the monitor; an eat already in
    /// flight finishes normally, like upstream removing the listener.
    pub fn disable_auto(&self) {
        auto_eat_state_mut(&self.bot.client, |s| {
            s.enabled = false;
            if let Some(cancel) = s.driver.take() {
                cancel.store(true, atomic::Ordering::Relaxed);
            }
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
        let cancel = Arc::new(AtomicBool::new(false));
        match auto_eat_state_mut(&self.bot.client, |s| {
            if s.is_eating {
                return false;
            }
            s.is_eating = true;
            true
        }) {
            None => return Err(Error::NotConnected),
            Some(false) => return Err(Error::Operation("Already eating!".into())),
            Some(true) => {}
        }
        let result = self.run_eat(opts, &cancel).await;
        // upstream leaks _eating on the equip-failure throw; releasing it
        // on every path is strictly better
        auto_eat_state_mut(&self.bot.client, |s| {
            s.is_eating = false;
            s.eat_cancel = None;
        });
        result
    }

    /// `bot.autoEat.cancelEat()` — cancel the in-flight eat (fails it
    /// with `Eating manually canceled!` and releases the item use), a
    /// no-op when nothing is eating.
    pub fn cancel_eat(&self) {
        let in_flight = auto_eat_state_mut(&self.bot.client, |s| {
            if let Some(cancel) = &s.eat_cancel {
                cancel.store(true, atomic::Ordering::Relaxed);
                true
            } else {
                false
            }
        })
        .unwrap_or(false);
        if !in_flight {
            return;
        }
        {
            // wake the eat's waiter the way a second consume() would —
            // it sees the generation bump and reports the cancellation
            let mut t = self.bot.windows.lock().unwrap();
            t.eat_generation += 1;
            t.eat_notify.notify_waiters();
        }
        // upstream deactivateItem() in cancelEat; deactivate_item is
        // async here, so the packet write goes on the runtime
        let bot = self.bot.clone();
        tokio::spawn(async move { bot.deactivate_item().await });
    }

    /// Everything between the `is_eating` claim and its release:
    /// resolve the food, equip it, activate, wait, restore, report.
    async fn run_eat(&self, opts: EatOpts, cancel: &Arc<AtomicBool>) -> Result<()> {
        let bot = self.bot;
        let (offhand, equip_old, priority, timeout, strict) = auto_eat_state(&bot.client, |s| {
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
            return Err(Error::Operation(
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
            bot.equip(&food, dest).await.map_err(|e| {
                Error::Operation(format!("Failed to equip: {}! ({e})", food.name()))
            })?;
        }

        // register the finish waiter before activating, like
        // `Bot::consume` — a finish packet landing between the use_item
        // write and the await can't be missed
        let (generation, notify) = {
            let mut t = bot.windows.lock().unwrap();
            t.eat_generation += 1;
            t.eat_notify.notify_waiters();
            (t.eat_generation, t.eat_notify.clone())
        };
        // `eat_cancel` goes live with the wait — upstream's
        // `_rejectionBinding` only exists during `buildEatingListener`,
        // so a `cancel_eat` before this point is a silent no-op there too
        auto_eat_state_mut(&bot.client, |s| s.eat_cancel = Some(cancel.clone()));
        let notified = notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();

        // sanitize any prior use state before activating, like upstream
        bot.deactivate_item().await;
        bot.activate_item(offhand).await;
        bot.emit_event(BotEvent::EatStart {
            food: food.clone(),
            offhand,
        });

        let outcome = if cancel.load(atomic::Ordering::Relaxed) {
            // cancel landed between waiter registration and here — its
            // notify_waiters fired before `enable()` and can't be caught
            Err(Error::Operation("Eating manually canceled!".into()))
        } else if tokio::time::timeout(timeout, notified).await.is_err() {
            Err(Error::Operation(format!(
                "Eating timed out with a time of {} milliseconds!",
                timeout.as_millis()
            )))
        } else if bot.windows.lock().unwrap().eat_generation != generation {
            if cancel.load(atomic::Ordering::Relaxed) {
                Err(Error::Operation("Eating manually canceled!".into()))
            } else {
                Err(Error::Operation(
                    "Eating cancelled due to calling bot.consume() again".into(),
                ))
            }
        } else {
            // the wake fired: upstream's itemListener rejects when the
            // slot's item changed type mid-eat — an empty slot is a
            // finished stack, not a switch. (Upstream appends
            // `\nItem: ${newItem}` — JS prints "[object Object]", so the
            // first line carries all the information.)
            match self.hand_item(offhand) {
                Some(held) if held.kind() != food.kind() => Err(Error::Operation(format!(
                    "Item switched early to: {}!",
                    held.name()
                ))),
                _ => Ok(()),
            }
        };

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
                bot.emit_event(BotEvent::EatFinish { food, offhand });
                Ok(())
            }
            Err(e) => {
                // upstream's `emit('eatFail')` sits AFTER the strictErrors
                // throw in its catch — dead code under the default
                // `strictErrors: true`; only non-strict callers see it
                if !strict {
                    bot.emit_event(BotEvent::EatFail(e.to_string()));
                }
                bot.emit_event(BotEvent::EatFinish { food, offhand });
                if strict { Err(e) } else { Ok(()) }
            }
        }
    }

    /// Upstream `findBestChoices(items, priority)`: the edible,
    /// non-banned items of `items`, sorted by `priority` descending.
    /// Ties keep the earlier slot (upstream's sort is stable).
    pub fn find_best_choices(&self, items: &[Item], priority: FoodPriority) -> Vec<Item> {
        // `auto_eat_state` is read-only — fall back to the real defaults
        // when no state component was ever inserted
        let banned = auto_eat_state(&self.bot.client, |s| s.opts.banned_food.clone())
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
/// `shutdown` is the disconnect signal: the `TickBroadcast` sender is a
/// World resource kept alive by the `Client` the driver holds, so
/// `ticks.recv()` can never observe `Closed` — without this arm the loop
/// would park forever after the bridge exits.
async fn drive(bot: Bot, cancel: Arc<AtomicBool>, mut shutdown: tokio::sync::watch::Receiver<()>) {
    let mut ticks = bot.client.get_tick_broadcaster();
    let _ = shutdown.borrow_and_update();
    loop {
        tokio::select! {
            r = ticks.recv() => {
                match r {
                    Ok(()) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
            _ = shutdown.changed() => return,
        }
        if cancel.load(atomic::Ordering::Relaxed) {
            return;
        }
        // upstream statusCheck: `(bot.food < opts.minHunger ||
        // bot.health < opts.minHealth) && !isEating` → eat. `eat` gates
        // on is_eating itself, so only the thresholds are read here.
        let Some((min_hunger, min_health)) =
            auto_eat_state(&bot.client, |s| (s.opts.min_hunger, s.opts.min_health))
        else {
            continue;
        };
        if bot.food() < min_hunger || bot.health() < min_health {
            let _ = bot.auto_eat().eat(EatOpts::default()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use azalea::{
        entity::{inventory::Inventory, metadata::Health},
        inventory::ItemStack,
        local_player::Hunger,
        registry::identifier::Identifier,
        world::WorldName,
    };

    use super::*;
    use crate::test_util::{bot_with_shutdown, live_entity_client, tick_client, tick_sender};

    /// An inventory carrying `items` (`slot` = absolute player-window
    /// index — hotbar is 36..45, main inventory 9..36).
    fn food_inventory(items: &[(usize, ItemKind)]) -> Inventory {
        let mut inventory = Inventory::default();
        for (slot, kind) in items {
            *inventory.inventory_menu.slot_mut(*slot).unwrap() = ItemStack::new(*kind, 1);
        }
        inventory
    }

    /// A scaffold bot for direct `eat()` calls — live entity carrying the
    /// inventory, no tick broadcast (the driver isn't spawned).
    fn eating_bot(items: &[(usize, ItemKind)]) -> Bot {
        Bot::new(live_entity_client((
            WorldName(Identifier::from("minecraft:overworld")),
            food_inventory(items),
        )))
    }

    /// A tick-capable bot for driver tests, wired to a shutdown sender.
    fn monitored_bot(
        food: u32,
        health: f32,
        items: &[(usize, ItemKind)],
    ) -> (Bot, tokio::sync::watch::Sender<()>) {
        bot_with_shutdown(tick_client((
            WorldName(Identifier::from("minecraft:overworld")),
            food_inventory(items),
            Hunger {
                food,
                saturation: 0.,
            },
            Health(health),
        )))
    }

    /// Drain pending events; collect the autoEat event names in order.
    fn auto_eat_events(rx: &mut tokio::sync::broadcast::Receiver<BotEvent>) -> Vec<&'static str> {
        let mut seen = Vec::new();
        while let Ok(event) = rx.try_recv() {
            seen.push(match event {
                BotEvent::EatStart { .. } => "eatStart",
                BotEvent::EatFail(_) => "eatFail",
                BotEvent::EatFinish { .. } => "eatFinish",
                _ => continue,
            });
        }
        seen
    }

    /// Send one tick once a receiver exists — `enable_auto`/spawned
    /// drivers subscribe to the broadcast asynchronously, so a send
    /// fired immediately can hit `SendError` before any `recv` is armed.
    async fn send_tick(client: &azalea::Client) {
        for _ in 0..200 {
            if tick_sender(client).send(()).is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("no tick receiver ever subscribed");
    }

    /// Wait (bounded) for the next EatStart, returning its food kind.
    async fn next_eat_start(rx: &mut tokio::sync::broadcast::Receiver<BotEvent>) -> ItemKind {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let event = tokio::time::timeout_at(deadline, rx.recv())
                .await
                .expect("no EatStart within 5s")
                .expect("event stream closed");
            if let BotEvent::EatStart { food, .. } = event {
                return food.kind();
            }
        }
    }

    /// The scoring key follows minecraft-data's field names — cooked
    /// beef and golden carrot disagree across priorities, so every
    /// branch is observable.
    #[test]
    fn priority_scores_match_minecraft_data() {
        let beef = AutoEat::food_data(ItemKind::CookedBeef).unwrap();
        let carrot = AutoEat::food_data(ItemKind::GoldenCarrot).unwrap();
        assert_eq!((beef.nutrition, carrot.nutrition), (8, 6));
        assert!((beef.saturation - 12.8).abs() < 0.001);
        assert!((carrot.saturation - 14.4).abs() < 0.001);

        assert!(FoodPriority::FoodPoints.score(&beef) > FoodPriority::FoodPoints.score(&carrot));
        assert!(FoodPriority::Saturation.score(&carrot) > FoodPriority::Saturation.score(&beef));
        assert!(
            FoodPriority::EffectiveQuality.score(&beef)
                > FoodPriority::EffectiveQuality.score(&carrot)
        );
        assert!(
            FoodPriority::SaturationRatio.score(&carrot)
                > FoodPriority::SaturationRatio.score(&beef)
        );

        // non-foods have no entry, like `name in foodsByName` upstream
        assert!(AutoEat::food_data(ItemKind::Stone).is_none());
    }

    #[test]
    fn best_food_picks_highest_score_and_skips_banned() {
        let bot = eating_bot(&[
            (9, ItemKind::Apple),
            (10, ItemKind::CookedBeef),
            (11, ItemKind::RottenFlesh),
        ]);
        let auto_eat = bot.auto_eat();
        assert_eq!(
            auto_eat
                .best_food(FoodPriority::FoodPoints)
                .map(|i| i.kind()),
            Some(ItemKind::CookedBeef)
        );
        // golden carrot out-saturates beef; rotten flesh stays banned
        let bot = eating_bot(&[(9, ItemKind::CookedBeef), (10, ItemKind::GoldenCarrot)]);
        let auto_eat = bot.auto_eat();
        assert_eq!(
            auto_eat
                .best_food(FoodPriority::Saturation)
                .map(|i| i.kind()),
            Some(ItemKind::GoldenCarrot)
        );
        assert_eq!(
            auto_eat
                .best_food(FoodPriority::FoodPoints)
                .map(|i| i.kind()),
            Some(ItemKind::CookedBeef)
        );
        // an all-banned/non-food inventory has no choice
        let bot = eating_bot(&[(9, ItemKind::RottenFlesh), (10, ItemKind::Stone)]);
        assert!(bot.auto_eat().best_food(FoodPriority::FoodPoints).is_none());
    }

    #[tokio::test]
    async fn eat_without_food_errors_before_any_event() {
        let bot = eating_bot(&[]);
        let mut rx = bot.events();
        let err = bot.auto_eat().eat(EatOpts::default()).await.unwrap_err();
        assert!(err.to_string().contains("couldn't find a choice"));
        assert!(auto_eat_events(&mut rx).is_empty());
        assert!(!bot.auto_eat().is_eating());
    }

    #[tokio::test]
    async fn eat_emits_start_then_finish_on_consume_end() {
        let bot = eating_bot(&[(36, ItemKind::Apple)]);
        let mut rx = bot.events();
        let eating = tokio::spawn({
            let bot = bot.clone();
            async move { bot.auto_eat().eat(EatOpts::default()).await }
        });
        assert_eq!(next_eat_start(&mut rx).await, ItemKind::Apple);
        assert!(bot.auto_eat().is_eating());

        // the bridge's real consume-finish (entity status 9 / held-stack
        // change) fires `eat_notify`; simulate the wake directly
        bot.windows.lock().unwrap().eat_notify.notify_waiters();

        eating.await.unwrap().unwrap();
        assert_eq!(auto_eat_events(&mut rx), ["eatFinish"]);
        assert!(!bot.auto_eat().is_eating());
    }

    #[tokio::test]
    async fn eat_timeout_errors_and_finishes_without_fail_under_strict() {
        let bot = eating_bot(&[(36, ItemKind::Apple)]);
        let mut opts = bot.auto_eat().opts();
        opts.eating_timeout = Duration::from_millis(50);
        bot.auto_eat().set_opts(opts);

        let mut rx = bot.events();
        let err = bot.auto_eat().eat(EatOpts::default()).await.unwrap_err();
        // upstream's timeout Error message, verbatim
        assert_eq!(
            err.to_string(),
            "Eating timed out with a time of 50 milliseconds!"
        );
        // strict mode: the throw precedes upstream's eatFail emit —
        // only eatFinish (the finally) is observable
        assert_eq!(auto_eat_events(&mut rx), ["eatStart", "eatFinish"]);
    }

    #[tokio::test]
    async fn strict_errors_off_still_reports_but_resolves_ok() {
        let bot = eating_bot(&[(36, ItemKind::Apple)]);
        let mut opts = bot.auto_eat().opts();
        opts.eating_timeout = Duration::from_millis(50);
        opts.strict_errors = false;
        bot.auto_eat().set_opts(opts);

        let mut rx = bot.events();
        bot.auto_eat().eat(EatOpts::default()).await.unwrap();
        assert_eq!(
            auto_eat_events(&mut rx),
            ["eatStart", "eatFail", "eatFinish"]
        );
    }

    #[tokio::test]
    async fn second_eat_errors_and_cancel_eat_fails_the_first() {
        let bot = eating_bot(&[(36, ItemKind::Apple)]);
        let mut rx = bot.events();
        let eating = tokio::spawn({
            let bot = bot.clone();
            async move { bot.auto_eat().eat(EatOpts::default()).await }
        });
        assert_eq!(next_eat_start(&mut rx).await, ItemKind::Apple);

        let err = bot.auto_eat().eat(EatOpts::default()).await.unwrap_err();
        assert_eq!(err.to_string(), "Already eating!");

        bot.auto_eat().cancel_eat();
        let err = eating.await.unwrap().unwrap_err();
        assert_eq!(err.to_string(), "Eating manually canceled!");
        // strict mode suppresses eatFail; the finally's eatFinish stays
        assert_eq!(auto_eat_events(&mut rx), ["eatFinish"]);
        assert!(!bot.auto_eat().is_eating());

        // no eat in flight → a silent no-op
        bot.auto_eat().cancel_eat();
    }

    #[tokio::test]
    async fn explicit_food_selection_bypasses_scoring_and_ban() {
        // rotten flesh is banned by default but an explicit selection
        // still uses it (upstream `normalizeFoodChoice` has no ban check)
        let bot = eating_bot(&[(36, ItemKind::RottenFlesh), (37, ItemKind::CookedBeef)]);
        let mut opts = bot.auto_eat().opts();
        opts.eating_timeout = Duration::from_millis(50);
        bot.auto_eat().set_opts(opts);

        let mut rx = bot.events();
        let _ = bot
            .auto_eat()
            .eat(EatOpts {
                food: Some(FoodSelection::Name("rotten_flesh".into())),
                ..EatOpts::default()
            })
            .await;
        assert_eq!(next_eat_start(&mut rx).await, ItemKind::RottenFlesh);
    }

    #[tokio::test]
    async fn monitor_eats_when_hungry_and_reports_each_attempt() {
        let (bot, _shutdown) = monitored_bot(10, 20., &[(36, ItemKind::Apple)]);
        let mut opts = bot.auto_eat().opts();
        opts.eating_timeout = Duration::from_millis(50);
        bot.auto_eat().set_opts(opts);

        let mut rx = bot.events();
        bot.auto_eat().enable_auto();
        assert!(bot.auto_eat().enabled());
        send_tick(&bot.client).await;

        assert_eq!(next_eat_start(&mut rx).await, ItemKind::Apple);
        // the attempt then fails on the scaffold (no finish packet) —
        // strict mode suppresses eatFail but the finally's eatFinish arrives
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(auto_eat_events(&mut rx), ["eatFinish"]);
    }

    #[tokio::test]
    async fn monitor_idles_when_full_and_healthy() {
        let (bot, _shutdown) = monitored_bot(20, 20., &[(36, ItemKind::Apple)]);
        let mut rx = bot.events();
        bot.auto_eat().enable_auto();
        send_tick(&bot.client).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(auto_eat_events(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn monitor_eats_below_min_health_even_when_full() {
        // upstream: `bot.food < minHunger || bot.health < minHealth`
        let (bot, _shutdown) = monitored_bot(20, 10., &[(36, ItemKind::Apple)]);
        let mut opts = bot.auto_eat().opts();
        opts.eating_timeout = Duration::from_millis(50);
        bot.auto_eat().set_opts(opts);

        let mut rx = bot.events();
        bot.auto_eat().enable_auto();
        send_tick(&bot.client).await;
        assert_eq!(next_eat_start(&mut rx).await, ItemKind::Apple);
    }

    #[tokio::test]
    async fn disable_stops_the_monitor() {
        let (bot, _shutdown) = monitored_bot(10, 20., &[(36, ItemKind::Apple)]);
        bot.auto_eat().enable_auto();
        bot.auto_eat().disable_auto();
        assert!(!bot.auto_eat().enabled());

        let mut rx = bot.events();
        // the receiver may already be gone once the driver sees the
        // cancel flag, so sends are allowed to fail here
        let _ = tick_sender(&bot.client).send(());
        let _ = tick_sender(&bot.client).send(());
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(auto_eat_events(&mut rx).is_empty());
    }

    /// Lagged tolerance: the capacity-1 tick broadcast turns two queued
    /// ticks into `Lagged(1)` — a driver that treated `Err` as fatal
    /// would exit before the surviving tick drives the eat.
    #[tokio::test]
    async fn driver_survives_lagged_ticks() {
        let (bot, _shutdown) = monitored_bot(10, 20., &[(36, ItemKind::Apple)]);
        let mut opts = bot.auto_eat().opts();
        opts.eating_timeout = Duration::from_millis(50);
        bot.auto_eat().set_opts(opts);

        let cancel = Arc::new(AtomicBool::new(false));
        let (_tx, rx) = tokio::sync::watch::channel(());
        let _driver = tokio::spawn(drive(bot.clone(), cancel, rx));
        let mut events = bot.events();

        // once the driver is parked on recv, overflow the capacity-1
        // ring — the sends land back-to-back while the current-thread
        // runtime keeps the driver parked, so its next recv is Lagged
        send_tick(&bot.client).await;
        tick_sender(&bot.client).send(()).unwrap();
        tick_sender(&bot.client).send(()).unwrap();

        assert_eq!(next_eat_start(&mut events).await, ItemKind::Apple);
    }

    /// The bridge's shutdown sender dropping (disconnect) is the only
    /// exit signal — the tick channel can never close while the driver's
    /// own `Client` holds the sender.
    #[tokio::test]
    async fn driver_exits_on_shutdown() {
        let (bot, shutdown_tx) = monitored_bot(10, 20., &[(36, ItemKind::Apple)]);
        let cancel = Arc::new(AtomicBool::new(false));
        let driver = tokio::spawn(drive(bot.clone(), cancel.clone(), bot.shutdown.clone()));

        // parked on ticks like a live connection — and cancel alone
        // can't be observed until a tick or shutdown arrives
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(shutdown_tx);

        tokio::time::timeout(Duration::from_secs(5), driver)
            .await
            .expect("driver outlived the connection")
            .unwrap();

        // and cancel retires it on the next tick
        let (bot, _tx) = monitored_bot(20, 20., &[]);
        let cancel = Arc::new(AtomicBool::new(false));
        let driver = tokio::spawn(drive(bot.clone(), cancel.clone(), bot.shutdown.clone()));
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel.store(true, atomic::Ordering::Relaxed);
        send_tick(&bot.client).await;
        tokio::time::timeout(Duration::from_secs(5), driver)
            .await
            .expect("cancelled driver kept running")
            .unwrap();
    }

    /// An `Item` like `window.slot(i)` returns — `find_best_choices`
    /// takes caller-built lists, upstream `findBestChoices(items, p)`.
    fn slot_item(slot: i16, kind: ItemKind) -> Item {
        Item::of(ItemStack::new(kind, 1), slot).unwrap()
    }

    #[test]
    fn find_best_choices_sorts_by_priority_and_skips_banned() {
        let bot = eating_bot(&[]);
        let items = vec![
            slot_item(9, ItemKind::Apple),
            slot_item(10, ItemKind::CookedBeef),
            slot_item(11, ItemKind::RottenFlesh), // banned by default
            slot_item(12, ItemKind::Stone),       // not a food
            slot_item(13, ItemKind::GoldenCarrot),
        ];
        let kinds = |priority: FoodPriority| -> Vec<ItemKind> {
            bot.auto_eat()
                .find_best_choices(&items, priority)
                .iter()
                .map(Item::kind)
                .collect()
        };
        assert_eq!(
            kinds(FoodPriority::FoodPoints),
            [
                ItemKind::CookedBeef,
                ItemKind::GoldenCarrot,
                ItemKind::Apple
            ]
        );
        // the saturation ranking flips the carrot above the beef
        assert_eq!(
            kinds(FoodPriority::Saturation),
            [
                ItemKind::GoldenCarrot,
                ItemKind::CookedBeef,
                ItemKind::Apple
            ]
        );
        // a caller-set banned list is honored too
        let mut opts = bot.auto_eat().opts();
        opts.banned_food.push("cooked_beef".into());
        bot.auto_eat().set_opts(opts);
        assert_eq!(
            kinds(FoodPriority::FoodPoints),
            [ItemKind::GoldenCarrot, ItemKind::Apple]
        );
    }

    #[test]
    fn offhand_slot_45_is_scanned_for_food() {
        // upstream getAllItems covers the offhand — food only in slot
        // 45 must be reachable by the fallback and explicit selection
        let bot = eating_bot(&[(45, ItemKind::CookedBeef)]);
        let auto_eat = bot.auto_eat();
        assert_eq!(
            auto_eat
                .best_food(FoodPriority::FoodPoints)
                .map(|i| (i.kind(), i.slot)),
            Some((ItemKind::CookedBeef, 45))
        );
        assert_eq!(
            auto_eat
                .resolve_food(
                    Some(&FoodSelection::Kind(ItemKind::CookedBeef)),
                    FoodPriority::FoodPoints
                )
                .map(|i| i.slot),
            Some(45)
        );
        assert_eq!(
            auto_eat
                .resolve_food(
                    Some(&FoodSelection::Name("cooked_beef".into())),
                    FoodPriority::FoodPoints
                )
                .map(|i| i.slot),
            Some(45)
        );
    }

    #[tokio::test]
    async fn eat_offhand_uses_the_offhand_stack() {
        // the canonical upstream case: `offhand: true` + food already in
        // the offhand → activateItem(true) with nothing to equip
        let bot = eating_bot(&[(45, ItemKind::Apple)]);
        let mut opts = bot.auto_eat().opts();
        opts.eating_timeout = Duration::from_millis(50);
        bot.auto_eat().set_opts(opts);

        let mut rx = bot.events();
        let eating = tokio::spawn({
            let bot = bot.clone();
            async move {
                bot.auto_eat()
                    .eat(EatOpts {
                        offhand: Some(true),
                        ..EatOpts::default()
                    })
                    .await
            }
        });
        let offhand = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let BotEvent::EatStart { offhand, .. } = rx.recv().await.unwrap() {
                    break offhand;
                }
            }
        })
        .await
        .expect("no EatStart within 5s");
        assert!(offhand);
        // the scaffold can't finish an eat — it times out
        eating.await.unwrap().unwrap_err();
    }

    #[tokio::test]
    async fn held_item_type_switch_fails_the_eat() {
        // upstream itemListener: a different-kind stack landing in the
        // food's slot mid-eat rejects `Item switched early to: <name>!`
        // rather than counting as the consume-finish wake
        let bot = eating_bot(&[(36, ItemKind::Apple)]);
        let mut opts = bot.auto_eat().opts();
        opts.eating_timeout = Duration::from_secs(5);
        bot.auto_eat().set_opts(opts);

        let mut rx = bot.events();
        let eating = tokio::spawn({
            let bot = bot.clone();
            async move { bot.auto_eat().eat(EatOpts::default()).await }
        });
        assert_eq!(next_eat_start(&mut rx).await, ItemKind::Apple);

        // swap the held stack for stone and fire the consume wake
        {
            let mut ecs = bot.client.ecs.write();
            let mut inv = ecs.get_mut::<Inventory>(bot.client.entity).unwrap();
            *inv.inventory_menu.slot_mut(36).unwrap() = ItemStack::new(ItemKind::Stone, 1);
        }
        bot.windows.lock().unwrap().eat_notify.notify_waiters();

        let err = eating.await.unwrap().unwrap_err();
        assert_eq!(err.to_string(), "Item switched early to: stone!");
        // strict: eatFail stays unemitted, the finally's eatFinish fires
        assert_eq!(auto_eat_events(&mut rx), ["eatFinish"]);
    }

    #[tokio::test]
    async fn superseding_consume_reports_distinctly() {
        // a generation bump without the cancel flag — what a second
        // consume() does — must not be mislabeled a manual cancellation
        let bot = eating_bot(&[(36, ItemKind::Apple)]);
        let mut opts = bot.auto_eat().opts();
        opts.eating_timeout = Duration::from_secs(5);
        bot.auto_eat().set_opts(opts);

        let mut rx = bot.events();
        let eating = tokio::spawn({
            let bot = bot.clone();
            async move { bot.auto_eat().eat(EatOpts::default()).await }
        });
        assert_eq!(next_eat_start(&mut rx).await, ItemKind::Apple);

        {
            let mut t = bot.windows.lock().unwrap();
            t.eat_generation += 1;
            t.eat_notify.notify_waiters();
        }
        let err = eating.await.unwrap().unwrap_err();
        assert_eq!(
            err.to_string(),
            "Eating cancelled due to calling bot.consume() again"
        );
        assert_eq!(auto_eat_events(&mut rx), ["eatFinish"]);
    }
}
