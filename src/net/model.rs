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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Guild {
    pub id: GuildId,
    pub name: String,
    pub icon: Option<String>,
    pub roles: Vec<Role>,
    pub owner_id: Option<UserId>,
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
        };
        assert_eq!(g.acronym(), "RPL");
    }
}
