use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ServerboundGamePacket;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundRecipeBookChangeSettings {
    pub book_type: RecipeBookType,
    pub is_open: bool,
    pub is_filtering: bool,
}

#[derive(McBuf, Clone, Copy, Debug, PartialEq)]
pub enum RecipeBookType {
    Crafting = 0,
    Furnace = 1,
    BlastFurnace = 2,
    Smoker = 3,
}
