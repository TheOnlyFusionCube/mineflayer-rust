use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundDebugSample {
    pub sample: Vec<u64>,
    pub debug_sample_type: RemoteDebugSampleType,
}

#[derive(McBuf, Clone, Copy, Debug, PartialEq)]
pub enum RemoteDebugSampleType {
    TickTime,
}
