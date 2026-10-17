use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_registry::data::WorldClock;
use indexmap::IndexMap;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundSetTime {
    pub game_time: u64,
    pub clock_updates: IndexMap<WorldClock, ClockState>,
}

#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct ClockState {
    #[var]
    pub total_ticks: u64,
    pub partial_tick: f32,
    pub rate: f32,
}
