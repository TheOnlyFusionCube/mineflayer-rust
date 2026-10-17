//! `bot.openEnchantmentTable` and the enchantment-table window: the
//! three option hints, the xp seed, and `enchant`.

use super::*;

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
pub(super) fn enchantment_options(
    data: &HashMap<u16, i16>,
    registries: &mineflayer_engine::core::registry_holder::RegistryHolder,
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
            .with_window_record(self.window.id(), |record| record.data.clone())
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
            .map_err(|_| Error::Timeout)?;
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
        .map_err(|_| Error::Timeout)?;
        let BotEvent::WindowUpdateSlot { new, .. } = event else {
            unreachable!()
        };
        new.ok_or_else(|| Error::Failed("enchant produced no item".into()))
    }

    /// Whether the three level costs have all arrived (the `ready`
    /// condition — becomes false again if a cost goes back to `-1`).
    pub fn is_ready(&self) -> bool {
        self.bot
            .with_window_record(self.window.id(), |record| {
                (0..3).all(|i| record.data.get(&i).is_some_and(|v| *v >= 0))
            })
            .unwrap_or(false)
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

impl Bot {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enchant_options_decode_clues() {
        // a holder with one enchantment entry — clue id 0 then resolves
        // like a real server's registry_data would
        let mut holder = mineflayer_engine::core::registry_holder::RegistryHolder::default();
        holder.append(
            mineflayer_engine::Identifier::new("minecraft:enchantment"),
            vec![(
                mineflayer_engine::Identifier::new("minecraft:sharpness"),
                Some(mineflayer_engine::protocol::simdnbt::owned::NbtCompound::new()),
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
}
