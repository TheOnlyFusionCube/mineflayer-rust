use std::io::{self, Cursor, Write};

use mineflayer_buf::{McBuf, BufReadError};
use mineflayer_core::{
    direction::Direction,
    hit_result::BlockHitResult,
    position::{BlockPos, Vec3, Vec3f32},
};
use mineflayer_protocol_macros::ServerboundGamePacket;

use crate::packets::game::s_interact::InteractionHand;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundUseItemOn {
    pub hand: InteractionHand,
    pub block_hit: BlockHit,
    #[var]
    pub seq: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BlockHit {
    /// The block that we clicked.
    pub block_pos: BlockPos,
    /// The face of the block that was clicked.
    pub direction: Direction,
    /// The exact coordinates of the world where the block was clicked.
    ///
    /// In the network, this is transmitted as the difference between the
    /// location and block position.
    pub location: Vec3,
    /// Whether the player's head is inside a block.
    pub inside: bool,
    /// Whether the player's hitting the world border.
    pub world_border: bool,
}

impl McBuf for BlockHit {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        let block_pos = BlockPos::mc_read(buf)?;
        let direction = Direction::mc_read(buf)?;
        let cursor = Vec3f32::mc_read(buf)?;
        let inside = bool::mc_read(buf)?;
        let world_border = bool::mc_read(buf)?;
        Ok(Self {
            block_pos,
            direction,
            location: Vec3 {
                x: f64::from(block_pos.x) + f64::from(cursor.x),
                y: f64::from(block_pos.y) + f64::from(cursor.y),
                z: f64::from(block_pos.z) + f64::from(cursor.z),
            },
            inside,
            world_border,
        })
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        self.block_pos.mc_write(buf)?;
        self.direction.mc_write(buf)?;
        let cursor = Vec3 {
            x: self.location.x - f64::from(self.block_pos.x),
            y: self.location.y - f64::from(self.block_pos.y),
            z: self.location.z - f64::from(self.block_pos.z),
        };
        Vec3f32::from(cursor).mc_write(buf)?;
        self.inside.mc_write(buf)?;
        self.world_border.mc_write(buf)?;
        Ok(())
    }
}

impl From<&BlockHitResult> for BlockHit {
    /// Converts a [`BlockHitResult`] to a [`BlockHit`].
    ///
    /// The only difference is that the `miss` field is not present in
    /// [`BlockHit`].
    fn from(hit_result: &BlockHitResult) -> Self {
        Self {
            block_pos: hit_result.block_pos,
            direction: hit_result.direction,
            location: hit_result.location,
            inside: hit_result.inside,
            world_border: hit_result.world_border,
        }
    }
}
