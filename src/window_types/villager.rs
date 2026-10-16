//! `bot.openVillager`, `bot.trade`, and the merchant offers
//! (`villager.trades`) decoded from `c_merchant_offers`.

use super::*;

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
pub(super) fn decode_offers(offers: &[MerchantOffer]) -> Vec<VillagerTrade> {
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
pub(super) fn count_inventory_items(window: &Window, kind: ItemKind, metadata: Option<i32>) -> i32 {
    window
        .inventory_items()
        .iter()
        .filter(|i| i.kind() == kind && metadata.is_none_or(|m| i.metadata() == Some(m)))
        .map(|i| i.count())
        .sum()
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
            .with_window_record(self.window.id(), |record| record.offers.clone())
            .flatten()
            .unwrap_or_default()
    }

    /// `villager.selectedTrade` — the offer `trade` last selected.
    pub fn selected_trade(&self) -> Option<VillagerTrade> {
        self.bot
            .with_window_record(self.window.id(), |record| {
                record.offers.as_ref()?.get(record.selected_trade?).cloned()
            })
            .flatten()
    }

    /// `villager.trade(index, count)` — same as `bot.trade(villager, …)`.
    pub async fn trade(&self, index: usize, count: Option<i32>) -> Result<()> {
        self.bot.trade(self, index, count).await
    }
}

impl Bot {
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
            .with_window_record(id, |record| record.offers.is_some())
            .unwrap_or(false);
        if !has_offers {
            await_event(
                &mut rx,
                WINDOW_WAIT,
                |e| matches!(e, BotEvent::VillagerReady { window_id } if *window_id == id),
            )
            .await;
        }
        if self
            .with_window_record(id, |record| record.offers.is_some())
            .unwrap_or(false)
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
    pub(super) async fn put_requirements(
        &self,
        window: &Window,
        trade: &VillagerTrade,
    ) -> Result<()> {
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
    pub(super) async fn deposit_trade_item(
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use azalea::{
        inventory::ItemStack,
        protocol::packets::game::c_merchant_offers::{DataComponentExactPredicate, ItemCost},
    };

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
}
