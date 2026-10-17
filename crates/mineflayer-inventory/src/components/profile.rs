use mineflayer_auth::game_profile::{
    GameProfile, GameProfileProperties, SerializableProfileProperties,
};
use mineflayer_buf::McBuf;
use mineflayer_core::codec_utils::*;
use mineflayer_registry::identifier::Identifier;
use serde::{Serialize, Serializer};
use uuid::Uuid;

#[derive(McBuf, Clone, Debug, Default, PartialEq, Serialize)]
#[doc(alias = "ResolvableProfile")]
pub struct Profile {
    #[serde(flatten)]
    pub unpack: Box<PartialOrFullProfile>,
    #[serde(flatten)]
    pub skin_patch: Box<PlayerSkinPatch>,
}

#[derive(McBuf, Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum PartialOrFullProfile {
    Partial(PartialProfile),
    Full(GameProfile),
}
impl Default for PartialOrFullProfile {
    fn default() -> Self {
        Self::Partial(PartialProfile::default())
    }
}

#[derive(McBuf, Clone, Debug, Default, PartialEq, Serialize)]
pub struct PartialProfile {
    #[limit(16)]
    #[serde(skip_serializing_if = "is_default")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "is_default")]
    pub id: Option<Uuid>,
    #[serde(serialize_with = "serialize_properties")]
    #[serde(skip_serializing_if = "is_default")]
    pub properties: GameProfileProperties,
}
fn serialize_properties<S: Serializer>(
    properties: &GameProfileProperties,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let serializable = SerializableProfileProperties::from(properties.clone());
    serializable.serialize(serializer)
}

#[derive(McBuf, Clone, Debug, Default, PartialEq, Serialize)]
pub struct PlayerSkinPatch {
    #[serde(rename = "texture")]
    #[serde(skip_serializing_if = "is_default")]
    pub body: Option<ResourceTexture>,
    #[serde(skip_serializing_if = "is_default")]
    pub cape: Option<ResourceTexture>,
    #[serde(skip_serializing_if = "is_default")]
    pub elytra: Option<ResourceTexture>,
    #[serde(skip_serializing_if = "is_default")]
    pub model: Option<PlayerModelType>,
}

#[derive(McBuf, Clone, Copy, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayerModelType {
    #[default]
    Wide,
    Slim,
}

#[derive(McBuf, Clone, Debug, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ResourceTexture {
    pub id: Identifier,
}
