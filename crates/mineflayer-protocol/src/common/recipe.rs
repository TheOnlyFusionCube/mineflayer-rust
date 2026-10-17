use mineflayer_buf::McBuf;
use mineflayer_inventory::ItemStack;
use mineflayer_registry::{
    HolderSet,
    builtin::{DataComponentKind, ItemKind},
    data::TrimPattern,
    identifier::Identifier,
};

/// [`mineflayer_registry::builtin::RecipeDisplay`]
#[derive(McBuf, Clone, Debug, PartialEq)]
pub enum RecipeDisplayData {
    Shapeless(ShapelessCraftingRecipeDisplay),
    Shaped(ShapedCraftingRecipeDisplay),
    Furnace(FurnaceRecipeDisplay),
    Stonecutter(StonecutterRecipeDisplay),
    Smithing(SmithingRecipeDisplay),
}

#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct ShapelessCraftingRecipeDisplay {
    pub ingredients: Vec<SlotDisplayData>,
    pub result: SlotDisplayData,
    pub crafting_station: SlotDisplayData,
}
#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct ShapedCraftingRecipeDisplay {
    #[var]
    pub width: u32,
    #[var]
    pub height: u32,
    pub ingredients: Vec<SlotDisplayData>,
    pub result: SlotDisplayData,
    pub crafting_station: SlotDisplayData,
}
#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct FurnaceRecipeDisplay {
    pub ingredient: SlotDisplayData,
    pub fuel: SlotDisplayData,
    pub result: SlotDisplayData,
    pub crafting_station: SlotDisplayData,
    #[var]
    pub duration: u32,
    pub experience: f32,
}
#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct StonecutterRecipeDisplay {
    pub input: SlotDisplayData,
    pub result: SlotDisplayData,
    pub crafting_station: SlotDisplayData,
}
#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct SmithingRecipeDisplay {
    pub template: SlotDisplayData,
    pub base: SlotDisplayData,
    pub addition: SlotDisplayData,
    pub result: SlotDisplayData,
    pub crafting_station: SlotDisplayData,
}

#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct Ingredient {
    pub allowed: HolderSet<ItemKind, Identifier>,
}

/// [`mineflayer_registry::builtin::SlotDisplay`]
#[derive(McBuf, Clone, Debug, PartialEq)]
pub enum SlotDisplayData {
    Empty,
    AnyFuel,
    WithAnyPotion(Box<WithAnyPotionSlotDisplay>),
    OnlyWithComponent(Box<OnlyWithComponentSlotDisplay>),
    Item(ItemSlotDisplay),
    ItemStack(ItemStackSlotDisplay),
    Tag(TagSlotDisplay),
    Dyed(Box<DyedSlotDemo>),
    SmithingTrim(Box<SmithingTrimDemoSlotDisplay>),
    WithRemainder(Box<WithRemainderSlotDisplay>),
    Composite(CompositeSlotDisplay),
}

#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct WithAnyPotionSlotDisplay {
    pub contents: SlotDisplayData,
}
#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct OnlyWithComponentSlotDisplay {
    pub contents: SlotDisplayData,
    pub component: DataComponentKind,
}

#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct ItemSlotDisplay {
    pub item: ItemKind,
}
#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct ItemStackSlotDisplay {
    pub stack: ItemStack,
}
#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct DyedSlotDemo {
    pub dye: SlotDisplayData,
    pub target: SlotDisplayData,
}
#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct TagSlotDisplay {
    pub tag: Identifier,
}
#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct SmithingTrimDemoSlotDisplay {
    pub base: SlotDisplayData,
    pub material: SlotDisplayData,
    pub pattern: TrimPattern,
}
#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct WithRemainderSlotDisplay {
    pub input: SlotDisplayData,
    pub remainder: SlotDisplayData,
}
#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct CompositeSlotDisplay {
    pub contents: Vec<SlotDisplayData>,
}
