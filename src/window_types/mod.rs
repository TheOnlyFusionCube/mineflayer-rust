//! Typed windows — the upstream `furnace.js`, `enchantment_table.js`,
//! `anvil.js`, `villager.js`, `book.js`, and `command_block.js` plugins
//! ported onto the U8 [`Window`] base.
//!
//! # Server-pushed window data
//!
//! Upstream attaches per-window fields (furnace fuel, enchant options,
//! trades) to the window object and updates them from packets the engine
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
//! # Slot layouts (the engine [`Menu`](mineflayer_engine::inventory::Menu) shapes, same as upstream)
//!
//! - Furnace/blast/smoker: `0` input, `1` fuel, `2` output.
//! - Enchantment table: `0` target item, `1` lapis.
//! - Anvil: `0` first, `1` second, `2` result.
//! - Merchant: `0`/`1` payment slots, `2` result.
//!
//! # Costs the server computes
//!
//! Upstream predicts anvil xp cost client-side via `Item.anvil`; the engine
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

use mineflayer_engine::{
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

mod anvil;
mod book;
mod enchant;
mod furnace;
mod villager;

pub use anvil::Anvil;
pub use book::{CommandBlockMode, CommandBlockOpts};
pub use enchant::{EnchantmentOption, EnchantmentTable, ExpectedEnchant};
pub use furnace::Furnace;
use villager::decode_offers;
pub use villager::{Villager, VillagerTrade};

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

impl Bot {
    /// Read one typed window's record under the `window_data` lock —
    /// `None` when the server sent nothing for that window id.
    fn with_window_record<R>(&self, id: i32, f: impl FnOnce(&WindowRecord) -> R) -> Option<R> {
        self.shared.window_data.lock().windows.get(&id).map(f)
    }
}

/// Decode one engine [`Event`] into typed-window state + [`BotEvent`]s.
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
// Bot methods — the openers and the book/command-block ops.
// ---------------------------------------------------------------------------

impl Bot {}

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
    use mineflayer_engine::protocol::packets::game::{
        c_container_close::ClientboundContainerClose,
        c_container_set_data::ClientboundContainerSetData, c_open_screen::ClientboundOpenScreen,
    };

    fn packet_event(p: ClientboundGamePacket) -> Event {
        Event::Packet(Arc::new(p))
    }

    fn open_screen(id: i32, kind: MenuKind) -> Event {
        packet_event(ClientboundGamePacket::OpenScreen(ClientboundOpenScreen {
            container_id: id,
            menu_type: kind,
            title: mineflayer_engine::FormattedText::from("t"),
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
}
