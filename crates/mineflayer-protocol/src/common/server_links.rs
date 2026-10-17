use mineflayer_buf::McBuf;
use mineflayer_chat::FormattedText;

#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct ServerLinkEntry {
    pub kind: ServerLinkKind,
    pub link: String,
}

#[derive(McBuf, Clone, Debug, PartialEq)]
pub enum ServerLinkKind {
    Component(FormattedText),
    Known(KnownLinkKind),
}

#[derive(McBuf, Clone, Copy, Debug, PartialEq)]
pub enum KnownLinkKind {
    BugReport,
    CommunityGuidelines,
    Support,
    Status,
    Feedback,
    Community,
    Website,
    Forums,
    News,
    Announcements,
}
