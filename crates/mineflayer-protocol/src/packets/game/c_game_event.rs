use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundGameEvent {
    pub event: EventType,
    pub param: f32,
}

#[derive(McBuf, Clone, Copy, Debug, PartialEq)]
pub enum EventType {
    NoRespawnBlockAvailable = 0,
    StartRaining = 1,
    StopRaining = 2,
    ChangeGameMode = 3,
    WinGame = 4,
    DemoEvent = 5,
    ArrowHitPlayer = 6,
    RainLevelChange = 7,
    ThunderLevelChange = 8,
    PufferFishSting = 9,
    GuardianElderEffect = 10,
    ImmediateRespawn = 11,
    LimitedCrafting = 12,
    WaitForLevelChunks = 13,
}
