use mineflayer_buf::McBuf;
use mineflayer_registry::identifier::Identifier;
use mineflayer_protocol_macros::ServerboundGamePacket;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundPlaceRecipe {
    #[var]
    pub container_id: i32,
    pub recipe: Identifier,
    pub shift_down: bool,
}
