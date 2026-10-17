//! `bot.creative` — the creative-mode namespace (slot writes, flight).
//!
//! Split out of `misc`: slot echoing/bookkeeping and the flight helpers are
//! their own concern; the shared `MiscState` (in-flight slot flags, fishing)
//! stays in the parent.

use std::time::Duration;

use mineflayer_engine::{
    Vec3,
    entity::{Position, inventory::Inventory},
    inventory::ItemStack,
    protocol::packets::game::{s_player_abilities, s_set_creative_mode_slot},
};
use tokio::sync::broadcast;

use super::misc_mut;
use crate::{Bot, BotEvent, Error, Result};

impl Bot {
    /// `bot.creative` — the creative-mode namespace (`set_inventory_slot`,
    /// `fly_to`, `start_flying`, …). The methods only take effect in
    /// creative mode, same as upstream.
    pub fn creative(&self) -> Creative {
        Creative {
            client: self.client.clone(),
            events: self.subscribe(),
        }
    }
}

/// `bot.creative`.
pub struct Creative {
    client: mineflayer_engine::Client,
    events: broadcast::Receiver<BotEvent>,
}

/// Releases a slot's `creative_slots_in_flight` entry on drop — including
/// when the caller's future is cancelled or panics, which a trailing
/// cleanup statement would skip and thereby poison the slot with the
/// "calling again" error.
struct CreativeSlotGuard {
    client: mineflayer_engine::Client,
    slot: u16,
}

impl Drop for CreativeSlotGuard {
    fn drop(&mut self) {
        let _ = misc_mut(&self.client, |m| {
            m.creative_slots_in_flight.remove(&self.slot);
        });
    }
}

impl Creative {
    /// `bot.creative.setInventorySlot(slot, item)` — the
    /// `s_set_creative_mode_slot` packet, then a wait for the server's
    /// `container_set_slot` echo like upstream (a mismatch is a rejection,
    /// and upstream rejects after 5s with no echo).
    ///
    /// Dropping the returned future (caller cancellation) releases the
    /// slot's in-flight flag instead of leaving it stuck with "calling
    /// again"; the wire write is not rolled back.
    ///
    /// Upstream asserts `0 <= slot <= 44`; slots are unsigned here.
    pub async fn set_inventory_slot(&self, slot: u16, item: ItemStack) -> Result<()> {
        if slot > 44 {
            return Err(Error::InvalidArgument("slot must be <= 44".into()));
        }
        // no-op when the slot already holds an identical stack, like
        // upstream's Item.equal check
        let same = self
            .client
            .get_component::<Inventory>()
            .map(|inv| inv.inventory_menu.slots().get(usize::from(slot)) == Some(&item))
            .unwrap_or(false);
        if same {
            return Ok(());
        }
        let in_flight = misc_mut(&self.client, |m| !m.creative_slots_in_flight.insert(slot))?;
        if in_flight {
            return Err(Error::Cancelled(format!(
                "Setting slot {slot} cancelled due to calling bot.creative.set_inventory_slot({slot}, ...) again"
            )));
        }
        let _guard = CreativeSlotGuard {
            client: self.client.clone(),
            slot,
        };
        self.set_inventory_slot_inner(slot, item).await
    }

    async fn set_inventory_slot_inner(&self, slot: u16, item: ItemStack) -> Result<()> {
        let mut rx = self.events.resubscribe();
        self.client
            .write_packet(s_set_creative_mode_slot::ServerboundSetCreativeModeSlot {
                slot_num: slot,
                item_stack: item.clone(),
            });
        let wait = async {
            loop {
                // the window observer's derived event, not the raw packet,
                // so the wait also works with `packet_events(false)`. A full
                // inventory resync (`container_set_content`) also emits
                // this event; one that arrives before the server applies the
                // write reports the old stack as "Server rejected" — a rare
                // race the raw-packet wait did not have.
                match rx.recv().await {
                    Ok(BotEvent::WindowUpdateSlot {
                        window_id: 0,
                        slot: s,
                        new,
                        ..
                    }) if s == slot => {
                        let echoed = new.map(|i| i.stack).unwrap_or(ItemStack::Empty);
                        return if echoed == item {
                            Ok(())
                        } else {
                            Err(Error::Failed("Server rejected".into()))
                        };
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return Err(Error::NotConnected),
                }
            }
        };
        match tokio::time::timeout(Duration::from_secs(5), wait).await {
            Ok(result) => result,
            Err(_) => Err(Error::Timeout),
        }
    }

    /// `bot.creative.clearSlot(slot)`.
    pub async fn clear_slot(&self, slot: u16) -> Result<()> {
        self.set_inventory_slot(slot, ItemStack::Empty).await
    }

    /// `bot.creative.clearInventory()` — clear every occupied slot of the
    /// player inventory concurrently, like upstream's `Promise.all` (each
    /// slot's confirmation is independent).
    pub async fn clear_inventory(&self) -> Result<()> {
        let occupied: Vec<u16> = {
            let Some(inv) = self.client.get_component::<Inventory>() else {
                return Err(Error::NotConnected);
            };
            inv.inventory_menu
                .slots()
                .iter()
                .enumerate()
                .filter(|(_, item)| !item.is_empty())
                .map(|(slot, _)| slot as u16)
                .collect()
        };
        // upstream `Promise.all` rejects at the first rejection while the
        // other promises keep settling (JS promises cannot be cancelled) —
        // `join_all` waits for every slot for the same shape, then reports
        // the first error rather than dropping sibling futures mid-flight
        let clears = occupied.iter().map(|&slot| self.clear_slot(slot));
        futures::future::join_all(clears)
            .await
            .into_iter()
            .collect()
    }

    /// `bot.creative.startFlying()` — set the flying ability locally and
    /// tell the server (`s_player_abilities`), matching what the vanilla
    /// client does on double-jump. (Upstream zeroes `bot.physics.gravity`;
    /// same effect.)
    pub fn start_flying(&self) {
        self.set_flying(true);
    }

    /// `bot.creative.stopFlying()`.
    pub fn stop_flying(&self) {
        self.set_flying(false);
    }

    fn set_flying(&self, flying: bool) {
        {
            let mut ecs = self.client.ecs.write();
            if let Ok(mut entity) = ecs.get_entity_mut(self.client.entity)
                && let Some(mut abilities) =
                    entity.get_mut::<mineflayer_engine::entity::PlayerAbilities>()
            {
                abilities.flying = flying;
            }
        }
        self.client
            .write_packet(s_player_abilities::ServerboundPlayerAbilities { is_flying: flying });
    }

    /// `bot.creative.flyTo(destination)` — a straight line like upstream:
    /// keep flying and step the position ~0.5 blocks per 50ms until the
    /// destination is reached.
    pub async fn fly_to(&self, destination: Vec3) {
        self.start_flying();
        loop {
            let pos = self
                .client
                .get_component::<Position>()
                .map(|p| **p)
                .unwrap_or_default();
            let delta = destination - pos;
            let magnitude = (delta.x * delta.x + delta.y * delta.y + delta.z * delta.z).sqrt();
            if magnitude <= 0.5 {
                self.set_position(destination);
                return;
            }
            self.set_position(pos + delta / magnitude * 0.5);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn set_position(&self, pos: Vec3) {
        let mut ecs = self.client.ecs.write();
        if let Ok(mut entity) = ecs.get_entity_mut(self.client.entity)
            && let Some(mut position) = entity.get_mut::<Position>()
        {
            **position = pos;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::misc::misc_mut;

    fn slot_flagged(bot: &Bot, slot: u16) -> bool {
        misc_mut(bot.client(), |m| m.creative_slots_in_flight.contains(&slot)).unwrap_or(false)
    }

    /// A cancelled `set_inventory_slot` releases its slot flag — a
    /// trailing cleanup statement would be skipped by the drop and every
    /// later call for the slot would fail with "calling again".
    #[tokio::test]
    async fn cancelled_set_inventory_slot_releases_the_in_flight_slot() {
        use mineflayer_engine::registry::builtin::ItemKind;

        let mut inventory = Inventory::default();
        *inventory.inventory_menu.slot_mut(5).unwrap() = ItemStack::new(ItemKind::Stone, 1);
        let bot = crate::test_util::bot(crate::test_util::live_entity_client(inventory));
        let creative = bot.creative();
        let item = ItemStack::new(ItemKind::Dirt, 1);
        let set = || creative.set_inventory_slot(5, item.clone());
        // the call parks waiting for the server echo; the timeout drops it
        let _ = tokio::time::timeout(Duration::from_millis(50), set()).await;
        assert!(
            !slot_flagged(&bot, 5),
            "cancelled call left the slot flagged"
        );
        // a fresh call is not rejected as "calling again" — it parks again
        let again = tokio::time::timeout(Duration::from_millis(50), set()).await;
        assert!(again.is_err(), "second call failed early: {again:?}");
        assert!(!slot_flagged(&bot, 5));
    }

    /// `clearInventory` settles every slot it started: the offhand range
    /// rejection must surface only after the sibling clear completed
    /// (upstream `Promise.all` shape), and no slot may stay flagged.
    #[tokio::test]
    async fn clear_inventory_settles_every_started_slot() {
        use mineflayer_engine::registry::builtin::ItemKind;

        let mut inventory = Inventory::default();
        *inventory.inventory_menu.slot_mut(10).unwrap() = ItemStack::new(ItemKind::Stone, 1);
        *inventory.inventory_menu.slot_mut(45).unwrap() = ItemStack::new(ItemKind::Dirt, 1);
        let bot = crate::test_util::bot(crate::test_util::live_entity_client(inventory));
        let bot2 = bot.clone();
        let clear = tokio::spawn(async move { bot2.creative().clear_inventory().await });

        // wait for slot 10's clear to start, then confirm it is still
        // pending: the failing sibling must not have cancelled it or let
        // the call return early
        for _ in 0..200 {
            if slot_flagged(&bot, 10) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(slot_flagged(&bot, 10), "slot 10's clear never started");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !clear.is_finished(),
            "clearInventory returned before the sibling settled"
        );

        // let slot 10 settle with its echo
        bot.emit_event(BotEvent::WindowUpdateSlot {
            window_id: 0,
            slot: 10,
            old: None,
            new: None,
        });
        let res = clear.await.unwrap();
        assert!(res.is_err(), "the offhand rejection must surface");
        assert!(!slot_flagged(&bot, 10) && !slot_flagged(&bot, 45));
    }
}
