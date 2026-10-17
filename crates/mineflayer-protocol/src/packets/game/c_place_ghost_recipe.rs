use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;

use crate::common::recipe::RecipeDisplayData;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundPlaceGhostRecipe {
    #[var]
    pub container_id: i32,
    pub recipe: RecipeDisplayData,
}
