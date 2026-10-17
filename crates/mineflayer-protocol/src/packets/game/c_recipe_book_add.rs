use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_registry::builtin::RecipeBookCategory;

use crate::common::recipe::{Ingredient, RecipeDisplayData};

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundRecipeBookAdd {
    pub entries: Vec<Entry>,
    pub replace: bool,
}

#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct Entry {
    pub contents: RecipeDisplayEntry,
    pub flags: u8,
}

#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct RecipeDisplayEntry {
    #[var]
    pub id: u32,
    pub display: RecipeDisplayData,
    // optional varint
    #[var]
    pub group: u32,
    pub category: RecipeBookCategory,
    pub crafting_requirements: Option<Vec<Ingredient>>,
}
