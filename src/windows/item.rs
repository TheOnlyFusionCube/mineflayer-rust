use mineflayer_engine::{
    inventory::{DataComponentPatch, ItemStack, item::MaxStackSizeExt},
    registry::builtin::ItemKind,
};

/// A stack of items in a window slot — the `prismarine-item` analog.
///
/// Wraps [`ItemStack`] with the absolute window slot index it occupies.
/// `slot == -1` marks the cursor (carried) stack, which lives in no slot.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// The protocol item stack (kind, count, component patch).
    pub stack: ItemStack,
    /// Absolute window slot index, or -1 for the carried/cursor stack.
    pub slot: i16,
}

impl Item {
    /// Wrap a stack; `None` when the stack is empty (upstream `null`).
    pub fn of(stack: ItemStack, slot: i16) -> Option<Item> {
        if stack.is_empty() {
            None
        } else {
            Some(Item { stack, slot })
        }
    }

    /// `item.type` analog — the item kind, e.g. [`ItemKind::Stone`]. The
    /// numeric mcdata id has no engine equivalent; the enum is richer.
    pub fn kind(&self) -> ItemKind {
        self.stack.kind()
    }

    /// `item.name` — e.g. `"stone"` (the identifier path, no namespace).
    pub fn name(&self) -> &'static str {
        // to_str() is "minecraft:<name>"; item kinds are all minecraft:
        &self.kind().to_str()["minecraft:".len()..]
    }

    /// `item.displayName` — the `custom_name` component when present,
    /// else the item name.
    pub fn display_name(&self) -> String {
        self.stack
            .get_component::<mineflayer_engine::inventory::components::CustomName>()
            .map(|c| c.name.to_string())
            .unwrap_or_else(|| self.name().to_string())
    }

    /// `item.count` — stack count (0 when absent).
    pub fn count(&self) -> i32 {
        self.stack.count()
    }

    /// `item.stackSize` — the max stack size for this kind.
    pub fn stack_size(&self) -> i32 {
        self.kind().max_stack_size()
    }

    /// `item.metadata` / `item.durabilityUsed` — the `damage` data
    /// component (`0` for undamaged tools, `None` for undamageable items).
    pub fn metadata(&self) -> Option<i32> {
        self.durability_used()
    }

    /// `item.durabilityUsed` — damage already taken.
    pub fn durability_used(&self) -> Option<i32> {
        self.stack
            .get_component::<mineflayer_engine::inventory::components::Damage>()
            .map(|d| d.amount)
    }

    /// `item.maxDurability` — the `max_damage` component, `None` for
    /// items without durability.
    pub fn max_durability(&self) -> Option<i32> {
        self.stack
            .get_component::<mineflayer_engine::inventory::components::MaxDamage>()
            .map(|d| d.amount)
    }

    /// `item.nbt` analog — the `custom_data` component. Modern items have
    /// component patches rather than free-form NBT; use
    /// [`Item::component_patch`] for the full set.
    pub fn custom_data(&self) -> Option<mineflayer_engine::inventory::components::CustomData> {
        self.stack
            .get_component::<mineflayer_engine::inventory::components::CustomData>()
            .map(|c| c.into_owned())
    }

    /// The raw component patch — upstream's `nbt` in component form.
    pub fn component_patch(&self) -> &DataComponentPatch {
        self.stack.component_patch()
    }

    /// Whether this and `other` are the same kind with the same
    /// components, ignoring count (upstream `Item.equal(x, false)`).
    pub fn same_item(&self, other: &Item) -> bool {
        match (self.stack.as_present(), other.stack.as_present()) {
            (Some(a), Some(b)) => a.is_same_item_and_components(b),
            _ => false,
        }
    }

    /// Whether this stack satisfies a transfer-style matcher: same kind,
    /// optional `metadata` (damage) equality, optional `nbt` (component
    /// patch) equality, optional `not_full` (room left on the stack).
    pub(crate) fn matches(
        &self,
        kind: ItemKind,
        metadata: Option<i32>,
        not_full: bool,
        nbt: Option<&DataComponentPatch>,
    ) -> bool {
        if self.kind() != kind {
            return false;
        }
        if let Some(meta) = metadata
            && self.durability_used() != Some(meta)
        {
            return false;
        }
        if let Some(patch) = nbt
            && self.component_patch() != patch
        {
            return false;
        }
        !(not_full && self.count() >= self.stack_size())
    }
}
