use mineflayer_buf::McBuf;
use mineflayer_chat::FormattedText;
use mineflayer_core::position::{BlockPos, Vec3i};
use mineflayer_protocol_macros::ServerboundGamePacket;
use mineflayer_registry::builtin::TestInstanceKind;

use super::s_set_structure_block::Rotation;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundTestInstanceBlockAction {
    pub pos: BlockPos,
    pub action: Action,
    pub data: TestInstanceBlockEntityData,
}

#[derive(McBuf, Clone, Copy, Debug, Default, PartialEq)]
pub enum Action {
    #[default]
    Init,
    Query,
    Set,
    Reset,
    Save,
    Export,
    Run,
}

#[derive(McBuf, Clone, Debug, Default, PartialEq)]
pub struct TestInstanceBlockEntityData {
    pub test: Option<TestInstanceKind>,
    pub size: Vec3i,
    pub rotation: Rotation,
    pub ignore_entities: bool,
    pub status: TestInstanceBlockEntityStatus,
    pub error_message: Option<FormattedText>,
}

#[derive(McBuf, Clone, Copy, Debug, Default, PartialEq)]
pub enum TestInstanceBlockEntityStatus {
    #[default]
    Cleared,
    Running,
    Finished,
}
