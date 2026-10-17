use mineflayer_buf::McBuf;
use mineflayer_registry::identifier::Identifier;

#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[derive(McBuf, Clone, Debug, simdnbt::Deserialize, PartialEq)]
pub struct CustomSound {
    pub sound_id: Identifier,
    pub range: Option<f32>,
}
