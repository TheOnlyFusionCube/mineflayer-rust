use std::io::{self, Cursor, Write};

use mineflayer_buf::{McBuf, BufReadError};
use mineflayer_core::{color::RgbColor, position::Vec3i};
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_registry::identifier::Identifier;
use uuid::Uuid;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundWaypoint {
    pub operation: WaypointOperation,
    pub waypoint: TrackedWaypoint,
}

#[derive(McBuf, Clone, Copy, Debug, PartialEq)]
pub enum WaypointOperation {
    Track,
    Untrack,
    Update,
}

#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct TrackedWaypoint {
    pub identifier: WaypointIdentifier,
    pub icon: WaypointIcon,
    pub data: WaypointData,
}

#[derive(McBuf, Clone, Debug, PartialEq)]
pub enum WaypointIdentifier {
    String(String),
    Uuid(Uuid),
}

#[derive(Clone, Debug, PartialEq)]
pub struct WaypointIcon {
    pub style: Identifier,
    pub color: Option<RgbColor>,
}
impl McBuf for WaypointIcon {
    fn mc_write(&self, buf: &mut impl Write) -> Result<(), io::Error> {
        self.style.mc_write(buf)?;
        let color = self.color.map(|c| CompactRgbColor {
            r: c.red(),
            g: c.green(),
            b: c.blue(),
        });
        color.mc_write(buf)?;
        Ok(())
    }
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        let style = Identifier::mc_read(buf)?;
        let color = Option::<CompactRgbColor>::mc_read(buf)?;
        let color = color.map(|c| RgbColor::new(c.r, c.g, c.b));

        Ok(Self { style, color })
    }
}

// usually RgbColor is encoded as 4 bytes, except here where it's 3
#[derive(McBuf)]
struct CompactRgbColor {
    r: u8,
    g: u8,
    b: u8,
}

#[derive(McBuf, Clone, Debug, PartialEq)]
pub enum WaypointData {
    Empty,
    Vec3i(Vec3i),
    Chunk {
        #[var]
        x: i32,
        #[var]
        z: i32,
    },
    Azimuth {
        angle: f32,
    },
}
