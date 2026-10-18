//! `bot.openFurnace` and the furnace window: fuel/progress ratios and
//! the input/fuel/output slot operations.

use super::*;

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
pub(super) fn furnace_ratios(data: &HashMap<u16, i16>) -> (f32, f32, f32, f32) {
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

    pub(super) fn data(&self, property: u16) -> Option<i16> {
        self.bot.window_property(self.window.id(), property)
    }

    pub(super) fn data_map(&self) -> Option<HashMap<u16, i16>> {
        self.bot
            .with_window_record(self.window.id(), |record| record.data.clone())
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

    pub(super) async fn take(&self, slot: u16) -> Result<Item> {
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

    pub(super) async fn put(
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
