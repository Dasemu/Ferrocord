//! Wire types (serde) for the Discord Gateway and REST API, and their
//! conversion to the domain types in `model.rs`.
//!
//! Deserialisation is deliberately lenient (`#[serde(default)]` everywhere):
//! the user-account API adds and removes fields often, and an unknown or
//! missing field must never drop an event.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Event;
use super::model::*;

/// Gateway opcodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum OpCode {
    Dispatch = 0,
    Heartbeat = 1,
    Identify = 2,
    PresenceUpdate = 3,
    VoiceStateUpdate = 4,
    Resume = 6,
    Reconnect = 7,
    RequestGuildMembers = 8,
    InvalidSession = 9,
    Hello = 10,
    HeartbeatAck = 11,
}

impl OpCode {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Dispatch,
            1 => Self::Heartbeat,
            2 => Self::Identify,
            3 => Self::PresenceUpdate,
            4 => Self::VoiceStateUpdate,
            6 => Self::Resume,
            7 => Self::Reconnect,
            8 => Self::RequestGuildMembers,
            9 => Self::InvalidSession,
            10 => Self::Hello,
            11 => Self::HeartbeatAck,
            _ => return None,
        })
    }
}

/// Raw gateway frame: `{ op, d, s, t }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayPayload {
    pub op: u8,
    #[serde(default)]
    pub d: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t: Option<String>,
}

impl GatewayPayload {
    pub fn new(op: OpCode, d: Value) -> Self {
        Self {
            op: op as u8,
            d,
            s: None,
            t: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Hello {
    pub heartbeat_interval: u64,
}

/// Client properties sent in IDENTIFY. We identify honestly as Ferrocord.
#[derive(Debug, Clone, Serialize)]
pub struct ClientProperties {
    pub os: String,
    pub browser: String,
    pub device: String,
    pub client_version: String,
}

impl Default for ClientProperties {
    fn default() -> Self {
        Self {
            os: std::env::consts::OS.to_string(),
            browser: crate::config::APP_NAME.into(),
            device: String::new(),
            client_version: env!("CARGO_PKG_VERSION").into(),
        }
    }
}

/// Capability bits for user-account IDENTIFY. We do NOT request READY
/// deduplication of users or the lazy "properties" layout, but the parser
/// copes with both in case the server sends them anyway.
pub const CAPABILITIES: u64 = 0;

pub fn identify_payload(token: &str, props: &ClientProperties) -> GatewayPayload {
    GatewayPayload::new(
        OpCode::Identify,
        serde_json::json!({
            "token": token,
            "capabilities": CAPABILITIES,
            "properties": props,
            "presence": { "status": "online", "since": 0, "activities": [], "afk": false },
            "compress": false,
            "client_state": { "guild_versions": {} },
            // Transport compression (zlib-stream) is negotiated in the URL;
            // per-payload compression stays off.
            "large_threshold": 100,
        }),
    )
}

pub fn resume_payload(token: &str, session_id: &str, seq: u64) -> GatewayPayload {
    GatewayPayload::new(
        OpCode::Resume,
        serde_json::json!({ "token": token, "session_id": session_id, "seq": seq }),
    )
}

pub fn heartbeat_payload(seq: Option<u64>) -> GatewayPayload {
    GatewayPayload::new(
        OpCode::Heartbeat,
        seq.map(Value::from).unwrap_or(Value::Null),
    )
}

// ---------------------------------------------------------------------------
// Wire structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireUser {
    pub id: Snowflake,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub global_name: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub bot: bool,
}

impl From<WireUser> for User {
    fn from(w: WireUser) -> Self {
        User {
            id: w.id,
            username: w.username,
            global_name: w.global_name,
            avatar: w.avatar,
            bot: w.bot,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireChannel {
    pub id: Snowflake,
    #[serde(rename = "type", default)]
    pub kind: u8,
    #[serde(default)]
    pub guild_id: Option<Snowflake>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub position: Option<i32>,
    #[serde(default)]
    pub parent_id: Option<Snowflake>,
    #[serde(default)]
    pub last_message_id: Option<Snowflake>,
    #[serde(default)]
    pub recipients: Vec<WireUser>,
    #[serde(default)]
    pub recipient_ids: Vec<Snowflake>,
    #[serde(default)]
    pub nsfw: bool,
}

impl WireChannel {
    /// Converts to a domain channel; recipient users are returned separately
    /// so they can be put in the user cache.
    pub fn into_domain(self, guild_id: Option<GuildId>) -> (Channel, Vec<User>) {
        let mut recipients: Vec<UserId> = self.recipients.iter().map(|u| u.id).collect();
        for id in &self.recipient_ids {
            if !recipients.contains(id) {
                recipients.push(*id);
            }
        }
        let users = self.recipients.into_iter().map(User::from).collect();
        (
            Channel {
                id: self.id,
                kind: ChannelKind::from_wire(self.kind),
                guild_id: self.guild_id.or(guild_id),
                name: self.name,
                topic: self.topic,
                position: self.position.unwrap_or(0),
                parent_id: self.parent_id,
                last_message_id: self.last_message_id.filter(|s| s.0 != 0),
                recipients,
                nsfw: self.nsfw,
            },
            users,
        )
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireRole {
    pub id: Snowflake,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub color: u32,
    #[serde(default)]
    pub position: i32,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireGuildProperties {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub owner_id: Option<Snowflake>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireGuild {
    pub id: Snowflake,
    #[serde(default)]
    pub unavailable: bool,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub owner_id: Option<Snowflake>,
    /// User accounts with the "lazy guild" capability receive name/icon here.
    #[serde(default)]
    pub properties: Option<WireGuildProperties>,
    #[serde(default)]
    pub channels: Vec<WireChannel>,
    #[serde(default)]
    pub threads: Vec<WireChannel>,
    #[serde(default)]
    pub roles: Vec<WireRole>,
}

impl WireGuild {
    pub fn into_domain(self) -> (Guild, Vec<Channel>) {
        let props = self.properties.unwrap_or_default();
        let guild = Guild {
            id: self.id,
            name: self.name.or(props.name).unwrap_or_default(),
            icon: self.icon.or(props.icon),
            roles: self
                .roles
                .into_iter()
                .map(|r| Role {
                    id: r.id,
                    name: r.name,
                    color: r.color,
                    position: r.position,
                })
                .collect(),
            owner_id: self.owner_id.or(props.owner_id),
        };
        let gid = guild.id;
        let channels = self
            .channels
            .into_iter()
            .chain(self.threads)
            .map(|c| c.into_domain(Some(gid)).0)
            .collect();
        (guild, channels)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireAttachment {
    pub id: Snowflake,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub proxy_url: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

impl From<WireAttachment> for Attachment {
    fn from(w: WireAttachment) -> Self {
        Attachment {
            id: w.id,
            filename: w.filename,
            size: w.size,
            url: w.url,
            proxy_url: w.proxy_url,
            content_type: w.content_type,
            width: w.width,
            height: w.height,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireEmbedMedia {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub proxy_url: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

impl From<WireEmbedMedia> for EmbedMedia {
    fn from(w: WireEmbedMedia) -> Self {
        EmbedMedia {
            url: w.proxy_url.unwrap_or(w.url),
            width: w.width,
            height: w.height,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireNamed {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireEmbedField {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub inline: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireEmbed {
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub color: Option<u32>,
    #[serde(default)]
    pub author: Option<WireNamed>,
    #[serde(default)]
    pub provider: Option<WireNamed>,
    #[serde(default)]
    pub footer: Option<WireNamed>,
    #[serde(default)]
    pub fields: Vec<WireEmbedField>,
    #[serde(default)]
    pub image: Option<WireEmbedMedia>,
    #[serde(default)]
    pub thumbnail: Option<WireEmbedMedia>,
}

impl From<WireEmbed> for Embed {
    fn from(w: WireEmbed) -> Self {
        Embed {
            kind: w.kind,
            title: w.title,
            description: w.description,
            url: w.url,
            color: w.color,
            author_name: w.author.as_ref().and_then(|a| a.name.clone()),
            author_url: w.author.and_then(|a| a.url),
            provider_name: w.provider.and_then(|p| p.name),
            footer: w.footer.and_then(|f| f.text),
            fields: w
                .fields
                .into_iter()
                .map(|f| EmbedField {
                    name: f.name,
                    value: f.value,
                    inline: f.inline,
                })
                .collect(),
            image: w.image.map(Into::into),
            thumbnail: w.thumbnail.map(Into::into),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireMember {
    #[serde(default)]
    pub roles: Vec<Snowflake>,
    #[serde(default)]
    pub user: Option<WireUser>,
    #[serde(default)]
    pub user_id: Option<Snowflake>,
    #[serde(default)]
    pub nick: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireMessageRef {
    #[serde(default)]
    pub message_id: Option<Snowflake>,
}

/// Nonces can be strings or integers on the wire.
fn nonce_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireMessage {
    pub id: Snowflake,
    pub channel_id: Snowflake,
    #[serde(default)]
    pub guild_id: Option<Snowflake>,
    #[serde(default)]
    pub author: Option<WireUser>,
    #[serde(default)]
    pub member: Option<WireMember>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub edited_timestamp: Option<String>,
    #[serde(default)]
    pub attachments: Option<Vec<WireAttachment>>,
    #[serde(default)]
    pub embeds: Option<Vec<WireEmbed>>,
    #[serde(default)]
    pub mentions: Vec<WireUser>,
    #[serde(default)]
    pub mention_roles: Vec<Snowflake>,
    #[serde(default)]
    pub mention_everyone: bool,
    #[serde(default)]
    pub message_reference: Option<WireMessageRef>,
    #[serde(default)]
    pub nonce: Value,
    #[serde(rename = "type", default)]
    pub kind: u8,
}

impl WireMessage {
    pub fn into_domain(self) -> Message {
        Message {
            id: self.id,
            channel_id: self.channel_id,
            guild_id: self.guild_id,
            author: self.author.map(User::from).unwrap_or(User {
                id: Snowflake(0),
                username: "Unknown".into(),
                global_name: None,
                avatar: None,
                bot: false,
            }),
            content: self.content.unwrap_or_default(),
            edited: self.edited_timestamp.is_some(),
            attachments: self
                .attachments
                .unwrap_or_default()
                .into_iter()
                .map(Into::into)
                .collect(),
            embeds: self
                .embeds
                .unwrap_or_default()
                .into_iter()
                .map(Into::into)
                .collect(),
            mentions: self.mentions.iter().map(|u| u.id).collect(),
            mention_roles: self.mention_roles,
            mention_everyone: self.mention_everyone,
            author_roles: self.member.map(|m| m.roles).unwrap_or_default(),
            reply_to: self.message_reference.and_then(|r| r.message_id),
            nonce: nonce_to_string(&self.nonce),
            kind: self.kind,
            pending: false,
            failed: false,
        }
    }

    pub fn into_patch(self) -> MessagePatch {
        MessagePatch {
            id: self.id,
            channel_id: self.channel_id,
            edited: self.edited_timestamp.is_some(),
            content: self.content,
            embeds: self.embeds.map(|v| v.into_iter().map(Into::into).collect()),
            attachments: self
                .attachments
                .map(|v| v.into_iter().map(Into::into).collect()),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireReadStateEntry {
    /// Channel id (for read_state_type 0).
    pub id: Snowflake,
    #[serde(default)]
    pub last_message_id: Option<Snowflake>,
    #[serde(default)]
    pub mention_count: u32,
    #[serde(default)]
    pub read_state_type: u8,
}

/// `read_state` is a plain array on old API versions and
/// `{ entries, partial, version }` on newer ones.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum WireReadStates {
    List(Vec<WireReadStateEntry>),
    Versioned {
        #[serde(default)]
        entries: Vec<WireReadStateEntry>,
    },
}

impl Default for WireReadStates {
    fn default() -> Self {
        Self::List(vec![])
    }
}

impl WireReadStates {
    fn entries(self) -> Vec<WireReadStateEntry> {
        match self {
            Self::List(v) => v,
            Self::Versioned { entries } => entries,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireChannelOverride {
    pub channel_id: Snowflake,
    #[serde(default)]
    pub muted: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireGuildSettingsEntry {
    #[serde(default)]
    pub guild_id: Option<Snowflake>,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub suppress_everyone: bool,
    #[serde(default)]
    pub suppress_roles: bool,
    #[serde(default)]
    pub channel_overrides: Vec<WireChannelOverride>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum WireGuildSettings {
    List(Vec<WireGuildSettingsEntry>),
    Versioned {
        #[serde(default)]
        entries: Vec<WireGuildSettingsEntry>,
    },
}

impl Default for WireGuildSettings {
    fn default() -> Self {
        Self::List(vec![])
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireGuildFolder {
    #[serde(default)]
    pub guild_ids: Vec<Snowflake>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireUserSettings {
    #[serde(default)]
    pub guild_positions: Vec<Snowflake>,
    #[serde(default)]
    pub guild_folders: Vec<WireGuildFolder>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireReady {
    pub user: WireUser,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub resume_gateway_url: Option<String>,
    #[serde(default)]
    pub guilds: Vec<WireGuild>,
    #[serde(default)]
    pub private_channels: Vec<WireChannel>,
    /// Present when the server deduplicates users.
    #[serde(default)]
    pub users: Vec<WireUser>,
    #[serde(default)]
    pub read_state: WireReadStates,
    #[serde(default)]
    pub user_guild_settings: WireGuildSettings,
    #[serde(default)]
    pub user_settings: Option<WireUserSettings>,
    /// One array per guild (same order as `guilds`) with the current user's
    /// member object.
    #[serde(default)]
    pub merged_members: Vec<Vec<WireMember>>,
}

impl WireReady {
    pub fn into_domain(self) -> ReadyData {
        let me: User = self.user.into();
        let mut users: Vec<User> = self.users.into_iter().map(User::from).collect();
        let mut channels = Vec::new();
        let mut guilds = Vec::new();
        let mut my_roles = Vec::new();

        for (idx, g) in self.guilds.into_iter().enumerate() {
            if g.unavailable && g.name.is_none() && g.properties.is_none() {
                continue;
            }
            let (guild, chans) = g.into_domain();
            if let Some(members) = self.merged_members.get(idx)
                && let Some(m) = members.iter().find(|m| {
                    m.user_id == Some(me.id) || m.user.as_ref().map(|u| u.id) == Some(me.id)
                })
            {
                my_roles.push((guild.id, m.roles.clone()));
            }
            channels.extend(chans);
            guilds.push(guild);
        }
        for pc in self.private_channels {
            let (c, us) = pc.into_domain(None);
            channels.push(c);
            for u in us {
                if !users.iter().any(|x| x.id == u.id) {
                    users.push(u);
                }
            }
        }
        let read_states = self
            .read_state
            .entries()
            .into_iter()
            .filter(|e| e.read_state_type == 0)
            .map(|e| {
                (
                    e.id,
                    ReadState {
                        last_read: e.last_message_id.filter(|s| s.0 != 0),
                        mention_count: e.mention_count,
                    },
                )
            })
            .collect();

        let settings_entries = match self.user_guild_settings {
            WireGuildSettings::List(v) => v,
            WireGuildSettings::Versioned { entries } => entries,
        };
        let mut muted_guilds = Vec::new();
        let mut muted_channels = Vec::new();
        let mut suppress_everyone = Vec::new();
        let mut suppress_roles = Vec::new();
        for e in settings_entries {
            if let Some(g) = e.guild_id {
                if e.suppress_everyone {
                    suppress_everyone.push(g);
                }
                if e.suppress_roles {
                    suppress_roles.push(g);
                }
            }
            if e.muted
                && let Some(g) = e.guild_id
            {
                muted_guilds.push(g);
            }
            muted_channels.extend(
                e.channel_overrides
                    .iter()
                    .filter(|o| o.muted)
                    .map(|o| o.channel_id),
            );
        }

        let guild_order = match self.user_settings {
            Some(s) if !s.guild_folders.is_empty() => s
                .guild_folders
                .into_iter()
                .flat_map(|f| f.guild_ids)
                .collect(),
            Some(s) => s.guild_positions,
            None => vec![],
        };

        ReadyData {
            me: Some(me),
            session_id: self.session_id,
            resume_gateway_url: self.resume_gateway_url,
            guilds,
            channels,
            users,
            read_states,
            guild_order,
            my_roles,
            muted_guilds,
            muted_channels,
            suppress_everyone,
            suppress_roles,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireTypingStart {
    pub channel_id: Snowflake,
    #[serde(default)]
    pub guild_id: Option<Snowflake>,
    pub user_id: Snowflake,
    #[serde(default)]
    pub timestamp: u64,
    #[serde(default)]
    pub member: Option<WireMember>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireMessageDelete {
    pub id: Snowflake,
    pub channel_id: Snowflake,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireMessageDeleteBulk {
    pub ids: Vec<Snowflake>,
    pub channel_id: Snowflake,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireMessageAck {
    pub channel_id: Snowflake,
    pub message_id: Snowflake,
    #[serde(default)]
    pub mention_count: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireIdOnly {
    pub id: Snowflake,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireReadySupplemental {
    #[serde(default)]
    pub merged_members: Vec<Vec<WireMember>>,
    #[serde(default)]
    pub guilds: Vec<WireIdOnly>,
}

/// Converts a DISPATCH (`op 0`) frame into a client event.
///
/// Returns `Ok(None)` for events the client does not use. A malformed
/// payload for a known event is an error (logged by the caller; the
/// connection stays up).
pub fn parse_dispatch(t: &str, d: Value) -> Result<Option<Event>, serde_json::Error> {
    Ok(Some(match t {
        "READY" => {
            let r: WireReady = serde_json::from_value(d)?;
            Event::Ready(Box::new(r.into_domain()))
        }
        "READY_SUPPLEMENTAL" => {
            let r: WireReadySupplemental = serde_json::from_value(d)?;
            let mut roles = Vec::new();
            for (idx, g) in r.guilds.iter().enumerate() {
                if let Some(ms) = r.merged_members.get(idx) {
                    for m in ms {
                        if let Some(uid) = m.user_id.or(m.user.as_ref().map(|u| u.id)) {
                            roles.push((g.id, uid, m.roles.clone()));
                        }
                    }
                }
            }
            Event::MemberRoles(roles)
        }
        "RESUMED" => Event::Resumed,
        "MESSAGE_CREATE" => {
            let m: WireMessage = serde_json::from_value(d)?;
            Event::MessageCreate(Box::new(m.into_domain()))
        }
        "MESSAGE_UPDATE" => {
            let m: WireMessage = serde_json::from_value(d)?;
            Event::MessageUpdate(Box::new(m.into_patch()))
        }
        "MESSAGE_DELETE" => {
            let m: WireMessageDelete = serde_json::from_value(d)?;
            Event::MessageDelete {
                channel_id: m.channel_id,
                id: m.id,
            }
        }
        "MESSAGE_DELETE_BULK" => {
            let m: WireMessageDeleteBulk = serde_json::from_value(d)?;
            Event::MessageDeleteBulk {
                channel_id: m.channel_id,
                ids: m.ids,
            }
        }
        "TYPING_START" => {
            let t: WireTypingStart = serde_json::from_value(d)?;
            Event::TypingStart {
                channel_id: t.channel_id,
                guild_id: t.guild_id,
                user_id: t.user_id,
                timestamp_s: t.timestamp,
                user: t.member.and_then(|m| m.user).map(Into::into),
            }
        }
        "MESSAGE_ACK" => {
            let a: WireMessageAck = serde_json::from_value(d)?;
            Event::MessageAck {
                channel_id: a.channel_id,
                message_id: a.message_id,
                mention_count: a.mention_count.unwrap_or(0),
            }
        }
        "CHANNEL_CREATE" | "CHANNEL_UPDATE" | "THREAD_CREATE" | "THREAD_UPDATE" => {
            let c: WireChannel = serde_json::from_value(d)?;
            let (c, users) = c.into_domain(None);
            Event::ChannelUpsert(Box::new(c), users)
        }
        "CHANNEL_DELETE" | "THREAD_DELETE" => {
            let c: WireIdOnly = serde_json::from_value(d)?;
            Event::ChannelDelete(c.id)
        }
        "GUILD_CREATE" | "GUILD_UPDATE" => {
            let g: WireGuild = serde_json::from_value(d)?;
            if g.unavailable && g.name.is_none() && g.properties.is_none() {
                return Ok(None);
            }
            let partial = t == "GUILD_UPDATE";
            let (guild, channels) = g.into_domain();
            Event::GuildUpsert {
                guild: Box::new(guild),
                channels,
                partial,
            }
        }
        "GUILD_DELETE" => {
            let g: WireIdOnly = serde_json::from_value(d)?;
            Event::GuildDelete(g.id)
        }
        _ => return Ok(None),
    }))
}

// ---------------------------------------------------------------------------
// REST bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct CreateMessage<'a> {
    pub content: &'a str,
    pub nonce: &'a str,
    pub tts: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RateLimitBody {
    #[serde(default)]
    pub retry_after: f64,
    #[serde(default)]
    pub global: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opcode_roundtrip() {
        for v in [0u8, 1, 2, 3, 4, 6, 7, 8, 9, 10, 11] {
            assert_eq!(OpCode::from_u8(v).map(|o| o as u8), Some(v));
        }
        assert!(OpCode::from_u8(5).is_none());
    }

    #[test]
    fn heartbeat_serialises_null_seq() {
        let p = heartbeat_payload(None);
        assert_eq!(serde_json::to_string(&p).unwrap(), r#"{"op":1,"d":null}"#);
        let p = heartbeat_payload(Some(5));
        assert_eq!(serde_json::to_string(&p).unwrap(), r#"{"op":1,"d":5}"#);
    }

    #[test]
    fn identify_contains_token_and_properties() {
        let p = identify_payload("tok", &ClientProperties::default());
        assert_eq!(p.op, 2);
        assert_eq!(p.d["token"], "tok");
        assert_eq!(p.d["properties"]["browser"], "Ferrocord");
    }

    #[test]
    fn unknown_dispatch_is_ignored() {
        assert!(
            parse_dispatch("PRESENCE_UPDATE", serde_json::json!({}))
                .unwrap()
                .is_none()
        );
    }
}
