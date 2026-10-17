use std::collections::HashMap;

use mineflayer_buf::McBuf;
use mineflayer_registry::identifier::Identifier;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_registry::builtin::ItemKind;

use crate::common::recipe::{Ingredient, SlotDisplayData};

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundUpdateRecipes {
    pub item_sets: HashMap<Identifier, RecipePropertySet>,
    pub stonecutter_recipes: Vec<SingleInputEntry>,
}

#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct SingleInputEntry {
    pub input: Ingredient,
    pub recipe: SelectableRecipe,
}
#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct SelectableRecipe {
    pub option_display: SlotDisplayData,
}

#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct RecipePropertySet {
    pub items: Vec<ItemKind>,
}
