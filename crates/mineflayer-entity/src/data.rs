//! Define some types needed for entity metadata.

// TODO: this is here because of a bug in enum_as_inner. remove when it's fixed
#![allow(clippy::double_parens)]

use std::io::{self, Cursor, Write};

use derive_more::Deref;
use enum_as_inner::EnumAsInner;
use mineflayer_buf::{BufReadError, McBuf, McBufVar};
use mineflayer_chat::FormattedText;
use mineflayer_core::{
    direction::Direction,
    position::{BlockPos, GlobalPos, Vec3f32},
};
use mineflayer_inventory::{ItemStack, components};
use mineflayer_registry::builtin::{VillagerKind, VillagerProfession};
use uuid::Uuid;

use crate::particle::Particle;

#[derive(Clone, Debug, Deref, PartialEq)]
pub struct EntityMetadataItems(pub Vec<EntityDataItem>);

#[derive(Clone, Debug, PartialEq)]
pub struct EntityDataItem {
    // we can't identify what the index is for here because we don't know the
    // entity type
    pub index: u8,
    pub value: EntityDataValue,
}

impl McBuf for EntityMetadataItems {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        let mut metadata = Vec::new();
        loop {
            let id = u8::mc_read(buf)?;
            if id == 0xff {
                break;
            }
            let value = EntityDataValue::mc_read(buf)?;
            metadata.push(EntityDataItem { index: id, value });
        }
        Ok(EntityMetadataItems(metadata))
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        for item in &self.0 {
            item.index.mc_write(buf)?;
            item.value.mc_write(buf)?;
        }
        0xffu8.mc_write(buf)?;
        Ok(())
    }
}

// Note: This enum is partially generated and parsed by
// codegen/lib/code/entity.py
#[derive(McBuf, Clone, Debug, EnumAsInner, PartialEq)]
pub enum EntityDataValue {
    Byte(u8),
    Int(#[var] i32),
    Long(#[var] i64),
    Float(f32),
    String(Box<str>),
    FormattedText(Box<FormattedText>),
    OptionalFormattedText(Option<Box<FormattedText>>),
    ItemStack(ItemStack),
    Boolean(bool),
    Rotations(Rotations),
    BlockPos(BlockPos),
    OptionalBlockPos(Option<BlockPos>),
    Direction(Direction),
    OptionalLivingEntityReference(Option<Uuid>),
    BlockState(mineflayer_block::BlockState),
    /// If this is air, that means it's absent,
    OptionalBlockState(mineflayer_block::BlockState),
    Particle(Particle),
    Particles(Box<[Particle]>),
    VillagerData(VillagerData),
    // 0 for absent; 1 + actual value otherwise. Used for entity IDs.
    OptionalUnsignedInt(OptionalUnsignedInt),
    Pose(Pose),
    CatVariant(mineflayer_registry::data::CatVariant),
    CatSoundVariant(mineflayer_registry::data::CatSoundVariant),
    CowVariant(mineflayer_registry::data::CowVariant),
    CowSoundVariant(mineflayer_registry::data::CowSoundVariant),
    WolfVariant(mineflayer_registry::data::WolfVariant),
    WolfSoundVariant(mineflayer_registry::data::WolfSoundVariant),
    FrogVariant(mineflayer_registry::data::FrogVariant),
    PigVariant(mineflayer_registry::data::PigVariant),
    PigSoundVariant(mineflayer_registry::data::PigSoundVariant),
    ChickenVariant(mineflayer_registry::data::ChickenVariant),
    ChickenSoundVariant(mineflayer_registry::data::ChickenSoundVariant),
    ZombieNautilusVariant(mineflayer_registry::data::ZombieNautilusVariant),
    OptionalGlobalPos(Option<Box<GlobalPos>>),
    PaintingVariant(mineflayer_registry::data::PaintingVariant),
    SnifferState(SnifferStateKind),
    ArmadilloState(ArmadilloStateKind),
    CopperGolemState(CopperGolemStateKind),
    WeatheringCopperState(WeatheringCopperStateKind),
    Vector3(Vec3f32),
    Quaternion(Quaternion),
    ResolvableProfile(components::Profile),
    HumanoidArm(HumanoidArm),
}

const _: () = assert!(size_of::<EntityDataValue>() == 24);

#[derive(Clone, Debug, PartialEq, Default)]
pub struct OptionalUnsignedInt(pub Option<u32>);

#[derive(McBuf, Clone, Debug, PartialEq, Default)]
pub struct Quaternion {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

// mojang just calls this ArmadilloState but i added "Kind" since otherwise it
// collides with a name in metadata.rs
#[derive(McBuf, Clone, Copy, Debug, Default, PartialEq)]
pub enum ArmadilloStateKind {
    #[default]
    Idle,
    Rolling,
    Scared,
}

impl McBuf for OptionalUnsignedInt {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        let val = u32::mc_read_var(buf)?;
        Ok(OptionalUnsignedInt(if val == 0 {
            None
        } else {
            Some(val - 1)
        }))
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        match self.0 {
            Some(val) => (val + 1).mc_write_var(buf),
            None => 0u32.mc_write_var(buf),
        }
    }
}

/// A set of x, y, and z rotations. This is used for armor stands.
#[derive(McBuf, Clone, Debug, Default, PartialEq)]
pub struct Rotations {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[cfg_attr(feature = "bevy_ecs", derive(bevy_ecs::component::Component))]
#[derive(McBuf, Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Pose {
    #[default]
    Standing = 0,
    FallFlying,
    Sleeping,
    Swimming,
    SpinAttack,
    Crouching,
    LongJumping,
    Dying,
    Croaking,
    UsingTongue,
    Sitting,
    Roaring,
    Sniffing,
    Emerging,
    Digging,
    Sliding,
    Shooting,
    Inhaling,
}

#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct VillagerData {
    pub kind: VillagerKind,
    pub profession: VillagerProfession,
    #[var]
    pub level: u32,
}

#[derive(McBuf, Clone, Copy, Debug, Default, PartialEq)]
pub enum SnifferStateKind {
    #[default]
    Idling,
    FeelingHappy,
    Scenting,
    Sniffing,
    Searching,
    Digging,
    Rising,
}

#[derive(McBuf, Clone, Copy, Debug, Default, PartialEq)]
pub enum CopperGolemStateKind {
    #[default]
    Idle,
    GettingItem,
    GettingNoItem,
    DroppingItem,
    DroppingNoItem,
}
#[derive(McBuf, Clone, Copy, Debug, Default, PartialEq)]
pub enum WeatheringCopperStateKind {
    #[default]
    Unaffected,
    Exposed,
    Weathered,
    Oxidized,
}

#[derive(McBuf, Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum HumanoidArm {
    Left = 0,
    #[default]
    Right = 1,
}
