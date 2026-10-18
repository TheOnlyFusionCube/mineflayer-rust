//! Chat — the message-family surface: `bot.chat`/`whisper` with upstream's
//! length splitting, chat patterns (`chat:{name}` events), `await_message`,
//! `tab_complete`, and the tab-list header/footer.
//!
//! Incoming messages are normalized by `ChatShared::message_events`, which
//! the bridge calls for every chat packet and for the dedicated action-bar
//! packet (the engine drops `c_set_action_bar_text`; upstream routes game_info
//! text through `message`/`messagestr`/`actionBar`, so we synthesize an
//! overlay [`ChatPacket::System`] to take the same path).

use std::{
    collections::{BTreeMap, HashMap},
    time::Duration,
};

use mineflayer_engine::{
    FormattedText, client_chat::ChatPacket,
    protocol::packets::game::s_command_suggestion::ServerboundCommandSuggestion,
};
use regex::Regex;
use tokio::sync::{broadcast, oneshot};

use crate::bot::wait::await_map;
use crate::{Bot, BotEvent, Error, Result};

/// Upstream's `CHAT_LENGTH_LIMIT` for >=1.11 (`options.chatLengthLimit`
/// default; the engine targets mc26 so the pre-1.11 100-char limit never applies).
pub(crate) const CHAT_LENGTH_LIMIT: usize = 256;

/// Upstream's default `awaitMessage` / `tabComplete` timeout behavior
/// (20s for awaitMessage, 5s for tabComplete).
const AWAIT_MESSAGE_TIMEOUT: Duration = Duration::from_secs(20);
const TAB_COMPLETE_TIMEOUT: Duration = Duration::from_secs(5);

/// The `position` argument of upstream's `message`/`messagestr` events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessagePosition {
    /// `chat` — a player (or disguised-player) chat message.
    Chat,
    /// `system` — a server/system message in the chat area.
    System,
    /// `game_info` — action-bar/overlay text.
    GameInfo,
}

/// The payload of `chat:{name}` events — what the matched patterns produced.
#[derive(Clone, Debug, PartialEq)]
pub enum ChatPatternMatch {
    /// `parse: false` — the whole message strings that matched each pattern,
    /// in pattern order.
    Messages(Vec<String>),
    /// `parse: true` — per-pattern capture groups (the full match is dropped,
    /// like upstream's `[, ...matches]` destructure). A group is `None` when
    /// it didn't participate.
    Groups(Vec<Vec<Option<String>>>),
}

/// Options for [`Bot::add_chat_pattern`] / [`Bot::add_chat_pattern_set`].
/// Upstream's `opts` — `deprecated` is skipped: it exists upstream only to
/// emit the built-in `chat`/`whisper` patterns, which [`BotEvent::Whisper`]
/// covers via [`ChatPacket::is_whisper`].
#[derive(Clone, Copy, Debug)]
pub struct ChatPatternOpts {
    /// Re-arm the pattern after a complete match (upstream default `true`).
    /// `false` removes it after the first completion.
    pub repeat: bool,
    /// Emit capture groups ([`ChatPatternMatch::Groups`]) instead of the
    /// whole matched messages (upstream default `false`).
    pub parse: bool,
}

impl Default for ChatPatternOpts {
    fn default() -> Self {
        Self {
            repeat: true,
            parse: false,
        }
    }
}

/// A registered chat pattern set — the [`Bot::chat_patterns`] element.
/// Upstream exposes `{pattern, type, description}`; ours reflects the
/// set-based registry (`name` is the `chat:{name}` event suffix, `id` the
/// value returned by `add_chat_pattern*`).
#[derive(Clone, Debug)]
pub struct ChatPattern {
    /// The id returned by `add_chat_pattern`/`add_chat_pattern_set`.
    pub id: usize,
    /// The event suffix — matches emit `BotEvent::ChatPattern { name, .. }`.
    pub name: String,
    /// The pattern sequence; `patterns[position]` must match the next
    /// message before the set completes.
    pub patterns: Vec<Regex>,
    /// Whether the set re-arms after completing.
    pub repeat: bool,
    /// Whether completion emits capture groups.
    pub parse: bool,
}

/// Key for [`Bot::remove_chat_pattern`] — upstream's `name|id` union.
#[derive(Clone, Debug)]
pub enum ChatPatternKey {
    /// Remove the single set with this id.
    Id(usize),
    /// Remove every set with this name (upstream removes all matches).
    Name(String),
}

impl From<usize> for ChatPatternKey {
    fn from(id: usize) -> Self {
        Self::Id(id)
    }
}
impl From<&str> for ChatPatternKey {
    fn from(name: &str) -> Self {
        Self::Name(name.to_string())
    }
}
impl From<String> for ChatPatternKey {
    fn from(name: String) -> Self {
        Self::Name(name)
    }
}

/// One `awaitMessage` argument — upstream accepts strings, string arrays,
/// and regexps; a string array is "any of".
#[derive(Clone, Debug)]
pub enum MessageMatcher {
    /// `awaitMessage("text")` — whole-string equality.
    Exact(String),
    /// `awaitMessage(["a", "b"])` — equality against any element.
    OneOf(Vec<String>),
    /// `awaitMessage(/re/)` — regex test.
    Regex(Regex),
}

impl MessageMatcher {
    fn matches(&self, message: &str) -> bool {
        match self {
            Self::Exact(want) => message == want,
            Self::OneOf(wants) => wants.iter().any(|want| message == want),
            Self::Regex(re) => re.is_match(message),
        }
    }
}

impl From<&str> for MessageMatcher {
    fn from(s: &str) -> Self {
        Self::Exact(s.to_string())
    }
}
impl From<String> for MessageMatcher {
    fn from(s: String) -> Self {
        Self::Exact(s)
    }
}
impl From<Regex> for MessageMatcher {
    fn from(re: Regex) -> Self {
        Self::Regex(re)
    }
}
impl From<Vec<String>> for MessageMatcher {
    fn from(v: Vec<String>) -> Self {
        Self::OneOf(v)
    }
}
impl From<Vec<&str>> for MessageMatcher {
    fn from(v: Vec<&str>) -> Self {
        Self::OneOf(v.into_iter().map(str::to_string).collect())
    }
}

/// `bot.tablist` — the tab-list header and footer, updated whenever the
/// server sends `c_tab_list` (which the engine drops; the bridge stores it).
/// Both fields are empty until the first packet arrives, like upstream's
/// `new ChatMessage('')` initializer.
#[derive(Clone, Debug, Default)]
pub struct TabList {
    /// `bot.tablist.header`
    pub header: FormattedText,
    /// `bot.tablist.footer`
    pub footer: FormattedText,
}

/// `ChatMessage` (prismarine-chat) — a chat component with upstream's
/// accessors: `toString`/`toAnsi`/`toHtml`, `text`/`translate`/`with`/`extra`,
/// `ChatMessage.fromNotch`.
///
/// A thin wrapper over the engine's [`FormattedText`] (`inner` is upstream's
/// `.json` — the parsed component). `Display`/`to_ansi`/`to_html` render it;
/// translatable components resolve through the bundled en_us strings
/// (mineflayer-language), the same default upstream's `toString()` uses.
///
/// [`ChatMessage::parse`] fails on `score`/`selector`/`keybind`/`nbt`/
/// `object` components — the engine's deserializer doesn't support them yet
/// (prismarine-chat does).
#[derive(Clone, Debug, PartialEq)]
pub struct ChatMessage {
    /// The parsed component (upstream's `.json`).
    pub inner: FormattedText,
}

impl ChatMessage {
    /// `new ChatMessage(component)` — wrap an already-parsed component.
    /// Chat packets arrive as `FormattedText`, so `ChatMessage::from(packet.message())`
    /// is the common path.
    pub fn new(inner: impl Into<FormattedText>) -> Self {
        Self {
            inner: inner.into(),
        }
    }

    /// `new ChatMessage(json)` — parse raw chat JSON (a string, object, or
    /// array form), like upstream's constructor.
    pub fn parse(json: &str) -> std::result::Result<Self, serde_json::Error> {
        serde_json::from_str::<FormattedText>(json).map(Self::new)
    }

    /// `ChatMessage.fromNotch(str)` — parse a legacy `§`-coded string.
    pub fn from_notch(text: &str) -> Self {
        Self::new(
            mineflayer_engine::chat::text_component::legacy_color_code_to_text_component(text),
        )
    }

    /// `msg.toAnsi()` — the plain-text render with ANSI style escapes.
    pub fn to_ansi(&self) -> String {
        self.inner.to_ansi()
    }

    /// `msg.toHtml()`.
    pub fn to_html(&self) -> String {
        self.inner.to_html()
    }

    /// `msg.text` — the literal text, when this is a text component.
    pub fn text(&self) -> Option<&str> {
        match &self.inner {
            FormattedText::Text(c) => Some(&c.text),
            _ => None,
        }
    }

    /// `msg.translate` — the translation key, when this is a translatable
    /// component.
    pub fn translate(&self) -> Option<&str> {
        match &self.inner {
            FormattedText::Translatable(c) => Some(&c.key),
            _ => None,
        }
    }

    /// `msg.with` — the translation arguments, when translatable.
    pub fn with(
        &self,
    ) -> Option<&[mineflayer_engine::chat::translatable_component::PrimitiveOrComponent]> {
        match &self.inner {
            FormattedText::Translatable(c) => Some(&c.args),
            _ => None,
        }
    }

    /// `msg.extra` — the sibling components.
    pub fn extra(&self) -> &[FormattedText] {
        &self.inner.get_base().siblings
    }

    /// `msg`'s style — upstream's `color`/`bold`/`clickEvent`/`hoverEvent`
    /// fields all live on the engine's `Style`.
    pub fn style(&self) -> &mineflayer_engine::chat::style::Style {
        &self.inner.get_base().style
    }

    /// Unwrap into the engine [`FormattedText`].
    pub fn into_inner(self) -> FormattedText {
        self.inner
    }
}

impl From<FormattedText> for ChatMessage {
    fn from(inner: FormattedText) -> Self {
        Self::new(inner)
    }
}
impl From<ChatMessage> for FormattedText {
    fn from(msg: ChatMessage) -> Self {
        msg.inner
    }
}
/// `msg.toString()` — plain text (translatable components rendered through
/// en_us, upstream's default language argument). `to_string` comes from
/// [`ToString`], like upstream's method.
impl std::fmt::Display for ChatMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.inner.fmt(f)
    }
}

/// A single registered pattern set and its in-flight match progress —
/// upstream's `_patterns[ix]` entry.
#[derive(Debug)]
struct ChatPatternEntry {
    name: String,
    patterns: Vec<Regex>,
    /// Index into `patterns` of the regex the NEXT message must match.
    position: usize,
    /// Messages that have matched `patterns[..position]` so far.
    matches: Vec<String>,
    repeat: bool,
    parse: bool,
}

/// The pattern registry — upstream's `_patterns` object: ids are
/// monotonically increasing and never reused; entries are removed, not
/// compacted (a `BTreeMap` gives upstream's ascending-id iteration order).
#[derive(Debug, Default)]
pub(crate) struct ChatPatterns {
    entries: BTreeMap<usize, ChatPatternEntry>,
    next_id: usize,
}

impl ChatPatterns {
    pub(crate) fn add(&mut self, name: &str, patterns: Vec<Regex>, opts: ChatPatternOpts) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        self.entries.insert(
            id,
            ChatPatternEntry {
                name: name.to_string(),
                patterns,
                position: 0,
                matches: Vec::new(),
                repeat: opts.repeat,
                parse: opts.parse,
            },
        );
        id
    }

    pub(crate) fn remove(&mut self, key: &ChatPatternKey) {
        match key {
            ChatPatternKey::Id(id) => {
                self.entries.remove(id);
            }
            ChatPatternKey::Name(name) => {
                self.entries.retain(|_, entry| entry.name != *name);
            }
        }
    }

    pub(crate) fn list(&self) -> Vec<ChatPattern> {
        self.entries
            .iter()
            .map(|(&id, e)| ChatPattern {
                id,
                name: e.name.clone(),
                patterns: e.patterns.clone(),
                repeat: e.repeat,
                parse: e.parse,
            })
            .collect()
    }

    /// Run one messagestr string past every pattern set, in id order —
    /// upstream's `messagestr` listener. Returns the `chat:{name}` events
    /// for sets that completed on this message.
    fn handle_message(&mut self, message: &str) -> Vec<BotEvent> {
        // Collect the sets whose next pattern matches this message first;
        // completing/removing a set below must not disturb the scan.
        let matched: Vec<usize> = self
            .entries
            .iter()
            .filter(|(_, e)| {
                e.patterns
                    .get(e.position)
                    .is_some_and(|re| re.is_match(message))
            })
            .map(|(&id, _)| id)
            .collect();
        let mut events = Vec::new();
        for id in matched {
            let entry = self.entries.get_mut(&id).expect("id just scanned");
            entry.matches.push(message.to_string());
            entry.position += 1;
            if entry.patterns.len() > entry.matches.len() {
                continue; // set incomplete — wait for the next message
            }
            let matches = if entry.parse {
                ChatPatternMatch::Groups(
                    entry
                        .patterns
                        .iter()
                        .zip(entry.matches.iter())
                        .map(|(re, m)| {
                            re.captures(m)
                                .map(|c| {
                                    c.iter()
                                        .skip(1)
                                        .map(|g| g.map(|m| m.as_str().to_string()))
                                        .collect()
                                })
                                .unwrap_or_default()
                        })
                        .collect(),
                )
            } else {
                ChatPatternMatch::Messages(std::mem::take(&mut entry.matches))
            };
            events.push(BotEvent::ChatPattern {
                name: entry.name.clone(),
                matches,
            });
            if entry.repeat {
                entry.position = 0;
                entry.matches.clear();
            } else {
                self.entries.remove(&id);
            }
        }
        events
    }
}

/// Chat-domain state shared between `Bot` and the bridge task — the reason
/// `Bot` is `Arc<Mutex<..>>` rather than purely a handle: `add_chat_pattern`
/// on any `Bot` clone must reach the matcher that runs inside the bridge,
/// and `tab_complete` replies arrive there too.
#[derive(Debug)]
pub(crate) struct ChatShared {
    pub(crate) patterns: ChatPatterns,
    pub(crate) tablist: TabList,
    /// Correlation counter for `tab_complete` — `s_command_suggestion` carries
    /// a transaction id the server echoes in `c_command_suggestions`.
    next_completion_id: u32,
    completion_pending: HashMap<u32, oneshot::Sender<Vec<String>>>,
    /// `options.defaultChatPatterns` — upstream's flag controls the legacy
    /// `chat`/`whisper` pattern registration; here it gates
    /// [`BotEvent::Whisper`], the `is_whisper`-derived stand-in for the
    /// whisper patterns (`BotEvent::Chat` is the raw packet stream and is
    /// already a documented divergence from upstream's pattern-sourced
    /// `chat` event).
    pub(crate) default_chat_patterns: bool,
}

impl Default for ChatShared {
    /// `options.defaultChatPatterns` defaults to `true` upstream.
    fn default() -> Self {
        Self {
            patterns: ChatPatterns::default(),
            tablist: TabList::default(),
            next_completion_id: 0,
            completion_pending: HashMap::new(),
            default_chat_patterns: true,
        }
    }
}

impl ChatShared {
    /// The upstream message-family events for one incoming chat packet, in
    /// emission order: `message`, `messagestr`, then (inside the messagestr
    /// step, like upstream) `whisper` and completed `chat:{name}` events,
    /// then `actionBar` for game_info text.
    pub(crate) fn message_events(&mut self, packet: &ChatPacket) -> Vec<BotEvent> {
        let position = match packet {
            ChatPacket::Player(_) | ChatPacket::Disguised(_) => MessagePosition::Chat,
            ChatPacket::System(p) if p.overlay => MessagePosition::GameInfo,
            ChatPacket::System(_) => MessagePosition::System,
        };
        let message = packet.message().to_string();
        let mut out = vec![
            BotEvent::Message {
                packet: packet.clone(),
                position,
            },
            BotEvent::MessageStr {
                message: message.clone(),
                position,
                packet: packet.clone(),
            },
        ];
        if packet.is_whisper() && self.default_chat_patterns {
            out.push(BotEvent::Whisper(packet.clone()));
        }
        out.extend(self.patterns.handle_message(&message));
        if position == MessagePosition::GameInfo {
            out.push(BotEvent::ActionBar(packet.message()));
        }
        out
    }

    /// Resolve a pending `tab_complete` call — the bridge calls this for
    /// every `c_command_suggestions` packet; ids we didn't register are
    /// ignored. A receiver that already timed out makes `send` fail quietly.
    pub(crate) fn complete_suggestions(&mut self, id: u32, matches: Vec<String>) {
        if let Some(tx) = self.completion_pending.remove(&id) {
            let _ = tx.send(matches);
        }
    }

    /// Register a pending `tab_complete` call, returning the transaction id
    /// to send and the receiver the bridge resolves.
    pub(crate) fn register_completion(&mut self) -> (u32, oneshot::Receiver<Vec<String>>) {
        let id = self.next_completion_id;
        self.next_completion_id = self.next_completion_id.wrapping_add(1);
        let (tx, rx) = oneshot::channel();
        self.completion_pending.insert(id, tx);
        (id, rx)
    }
}

/// Drop guard for one `tab_complete` call: removes its correlation entry
/// when the call ends for any reason. A reply already removed it, so the
/// second remove is a no-op.
struct PendingCompletion<'a> {
    chat: &'a parking_lot::Mutex<ChatShared>,
    id: u32,
}

impl Drop for PendingCompletion<'_> {
    fn drop(&mut self) {
        self.chat.lock().completion_pending.remove(&self.id);
    }
}

/// Split `message` for sending with a `header` prefix — upstream's
/// `chatWithHeader`. Commands (leading `/`, empty header) pass through
/// unsplit; everything else is split on newlines (empty lines dropped) and
/// chunked so each part — header included — fits `limit`
/// (`options.chatLengthLimit`, default [`CHAT_LENGTH_LIMIT`]).
fn split_chat_message(header: &str, message: &str, limit: usize) -> Vec<String> {
    if header.is_empty() && message.starts_with('/') {
        return vec![message.to_string()];
    }
    let limit = limit.saturating_sub(header.chars().count()).max(1);
    let mut out = Vec::new();
    for sub in message.split('\n') {
        if sub.is_empty() {
            continue;
        }
        let mut chars = sub.chars();
        loop {
            let chunk: String = chars.by_ref().take(limit).collect();
            if chunk.is_empty() {
                break;
            }
            out.push(format!("{header}{chunk}"));
        }
    }
    out
}

/// The `await_message` receive loop, factored off `Bot` so tests can feed a
/// synthetic event stream.
async fn await_message_on(
    mut rx: broadcast::Receiver<BotEvent>,
    matchers: &[MessageMatcher],
) -> Result<String> {
    Ok(await_map(&mut rx, AWAIT_MESSAGE_TIMEOUT, |e| match e {
        BotEvent::MessageStr { message, .. }
            if matchers.iter().any(|m| m.matches(message.as_str())) =>
        {
            Some(message.clone())
        }
        _ => None,
    })
    .await?)
}

impl Bot {
    /// `bot.chat(message)` — send a chat message, or a command when it starts
    /// with `/`. Long messages and `\n` newlines are split into multiple
    /// sends like upstream (256-char limit by default —
    /// `options.chatLengthLimit` via [`crate::BotBuilder::chat_length_limit`];
    /// commands never split).
    ///
    /// Errors when not in-game, matching upstream throwing before `login` or
    /// after an early disconnect.
    ///
    /// ```no_run
    /// use mineflayer::prelude::*;
    ///
    /// # fn example(bot: Bot) -> Result<()> {
    /// bot.chat("hello")?;
    /// bot.chat("/time set day")?; // a command: never split
    /// bot.whisper("Steve", "meet at spawn")?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn chat(&self, message: &str) -> Result<()> {
        self.chat_with_header("", message)
    }

    /// `bot.whisper(username, message)` — `/tell username message`, split
    /// with the `/tell username ` prefix counting against the length limit.
    pub fn whisper(&self, username: &str, message: &str) -> Result<()> {
        self.chat_with_header(&format!("/tell {username} "), message)
    }

    /// Shared send path for [`Bot::chat`]/[`Bot::whisper`] — upstream's
    /// `chatWithHeader`.
    fn chat_with_header(&self, header: &str, message: &str) -> Result<()> {
        if !self.logged_in() {
            // Upstream splits this guard on its stored `endReason`: a
            // call before play state and a call after a pre-play
            // disconnect are different errors (and the latter names the
            // reason).
            if let Some(reason) = self.shared.end_reason.lock().clone() {
                return Err(Error::InvalidState(format!(
                    "bot.chat() called after the client disconnected before \
                     entering the play state ({reason})"
                )));
            }
            return Err(Error::InvalidState(
                "bot.chat() called before the client entered the play state; \
                 wait for the \"login\" or \"spawn\" event"
                    .to_string(),
            ));
        }
        for part in split_chat_message(header, message, self.chat_length_limit) {
            if part.starts_with('/') {
                // commands go through the engine's command path (leading `/`
                // stripped into a signed command packet)
                self.client.chat(&part);
            } else {
                // a chat message, even if a mid-message `/` lands on a split
                // boundary — upstream's `_client.chat` never auto-commands
                self.client.write_chat_packet(&part);
            }
        }
        Ok(())
    }

    /// `bot.addChatPattern(name, pattern, opts)` — register a pattern that
    /// must match a single incoming `messagestr`; returns the pattern id.
    pub fn add_chat_pattern(&self, name: &str, pattern: Regex, opts: ChatPatternOpts) -> usize {
        self.add_chat_pattern_set(name, vec![pattern], opts)
    }

    /// `bot.addChatPatternSet(name, patterns, opts)` — register a sequence of
    /// patterns matched against consecutive incoming `messagestr`s; the
    /// `chat:{name}` event ([`BotEvent::ChatPattern`]) fires when the last
    /// pattern matches. Returns the pattern id — the real registry key, not
    /// upstream's off-by-one return for sets (see api-parity.md).
    pub fn add_chat_pattern_set(
        &self,
        name: &str,
        patterns: Vec<Regex>,
        opts: ChatPatternOpts,
    ) -> usize {
        self.shared.chat.lock().patterns.add(name, patterns, opts)
    }

    /// `bot.removeChatPattern(name|id)` — by id removes that set; by name
    /// removes every set with the name.
    pub fn remove_chat_pattern(&self, key: impl Into<ChatPatternKey>) {
        self.shared.chat.lock().patterns.remove(&key.into());
    }

    /// `bot.chatPatterns` — a snapshot of the registered pattern sets.
    pub fn chat_patterns(&self) -> Vec<ChatPattern> {
        self.shared.chat.lock().patterns.list()
    }

    /// `bot.awaitMessage(...args)` — resolves with the `messagestr` text of
    /// the first incoming message matching any matcher, or
    /// [`Error::Timeout`] after 20s (upstream's default).
    ///
    /// ```no_run
    /// use mineflayer::prelude::*;
    ///
    /// # async fn example(bot: Bot) -> Result<()> {
    /// bot.chat("/list")?;
    /// let reply = bot
    ///     .await_message([regex::Regex::new("^There are \\d+").unwrap()])
    ///     .await?;
    /// println!("{reply}");
    /// # Ok(())
    /// # }
    /// ```
    pub async fn await_message(
        &self,
        matchers: impl IntoIterator<Item = impl Into<MessageMatcher>>,
    ) -> Result<String> {
        let matchers: Vec<MessageMatcher> = matchers.into_iter().map(Into::into).collect();
        await_message_on(self.subscribe(), &matchers).await
    }

    /// `bot.tabComplete(text)` — ask the server to complete `text`; resolves
    /// with the suggestion strings. Times out after 5s like upstream.
    ///
    /// `assumeCommand`/`sendBlockInSight` are dropped: mc26's
    /// `s_command_suggestion` carries only `{id, command}` — those fields
    /// don't exist on the wire. Include a leading `/` in `text` for command
    /// completion (the server parses the whole string either way).
    pub async fn tab_complete(&self, text: &str) -> Result<Vec<String>> {
        if !self.logged_in() {
            return Err(Error::NotConnected);
        }
        let (id, rx) = self.shared.chat.lock().register_completion();
        // removes the correlation entry on every exit, including a caller
        // that drops this future before the reply or the timeout
        let _pending = PendingCompletion {
            chat: &self.shared.chat,
            id,
        };
        self.client.write_packet(ServerboundCommandSuggestion {
            id,
            command: text.to_string(),
        });
        match tokio::time::timeout(TAB_COMPLETE_TIMEOUT, rx).await {
            Ok(Ok(matches)) => Ok(matches),
            // sender dropped without a reply (shouldn't happen; entries are
            // only removed on reply or by the guard)
            Ok(Err(_)) => Err(Error::NotConnected),
            Err(_) => Err(Error::Timeout),
        }
    }

    /// `bot.tablist` — the tab-list header/footer the server last sent
    /// (empty until the first `c_tab_list` packet).
    pub fn tablist(&self) -> TabList {
        self.shared.chat.lock().tablist.clone()
    }
}

#[cfg(test)]
mod tests;
