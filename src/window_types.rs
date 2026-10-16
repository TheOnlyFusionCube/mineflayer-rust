//! Typed windows — the upstream `furnace.js`, `enchantment_table.js`,
//! `anvil.js`, `villager.js`, `book.js`, and `command_block.js` plugins
//! ported onto the U8 [`Window`] base.
//!
//! # Server-pushed window data
//!
//! Upstream attaches per-window fields (furnace fuel, enchant options,
//! trades) to the window object and updates them from packets azalea
//! drops entirely (`container_set_data` = upstream `craft_progress_bar`,
//! `merchant_offers` = upstream `trade_list`). `WindowDataTracker`
//! decodes them per window id; `observe_event` runs in the bridge next
//! to `windows::observe_event` and emits the typed events
//! ([`BotEvent::FurnaceUpdate`], [`BotEvent::EnchantmentTableReady`],
//! [`BotEvent::VillagerReady`]) plus the generic
//! [`BotEvent::WindowDataUpdate`] feed.
//!
//! Windows aren't event emitters here (same call U8 made): typed-window
//! events are bot-level and carry the `window_id` they belong to.
//!
//! # Slot layouts (azalea [`Menu`](azalea::inventory::Menu) shapes, same as upstream)
//!
//! - Furnace/blast/smoker: `0` input, `1` fuel, `2` output.
//! - Enchantment table: `0` target item, `1` lapis.
//! - Anvil: `0` first, `1` second, `2` result.
//! - Merchant: `0`/`1` payment slots, `2` result.
//!
//! # Costs the server computes
//!
//! Upstream predicts anvil xp cost client-side via `Item.anvil`; azalea
//! has no equivalent, so [`Anvil::combine`]/[`Anvil::rename`] measure the
//! `container_set_data` repair-cost field the server reports after the
//! inputs land — same guard, different information source (noted in
//! `docs/api-parity.md`).

use std::{
    collections::{HashMap, HashSet},
    ops::Deref,
    sync::Arc,
    time::Duration,
};

use parking_lot::Mutex;

use azalea::{
    BlockPos, EntityRef, Event,
    core::{data_registry::DataRegistryWithKey, game_type::GameMode},
    entity::EntityKindComponent,
    inventory::components::WritableBookContent,
    protocol::packets::game::{
        ClientboundGamePacket,
        c_merchant_offers::MerchantOffer,
        s_container_button_click::ServerboundContainerButtonClick,
        s_edit_book::ServerboundEditBook,
        s_rename_item::ServerboundRenameItem,
        s_select_trade::ServerboundSelectTrade,
        s_set_command_block::{self, ServerboundSetCommandBlock},
    },
    registry::{
        DataRegistry,
        builtin::{BlockKind, EntityKind, ItemKind, MenuKind},
        data,
    },
};
use tokio::sync::broadcast;

use crate::{
    Bot, BotEvent, Error, Result,
    bot::wait::await_event,
    inventory::{TransferOpts, transfer},
    windows::{Item, Window},
    world::Block,
};

/// `craft_progress_bar` property ids on a furnace-menu window: fuel
/// ticks remaining, fuel ticks total, cook ticks elapsed, cook ticks
/// total (vanilla `AbstractFurnaceMenu` data slots).
const FUEL_LEFT: u16 = 0;
const FUEL_TOTAL: u16 = 1;
const COOK_DONE: u16 = 2;
const COOK_TOTAL: u16 = 3;

/// Enchantment-menu property ids: three xp-level costs, the enchantment
/// seed, then three enchantment-id + three enchantment-level hints
/// (vanilla `EnchantmentMenu`; the hint fields are `-1` when absent).
const ENCHANT_XP_SEED: u16 = 3;
const ENCHANT_CLUE: u16 = 4;
const ENCHANT_LEVEL_CLUE: u16 = 7;

/// Anvil-menu property 0 is the level cost (`AnvilMenu.cost`).
const ANVIL_COST: u16 = 0;

/// Merchant-menu slot indices (`MerchantMenu`).
const TRADE_INPUT_1: u16 = 0;
const TRADE_INPUT_2: u16 = 1;
const TRADE_RESULT: u16 = 2;

/// `edit_book` wire limits the vanilla server disconnects over
/// (`EditBookPacket`/`WritableBookContent` guards) — enforced at our
/// boundary so a bad call errors instead of kicking the bot.
const BOOK_MAX_PAGES: usize = 100;
const BOOK_MAX_TITLE_CHARS: usize = 32;
/// Per-page cap — `WritableBookContent.PAGE_EDIT_LENGTH`, counted in
/// UTF-16 units like the Java `String.length()` the wire decode checks.
const BOOK_MAX_PAGE_UTF16: usize = 1024;

/// The `edit_book` boundary guards, split out so they're testable
/// without a connection (upstream's book.js asserts only the slot
/// range; the rest mirror the wire limits the server kicks over).
fn check_book_input(slot: i32, pages: &[String], title: Option<&str>) -> Result<()> {
    if !(0..=44).contains(&slot) {
        return Err(Error::InvalidArgument("slot out of inventory range".into()));
    }
    if pages.len() > BOOK_MAX_PAGES {
        return Err(Error::InvalidArgument(format!(
            "book has too many pages ({BOOK_MAX_PAGES} max)"
        )));
    }
    if pages
        .iter()
        .any(|p| p.encode_utf16().count() > BOOK_MAX_PAGE_UTF16)
    {
        return Err(Error::InvalidArgument(format!(
            "book page too long ({BOOK_MAX_PAGE_UTF16} chars max)"
        )));
    }
    if title.is_some_and(|t| t.chars().count() > BOOK_MAX_TITLE_CHARS) {
        return Err(Error::InvalidArgument(format!(
            "book title too long ({BOOK_MAX_TITLE_CHARS} chars max)"
        )));
    }
    Ok(())
}

const WINDOW_WAIT: Duration = Duration::from_secs(5);
/// Upstream's `onceWithCleanup` timeout for the `edit_book` ack.
const BOOK_ACK_TIMEOUT: Duration = Duration::from_secs(20);
/// Bounded wait for the server's anvil `cost` resync — the packet only
/// goes out when the value changes, so a same-value swap can't be
/// awaited indefinitely.
const ANVIL_COST_WAIT: Duration = Duration::from_millis(1500);

// ---------------------------------------------------------------------------
// Server-pushed per-window state.
// ---------------------------------------------------------------------------

/// Packet-decoded per-window state for the typed windows — one table
/// keyed by protocol window id rather than per-handle fields, so the
/// data is readable by every clone of a [`Window`] and dies with the
/// window (cleared on close/replace/login, like upstream removing its
/// packet listeners on `close`).
#[derive(Default)]
pub(crate) struct WindowDataTracker {
    /// One record per open window id; a record is created on first touch
    /// (packets can race ahead of `open_screen`) and dies whole.
    windows: HashMap<i32, WindowRecord>,
}

/// Everything server-pushed for one window id — kind, data properties,
/// merchant offers, the selected trade, and the readiness latch. One
/// record per id means one lifecycle (create, replace on a fresh
/// `open_screen`, remove on close/replace/login) instead of five tables
/// kept in lockstep.
#[derive(Default)]
struct WindowRecord {
    /// `open_screen` menu kind — routes `container_set_data` into the
    /// right typed event. `None` until the screen packet arrives.
    kind: Option<MenuKind>,
    /// `container_set_data` values (property → value, already
    /// sign-extended from the wire's u16).
    data: HashMap<u16, i16>,
    /// Decoded `merchant_offers` — `Some` once the packet arrived
    /// (possibly empty), which is what `open_villager` waits on.
    offers: Option<Vec<VillagerTrade>>,
    /// The trade index `trade`/`Villager::trade` last selected
    /// (upstream `villager.selectedTrade`).
    selected_trade: Option<usize>,
    /// Readiness latch — `ready` events fire on the false→true edge only,
    /// like upstream's `if (!ready) { ready = true; emit('ready') }`.
    ready: bool,
}

impl WindowDataTracker {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Everything stored for a closed or displaced window dies with it.
    fn clear_window(&mut self, id: i32) {
        self.windows.remove(&id);
    }

    /// Read one `container_set_data` value for a window.
    fn data(&self, window_id: i32, property: u16) -> Option<i16> {
        self.windows.get(&window_id)?.data.get(&property).copied()
    }
}

/// Decode one azalea [`Event`] into typed-window state + [`BotEvent`]s.
/// Needs no ECS access: everything it writes comes straight off the
/// packets. Emitted events run through the same bridge arm as
/// `windows::observe_event`'s.
pub(crate) fn observe_event(
    tracker: &Arc<Mutex<WindowDataTracker>>,
    event: &Event,
) -> Vec<BotEvent> {
    let Event::Packet(packet) = event else {
        return Vec::new();
    };
    match &**packet {
        ClientboundGamePacket::OpenScreen(p) => {
            // a fresh screen under a reused id starts clean — upstream's
            // per-window listeners attach to the new window object.
            let mut t = tracker.lock();
            t.clear_window(p.container_id);
            t.windows.entry(p.container_id).or_default().kind = Some(p.menu_type);
            Vec::new()
        }
        ClientboundGamePacket::ContainerSetData(p) => {
            let mut t = tracker.lock();
            // the wire field is a signed short read as u16 — recover the
            // sign so `-1` enchant clues compare correctly
            let value = p.value as i16;
            let record = t.windows.entry(p.container_id).or_default();
            record.data.insert(p.id, value);
            let mut out = vec![BotEvent::WindowDataUpdate {
                window_id: p.container_id,
                property: p.id,
                value,
            }];
            match record.kind {
                Some(MenuKind::Furnace | MenuKind::BlastFurnace | MenuKind::Smoker) => {
                    out.push(BotEvent::FurnaceUpdate {
                        window_id: p.container_id,
                    });
                }
                Some(MenuKind::Enchantment) => {
                    // 'ready' = all three level costs known and >= 0
                    // (upstream resets to not-ready if any goes -1)
                    let ready = (0..3).all(|i| record.data.get(&i).is_some_and(|v| *v >= 0));
                    if ready && !record.ready {
                        record.ready = true;
                        out.push(BotEvent::EnchantmentTableReady {
                            window_id: p.container_id,
                        });
                    } else if !ready {
                        record.ready = false;
                    }
                }
                _ => {}
            }
            out
        }
        ClientboundGamePacket::MerchantOffers(p) => {
            let mut t = tracker.lock();
            let record = t.windows.entry(p.container_id).or_default();
            record.offers = Some(decode_offers(&p.offers));
            if !record.ready {
                record.ready = true;
                vec![BotEvent::VillagerReady {
                    window_id: p.container_id,
                }]
            } else {
                Vec::new()
            }
        }
        ClientboundGamePacket::ContainerClose(p) => {
            tracker.lock().clear_window(p.container_id);
            Vec::new()
        }
        ClientboundGamePacket::Login(_) | ClientboundGamePacket::Respawn(_) => {
            tracker.lock().windows.clear();
            Vec::new()
        }
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Merchant offers — `villager.trades`.
// ---------------------------------------------------------------------------

/// One entry of `villager.trades` — upstream's decoded `trade_list`
/// element (`inputItem1`/`inputItem2`/`outputItem`/`tradeDisabled`/
/// `nbTradeUses`/`maximumNbTradeUses`/`xp`/`specialPrice`/
/// `priceMultiplier`/`demand`/`realPrice`).
#[derive(Clone, Debug)]
pub struct VillagerTrade {
    /// `inputItem1` — the first payment stack ([`Item::slot`] is -1; it
    /// doesn't live in a window slot).
    pub input_item_1: Item,
    /// `inputItem2` — the optional second payment (`hasItem2`).
    pub input_item_2: Option<Item>,
    /// `outputItem` — what the trade sells.
    pub output_item: Item,
    /// `tradeDisabled` — out of stock (or exhausted by `trade`).
    pub disabled: bool,
    /// `nbTradeUses` — times this offer has been used.
    pub uses: i32,
    /// `maximumNbTradeUses` — restock threshold.
    pub max_uses: i32,
    /// `xp` — villager xp the trade grants.
    pub xp: i32,
    /// `specialPrice` — reputation/discount adjustment to the price.
    pub special_price: i32,
    /// `priceMultiplier` — demand scaling factor.
    pub price_multiplier: f32,
    /// `demand` — resupply demand counter.
    pub demand: i32,
    /// `realPrice` — the stack count the server actually charges for
    /// `input_item_1`, per upstream's formula.
    pub real_price: i32,
}

/// Decode the wire offer list into [`VillagerTrade`]s (upstream
/// `gotTrades`'s mapping). `real_price` follows villager.js:
/// `min(max(count + specialPrice + max(0, floor(count·demand·priceMultiplier)), 1), stackSize)`.
fn decode_offers(offers: &[MerchantOffer]) -> Vec<VillagerTrade> {
    offers
        .iter()
        .map(|offer| {
            let input_1 = Item {
                stack: azalea::inventory::ItemStack::Present(
                    offer.base_cost_a.clone().into_item_stack(),
                ),
                slot: -1,
            };
            let input_2 = offer.cost_b.clone().map(|cost| Item {
                stack: azalea::inventory::ItemStack::Present(cost.into_item_stack()),
                slot: -1,
            });
            let output = Item {
                stack: offer.result.clone(),
                slot: -1,
            };
            let demand_diff =
                (input_1.count() as f32 * offer.demand as f32 * offer.price_multiplier)
                    .floor()
                    .max(0.) as i32;
            let real_price = (input_1.count() + offer.special_price_diff + demand_diff)
                .clamp(1, input_1.stack_size());
            VillagerTrade {
                input_item_1: input_1,
                input_item_2: input_2,
                output_item: output,
                disabled: offer.out_of_stock,
                uses: offer.uses,
                max_uses: offer.max_uses,
                xp: offer.xp,
                special_price: offer.special_price_diff,
                price_multiplier: offer.price_multiplier,
                demand: offer.demand,
                real_price,
            }
        })
        .collect()
}

/// Stacks of `kind` (with `metadata` when given) in the window's player
/// region — upstream `window.count(itemType, metadata)` over
/// `inventoryItems()`.
fn count_inventory_items(window: &Window, kind: ItemKind, metadata: Option<i32>) -> i32 {
    window
        .inventory_items()
        .iter()
        .filter(|i| i.kind() == kind && metadata.is_none_or(|m| i.metadata() == Some(m)))
        .map(|i| i.count())
        .sum()
}

// ---------------------------------------------------------------------------
// Furnace.
// ---------------------------------------------------------------------------

/// `bot.openFurnace` result — upstream's furnace window with fuel /
/// progress / take-put helpers. [`Deref`]s to the [`Window`].
#[derive(Clone)]
pub struct Furnace {
    window: Window,
    bot: Bot,
}

impl Deref for Furnace {
    type Target = Window;
    fn deref(&self) -> &Window {
        &self.window
    }
}

/// Fuel/progress ratios from the four furnace data slots, split out so
/// the math is unit-testable (`0` when the totals aren't known yet —
/// upstream's `if (furnace.totalFuel)` guard).
fn furnace_ratios(data: &HashMap<u16, i16>) -> (f32, f32, f32, f32) {
    let get = |id: u16| data.get(&id).copied().unwrap_or(0) as f32;
    let (left, total) = (get(FUEL_LEFT), get(FUEL_TOTAL));
    let (done, cook_total) = (get(COOK_DONE), get(COOK_TOTAL));
    let fuel = if total > 0. { left / total } else { 0. };
    let progress = if cook_total > 0. {
        done / cook_total
    } else {
        0.
    };
    (
        fuel,
        fuel * total * 0.05,
        progress,
        (cook_total - done) * 0.05,
    )
}

impl Furnace {
    /// `furnace.fuel` — fraction of the current fuel left (`0..1`).
    pub fn fuel(&self) -> f32 {
        self.data_map().map(|d| furnace_ratios(&d).0).unwrap_or(0.)
    }

    /// `furnace.fuelSeconds` — `fuel` rescaled to seconds of burn left.
    pub fn fuel_seconds(&self) -> f32 {
        self.data_map().map(|d| furnace_ratios(&d).1).unwrap_or(0.)
    }

    /// `furnace.progress` — fraction of the current smelt done (`0..1`).
    pub fn progress(&self) -> f32 {
        self.data_map().map(|d| furnace_ratios(&d).2).unwrap_or(0.)
    }

    /// `furnace.progressSeconds` — smelt seconds remaining.
    pub fn progress_seconds(&self) -> f32 {
        self.data_map().map(|d| furnace_ratios(&d).3).unwrap_or(0.)
    }

    /// `furnace.totalFuel` — ticks the current fuel item burns for, as
    /// the server last reported (`None` until the data slot arrives,
    /// like upstream's `null`).
    pub fn total_fuel(&self) -> Option<i32> {
        self.data(FUEL_TOTAL).map(i32::from)
    }

    /// `furnace.totalFuelSeconds` — `totalFuel` in seconds.
    pub fn total_fuel_seconds(&self) -> Option<f32> {
        self.total_fuel().map(|t| t as f32 * 0.05)
    }

    /// `furnace.totalProgress` — ticks one smelt takes, as the server
    /// last reported (`None` until the data slot arrives).
    pub fn total_progress(&self) -> Option<i32> {
        self.data(COOK_TOTAL).map(i32::from)
    }

    /// `furnace.totalProgressSeconds` — `totalProgress` in seconds.
    pub fn total_progress_seconds(&self) -> Option<f32> {
        self.total_progress().map(|t| t as f32 * 0.05)
    }

    fn data(&self, property: u16) -> Option<i16> {
        self.bot
            .shared
            .window_data
            .lock()
            .data(self.window.id(), property)
    }

    fn data_map(&self) -> Option<HashMap<u16, i16>> {
        self.bot
            .shared
            .window_data
            .lock()
            .windows
            .get(&self.window.id())
            .map(|record| record.data.clone())
    }

    /// `furnace.inputItem()` — slot 0.
    pub fn input_item(&self) -> Option<Item> {
        self.window.slot(0)
    }

    /// `furnace.fuelItem()` — slot 1.
    pub fn fuel_item(&self) -> Option<Item> {
        self.window.slot(1)
    }

    /// `furnace.outputItem()` — slot 2.
    pub fn output_item(&self) -> Option<Item> {
        self.window.slot(2)
    }

    async fn take(&self, slot: u16) -> Result<Item> {
        // upstream `assert.ok(item)` then `putAway(item.slot)`
        let item = self
            .window
            .slot(i32::from(slot))
            .ok_or_else(|| Error::NotFound(format!("no item in furnace slot {slot}")))?;
        self.bot.put_away(slot).await?;
        Ok(item)
    }

    /// `furnace.takeInput()` — move the input stack to the inventory.
    pub async fn take_input(&self) -> Result<Item> {
        self.take(0).await
    }

    /// `furnace.takeFuel()` — move the fuel stack to the inventory.
    pub async fn take_fuel(&self) -> Result<Item> {
        self.take(1).await
    }

    /// `furnace.takeOutput()` — move the output stack to the inventory.
    pub async fn take_output(&self) -> Result<Item> {
        self.take(2).await
    }

    async fn put(
        &self,
        slot: i32,
        kind: ItemKind,
        metadata: Option<i32>,
        count: Option<i32>,
    ) -> Result<()> {
        let player = self.window.player_range();
        transfer(TransferOpts {
            window: Some(self.window.clone()),
            kind,
            metadata,
            nbt: None,
            count,
            source_start: player.start as i32,
            source_end: Some(player.end as i32),
            dest_start: slot,
            dest_end: Some(slot + 1),
        })
        .await
    }

    /// `furnace.putInput(itemType, metadata, count)` — `count: None` moves
    /// one item (upstream `count ?? 1`).
    pub async fn put_input(
        &self,
        kind: ItemKind,
        metadata: Option<i32>,
        count: Option<i32>,
    ) -> Result<()> {
        self.put(0, kind, metadata, count).await
    }

    /// `furnace.putFuel(itemType, metadata, count)`.
    pub async fn put_fuel(
        &self,
        kind: ItemKind,
        metadata: Option<i32>,
        count: Option<i32>,
    ) -> Result<()> {
        self.put(1, kind, metadata, count).await
    }
}

// ---------------------------------------------------------------------------
// Enchantment table.
// ---------------------------------------------------------------------------

/// `enchantmentTable.enchantments[i]` — the server's hint for one of the
/// three enchant buttons. `level == -1` until the server reports it.
#[derive(Clone, Debug)]
pub struct EnchantmentOption {
    /// xp-level cost shown on the button; `-1` while unknown.
    pub level: i32,
    /// The hinted outcome, when the server sent one (clue id + level).
    pub expected: Option<ExpectedEnchant>,
}

/// The enchantment the table hints a button will apply.
#[derive(Clone, Debug, PartialEq)]
pub struct ExpectedEnchant {
    /// The hinted enchantment — e.g. `EnchantmentKey::Sharpness`
    /// (resolved through the server's dynamic registry, so datapack
    /// enchantments surface as `Other`).
    pub enchant: data::EnchantmentKey,
    /// The hinted enchantment level.
    pub level: i32,
}

/// `bot.openEnchantmentTable` result. [`Deref`]s to the [`Window`].
#[derive(Clone)]
pub struct EnchantmentTable {
    window: Window,
    bot: Bot,
}

impl Deref for EnchantmentTable {
    type Target = Window;
    fn deref(&self) -> &Window {
        &self.window
    }
}

/// Decode the three enchant options out of the raw data map.
/// `enchant_clue`/`level_clue` are `-1` for "no hint" (the sign-extended
/// wire value); the id resolves through the server's dynamic registry.
fn enchantment_options(
    data: &HashMap<u16, i16>,
    registries: &azalea::core::registry_holder::RegistryHolder,
) -> [EnchantmentOption; 3] {
    let get = |id: u16| data.get(&id).copied().unwrap_or(-1) as i32;
    std::array::from_fn(|i| {
        let enchant_id = get(ENCHANT_CLUE + i as u16);
        let level = get(ENCHANT_LEVEL_CLUE + i as u16);
        let expected = if enchant_id >= 0 && level > 0 {
            data::Enchantment::new_raw(enchant_id as u32)
                .key_owned(registries)
                .map(|enchant| ExpectedEnchant { enchant, level })
        } else {
            None
        };
        EnchantmentOption {
            level: get(i as u16),
            expected,
        }
    })
}

impl EnchantmentTable {
    /// `enchantmentTable.enchantments` — the three button options, `-1`s
    /// for fields the server hasn't sent yet.
    pub fn enchantments(&self) -> [EnchantmentOption; 3] {
        let data = self
            .bot
            .shared
            .window_data
            .lock()
            .windows
            .get(&self.window.id())
            .map(|record| record.data.clone())
            .unwrap_or_default();
        self.bot
            .client
            .with_registry_holder(|r| enchantment_options(&data, r))
    }

    /// `enchantmentTable.xpseed` — `-1` until the server sends it.
    pub fn xp_seed(&self) -> i32 {
        self.bot
            .shared
            .window_data
            .lock()
            .data(self.window.id(), ENCHANT_XP_SEED)
            .unwrap_or(-1) as i32
    }

    /// `enchantmentTable.targetItem()` — the item in slot 0.
    pub fn target_item(&self) -> Option<Item> {
        self.window.slot(0)
    }

    /// `enchantmentTable.enchant(choice)` — wait for `ready`, send the
    /// `container_button_click` for `choice` (0..=2), and resolve with
    /// the item now in the target slot (upstream returns `updateSlot:0`'s
    /// new item).
    pub async fn enchant(&self, choice: u8) -> Result<Item> {
        if choice > 2 {
            return Err(Error::InvalidArgument(format!(
                "enchantment choice must be 0..=2, got {choice}"
            )));
        }
        let mut rx = self.bot.subscribe();
        let id = self.window.id();
        if !self.is_ready() {
            await_event(
                &mut rx,
                WINDOW_WAIT,
                |e| matches!(e, BotEvent::EnchantmentTableReady { window_id } if *window_id == id),
            )
            .await
            .ok_or(Error::Timeout)?;
        }
        if self.enchantments()[usize::from(choice)].level == -1 {
            return Err(Error::InvalidState(format!(
                "enchantment choice {choice} is not available"
            )));
        }
        self.bot
            .client
            .write_packet(ServerboundContainerButtonClick {
                container_id: id,
                button_id: u32::from(choice),
            });
        let event = await_event(&mut rx, WINDOW_WAIT, |e| {
            matches!(e, BotEvent::WindowUpdateSlot { window_id, slot: 0, .. } if *window_id == id)
        })
        .await
        .ok_or(Error::Timeout)?;
        let BotEvent::WindowUpdateSlot { new, .. } = event else {
            unreachable!()
        };
        new.ok_or_else(|| Error::Failed("enchant produced no item".into()))
    }

    /// Whether the three level costs have all arrived (the `ready`
    /// condition — becomes false again if a cost goes back to `-1`).
    pub fn is_ready(&self) -> bool {
        self.bot
            .shared
            .window_data
            .lock()
            .windows
            .get(&self.window.id())
            .is_some_and(|record| (0..3).all(|i| record.data.get(&i).is_some_and(|v| *v >= 0)))
    }

    /// `enchantmentTable.takeTargetItem()` — move the target item back
    /// to the inventory.
    pub async fn take_target_item(&self) -> Result<Item> {
        let item = self
            .target_item()
            .ok_or_else(|| Error::NotFound("no item in enchantment target slot".into()))?;
        self.bot.put_away(0).await?;
        Ok(item)
    }

    /// `enchantmentTable.putTargetItem(item)` — move `item` into slot 0.
    pub async fn put_target_item(&self, item: &Item) -> Result<()> {
        self.bot.move_slot_item(i32::from(item.slot), 0).await
    }

    /// `enchantmentTable.putLapis(item)` — move `item` into slot 1.
    pub async fn put_lapis(&self, item: &Item) -> Result<()> {
        self.bot.move_slot_item(i32::from(item.slot), 1).await
    }
}

// ---------------------------------------------------------------------------
// Anvil.
// ---------------------------------------------------------------------------

/// `bot.openAnvil` result — combine/rename over the anvil window.
/// [`Deref`]s to the [`Window`].
#[derive(Clone)]
pub struct Anvil {
    window: Window,
    bot: Bot,
}

impl Deref for Anvil {
    type Target = Window;
    fn deref(&self) -> &Window {
        &self.window
    }
}

/// Upstream `err('Name is too long.')` guard — names over 35 chars are
/// rejected before any clicking.
fn check_anvil_name(name: &str) -> Result<()> {
    if name.chars().count() > 35 {
        return Err(Error::InvalidArgument("Name is too long.".into()));
    }
    Ok(())
}

impl Anvil {
    /// The server's repair/rename level cost (`container_set_data` 0) —
    /// `None` until the first report. The upstream `Item.anvil` cost
    /// preview has no azalea equivalent, so the checks below read this.
    pub fn repair_cost(&self) -> Option<i32> {
        self.bot
            .shared
            .window_data
            .lock()
            .data(self.window.id(), ANVIL_COST)
            .map(|v| v as i32)
    }

    /// `anvil.combine(itemOne, itemTwo, name)` — place both items in the
    /// anvil, optionally rename, take the result, and let the xp update
    /// land. Tries the swapped order when the given order produces no
    /// affordable result (upstream picks the cheaper of the two orders
    /// client-side; we can only compare server-reported costs, so the
    /// given order wins when it already works — see api-parity).
    pub async fn combine(
        &self,
        item_one: &Item,
        item_two: &Item,
        name: Option<&str>,
    ) -> Result<()> {
        if let Some(name) = name {
            check_anvil_name(name).inspect_err(|_| self.window.close())?;
        }
        // upstream `assert.ok(itemOne && itemTwo)`
        if item_one.stack.is_empty() || item_two.stack.is_empty() {
            self.window.close();
            return Err(Error::InvalidArgument("Not anvil-able (empty item)".into()));
        }
        let creative = self.bot.game_mode() == Some(GameMode::Creative);
        let mut rx = self.bot.subscribe();
        self.put_in_anvil(item_one, item_two).await?;
        // the name goes in before the check — upstream's client-side
        // cost computation includes it, and ours must read the server's
        // post-rename cost the same way (the text persists across an
        // input swap, so no resend is needed)
        self.send_item_name(name.unwrap_or("")).await;

        let first_ok = self.wait_and_check(creative, &mut rx).await;
        if !first_ok && self.swap_inputs().await.is_ok() {
            // the reverse direction can work (or be cheaper) when the
            // given one doesn't
            let second_ok = self.wait_and_check(creative, &mut rx).await;
            if !second_ok {
                self.window.close();
                return Err(self.anvil_fail());
            }
        } else if !first_ok {
            self.window.close();
            return Err(self.anvil_fail());
        }

        self.take_result(creative, &mut rx).await
    }

    /// `anvil.rename(item, name)` — rename a single item via the anvil.
    pub async fn rename(&self, item: &Item, name: &str) -> Result<()> {
        check_anvil_name(name).inspect_err(|_| self.window.close())?;
        if item.stack.is_empty() {
            self.window.close();
            return Err(Error::InvalidArgument(
                "Not valid rename (empty item)".into(),
            ));
        }
        let creative = self.bot.game_mode() == Some(GameMode::Creative);
        let mut rx = self.bot.subscribe();
        let player = self.window.player_range();
        transfer(TransferOpts {
            window: Some(self.window.clone()),
            kind: item.kind(),
            metadata: item.metadata(),
            nbt: Some(item.component_patch().clone()),
            count: Some(item.count()),
            source_start: player.start as i32,
            source_end: Some(player.end as i32),
            dest_start: 0,
            dest_end: Some(1),
        })
        .await?;
        // vanilla sends the empty name after the first input, twice on
        // the modern `name_item` packet (upstream `sendItemName('')` x2)
        self.bot.client.write_packet(ServerboundRenameItem {
            name: String::new(),
        });
        self.bot.client.write_packet(ServerboundRenameItem {
            name: String::new(),
        });
        self.send_item_name(name).await;

        if !self.wait_and_check(creative, &mut rx).await {
            self.window.close();
            return Err(self.anvil_fail());
        }
        self.take_result(creative, &mut rx).await
    }

    /// Upstream `putInAnvil` — first input, the two empty `name_item`
    /// packets vanilla sends, then the second input.
    async fn put_in_anvil(&self, item_one: &Item, item_two: &Item) -> Result<()> {
        self.put_something(0, item_one).await?;
        self.bot.client.write_packet(ServerboundRenameItem {
            name: String::new(),
        });
        self.bot.client.write_packet(ServerboundRenameItem {
            name: String::new(),
        });
        self.put_something(1, item_two).await
    }

    /// Upstream `putSomething` — transfer a matching stack from the
    /// player region into an anvil input slot.
    async fn put_something(&self, dest: i32, item: &Item) -> Result<()> {
        let player = self.window.player_range();
        transfer(TransferOpts {
            window: Some(self.window.clone()),
            kind: item.kind(),
            metadata: item.metadata(),
            nbt: Some(item.component_patch().clone()),
            count: Some(item.count()),
            source_start: player.start as i32,
            source_end: Some(player.end as i32),
            dest_start: dest,
            dest_end: Some(dest + 1),
        })
        .await
    }

    /// Swap the two anvil inputs in place (slot 0 ↔ 1 via the
    /// pick-up/swap/put-back choreography of `moveSlotItem`).
    async fn swap_inputs(&self) -> Result<()> {
        self.bot.move_slot_item(0, 1).await
    }

    /// Wait for the server to resync the result slot or cost after an
    /// input change, then report whether the current inputs produce an
    /// affordable result. The cost packet only goes out when the value
    /// changes, so the wait is bounded and the latest stored value is
    /// read afterwards.
    async fn wait_and_check(&self, creative: bool, rx: &mut broadcast::Receiver<BotEvent>) -> bool {
        let id = self.window.id();
        await_event(rx, ANVIL_COST_WAIT, |e| {
            matches!(
                e,
                BotEvent::WindowDataUpdate { window_id, property: ANVIL_COST, .. }
                    | BotEvent::WindowUpdateSlot { window_id, slot: 2, .. }
                    if *window_id == id
            )
        })
        .await;
        self.window.slot(2).is_some()
            && (creative
                || self
                    .repair_cost()
                    .is_some_and(|c| self.bot.experience().level >= c as u32))
    }

    /// The failure to report when the result never becomes affordable —
    /// `Not anvil-able` when the server never produced a result at all,
    /// else the xp complaint (upstream's two `err()` sites).
    fn anvil_fail(&self) -> Error {
        if self.window.slot(2).is_some() {
            Error::InvalidState("Player does not have enough xp to do action, cancelling.".into())
        } else {
            Error::InvalidArgument("Not anvil-able (in either direction), cancelling.".into())
        }
    }

    /// Upstream `addCustomName` — the packet is sent for each name
    /// prefix like vanilla typing, 50 ms apart.
    async fn send_item_name(&self, name: &str) {
        if name.is_empty() {
            return;
        }
        for i in 1..=name.chars().count() {
            let prefix: String = name.chars().take(i).collect();
            self.bot
                .client
                .write_packet(ServerboundRenameItem { name: prefix });
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// `putAway(2)` the result into the inventory, then wait for the xp
    /// deduction upstream waits on (`once(bot, 'experience')`, skipped
    /// in creative).
    async fn take_result(
        &self,
        creative: bool,
        rx: &mut broadcast::Receiver<BotEvent>,
    ) -> Result<()> {
        self.bot.put_away(TRADE_RESULT).await?;
        if !creative {
            await_event(rx, WINDOW_WAIT, |e| matches!(e, BotEvent::Experience)).await;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Villager.
// ---------------------------------------------------------------------------

/// `bot.openVillager` result — the merchant window plus `trades`.
/// [`Deref`]s to the [`Window`].
#[derive(Clone)]
pub struct Villager {
    window: Window,
    bot: Bot,
}

impl Deref for Villager {
    type Target = Window;
    fn deref(&self) -> &Window {
        &self.window
    }
}

impl Villager {
    /// `villager.trades` — the offers from the latest `merchant_offers`
    /// packet (empty until it arrives, which `open_villager` waits on).
    pub fn trades(&self) -> Vec<VillagerTrade> {
        self.bot
            .shared
            .window_data
            .lock()
            .windows
            .get(&self.window.id())
            .and_then(|record| record.offers.clone())
            .unwrap_or_default()
    }

    /// `villager.selectedTrade` — the offer `trade` last selected.
    pub fn selected_trade(&self) -> Option<VillagerTrade> {
        let t = self.bot.shared.window_data.lock();
        let record = t.windows.get(&self.window.id())?;
        record.offers.as_ref()?.get(record.selected_trade?).cloned()
    }

    /// `villager.trade(index, count)` — same as `bot.trade(villager, …)`.
    pub async fn trade(&self, index: usize, count: Option<i32>) -> Result<()> {
        self.bot.trade(self, index, count).await
    }
}

// ---------------------------------------------------------------------------
// Command block.
// ---------------------------------------------------------------------------

/// `options.mode` for [`Bot::set_command_block`] — the vanilla modes:
/// `Sequence` (chain, 0), `Auto` (repeating, 1), `Redstone` (impulse, 2,
/// the upstream default).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CommandBlockMode {
    /// Chain command block (upstream mode 0).
    Sequence,
    /// Repeating command block (upstream mode 1).
    Auto,
    /// Impulse command block (upstream mode 2, the default).
    #[default]
    Redstone,
}

/// `options` for [`Bot::set_command_block`] — defaults match a freshly
/// placed vanilla command block (upstream's `?? false` / `?? 2`).
#[derive(Clone, Debug, Default)]
pub struct CommandBlockOpts {
    /// `trackOutput` — keep the last output in the block's GUI.
    pub track_output: bool,
    /// `conditional` — run only when the block behind it succeeded.
    pub conditional: bool,
    /// `alwaysActive` — run without a redstone pulse.
    pub always_active: bool,
    /// `mode` — see [`CommandBlockMode`]; default `Redstone` (impulse).
    pub mode: CommandBlockMode,
}

// ---------------------------------------------------------------------------
// Bot methods — the openers and the book/command-block ops.
// ---------------------------------------------------------------------------

impl Bot {
    /// `bot.openFurnace(block)` — right-click the block and verify the
    /// opened window is a furnace kind (furnace/blast_furnace/smoker —
    /// upstream `allowedWindowTypes`). On a mismatch the window is
    /// closed before erroring (upstream leaves it open — we close like
    /// [`Bot::open_container`] does).
    pub async fn open_furnace(&self, block: &Block) -> Result<Furnace> {
        let window = self.open_block(block).await?;
        match window.menu_kind() {
            Some(MenuKind::Furnace | MenuKind::BlastFurnace | MenuKind::Smoker) => Ok(Furnace {
                window,
                bot: self.clone(),
            }),
            other => {
                window.close();
                Err(Error::InvalidArgument(format!(
                    "This is not a furnace-like window: {other:?}"
                )))
            }
        }
    }

    /// `bot.openEnchantmentTable(block)` — upstream asserts the block is
    /// `enchanting_table` before opening, then verifies the menu kind.
    pub async fn open_enchantment_table(&self, block: &Block) -> Result<EnchantmentTable> {
        if block.kind() != BlockKind::EnchantingTable {
            return Err(Error::InvalidArgument(format!(
                "Expected enchanting_table block, got {}",
                block.name()
            )));
        }
        let window = self.open_block(block).await?;
        match window.menu_kind() {
            Some(MenuKind::Enchantment) => Ok(EnchantmentTable {
                window,
                bot: self.clone(),
            }),
            other => {
                window.close();
                Err(Error::InvalidArgument(format!(
                    "Expected minecraft:enchant when opening table but got {other:?}"
                )))
            }
        }
    }

    /// `bot.openAnvil(block)` — right-click the block and verify the
    /// `anvil` menu kind (covers damaged/chipped anvils — they all map
    /// to the same menu).
    pub async fn open_anvil(&self, block: &Block) -> Result<Anvil> {
        let window = self.open_block(block).await?;
        match window.menu_kind() {
            Some(MenuKind::Anvil) => Ok(Anvil {
                window,
                bot: self.clone(),
            }),
            other => {
                window.close();
                Err(Error::InvalidArgument(format!(
                    "Not a anvil-like window: {other:?}"
                )))
            }
        }
    }

    /// `bot.openVillager(entity)` — right-click a villager entity,
    /// verify the `merchant` menu kind (upstream accepts `villager` or
    /// `merchant`), and wait for `merchant_offers` to arrive (upstream's
    /// `ready` gate).
    pub async fn open_villager(&self, entity: &EntityRef) -> Result<Villager> {
        if entity.get_component::<EntityKindComponent>().map(|k| k.0) != Some(EntityKind::Villager)
        {
            return Err(Error::InvalidArgument(
                "Expected villager entity type".into(),
            ));
        }
        let mut rx = self.subscribe();
        let window = self.open_entity(entity).await?;
        let id = window.id();
        match window.menu_kind() {
            Some(MenuKind::Merchant) => {}
            other => {
                window.close();
                return Err(Error::InvalidArgument(format!(
                    "Expected minecraft:villager or minecraft:mechant type, but got {other:?}"
                )));
            }
        }
        // offers can race ahead of the open — check before waiting
        let has_offers = self
            .shared
            .window_data
            .lock()
            .windows
            .get(&id)
            .is_some_and(|record| record.offers.is_some());
        if !has_offers {
            await_event(
                &mut rx,
                WINDOW_WAIT,
                |e| matches!(e, BotEvent::VillagerReady { window_id } if *window_id == id),
            )
            .await;
        }
        if self
            .shared
            .window_data
            .lock()
            .windows
            .get(&id)
            .is_some_and(|record| record.offers.is_some())
        {
            Ok(Villager {
                window,
                bot: self.clone(),
            })
        } else {
            window.close();
            Err(Error::Timeout)
        }
    }

    /// `bot.trade(villager, index, count)` — select the offer, let the
    /// server stock the trade slots, top up the inputs from the player
    /// region, and shift the results out `count` times (upstream
    /// `villager.js`'s `trade`; `count: None` = all remaining uses).
    pub async fn trade(&self, villager: &Villager, index: usize, count: Option<i32>) -> Result<()> {
        let window = &villager.window;
        let id = window.id();
        let mut rx = self.subscribe();

        let (trade, count) = {
            let mut t = self.shared.window_data.lock();
            let record = t
                .windows
                .get_mut(&id)
                .ok_or_else(|| Error::NotFound("villager has no trades yet".into()))?;
            let offers = record
                .offers
                .as_mut()
                .ok_or_else(|| Error::NotFound("villager has no trades yet".into()))?;
            let trade = offers
                .get(index)
                .cloned()
                .ok_or_else(|| Error::InvalidArgument(format!("no trade at index {index}")))?;
            let remaining = trade.max_uses - trade.uses;
            if remaining <= 0 {
                return Err(Error::InvalidState("trade blocked".into()));
            }
            let count = count.unwrap_or(remaining);
            if count > remaining {
                return Err(Error::InvalidArgument(format!(
                    "cannot trade {count} times, only {remaining} uses left"
                )));
            }
            record.selected_trade = Some(index);
            (trade, count)
        };

        let have_1 = count_inventory_items(
            window,
            trade.input_item_1.kind(),
            trade.input_item_1.metadata(),
        );
        if have_1 < trade.real_price * count {
            return Err(Error::NotFound("Not enough item 1 to trade".into()));
        }
        if let Some(input_2) = &trade.input_item_2 {
            let have_2 = count_inventory_items(window, input_2.kind(), input_2.metadata());
            if have_2 < input_2.count() * count {
                return Err(Error::NotFound("Not enough item 2 to trade".into()));
            }
        }

        self.client
            .write_packet(ServerboundSelectTrade { item: index as u32 });
        // 1.14+ (`selectingTradeMovesItems`): the server stocks the trade
        // slots itself — wait for all three echoes before topping up.
        let mut needed = vec![TRADE_INPUT_1, TRADE_RESULT];
        if trade.input_item_2.is_some() {
            needed.push(TRADE_INPUT_2);
        }
        wait_for_slot_updates(&mut rx, id, &needed, WINDOW_WAIT).await;

        for _ in 0..count {
            self.put_requirements(&villager.window, &trade).await?;
            // upstream updates the local offer copy after each trade
            {
                let mut t = self.shared.window_data.lock();
                if let Some(t) = t
                    .windows
                    .get_mut(&id)
                    .and_then(|record| record.offers.as_mut())
                    .and_then(|offers| offers.get_mut(index))
                {
                    t.uses += 1;
                    if t.max_uses - t.uses == 0 {
                        t.disabled = true;
                    }
                }
            }
            self.put_away(TRADE_RESULT).await?;
        }
        // leftover input stacks go back to the inventory
        for slot in [TRADE_INPUT_1, TRADE_INPUT_2] {
            if window.slot(i32::from(slot)).is_some() {
                self.put_away(slot).await?;
            }
        }
        Ok(())
    }

    /// Upstream `putRequirements` — deposit the missing counts into the
    /// two trade input slots (slot already partially stocked counts
    /// toward the price).
    async fn put_requirements(&self, window: &Window, trade: &VillagerTrade) -> Result<()> {
        let have_1 = window
            .slot(i32::from(TRADE_INPUT_1))
            .map(|i| i.count())
            .unwrap_or(0);
        let need_1 = trade.real_price - have_1;
        if need_1 > 0 {
            self.deposit_trade_item(
                window,
                trade.input_item_1.kind(),
                trade.input_item_1.metadata(),
                need_1,
                TRADE_INPUT_1,
            )
            .await?;
        }
        if let Some(input_2) = &trade.input_item_2 {
            let have_2 = window
                .slot(i32::from(TRADE_INPUT_2))
                .map(|i| i.count())
                .unwrap_or(0);
            let need_2 = input_2.count() - have_2;
            if need_2 > 0 {
                self.deposit_trade_item(
                    window,
                    input_2.kind(),
                    input_2.metadata(),
                    need_2,
                    TRADE_INPUT_2,
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Upstream `deposit` in villager.js — `transfer` into a specific
    /// trade slot from the player region.
    async fn deposit_trade_item(
        &self,
        window: &Window,
        kind: ItemKind,
        metadata: Option<i32>,
        count: i32,
        slot: u16,
    ) -> Result<()> {
        let player = window.player_range();
        transfer(TransferOpts {
            window: Some(window.clone()),
            kind,
            metadata,
            nbt: None,
            count: Some(count),
            source_start: player.start as i32,
            source_end: Some(player.end as i32),
            dest_start: i32::from(slot),
            dest_end: Some(i32::from(slot) + 1),
        })
        .await
    }

    /// `bot.writeBook(slot, pages)` — edit a writable book in place:
    /// moves it to the hotbar if it isn't there already (the packet only
    /// addresses a hotbar slot), sends `s_edit_book`, waits for the
    /// server to apply the `writable_book_content` component back onto
    /// the item, then restores the slot and quickbar selection.
    pub async fn write_book(&self, slot: i32, pages: Vec<String>) -> Result<()> {
        self.edit_book(slot, pages, None, None).await
    }

    /// `bot.signBook(slot, pages, author, title)` — same dance but sends
    /// the title so the server turns the item into a `written_book`.
    /// `author` is accepted for signature parity but unused — the server
    /// credits the signing player (upstream's `hasEditBookPacket` path
    /// doesn't send it either).
    pub async fn sign_book(
        &self,
        slot: i32,
        pages: Vec<String>,
        author: &str,
        title: &str,
    ) -> Result<()> {
        self.edit_book(slot, pages, Some(author), Some(title)).await
    }

    /// The shared `write` behind `writeBook`/`signBook`.
    async fn edit_book(
        &self,
        slot: i32,
        pages: Vec<String>,
        _author: Option<&str>,
        title: Option<&str>,
    ) -> Result<()> {
        check_book_input(slot, &pages, title)?;
        let inventory = self.inventory();
        let book = inventory
            .slot(slot)
            .ok_or_else(|| Error::NotFound(format!("no book found in slot {slot}")))?;
        if book.kind() != ItemKind::WritableBook {
            return Err(Error::NotFound(format!("no book found in slot {slot}")));
        }

        let mut rx = self.subscribe();
        let quick_bar_slot = self.quick_bar_slot();
        let move_to_quick_bar = slot < 36;
        if move_to_quick_bar {
            self.move_slot_item(slot, 36).await?;
            // 1.21.9+ never acknowledges an edit handled in the same
            // tick as the clicks that put the book in hand — wait for
            // the slot echoes like upstream's `_syncWindow`
            wait_for_slot_updates(&mut rx, 0, &[slot as u16, 36], WINDOW_WAIT).await;
        }
        self.set_quick_bar_slot(if move_to_quick_bar {
            0
        } else {
            (slot - 36) as u8
        })
        .await?;

        let book_slot = if move_to_quick_bar { 36 } else { slot };
        self.client.write_packet(ServerboundEditBook {
            slot: (book_slot - 36) as u32,
            pages,
            title: title.map(|t| t.to_string()),
        });

        // the edit lands as a slot update replacing the item — signing
        // produces a written_book, writing adds the pages component
        let signing = title.is_some();
        let acked = await_event(&mut rx, BOOK_ACK_TIMEOUT, |e| {
            matches!(e, BotEvent::WindowUpdateSlot { window_id: 0, slot: s, new: Some(item), .. }
            if *s as i32 == book_slot
                && if signing {
                    item.kind() == ItemKind::WrittenBook
                } else {
                    item.stack.get_component::<WritableBookContent>().is_some()
                })
        })
        .await
        .is_some();

        if acked {
            self.set_quick_bar_slot(quick_bar_slot).await?;
            if move_to_quick_bar {
                self.move_slot_item(36, slot).await?;
            }
            Ok(())
        } else {
            Err(Error::Timeout)
        }
    }

    /// `bot.setCommandBlock(pos, command, options)` — write the
    /// `s_set_command_block` packet. Requires creative mode (upstream
    /// asserts `gamemode == 1`) and a `*_command_block` at `pos`.
    pub fn set_command_block(
        &self,
        pos: BlockPos,
        command: &str,
        options: CommandBlockOpts,
    ) -> Result<()> {
        if self.game_mode() != Some(GameMode::Creative) {
            return Err(Error::InvalidState(
                "The bot has to be in creative mode to open the command block window".into(),
            ));
        }
        let block = self
            .block_at(pos)
            .ok_or_else(|| Error::InvalidArgument("The block isn't a command block".into()))?;
        if !block.name().contains("command_block") {
            return Err(Error::InvalidArgument(
                "The block isn't a command block".into(),
            ));
        }
        self.client.write_packet(ServerboundSetCommandBlock {
            pos,
            command: command.to_string(),
            mode: match options.mode {
                CommandBlockMode::Sequence => s_set_command_block::Mode::Sequence,
                CommandBlockMode::Auto => s_set_command_block::Mode::Auto,
                CommandBlockMode::Redstone => s_set_command_block::Mode::Redstone,
            },
            track_output: options.track_output,
            conditional: options.conditional,
            automatic: options.always_active,
        });
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Wait helpers.
// ---------------------------------------------------------------------------

/// Wait until `WindowUpdateSlot` has been seen for every slot in
/// `needed` (single consumer loop — sequential one-slot waits would eat
/// each other's events). Returns early on timeout or `End`, like
/// upstream's bounded `waitForWindowUpdate`.
async fn wait_for_slot_updates(
    rx: &mut broadcast::Receiver<BotEvent>,
    window_id: i32,
    needed: &[u16],
    timeout: Duration,
) {
    let mut remaining: HashSet<u16> = needed.iter().copied().collect();
    await_event(rx, timeout, |e| {
        if let BotEvent::WindowUpdateSlot {
            window_id: wid,
            slot,
            ..
        } = e
            && *wid == window_id
        {
            remaining.remove(slot);
            return remaining.is_empty();
        }
        false
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use azalea::{
        inventory::ItemStack,
        protocol::packets::game::{
            c_container_close::ClientboundContainerClose,
            c_container_set_data::ClientboundContainerSetData,
            c_merchant_offers::{DataComponentExactPredicate, ItemCost},
            c_open_screen::ClientboundOpenScreen,
        },
    };

    fn packet_event(p: ClientboundGamePacket) -> Event {
        Event::Packet(Arc::new(p))
    }

    fn open_screen(id: i32, kind: MenuKind) -> Event {
        packet_event(ClientboundGamePacket::OpenScreen(ClientboundOpenScreen {
            container_id: id,
            menu_type: kind,
            title: azalea::FormattedText::from("t"),
        }))
    }

    fn set_data(id: i32, property: u16, value: u16) -> Event {
        packet_event(ClientboundGamePacket::ContainerSetData(
            ClientboundContainerSetData {
                container_id: id,
                id: property,
                value,
            },
        ))
    }

    fn has_event(events: &[BotEvent], pred: impl Fn(&BotEvent) -> bool) -> bool {
        events.iter().any(pred)
    }

    #[test]
    fn set_data_emits_generic_and_furnace_events() {
        let tracker = Arc::new(Mutex::new(WindowDataTracker::new()));
        // a furnace-kind window must be registered for FurnaceUpdate
        assert!(observe_event(&tracker, &open_screen(3, MenuKind::Furnace)).is_empty());
        let out = observe_event(&tracker, &set_data(3, FUEL_LEFT, 40));
        assert!(has_event(&out, |e| matches!(
            e,
            BotEvent::WindowDataUpdate {
                window_id: 3,
                property: 0,
                value: 40
            }
        )));
        assert!(has_event(&out, |e| matches!(
            e,
            BotEvent::FurnaceUpdate { window_id: 3 }
        )));
        // a non-furnace kind only gets the generic event
        observe_event(&tracker, &open_screen(4, MenuKind::Anvil));
        let out = observe_event(&tracker, &set_data(4, ANVIL_COST, 9));
        assert!(has_event(&out, |e| matches!(
            e,
            BotEvent::WindowDataUpdate {
                window_id: 4,
                property: 0,
                value: 9
            }
        )));
        assert!(!has_event(&out, |e| matches!(
            e,
            BotEvent::FurnaceUpdate { .. }
        )));
        // the value is readable through the tracker
        assert_eq!(tracker.lock().data(4, ANVIL_COST), Some(9));
        // the totals (`furnace.totalFuel`/`totalProgress`) ride the same
        // retention — a total-only update leaves the current value alone
        observe_event(&tracker, &set_data(3, FUEL_TOTAL, 1600));
        observe_event(&tracker, &set_data(3, COOK_TOTAL, 200));
        let t = tracker.lock();
        assert_eq!(t.data(3, FUEL_TOTAL), Some(1600));
        assert_eq!(t.data(3, COOK_TOTAL), Some(200));
        assert_eq!(t.data(3, FUEL_LEFT), Some(40));
    }

    #[test]
    fn enchant_ready_is_an_edge_latch() {
        let tracker = Arc::new(Mutex::new(WindowDataTracker::new()));
        observe_event(&tracker, &open_screen(1, MenuKind::Enchantment));
        observe_event(&tracker, &set_data(1, 0, 3));
        observe_event(&tracker, &set_data(1, 1, 15));
        // third level cost lands → one ready event
        let out = observe_event(&tracker, &set_data(1, 2, 30));
        assert!(has_event(&out, |e| matches!(
            e,
            BotEvent::EnchantmentTableReady { window_id: 1 }
        )));
        // a later property update doesn't re-fire
        let out = observe_event(&tracker, &set_data(1, ENCHANT_XP_SEED, 1234));
        assert!(!has_event(&out, |e| matches!(
            e,
            BotEvent::EnchantmentTableReady { .. }
        )));
        // a level cost dropping to -1 (wire 0xFFFF) unlatches — the next
        // full set re-fires 'ready' like upstream
        observe_event(&tracker, &set_data(1, 2, 0xFFFF));
        let out = observe_event(&tracker, &set_data(1, 2, 22));
        assert!(has_event(&out, |e| matches!(
            e,
            BotEvent::EnchantmentTableReady { window_id: 1 }
        )));
        // close drops all state for the id
        observe_event(
            &tracker,
            &packet_event(ClientboundGamePacket::ContainerClose(
                ClientboundContainerClose { container_id: 1 },
            )),
        );
        assert!(!tracker.lock().windows.contains_key(&1));
    }

    fn offer(
        kind: ItemKind,
        count: i32,
        special_price: i32,
        multiplier: f32,
        demand: i32,
    ) -> MerchantOffer {
        MerchantOffer {
            base_cost_a: ItemCost {
                item: kind,
                count,
                components: DataComponentExactPredicate { expected: vec![] },
            },
            result: ItemStack::new(ItemKind::Diamond, 1),
            cost_b: None,
            out_of_stock: false,
            uses: 2,
            max_uses: 12,
            xp: 5,
            special_price_diff: special_price,
            price_multiplier: multiplier,
            demand,
        }
    }

    #[test]
    fn merchant_offer_decodes_upstream_fields() {
        let mut o = offer(ItemKind::Emerald, 5, -2, 0.05, 10);
        o.cost_b = Some(ItemCost {
            item: ItemKind::Book,
            count: 1,
            components: DataComponentExactPredicate { expected: vec![] },
        });
        let trades = decode_offers(&[o]);
        assert_eq!(trades.len(), 1);
        let t = &trades[0];
        assert_eq!(t.input_item_1.kind(), ItemKind::Emerald);
        assert_eq!(t.input_item_1.count(), 5);
        assert_eq!(t.input_item_2.as_ref().unwrap().kind(), ItemKind::Book);
        assert_eq!(t.output_item.kind(), ItemKind::Diamond);
        assert!(!t.disabled);
        assert_eq!(t.uses, 2);
        assert_eq!(t.max_uses, 12);
        assert_eq!(t.xp, 5);
        assert_eq!(t.special_price, -2);
        assert_eq!(t.demand, 10);
        // upstream: demandDiff = max(0, floor(5*10*0.05)) = 2;
        // realPrice = clamp(5 + (-2) + 2, 1, 64) = 5
        assert_eq!(t.real_price, 5);
    }

    #[test]
    fn merchant_real_price_clamps_like_upstream() {
        // floor(20*5*0.2) = 20 → 20 + 10 + 20 = 50 (under 64 cap)
        let t = decode_offers(&[offer(ItemKind::Emerald, 20, 10, 0.2, 5)]);
        assert_eq!(t[0].real_price, 50);
        // clamps up at the kind's stack size: 60 + 20 > 64 → 64
        let t = decode_offers(&[offer(ItemKind::Emerald, 60, 20, 0.0, 0)]);
        assert_eq!(t[0].real_price, 64);
        // clamps down at 1 even with deep discounts
        let t = decode_offers(&[offer(ItemKind::Emerald, 5, -50, 0.0, 0)]);
        assert_eq!(t[0].real_price, 1);
        // negative demand still floors the diff at 0
        let t = decode_offers(&[offer(ItemKind::Emerald, 5, 0, 0.5, -3)]);
        assert_eq!(t[0].real_price, 5);
        // out_of_stock maps to disabled
        let mut o = offer(ItemKind::Emerald, 1, 0, 0.0, 0);
        o.out_of_stock = true;
        assert!(decode_offers(&[o])[0].disabled);
    }

    #[test]
    fn furnace_ratio_math_matches_upstream() {
        let data = HashMap::from([
            (FUEL_LEFT, 50),
            (FUEL_TOTAL, 100),
            (COOK_DONE, 25),
            (COOK_TOTAL, 200),
        ]);
        let (fuel, fuel_s, progress, progress_s) = furnace_ratios(&data);
        assert_eq!(fuel, 0.5);
        // fuelSeconds = fuel * totalFuel * 0.05
        assert_eq!(fuel_s, 2.5);
        assert_eq!(progress, 0.125);
        // progressSeconds = remaining ticks * 0.05
        assert_eq!(progress_s, 8.75);
        // no totals → zeros like upstream's null guards
        assert_eq!(furnace_ratios(&HashMap::new()), (0., 0., 0., 0.));
        assert_eq!(
            furnace_ratios(&HashMap::from([(FUEL_LEFT, 30)])).0,
            0.,
            "no total yet → fuel 0 like upstream"
        );
    }

    #[test]
    fn enchant_options_decode_clues() {
        // a holder with one enchantment entry — clue id 0 then resolves
        // like a real server's registry_data would
        let mut holder = azalea::core::registry_holder::RegistryHolder::default();
        holder.append(
            azalea::Identifier::new("minecraft:enchantment"),
            vec![(
                azalea::Identifier::new("minecraft:sharpness"),
                Some(azalea::protocol::simdnbt::owned::NbtCompound::new()),
            )],
        );
        // level costs 1/2/3, seed 42, clue 0 = sharpness at level 4,
        // clues 1/2 = -1
        let data = HashMap::from([
            (0u16, 1i16),
            (1, 2),
            (2, 3),
            (ENCHANT_XP_SEED, 42),
            (ENCHANT_CLUE, 0),
            (ENCHANT_LEVEL_CLUE, 4),
            (ENCHANT_CLUE + 1, -1),
            (ENCHANT_LEVEL_CLUE + 1, -1),
        ]);
        let opts = enchantment_options(&data, &holder);
        assert_eq!(opts[0].level, 1);
        assert_eq!(opts[1].level, 2);
        assert_eq!(opts[2].level, 3);
        assert_eq!(
            opts[0].expected,
            Some(ExpectedEnchant {
                enchant: data::EnchantmentKey::Sharpness,
                level: 4,
            })
        );
        // -1 clues never resolve
        assert!(opts[1].expected.is_none());
        assert!(opts[2].expected.is_none());
        // and an id past the registry's end can't resolve either
        let mut data = data;
        data.insert(ENCHANT_CLUE, 99);
        assert!(enchantment_options(&data, &holder)[0].expected.is_none());
    }

    #[test]
    fn book_input_guards() {
        // every edit_book boundary check is exercised through the same
        // function write_book/sign_book call
        let page = || vec!["page".to_string()];
        // slot range (upstream's `assert.ok(slot >= 0 && slot <= 44)`)
        assert!(check_book_input(0, &page(), None).is_ok());
        assert!(check_book_input(44, &page(), None).is_ok());
        assert!(check_book_input(-1, &page(), None).is_err());
        assert!(check_book_input(45, &page(), None).is_err());
        // page count / per-page length / title length — the wire caps
        // the server disconnects over
        assert!(check_book_input(9, &vec![String::new(); 100], None).is_ok());
        assert!(check_book_input(9, &vec![String::new(); 101], None).is_err());
        assert!(check_book_input(9, &["x".repeat(1024)], None).is_ok());
        assert!(check_book_input(9, &["x".repeat(1025)], None).is_err());
        assert!(check_book_input(9, &page(), Some(&"t".repeat(32))).is_ok());
        assert!(check_book_input(9, &page(), Some(&"t".repeat(33))).is_err());
    }

    #[test]
    fn writable_book_default_component() {
        // WritableBookContent is a default component on writable_book
        // (empty pages — vanilla's componentMap.has is true for it too),
        // so what actually distinguishes an edited book is non-empty
        // pages — which the write-ack predicate in edit_book relies on
        let book = ItemStack::new(ItemKind::WritableBook, 1).with_component(WritableBookContent {
            pages: vec![azalea::core::filterable::Filterable {
                raw: "page one".to_string(),
                filtered: None,
            }],
        });
        assert_eq!(
            book.get_component::<WritableBookContent>()
                .unwrap()
                .pages
                .len(),
            1
        );
        let plain = ItemStack::new(ItemKind::WritableBook, 1);
        assert!(
            plain
                .get_component::<WritableBookContent>()
                .is_some_and(|c| c.pages.is_empty()),
            "fresh writable_book exposes the default (empty) component"
        );
    }

    #[test]
    fn anvil_name_limit() {
        assert!(check_anvil_name(&"x".repeat(35)).is_ok());
        assert!(check_anvil_name(&"x".repeat(36)).is_err());
    }
}
