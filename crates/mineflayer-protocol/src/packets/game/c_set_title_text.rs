use mineflayer_buf::McBuf;
use mineflayer_chat::FormattedText;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundSetTitleText {
    pub text: FormattedText,
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use simdnbt::owned::{BaseNbt, NbtCompound, NbtTag};

    use super::*;

    #[test]
    fn a_keybind_title_decodes_to_the_key_name() {
        let mut bytes = Vec::new();
        BaseNbt::from(NbtCompound::from_values(vec![(
            "keybind".into(),
            NbtTag::String("key.jump".into()),
        )]))
        .write_unnamed(&mut bytes);

        let packet = ClientboundSetTitleText::mc_read(&mut Cursor::new(&bytes)).unwrap();
        assert_eq!(packet.text.to_string(), "key.jump");
    }
}
