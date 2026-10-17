use mineflayer_buf::McBuf;
use mineflayer_core::{delta::LpVec3, entity_id::MinecraftEntityId, position::Vec3};
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_registry::builtin::EntityKind;
#[cfg(feature = "bevy_ecs")]
use mineflayer_world::WorldName;
use uuid::Uuid;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundAddEntity {
    /// The numeric ID of the entity being added to the world.
    #[var]
    pub id: MinecraftEntityId,
    pub uuid: Uuid,
    pub entity_type: EntityKind,
    pub position: Vec3,
    pub movement: LpVec3,
    pub x_rot: i8,
    pub y_rot: i8,
    pub y_head_rot: i8,
    /// The entity's "object data". This is unused for most entities.
    ///
    /// Projectiles and fishing hooks treat this like a [`MinecraftEntityId`].
    /// Falling blocks treat it as a [`BlockState`](mineflayer_block::BlockState).
    /// Other entities may treat it as another enum variant.
    ///
    /// See [the wiki](https://minecraft.wiki/w/Java_Edition_protocol/Object_data)
    /// for more information about this field.
    #[var]
    pub data: i32,
}

impl ClientboundAddEntity {
    /// Make the entity into a bundle that can be inserted into the ECS.
    ///
    /// You must apply the metadata after inserting the bundle with
    /// [`Self::apply_metadata`].
    #[cfg(feature = "bevy_ecs")]
    pub fn as_entity_bundle(&self, world_name: WorldName) -> mineflayer_entity::EntityBundle {
        mineflayer_entity::EntityBundle::new(self.uuid, self.position, self.entity_type, world_name)
    }

    /// Apply the default metadata for the given entity.
    #[cfg(feature = "bevy_ecs")]
    pub fn apply_metadata(&self, entity: &mut bevy_ecs::system::EntityCommands) {
        mineflayer_entity::metadata::apply_default_metadata(entity, self.entity_type);
    }
}
