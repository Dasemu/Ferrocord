//! Domain types shared by `net`, `store` and `app`.
//!
//! These are normalised versions of the wire types in `types.rs`: ids are
//! parsed, optional fields are flattened and user-account quirks (READY
//! deduplication, `properties` objects) are resolved.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Discord epoch (2015-01-01T00:00:00Z) in Unix milliseconds.
pub const DISCORD_EPOCH_MS: u64 = 1_420_070_400_000;

/// A Discord snowflake id. Serialised as a decimal string like the API does.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Snowflake(pub u64);

impl Snowflake {
    /// Creation time encoded in the id, in Unix milliseconds.
    pub fn timestamp_ms(self) -> u64 {
        (self.0 >> 22) + DISCORD_EPOCH_MS
    }

    /// Builds a synthetic id for a given Unix millisecond timestamp
    /// (used for nonces and mock data).
    pub fn from_timestamp_ms(ms: u64, increment: u64) -> Self {
        Self((ms.saturating_sub(DISCORD_EPOCH_MS) << 22) | (increment & 0xfff))
    }
}

impl fmt::Debug for Snowflake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Display for Snowflake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for Snowflake {
    type Err = std::num::ParseIntError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(Snowflake)
    }
}

impl Serialize for Snowflake {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Snowflake {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = Snowflake;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a snowflake as string or integer")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Snowflake, E> {
                v.parse().map(Snowflake).map_err(E::custom)
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Snowflake, E> {
                Ok(Snowflake(v))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Snowflake, E> {
                u64::try_from(v).map(Snowflake).map_err(E::custom)
            }
        }
        d.deserialize_any(V)
    }
}

pub type UserId = Snowflake;
pub type GuildId = Snowflake;
pub type ChannelId = Snowflake;
pub type MessageId = Snowflake;
pub type RoleId = Snowflake;

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct User {
    pub id: UserId,
    pub username: String,
    #[serde(default)]
    pub global_name: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub bot: bool,
}

impl fmt::Debug for User {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Usernames are personal data: only log the id.
        write!(f, "User({})", self.id)
    }
}

impl User {
    /// Name shown in the UI: global display name, falling back to username.
    pub fn display_name(&self) -> &str {
        self.global_name
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.username)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChannelKind {
    Text,
    Dm,
    Voice,
    GroupDm,
    Category,
    Announcement,
    Thread,
    Stage,
    Forum,
    Other(u8),
}

impl ChannelKind {
    pub fn from_wire(t: u8) -> Self {
        match t {
            0 => Self::Text,
            1 => Self::Dm,
            2 => Self::Voice,
            3 => Self::GroupDm,
            4 => Self::Category,
            5 => Self::Announcement,
            10..=12 => Self::Thread,
            13 => Self::Stage,
            15 | 16 => Self::Forum,
            o => Self::Other(o),
        }
    }

    pub fn is_private(self) -> bool {
        matches!(self, Self::Dm | Self::GroupDm)
    }

    /// Channels that can hold text messages and are shown in the list.
    pub fn is_textual(self) -> bool {
        matches!(
            self,
            Self::Text | Self::Dm | Self::GroupDm | Self::Announcement | Self::Thread
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Channel {
    pub id: ChannelId,
    pub kind: ChannelKind,
    pub guild_id: Option<GuildId>,
    pub name: Option<String>,
    pub topic: Option<String>,
    pub position: i32,
    pub parent_id: Option<ChannelId>,
    pub last_message_id: Option<MessageId>,
    /// DM / group DM participants (excluding the current user).
    pub recipients: Vec<UserId>,
    pub nsfw: bool,
}

impl Channel {
    /// Name for the UI: channel name, or for DMs the recipients' names.
    pub fn label<'a>(&'a self, user_name: impl Fn(UserId) -> Option<&'a str>) -> String {
        if let Some(n) = self.name.as_deref().filter(|n| !n.is_empty()) {
            return n.to_owned();
        }
        let names: Vec<&str> = self
            .recipients
            .iter()
            .filter_map(|id| user_name(*id))
            .collect();
        if names.is_empty() {
            "Unknown".into()
        } else {
            names.join(", ")
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Role {
    pub id: RoleId,
    pub name: String,
    /// RGB colour, 0 = no colour.
    pub color: u32,
    pub position: i32,
    /// Permission bits (server-wide; channel overwrites are not applied).
    #[serde(default)]
    pub permissions: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Guild {
    pub id: GuildId,
    pub name: String,
    pub icon: Option<String>,
    pub roles: Vec<Role>,
    pub owner_id: Option<UserId>,
    /// Custom emoji of the server (for the picker).
    #[serde(default)]
    pub emojis: Vec<CustomEmoji>,
    /// Boost tier (0-3); raises the upload limit.
    #[serde(default)]
    pub premium_tier: u8,
}

impl Guild {
    /// Acronym shown when a guild has no icon ("Rust Programming" -> "RP").
    pub fn acronym(&self) -> String {
        let a: String = self
            .name
            .split_whitespace()
            .filter_map(|w| w.chars().next())
            .take(3)
            .collect();
        if a.is_empty() { "?".into() } else { a }
    }
}

/// A server's custom emoji.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomEmoji {
    pub id: Snowflake,
    pub name: String,
    #[serde(default)]
    pub animated: bool,
    #[serde(default = "default_true")]
    pub available: bool,
}

fn default_true() -> bool {
    true
}

/// An emoji used in a reaction: Unicode (`id == None`, `name` is the
/// character sequence) or custom (`id` set).
#[derive(Debug, Clone, Eq, Serialize, Deserialize)]
pub struct Emoji {
    pub id: Option<Snowflake>,
    pub name: String,
    #[serde(default)]
    pub animated: bool,
}

/// Identity: custom emoji by id, Unicode by sequence (names of custom
/// emoji can change).
impl PartialEq for Emoji {
    fn eq(&self, other: &Self) -> bool {
        match (self.id, other.id) {
            (Some(a), Some(b)) => a == b,
            (None, None) => self.name == other.name,
            _ => false,
        }
    }
}

impl Emoji {
    pub fn unicode(s: &str) -> Self {
        Self {
            id: None,
            name: s.to_owned(),
            animated: false,
        }
    }

    pub fn custom(e: &CustomEmoji) -> Self {
        Self {
            id: Some(e.id),
            name: e.name.clone(),
            animated: e.animated,
        }
    }

    /// Path segment for the reaction endpoints: percent-encoded Unicode, or
    /// `name:id` for custom emoji.
    pub fn api_segment(&self) -> String {
        match self.id {
            Some(id) => format!("{}:{id}", percent_encode(&self.name)),
            None => percent_encode(&self.name),
        }
    }

    /// Text inserted in the composer: the character(s), or `<:name:id>`.
    pub fn composer_text(&self) -> String {
        match self.id {
            Some(id) => format!(
                "<{}:{}:{id}>",
                if self.animated { "a" } else { "" },
                self.name
            ),
            None => self.name.clone(),
        }
    }
}

/// Percent-encodes everything except unreserved characters.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// One reaction pill on a message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reaction {
    pub emoji: Emoji,
    pub count: u32,
    /// The current user reacted with this emoji.
    pub me: bool,
}

/// The message a reply points at, as shown in the reply header.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplyPreview {
    pub message_id: Option<MessageId>,
    pub author: Option<User>,
    /// Truncated to [`REPLY_SNIPPET_CHARS`].
    pub content: String,
    pub has_attachment: bool,
    /// The original was deleted (or could not be loaded).
    pub deleted: bool,
}

impl std::fmt::Debug for ReplyPreview {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ReplyPreview({:?})", self.message_id)
    }
}

pub const REPLY_SNIPPET_CHARS: usize = 200;

/// Truncates to `max` chars on a char boundary, adding an ellipsis.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    s.chars().take(max).collect::<String>() + "…"
}

impl ReplyPreview {
    pub fn from_message(m: &Message) -> Self {
        Self {
            message_id: Some(m.id),
            author: Some(m.author.clone()),
            content: truncate_chars(&m.content, REPLY_SNIPPET_CHARS),
            has_attachment: !m.attachments.is_empty(),
            deleted: false,
        }
    }

    pub fn deleted(id: Option<MessageId>) -> Self {
        Self {
            message_id: id,
            author: None,
            content: String::new(),
            has_attachment: false,
            deleted: true,
        }
    }
}

/// User status shown as a coloured dot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Presence {
    Online,
    Idle,
    Dnd,
    #[default]
    Offline,
}

impl Presence {
    /// Parses the gateway `status` string (`invisible` counts as offline).
    pub fn from_wire(s: &str) -> Self {
        match s {
            "online" => Self::Online,
            "idle" => Self::Idle,
            "dnd" => Self::Dnd,
            _ => Self::Offline,
        }
    }

    /// Value for IDENTIFY / op 3 / user settings.
    pub fn wire(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Idle => "idle",
            Self::Dnd => "dnd",
            Self::Offline => "invisible",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Online => "Online",
            Self::Idle => "Idle",
            Self::Dnd => "Do Not Disturb",
            Self::Offline => "Invisible",
        }
    }

    pub fn is_online(self) -> bool {
        self != Self::Offline
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: Snowflake,
    pub filename: String,
    pub size: u64,
    pub url: String,
    pub proxy_url: Option<String>,
    pub content_type: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl fmt::Debug for Attachment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Attachment({}, {} bytes)", self.id, self.size)
    }
}

impl Attachment {
    pub fn is_image(&self) -> bool {
        self.content_type
            .as_deref()
            .is_some_and(|c| c.starts_with("image/"))
            || [".png", ".jpg", ".jpeg", ".gif", ".webp"]
                .iter()
                .any(|e| self.filename.to_ascii_lowercase().ends_with(e))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EmbedField {
    pub name: String,
    pub value: String,
    pub inline: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EmbedMedia {
    pub url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Embed {
    pub kind: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub url: Option<String>,
    pub color: Option<u32>,
    pub author_name: Option<String>,
    pub author_url: Option<String>,
    pub provider_name: Option<String>,
    pub footer: Option<String>,
    pub fields: Vec<EmbedField>,
    pub image: Option<EmbedMedia>,
    pub thumbnail: Option<EmbedMedia>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub channel_id: ChannelId,
    pub guild_id: Option<GuildId>,
    pub author: User,
    pub content: String,
    pub edited: bool,
    pub attachments: Vec<Attachment>,
    pub embeds: Vec<Embed>,
    pub mentions: Vec<UserId>,
    pub mention_roles: Vec<RoleId>,
    pub mention_everyone: bool,
    /// Role ids of the author in this guild (from `member`), for colour.
    pub author_roles: Vec<RoleId>,
    pub reply_to: Option<MessageId>,
    /// Snapshot of the replied-to message for the reply header.
    #[serde(default)]
    pub referenced: Option<Box<ReplyPreview>>,
    #[serde(default)]
    pub reactions: Vec<Reaction>,
    pub nonce: Option<String>,
    /// Message type (0 = default, 19 = reply, 7 = member join, ...).
    pub kind: u8,
    /// Local-only: optimistic message not yet acknowledged by the server.
    #[serde(default)]
    pub pending: bool,
    /// Local-only: sending failed.
    #[serde(default)]
    pub failed: bool,
}

impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print the body.
        f.debug_struct("Message")
            .field("id", &self.id)
            .field("channel_id", &self.channel_id)
            .field("author", &self.author.id)
            .field("body_len", &self.content.len())
            .finish()
    }
}

impl Message {
    pub fn timestamp_ms(&self) -> u64 {
        self.id.timestamp_ms()
    }

    /// Applies a MESSAGE_UPDATE patch (only the fields that were sent).
    pub fn apply_patch(&mut self, p: &MessagePatch) {
        if let Some(c) = &p.content {
            self.content = c.clone();
        }
        if let Some(e) = &p.embeds {
            self.embeds = e.clone();
        }
        if let Some(a) = &p.attachments {
            self.attachments = a.clone();
        }
        self.edited |= p.edited;
    }
}

/// Partial update from MESSAGE_UPDATE: only fields that were present.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MessagePatch {
    pub id: MessageId,
    pub channel_id: ChannelId,
    pub content: Option<String>,
    pub embeds: Option<Vec<Embed>>,
    pub attachments: Option<Vec<Attachment>>,
    pub edited: bool,
}

/// Read state of one channel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadState {
    pub last_read: Option<MessageId>,
    pub mention_count: u32,
}

/// One entry of the user's server list: a folder, or (with `id == None`) a
/// single ungrouped server.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GuildFolder {
    pub id: Option<u64>,
    pub name: Option<String>,
    /// RGB colour.
    pub color: Option<u32>,
    pub guild_ids: Vec<GuildId>,
}

impl GuildFolder {
    pub fn single(g: GuildId) -> Self {
        Self {
            id: None,
            name: None,
            color: None,
            guild_ids: vec![g],
        }
    }
}

/// Everything the client needs from READY (+ READY_SUPPLEMENTAL).
#[derive(Debug, Clone, Default)]
pub struct ReadyData {
    pub me: Option<User>,
    pub session_id: String,
    pub resume_gateway_url: Option<String>,
    pub guilds: Vec<Guild>,
    pub channels: Vec<Channel>,
    pub users: Vec<User>,
    pub read_states: Vec<(ChannelId, ReadState)>,
    /// Guild order from the user's settings (guild folders), if present.
    pub guild_order: Vec<GuildId>,
    /// Role ids of the current user per guild.
    pub my_roles: Vec<(GuildId, Vec<RoleId>)>,
    /// Muted guilds / channels from user guild settings.
    pub muted_guilds: Vec<GuildId>,
    pub muted_channels: Vec<ChannelId>,
    /// Servers with "suppress @everyone and @here" / "suppress role mentions".
    pub suppress_everyone: Vec<GuildId>,
    pub suppress_roles: Vec<GuildId>,
    /// Status of friends from READY `presences`.
    pub presences: Vec<(UserId, Presence)>,
    /// The user's own status (`user_settings.status`).
    pub my_status: Option<Presence>,
    /// Nitro type of the current user (0 none, 1 classic, 2 nitro, 3 basic).
    pub premium_type: u8,
    /// Server folders from the user settings (empty when only the legacy
    /// `guild_positions` order exists).
    pub guild_folders: Vec<GuildFolder>,
    /// Per-channel notification overrides (level and timed mutes).
    pub channel_notify: Vec<(ChannelId, super::extras::ChannelNotify)>,
}

/// A member shown in the member list.
#[derive(Clone, PartialEq)]
pub struct MemberEntry {
    pub user: User,
    pub nick: Option<String>,
    pub roles: Vec<RoleId>,
    pub presence: Presence,
}

impl std::fmt::Debug for MemberEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MemberEntry({})", self.user.id)
    }
}

impl MemberEntry {
    pub fn display_name(&self) -> &str {
        self.nick
            .as_deref()
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| self.user.display_name())
    }
}

/// One row of the member list: a group header or a member.
#[derive(Debug, Clone, PartialEq)]
pub enum MemberItem {
    /// `id` is a role id, `online` or `offline`.
    Group {
        id: String,
        count: u32,
    },
    Member(Box<MemberEntry>),
}

/// An operation of GUILD_MEMBER_LIST_UPDATE (indexes are into the flat
/// list of groups and members).
#[derive(Debug, Clone, PartialEq)]
pub enum MemberOp {
    Sync {
        start: usize,
        items: Vec<MemberItem>,
    },
    Insert {
        index: usize,
        item: MemberItem,
    },
    Update {
        index: usize,
        item: MemberItem,
    },
    Delete {
        index: usize,
    },
    Invalidate {
        start: usize,
        end: usize,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemberListUpdate {
    pub guild_id: GuildId,
    /// List id (`everyone` or a hash of the channel permissions).
    pub list_id: String,
    pub member_count: u32,
    pub online_count: u32,
    pub groups: Vec<(String, u32)>,
    pub ops: Vec<MemberOp>,
}

/// MESSAGE_REACTION_* events.
#[derive(Debug, Clone, PartialEq)]
pub enum ReactionEvent {
    Add {
        channel_id: ChannelId,
        message_id: MessageId,
        user_id: UserId,
        emoji: Emoji,
    },
    Remove {
        channel_id: ChannelId,
        message_id: MessageId,
        user_id: UserId,
        emoji: Emoji,
    },
    RemoveAll {
        channel_id: ChannelId,
        message_id: MessageId,
    },
    RemoveEmoji {
        channel_id: ChannelId,
        message_id: MessageId,
        emoji: Emoji,
    },
}

/// One search hit with the channel it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchPage {
    pub total: u32,
    pub messages: Vec<Message>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emoji_identity_and_segments() {
        let a = Emoji::unicode("👍");
        assert_eq!(a.api_segment(), "%F0%9F%91%8D");
        let c = Emoji {
            id: Some(Snowflake(5)),
            name: "party_blob".into(),
            animated: true,
        };
        assert_eq!(c.api_segment(), "party_blob:5");
        assert_eq!(c.composer_text(), "<a:party_blob:5>");
        let renamed = Emoji {
            name: "other".into(),
            ..c.clone()
        };
        assert_eq!(c, renamed);
        assert_ne!(a, Emoji::unicode("👎"));
        assert_ne!(a, c);
    }

    #[test]
    fn truncation_is_char_safe() {
        assert_eq!(truncate_chars("héllo", 10), "héllo");
        assert_eq!(truncate_chars("héllo wörld", 5), "héllo…");
        assert_eq!(truncate_chars("🦀🦀🦀", 2), "🦀🦀…");
    }

    #[test]
    fn presence_wire() {
        assert_eq!(Presence::from_wire("dnd"), Presence::Dnd);
        assert_eq!(Presence::from_wire("invisible"), Presence::Offline);
        assert_eq!(Presence::from_wire("???"), Presence::Offline);
        assert_eq!(Presence::Idle.wire(), "idle");
        assert!(Presence::Online.is_online() && !Presence::Offline.is_online());
    }

    #[test]
    fn snowflake_timestamp() {
        // Example from the Discord docs.
        let id = Snowflake(175928847299117063);
        assert_eq!(id.timestamp_ms(), 1462015105796);
        let back = Snowflake::from_timestamp_ms(1462015105796, 0);
        assert_eq!(back.timestamp_ms(), 1462015105796);
    }

    #[test]
    fn snowflake_serde() {
        let s: Snowflake = serde_json::from_str("\"123\"").unwrap();
        assert_eq!(s, Snowflake(123));
        let n: Snowflake = serde_json::from_str("456").unwrap();
        assert_eq!(n, Snowflake(456));
        assert_eq!(serde_json::to_string(&s).unwrap(), "\"123\"");
    }

    #[test]
    fn message_debug_hides_body() {
        let m = Message {
            id: Snowflake(1),
            channel_id: Snowflake(2),
            guild_id: None,
            author: User {
                id: Snowflake(3),
                username: "alice".into(),
                global_name: None,
                avatar: None,
                bot: false,
            },
            content: "very private words".into(),
            edited: false,
            attachments: vec![],
            embeds: vec![],
            mentions: vec![],
            mention_roles: vec![],
            mention_everyone: false,
            author_roles: vec![],
            reply_to: None,
            referenced: None,
            reactions: vec![],
            nonce: None,
            kind: 0,
            pending: false,
            failed: false,
        };
        let d = format!("{m:?}");
        assert!(!d.contains("private"));
        assert!(!d.contains("alice"));
    }

    #[test]
    fn guild_acronym() {
        let g = Guild {
            id: Snowflake(1),
            name: "Rust Programming Language".into(),
            icon: None,
            roles: vec![],
            owner_id: None,
            emojis: vec![],
            premium_tier: 0,
        };
        assert_eq!(g.acronym(), "RPL");
    }
}
