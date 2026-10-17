use mineflayer_brigadier::suggestion::Suggestions;
use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundCommandSuggestions {
    #[var]
    pub id: u32,
    pub suggestions: Suggestions,
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use mineflayer_brigadier::{context::StringRange, suggestion::Suggestion};
    use mineflayer_buf::McBuf;

    use super::*;

    #[test]
    fn test_suggestions() {
        let suggestions = Suggestions::new(
            StringRange::new(0, 5),
            vec![Suggestion::new_with_tooltip(
                StringRange::new(1, 4),
                "foo",
                "bar".to_owned(),
            )],
        );
        let mut buf = Vec::new();
        suggestions.mc_write(&mut buf).unwrap();
        let mut cursor = Cursor::new(&buf[..]);
        let suggestions = Suggestions::mc_read(&mut cursor).unwrap();
        assert_eq!(suggestions, suggestions);
    }
}
