//! Block entities: the NBT the server sends for signs, chests, banners and
//! the other blocks with data, kept by position. The engine does not store
//! them (D-redecode), so the world observer decodes the chunk and
//! `block_entity_data` packets into a component on the client entity.

use std::{collections::HashMap, io::Cursor};

use mineflayer_engine::{
    BlockPos, FormattedText,
    core::position::ChunkPos,
    ecs::{self as bevy_ecs, prelude::Component},
    protocol::{
        packets::game::ClientboundGamePacket,
        simdnbt::{
            self,
            owned::{Nbt, NbtCompound, NbtTag},
        },
    },
    registry::builtin::BlockEntityKind,
};

use super::Block;
use crate::{Bot, bot::with_component_mut};

/// `block.blockEntity` — a block entity's kind and NBT data.
#[derive(Clone, Debug, PartialEq)]
pub struct BlockEntity {
    /// What the block entity is (`minecraft:sign`, `minecraft:chest`, …).
    pub kind: BlockEntityKind,
    /// The data the server sent, without the position fields.
    pub data: NbtCompound,
}

/// The block entities of the loaded chunks.
#[derive(Component, Default)]
pub(crate) struct BlockEntities(HashMap<BlockPos, BlockEntity>);

/// Keep the block entities that `packet` sends, and drop those of a chunk
/// that it unloads. Login and respawn drop all, because they change the
/// world.
pub(super) fn observe(client: &mineflayer_engine::Client, packet: &ClientboundGamePacket) {
    use ClientboundGamePacket as P;
    if !matches!(
        packet,
        P::LevelChunkWithLight(_)
            | P::BlockEntityData(_)
            | P::ForgetLevelChunk(_)
            | P::Login(_)
            | P::Respawn(_)
    ) {
        return;
    }
    with_component_mut::<BlockEntities, _>(client, |BlockEntities(map)| match packet {
        P::LevelChunkWithLight(p) => {
            let chunk = ChunkPos::new(p.x, p.z);
            map.retain(|pos, _| ChunkPos::from(*pos) != chunk);
            map.extend(p.chunk_data.block_entities.iter().filter_map(|e| {
                let pos = BlockPos::new(
                    chunk.x * 16 + i32::from(e.packed_xz >> 4),
                    i32::from(e.y as i16),
                    chunk.z * 16 + i32::from(e.packed_xz & 15),
                );
                Some((pos, entity(e.kind, &e.data)?))
            }));
        }
        P::BlockEntityData(p) => match entity(p.block_entity_type, &p.tag) {
            Some(entity) => {
                map.insert(p.pos, entity);
            }
            None => {
                map.remove(&p.pos);
            }
        },
        P::ForgetLevelChunk(p) => map.retain(|pos, _| ChunkPos::from(*pos) != p.pos),
        _ => map.clear(),
    });
}

/// Drop the block entity at `pos`: its block changed to another kind.
pub(super) fn forget(client: &mineflayer_engine::Client, pos: BlockPos) {
    with_component_mut::<BlockEntities, _>(client, |entities| {
        entities.0.remove(&pos);
    });
}

fn entity(kind: BlockEntityKind, data: &Nbt) -> Option<BlockEntity> {
    match data {
        Nbt::Some(base) => Some(BlockEntity {
            kind,
            data: base.clone().as_compound(),
        }),
        Nbt::None => None,
    }
}

impl Block {
    /// `block.blockEntity` — `None` when the block has none, or its chunk
    /// sent none.
    pub fn block_entity(&self) -> Option<BlockEntity> {
        crate::bot::with_component::<BlockEntities, _>(&self.world.client, |entities| {
            entities.0.get(&self.pos).cloned()
        })
        .flatten()
    }
}

impl Bot {
    /// `block.getSignText()` for one side: the four lines of the front
    /// (or, with `back`, the back) text of a sign or hanging sign, joined
    /// with `\n`. `None` when the block has no sign data.
    pub fn sign_text(&self, block: &Block, back: bool) -> Option<String> {
        let entity = block.block_entity()?;
        let side = entity
            .data
            .compound(if back { "back_text" } else { "front_text" })?;
        let lines: Vec<String> = side
            .list("messages")?
            .as_nbt_tags()
            .iter()
            .map(|line| text(line).map(|t| t.to_string()).unwrap_or_default())
            .collect();
        Some(lines.join("\n"))
    }
}

/// A chat component in NBT form, through the engine's decoder.
fn text(tag: &NbtTag) -> Option<FormattedText> {
    let mut bytes = Vec::new();
    tag.write(&mut bytes);
    let base = simdnbt::borrow::read_tag(&mut Cursor::new(&bytes)).ok()?;
    simdnbt::FromNbtTag::from_nbt_tag(base.as_tag())
}

#[cfg(test)]
mod tests {
    use mineflayer_engine::{
        block::BlockState,
        protocol::{
            packets::game::{
                ClientboundBlockEntityData, ClientboundBlockUpdate,
                c_level_chunk_with_light::BlockEntity as WireBlockEntity,
            },
            simdnbt::owned::{BaseNbt, NbtList},
        },
        registry::builtin::BlockKind,
        test_utils::simulation::make_basic_empty_chunk,
    };

    use super::*;
    use crate::test_util::sim::BotSim;

    fn sign(front: &[&str], back: NbtTag) -> Nbt {
        let side = |messages: NbtTag| {
            NbtTag::Compound(NbtCompound::from_values(vec![(
                "messages".into(),
                messages,
            )]))
        };
        let front = NbtTag::List(NbtList::String(
            front.iter().map(|line| (*line).into()).collect(),
        ));
        Nbt::Some(BaseNbt::new(
            "",
            NbtCompound::from_values(vec![
                ("front_text".into(), side(front)),
                ("back_text".into(), side(back)),
            ]),
        ))
    }

    fn back_lines() -> NbtTag {
        // a styled component in NBT form, as servers send it
        let styled = NbtCompound::from_values(vec![
            ("text".into(), NbtTag::String("bold".into())),
            ("bold".into(), NbtTag::Byte(1)),
        ]);
        NbtTag::List(NbtList::Compound(vec![
            styled,
            NbtCompound::new(),
            NbtCompound::new(),
            NbtCompound::new(),
        ]))
    }

    #[test]
    fn sign_text_from_the_chunk_then_from_an_update() {
        let mut sim = BotSim::new();
        let pos = BlockPos::new(1, -3, 2);
        let mut chunk = make_basic_empty_chunk(ChunkPos::new(0, 0), 384 / 16);
        chunk.chunk_data.block_entities.push(WireBlockEntity {
            packed_xz: (1 << 4) | 2,
            y: -3_i16 as u16,
            kind: BlockEntityKind::Sign,
            data: sign(&["hello", "world", "", ""], back_lines()),
        });
        sim.receive(chunk);
        sim.tick();
        let block = sim.bot.block_at(pos).expect("a loaded block");
        assert_eq!(
            block.block_entity().map(|e| e.kind),
            Some(BlockEntityKind::Sign)
        );
        assert_eq!(
            sim.bot.sign_text(&block, false).as_deref(),
            Some("hello\nworld\n\n")
        );
        assert_eq!(
            sim.bot.sign_text(&block, true).as_deref(),
            Some("bold\n\n\n")
        );

        sim.receive(ClientboundBlockEntityData {
            pos,
            block_entity_type: BlockEntityKind::Sign,
            tag: sign(&["edited", "", "", ""], back_lines()),
        });
        sim.tick();
        assert_eq!(
            sim.bot.sign_text(&block, false).as_deref(),
            Some("edited\n\n\n")
        );

        // the sign is broken: its data goes with it
        sim.receive(ClientboundBlockUpdate {
            pos,
            block_state: BlockState::from(BlockKind::Stone),
        });
        sim.tick();
        assert_eq!(block.block_entity(), None);
        assert_eq!(sim.bot.sign_text(&block, false), None);
    }
}
