use mineflayer_buf::McBuf;
use mineflayer_core::sound::CustomSound;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_registry::builtin::SoundEvent;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundSound {
    pub sound: mineflayer_registry::Holder<SoundEvent, CustomSound>,
    pub source: SoundSource,
    // this can't be a BlockPos because it serializes differently :(
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub volume: f32,
    pub pitch: f32,
    pub seed: u64,
}

#[derive(McBuf, Clone, Copy, Debug, PartialEq)]
pub enum SoundSource {
    Master = 0,
    Music = 1,
    Records = 2,
    Weather = 3,
    Blocks = 4,
    Hostile = 5,
    Neutral = 6,
    Players = 7,
    Ambient = 8,
    Voice = 9,
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use mineflayer_buf::McBuf;

    use crate::packets::game::ClientboundSound;

    #[test]
    fn test_read_write_custom_sound() {
        let contents = [
            0, 21, 109, 105, 110, 101, 99, 114, 97, 102, 116, 58, 97, 115, 102, 97, 115, 100, 102,
            115, 100, 102, 103, 0, 8, 0, 0, 0, 63, 255, 255, 254, 32, 0, 0, 0, 82, 66, 200, 0, 0,
            63, 128, 0, 0, 71, 94, 219, 133, 200, 13, 150, 31,
        ];
        let mut buf = Cursor::new(contents.as_slice());
        let packet = ClientboundSound::mc_read(&mut buf).unwrap();
        println!("{packet:?}");

        assert_eq!(buf.position(), contents.len() as u64);

        let mut buf = Vec::new();
        packet.write(&mut buf).unwrap();
        assert_eq!(buf, contents);
    }
}
