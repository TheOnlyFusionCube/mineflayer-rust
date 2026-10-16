//! `bot.transfer` and its cursor choreography: `TransferOpts`,
//! `putSelectedItemRange`, and the left-click/right-click/half-stack
//! moves upstream uses.

use super::*;

/// `bot.transfer(options)` parameters. Ranges are `[start, end)` in
/// absolute window-slot indices (the same convention upstream's
/// `sourceStart`/`sourceEnd`/`destStart`/`destEnd` use); `dest_start ==
/// -999` drops items on the ground.
#[derive(Clone, Debug)]
pub struct TransferOpts {
    /// Window to act on — `None` means the open window, else the player
    /// inventory (upstream `options.window || currentWindow ||
    /// inventory`).
    pub window: Option<Window>,
    /// Only stacks of this kind move (upstream `itemType`).
    pub kind: ItemKind,
    /// Required `damage` component (upstream `metadata`); `None` = any.
    pub metadata: Option<i32>,
    /// Required component patch (upstream `nbt`); `None` = any.
    pub nbt: Option<DataComponentPatch>,
    /// How many items to move; `None` = 1 (upstream `count ?? 1` — see
    /// [`TransferOpts::effective_count`]).
    pub count: Option<i32>,
    /// Source range start (absolute window slot).
    pub source_start: i32,
    /// Source range end, exclusive — defaults to `source_start + 1`.
    pub source_end: Option<i32>,
    /// Destination range start; `-999` drops the items.
    pub dest_start: i32,
    /// Destination range end, exclusive — defaults to `dest_start + 1`.
    pub dest_end: Option<i32>,
}

impl TransferOpts {
    /// The resolved move count: `count ?? 1` like upstream
    /// (`inventory.js`: `count === undefined || count === null ? 1 :
    /// count`). `None` moves ONE item, not the whole stock.
    pub fn effective_count(&self) -> i32 {
        self.count.unwrap_or(1)
    }
}

/// `putSelectedItemRange` (upstream): while the cursor holds a stack,
/// merge it onto a not-full matching stack in `[start, end)`, else the
/// first empty slot, else (reserved `slot`) click it, else toss the
/// leftover outside.
pub(super) async fn put_selected(
    window: &Window,
    range: Range<i32>,
    slot: Option<i32>,
) -> Result<()> {
    while let Some(carried) = window.selected_item() {
        let slots = window.slots().unwrap_or_default();
        let range = range.start.max(0) as usize..range.end.max(0) as usize;
        let merge = find_item_in(
            &slots,
            range.clone(),
            carried.kind(),
            carried.metadata(),
            true,
            Some(carried.component_patch()),
        );
        if let Some(item) = merge
            && item.stack_size() != item.count()
        {
            window.click(PickupClick::Left {
                slot: Some(item.slot as u16),
            });
            continue;
        }
        match first_empty_in(&slots, range) {
            Some(empty) => {
                window.click(PickupClick::Left {
                    slot: Some(empty as u16),
                });
            }
            None => {
                if let Some(reserved) = slot {
                    window.click(PickupClick::Left {
                        slot: Some(reserved as u16),
                    });
                }
                toss_leftover(window);
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Upstream `tossLeftover` — click outside to drop the cursor stack.
pub(super) fn toss_leftover(window: &Window) {
    if window.selected_item().is_some() {
        window.click(PickupClick::LeftOutside);
    }
}

/// `bot.transfer` (upstream inventory.js) — move `count` items of `kind`
/// from `[source_start, source_end)` to `[dest_start, dest_end)` using
/// pick-up/merge/place click choreography. `dest_start == -999` drops
/// them on the ground.
///
/// Unlike upstream (which clicks through `clickWindow` on the *current*
/// window), the operations go to `options.window` directly — the same
/// thing for every call site that matters, and the only correct thing
/// when a different window is passed.
pub(crate) async fn transfer(options: TransferOpts) -> Result<()> {
    let window = options
        .window
        .clone()
        .ok_or_else(|| Error::InvalidArgument("transfer requires a window".into()))?;
    let mut count = options.effective_count();
    if count < 0 {
        // upstream's loop only stops at exactly zero (`count -= 1` walks a
        // negative count away from it and clicks until the source runs
        // out); refuse instead of clicking unbounded
        return Err(Error::InvalidArgument("transfer count must be >= 0".into()));
    }
    let kind = options.kind;
    let metadata = options.metadata;
    let nbt = options.nbt;
    let source = options.source_start..options.source_end.unwrap_or(options.source_start + 1);
    let dest_start = options.dest_start;
    let dest_end = options.dest_end.unwrap_or(dest_start + 1);
    let dest = dest_start..dest_end;
    let mut first_source_slot: Option<i32> = None;

    loop {
        if count == 0 {
            // put anything left on the cursor back into the source range
            put_selected(&window, source.clone(), first_source_slot).await?;
            return Ok(());
        }

        let held_matches = window.selected_item().is_some_and(|sel| {
            sel.kind() == kind
                && metadata.is_none_or(|m| sel.metadata() == Some(m))
                && nbt.as_ref().is_none_or(|p| sel.component_patch() == p)
        });
        if !held_matches {
            // we are not holding the item we need — click it
            let source_item = window
                .find_item_range(
                    source.start.max(0) as usize..source.end.max(0) as usize,
                    kind,
                    metadata,
                    false,
                    nbt.as_ref(),
                )
                .ok_or_else(|| {
                    Error::NotFound(format!(
                        "Can't find {} in slots [{} - {}], (item id: {})",
                        kind.to_str(),
                        source.start,
                        source.end,
                        kind.to_str()
                    ))
                })?;
            if first_source_slot.is_none() {
                first_source_slot = Some(i32::from(source_item.slot));
            }
            window.click(PickupClick::Left {
                slot: Some(source_item.slot as u16),
            });
        }

        // clickDest()
        let Some(selected) = window.selected_item() else {
            return Err(Error::Failed("transfer: cursor empty after click".into()));
        };
        let dest_slot: i32;
        let mut dest_slot_count = 0;
        if dest_start == -999 {
            dest_slot = -999;
        } else {
            let slots = window.slots().unwrap_or_default();
            let range = dest.start.max(0) as usize..dest.end.max(0) as usize;
            // a non-full same-item stack we can merge into
            let dest_item = find_item_in(
                &slots,
                range.clone(),
                selected.kind(),
                selected.metadata(),
                true,
                nbt.as_ref(),
            );
            match dest_item {
                Some(item) => {
                    dest_slot = i32::from(item.slot);
                    dest_slot_count = item.count();
                }
                None => match first_empty_in(&slots, range) {
                    Some(i) => dest_slot = i as i32,
                    None => {
                        return Err(Error::InvalidState("destination full".into()));
                    }
                },
            }
        }
        // move the maximum number of items a click can move
        let stack_size = selected.stack_size();
        let moved_items = (stack_size - dest_slot_count).min(selected.count());
        let target = dest_slot_count + count;
        let half = ((dest_slot_count + selected.count()).min(stack_size)) / 2;
        if moved_items <= count {
            // several at the same time (left click)
            window.click(if dest_slot == -999 {
                PickupClick::LeftOutside
            } else {
                PickupClick::Left {
                    slot: Some(dest_slot as u16),
                }
            });
            count -= moved_items;
        } else if dest_slot != -999 && half <= target && 3 + target - half < count {
            // A right click on a stack with an empty cursor takes the
            // larger half — cheaper than `count` right clicks.
            window.click(PickupClick::Left {
                slot: Some(dest_slot as u16),
            });
            if window.selected_item().is_some() {
                put_selected(&window, source.clone(), first_source_slot).await?;
            }
            window.click(PickupClick::Right {
                slot: Some(dest_slot as u16),
            });
            let mut missing = target - half;
            while missing > 0 {
                window.click(PickupClick::Right {
                    slot: Some(dest_slot as u16),
                });
                missing -= 1;
            }
            count = 0;
        } else {
            // one by one (right click)
            window.click(if dest_slot == -999 {
                PickupClick::RightOutside
            } else {
                PickupClick::Right {
                    slot: Some(dest_slot as u16),
                }
            });
            count -= 1;
        }
    }
}

impl Bot {
    /// `bot.putSelectedItemRange(start, end, window, reservedSlot)` —
    /// put whatever is on the cursor into `[start, end)`: merge onto
    /// matching not-full stacks first, then the first empty slot, else
    /// click `reserved_slot` and toss the leftover outside (upstream's
    /// nested helper, with the `slot` parameter).
    pub async fn put_selected_item_range(
        &self,
        start: i32,
        end: i32,
        window: &Window,
        reserved_slot: Option<i32>,
    ) -> Result<()> {
        put_selected(window, start..end, reserved_slot).await
    }

    /// `bot.transfer(options)` — move items between slot ranges with the
    /// upstream click choreography (see `transfer`).
    pub async fn transfer(&self, options: TransferOpts) -> Result<()> {
        let window = options
            .window
            .clone()
            .or_else(|| self.current_window())
            .unwrap_or_else(|| self.inventory());
        transfer(TransferOpts {
            window: Some(window),
            ..options
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A negative `count` must error before any click. Zero keeps
    /// upstream's behavior: the `count === 0` branch runs the cursor
    /// cleanup and completes.
    #[tokio::test]
    async fn transfer_rejects_negative_counts_and_noops_zero() {
        let bot = Bot::new(crate::test_util::despawned_client());
        let opts = |count: i32| TransferOpts {
            window: Some(bot.inventory()),
            kind: azalea::registry::builtin::ItemKind::Stone,
            metadata: None,
            nbt: None,
            count: Some(count),
            source_start: 9,
            source_end: Some(36),
            dest_start: -999,
            dest_end: None,
        };
        let err = transfer(opts(-1)).await.unwrap_err();
        assert!(
            matches!(&err, Error::InvalidArgument(m) if m.contains(">= 0")),
            "got {err:?}"
        );
        // zero completes without an error, like upstream
        transfer(opts(0)).await.unwrap();
    }

    #[test]
    fn transfer_count_defaults_to_one() {
        // upstream `count ?? 1` — None moves ONE item, not all matching
        let opts = TransferOpts {
            window: None,
            kind: ItemKind::Stone,
            metadata: None,
            nbt: None,
            count: None,
            source_start: 9,
            source_end: Some(45),
            dest_start: 0,
            dest_end: Some(9),
        };
        assert_eq!(opts.effective_count(), 1);
        let opts = TransferOpts {
            count: Some(7),
            ..opts
        };
        assert_eq!(opts.effective_count(), 7);
    }

    #[test]
    fn transfer_range_defaults() {
        // [start, end) with end defaulting to start + 1 mirrors upstream
        let opts = TransferOpts {
            window: None,
            kind: ItemKind::Stone,
            metadata: None,
            nbt: None,
            count: None,
            source_start: 9,
            source_end: None,
            dest_start: 27,
            dest_end: None,
        };
        assert_eq!(opts.source_end.unwrap_or(opts.source_start + 1), 10);
        assert_eq!(opts.dest_end.unwrap_or(opts.dest_start + 1), 28);
        assert!(opts.window.is_none());
    }
}
