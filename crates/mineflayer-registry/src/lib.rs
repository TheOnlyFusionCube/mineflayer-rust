#![doc = include_str!("../README.md")]

// The contents of the macros below are generated in
// codegen/lib/code/registry.py, though the rest of the file isn't
// auto-generated (so you can add doc comments to the registry enums if you
// want).

pub mod builtin;
pub mod data;
pub mod identifier;
pub mod tags;

use std::{
    fmt::{self, Debug},
    hash::Hash,
    io::{self, Cursor, Write},
};

use mineflayer_buf::{BufReadError, McBuf, McBufVar};
#[cfg(feature = "serde")]
use serde::Serialize;
use simdnbt::{FromNbtTag, borrow::NbtTag};

use crate::identifier::Identifier;

// TODO: remove this next update
macro_rules! define_deprecated_builtin {
    ($($r:ident) *) => {
        $(
            #[doc(hidden)]
            #[deprecated = concat!("moved to `mineflayer_registry::builtin::", stringify!($r), "`")]
            pub type $r = builtin::$r;
        )*
    };
}
define_deprecated_builtin!(Activity Attribute BlockEntityKind BlockPredicateKind ChunkStatus CommandArgumentKind CustomStat EntityKind FloatProviderKind Fluid GameEvent HeightProviderKind IntProviderKind LootConditionKind LootFunctionKind LootNbtProviderKind LootNumberProviderKind LootPoolEntryKind LootScoreProviderKind MemoryModuleKind MobEffect ParticleKind PointOfInterestKind PosRuleTest PositionSourceKind Potion RecipeSerializer RecipeKind RuleTest SensorKind SoundEvent StatKind VillagerProfession VillagerKind WorldgenBiomeSource WorldgenBlockStateProviderKind WorldgenCarver WorldgenChunkGenerator WorldgenDensityFunctionKind WorldgenFeature WorldgenFeatureSizeKind WorldgenFoliagePlacerKind WorldgenMaterialCondition WorldgenMaterialRule WorldgenPlacementModifierKind WorldgenRootPlacerKind WorldgenStructurePiece WorldgenStructurePlacement WorldgenStructurePoolElement WorldgenStructureProcessor WorldgenStructureKind WorldgenTreeDecoratorKind WorldgenTrunkPlacerKind RuleBlockEntityModifier CreativeModeTab MenuKind WorldgenPoolAliasBinding TriggerKind NumberFormatKind DataComponentKind EntitySubPredicateKind MapDecorationKind EnchantmentEffectComponentKind EnchantmentEntityEffectKind EnchantmentLevelBasedValueKind EnchantmentLocationBasedEffectKind EnchantmentProviderKind EnchantmentValueEffectKind DecoratedPotPattern ConsumeEffectKind RecipeBookCategory RecipeDisplay SlotDisplay TicketKind TestEnvironmentDefinitionKind TestFunction TestInstanceKind DataComponentPredicateKind SpawnConditionKind DialogBodyKind DialogKind InputControlKind DialogActionKind DebugSubscription IncomingRpcMethods OutgoingRpcMethods AttributeKind EnvironmentAttribute GameRule PermissionCheckKind PermissionKind SlotSourceKind);
macro_rules! define_deprecated_data {
    ($($r:ident) *) => {
        $(
            #[doc(hidden)]
            #[deprecated = concat!("moved to `mineflayer_registry::data::", stringify!($r), "`")]
            pub type $r = data::$r;
        )*
    };
}
define_deprecated_data!(Enchantment DamageKind Dialog WolfSoundVariant CowVariant ChickenVariant FrogVariant CatVariant PigVariant PaintingVariant WolfVariant ZombieNautilusVariant Biome);

#[doc(hidden)]
#[deprecated = "renamed to `mineflayer_registry::builtin::ItemKind`"]
pub type Item = builtin::ItemKind;
#[doc(hidden)]
#[deprecated = "renamed to `mineflayer_registry::builtin::BlockKind`"]
pub type Block = builtin::BlockKind;
#[doc(hidden)]
#[deprecated = "renamed to `mineflayer_registry::data::DimensionKind`"]
pub type DimensionType = data::DimensionKind;

pub trait Registry: McBuf + PartialEq + PartialOrd + Ord + Copy + Hash
where
    Self: Sized,
{
    fn from_u32(value: u32) -> Option<Self>;
    fn to_u32(&self) -> u32;
}

/// A registry that might not be present.
///
/// This is transmitted as a single varint in the protocol.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OptionalRegistry<T: Registry>(pub Option<T>);

impl<T: Registry> McBuf for OptionalRegistry<T> {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        Ok(OptionalRegistry(match u32::mc_read_var(buf)? {
            0 => None,
            value => Some(
                T::from_u32(value - 1)
                    .ok_or(BufReadError::UnexpectedEnumVariant { id: value as i32 })?,
            ),
        }))
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        match &self.0 {
            None => 0u32.mc_write_var(buf),
            Some(value) => (value.to_u32() + 1).mc_write_var(buf),
        }
    }
}

/// A registry that will either take an ID or a resource location.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CustomRegistry<D: Registry, C: McBuf> {
    Direct(D),
    Custom(C),
}

impl<D: Registry, C: McBuf> McBuf for CustomRegistry<D, C> {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        let direct_registry = OptionalRegistry::<D>::mc_read(buf)?;
        if let Some(direct_registry) = direct_registry.0 {
            return Ok(CustomRegistry::Direct(direct_registry));
        }
        Ok(CustomRegistry::Custom(C::mc_read(buf)?))
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        match self {
            CustomRegistry::Direct(direct_registry) => {
                // write the id + 1
                (direct_registry.to_u32() + 1).mc_write_var(buf)
            }
            CustomRegistry::Custom(custom_registry) => {
                // write 0, then the custom registry
                0u32.mc_write_var(buf)?;
                custom_registry.mc_write(buf)
            }
        }
    }
}

#[derive(Clone, PartialEq)]
pub enum HolderSet<D: Registry, Identifier: McBuf> {
    Direct {
        contents: Vec<D>,
    },
    Named {
        key: Identifier,
        contents: Vec<Identifier>,
    },
}
impl<D: Registry, Identifier: McBuf> McBuf for HolderSet<D, Identifier> {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        let size = i32::mc_read_var(buf)?.wrapping_sub(1);
        if size == -1 {
            let key = Identifier::mc_read(buf)?;
            Ok(Self::Named {
                key,
                contents: Vec::new(),
            })
        } else {
            let mut contents = Vec::new();
            for _ in 0..size {
                contents.push(D::mc_read(buf)?);
            }
            Ok(Self::Direct { contents })
        }
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        match self {
            Self::Direct { contents } => {
                (contents.len() as i32 + 1).mc_write_var(buf)?;
                for item in contents {
                    item.mc_write(buf)?;
                }
            }
            Self::Named { key, contents: _ } => {
                0i32.mc_write_var(buf)?;
                key.mc_write(buf)?;
            }
        }
        Ok(())
    }
}
impl<D: Registry + Debug, Identifier: McBuf + Debug> Debug for HolderSet<D, Identifier> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Direct { contents } => f.debug_list().entries(contents).finish(),
            Self::Named { key, contents } => f
                .debug_struct("Named")
                .field("key", key)
                .field("contents", contents)
                .finish(),
        }
    }
}
impl<D: Registry, Identifier: McBuf> From<Vec<D>> for HolderSet<D, Identifier> {
    fn from(contents: Vec<D>) -> Self {
        Self::Direct { contents }
    }
}
#[cfg(feature = "serde")]
impl<D: Registry + Serialize, Identifier: McBuf + Serialize> Serialize
    for HolderSet<D, Identifier>
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Direct { contents } => {
                if contents.len() == 1 {
                    contents[0].serialize(serializer)
                } else {
                    contents.serialize(serializer)
                }
            }
            Self::Named { key, contents: _ } => key.serialize(serializer),
        }
    }
}
impl<D: Registry, Identifier: McBuf> Default for HolderSet<D, Identifier> {
    fn default() -> Self {
        Self::Direct {
            contents: Vec::new(),
        }
    }
}
impl<D: Registry, Identifier: McBuf + From<D> + PartialEq> HolderSet<D, Identifier> {
    pub fn contains(&self, value: D) -> bool {
        match self {
            HolderSet::Direct { contents } => contents.contains(&value),
            HolderSet::Named { key: _, contents } => contents.contains(&Identifier::from(value)),
        }
    }
}

/// A reference to either a registry or a custom value (usually something with
/// an `Identifier`).
pub enum Holder<R: Registry, Direct: McBuf> {
    Reference(R),
    Direct(Direct),
}
impl<R: Registry, Direct: McBuf> McBuf for Holder<R, Direct> {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        let id = u32::mc_read_var(buf)?;
        if id == 0 {
            Ok(Self::Direct(Direct::mc_read(buf)?))
        } else {
            let id = id - 1;
            let Some(value) = R::from_u32(id) else {
                return Err(BufReadError::UnexpectedEnumVariant { id: id as i32 });
            };
            Ok(Self::Reference(value))
        }
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        match self {
            Self::Reference(value) => (value.to_u32() + 1).mc_write_var(buf),
            Self::Direct(value) => {
                0u32.mc_write_var(buf)?;
                value.mc_write(buf)
            }
        }
    }
}
impl<R: Registry + Debug, Direct: McBuf + Debug> Debug for Holder<R, Direct> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reference(value) => f.debug_tuple("Reference").field(value).finish(),
            Self::Direct(value) => f.debug_tuple("Direct").field(value).finish(),
        }
    }
}
impl<R: Registry + Clone, Direct: McBuf + Clone> Clone for Holder<R, Direct> {
    fn clone(&self) -> Self {
        match self {
            Self::Reference(value) => Self::Reference(*value),
            Self::Direct(value) => Self::Direct(value.clone()),
        }
    }
}
impl<R: Registry + PartialEq, Direct: McBuf + PartialEq> PartialEq for Holder<R, Direct> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Reference(a), Self::Reference(b)) => a == b,
            (Self::Direct(a), Self::Direct(b)) => a == b,
            _ => false,
        }
    }
}
impl<R: Registry + Default, Direct: McBuf> Default for Holder<R, Direct> {
    fn default() -> Self {
        Self::Reference(R::default())
    }
}
#[cfg(feature = "serde")]
impl<R: Registry + Serialize, Direct: McBuf + Serialize> Serialize for Holder<R, Direct> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Reference(value) => value.serialize(serializer),
            Self::Direct(value) => value.serialize(serializer),
        }
    }
}

impl<R: Registry + FromNbtTag, Direct: McBuf + FromNbtTag> FromNbtTag for Holder<R, Direct> {
    fn from_nbt_tag(tag: NbtTag) -> Option<Self> {
        if let Some(reference) = R::from_nbt_tag(tag) {
            return Some(Self::Reference(reference));
        };
        Direct::from_nbt_tag(tag).map(Self::Direct)
    }
}

/// A registry which has its values decided by the server in the
/// `ClientboundRegistryData` packet.
///
/// These can be resolved into their actual values with
/// `ResolvableDataRegistry` from mineflayer-core.
pub trait DataRegistry: McBuf + PartialEq + PartialOrd + Ord + Copy + Hash {
    const NAME: &'static str;
    type Key: DataRegistryKey;

    fn protocol_id(&self) -> u32;
    fn new_raw(id: u32) -> Self;
}
pub trait DataRegistryKey {
    type Borrow<'a>: DataRegistryKeyRef<'a>;

    fn from_ident(ident: Identifier) -> Self;
    fn into_ident(self) -> Identifier;
}
pub trait DataRegistryKeyRef<'a> {
    type Owned: DataRegistryKey;

    fn to_owned(self) -> Self::Owned;
    fn from_ident(ident: &'a Identifier) -> Self;
    fn into_ident(self) -> Identifier;
}
impl<T: DataRegistry> Registry for T {
    fn from_u32(value: u32) -> Option<Self> {
        Some(Self::new_raw(value))
    }

    fn to_u32(&self) -> u32 {
        self.protocol_id()
    }
}
