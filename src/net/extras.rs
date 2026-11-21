//! Wire parsing and payload builders for pins, the mentions inbox, channel
//! notification settings, invites, server creation and discovery.
//!
//! Everything here is pure (no I/O) and unit-tested; the REST calls live in
//! `rest/ext.rs`.

use serde_json::{Value, json};

use super::model::*;
use super::types::WireMessage;

fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|x| !x.is_empty())
        .map(String::from)
}

fn n(v: &Value, key: &str) -> Option<u32> {
    v.get(key).and_then(Value::as_u64).map(|x| x as u32)
}

fn sf(v: &Value, key: &str) -> Option<Snowflake> {
    match v.get(key)? {
        Value::String(x) => x.parse().ok(),
        Value::Number(x) => x.as_u64().map(Snowflake),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Pins and mentions
// ---------------------------------------------------------------------------

/// Parses both pin layouts: the legacy array of messages and the paginated
/// `{ items: [{ pinned_at, message }], has_more }`. Malformed entries are
/// skipped. Newest pin first (as sent).
pub fn parse_pins(v: Value) -> Vec<Message> {
    let items = match v {
        Value::Array(a) => a,
        Value::Object(mut o) => match o.remove("items") {
            Some(Value::Array(a)) => a,
            _ => vec![],
        },
        _ => vec![],
    };
    items
        .into_iter()
        .filter_map(|item| {
            let msg = match item.get("message") {
                Some(m) => m.clone(),
                None => item,
            };
            serde_json::from_value::<WireMessage>(msg)
                .ok()
                .map(WireMessage::into_domain)
        })
        .collect()
}

/// Parses `GET /users/@me/mentions` (array of messages, newest first).
pub fn parse_mentions(v: Value) -> Vec<Message> {
    parse_pins(v)
}

pub const MENTIONS_PAGE: u32 = 25;

pub fn mentions_path(before: Option<MessageId>, guild: Option<GuildId>) -> String {
    let mut p = format!("/users/@me/mentions?limit={MENTIONS_PAGE}&roles=true&everyone=true");
    if let Some(b) = before {
        p.push_str(&format!("&before={b}"));
    }
    if let Some(g) = guild {
        p.push_str(&format!("&guild_id={g}"));
    }
    p
}

// ---------------------------------------------------------------------------
// Search filters
// ---------------------------------------------------------------------------

/// `has:` values the search endpoint understands.
pub const SEARCH_HAS: [&str; 7] = [
    "image", "video", "sound", "file", "link", "embed", "sticker",
];

/// A search box entry split into free text and filters
/// (`from:name`, `has:image`, `in:#channel`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchQuery {
    pub content: String,
    pub authors: Vec<UserId>,
    pub has: Vec<&'static str>,
    pub channels: Vec<ChannelId>,
}

impl SearchQuery {
    pub fn text(content: &str) -> Self {
        Self {
            content: content.trim().to_owned(),
            ..Default::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.content.is_empty()
            && self.authors.is_empty()
            && self.has.is_empty()
            && self.channels.is_empty()
    }
}

/// Splits `input` into text and filters. `user` / `channel` resolve the
/// names after `from:` / `in:` (case-insensitive matching is the
/// resolver's job). An unknown name or `has:` value is an error naming it,
/// so the user sees why nothing was searched.
pub fn parse_search_query(
    input: &str,
    user: impl Fn(&str) -> Option<UserId>,
    channel: impl Fn(&str) -> Option<ChannelId>,
) -> Result<SearchQuery, String> {
    let mut q = SearchQuery::default();
    let mut words: Vec<&str> = Vec::new();
    for tok in input.split_whitespace() {
        let lower = tok.to_ascii_lowercase();
        if lower.starts_with("from:") {
            let raw = tok[5..].trim_start_matches('@');
            if raw.is_empty() {
                words.push(tok);
                continue;
            }
            match user(raw) {
                Some(id) => {
                    if !q.authors.contains(&id) {
                        q.authors.push(id);
                    }
                }
                None => return Err(format!("No user named \"{raw}\" found for from:")),
            }
        } else if let Some(v) = lower.strip_prefix("has:") {
            match SEARCH_HAS.iter().find(|h| **h == v) {
                Some(h) => {
                    if !q.has.contains(h) {
                        q.has.push(h);
                    }
                }
                None => return Err(format!("Unknown has: filter \"{v}\"")),
            }
        } else if lower.starts_with("in:") {
            let raw = tok[3..].trim_start_matches('#');
            if raw.is_empty() {
                words.push(tok);
                continue;
            }
            match channel(raw) {
                Some(id) => {
                    if !q.channels.contains(&id) {
                        q.channels.push(id);
                    }
                }
                None => return Err(format!("No channel named \"{raw}\" found for in:")),
            }
        } else {
            words.push(tok);
        }
    }
    q.content = words.join(" ");
    Ok(q)
}

// ---------------------------------------------------------------------------
// Notification settings
// ---------------------------------------------------------------------------

/// "Notification Settings" of a channel (and the same values per server).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NotifyLevel {
    /// Inherit (the category / server setting). Wire value 3.
    #[default]
    Default,
    All,
    Mentions,
    Nothing,
}

impl NotifyLevel {
    pub fn from_wire(v: Option<i64>) -> Self {
        match v {
            Some(0) => Self::All,
            Some(1) => Self::Mentions,
            Some(2) => Self::Nothing,
            _ => Self::Default,
        }
    }

    pub fn wire(self) -> u8 {
        match self {
            Self::All => 0,
            Self::Mentions => 1,
            Self::Nothing => 2,
            Self::Default => 3,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "Use Category Default",
            Self::All => "All Messages",
            Self::Mentions => "Only @mentions",
            Self::Nothing => "Nothing",
        }
    }
}

/// Per-channel notification override from `user_guild_settings`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChannelNotify {
    pub level: NotifyLevel,
    pub muted: bool,
    /// Unix ms when a timed mute ends; `None` while muted means "until I
    /// turn it back on".
    pub mute_end_ms: Option<u64>,
}

impl ChannelNotify {
    pub fn is_muted(&self, now_ms: u64) -> bool {
        self.muted && self.mute_end_ms.is_none_or(|end| end > now_ms)
    }

    /// Nothing worth storing (inherit everything).
    pub fn is_default(&self) -> bool {
        self.level == NotifyLevel::Default && !self.muted
    }

    /// Parses a `channel_overrides` entry (or a bare channel settings
    /// object).
    pub fn from_wire(v: &Value) -> Self {
        let end = v
            .get("mute_config")
            .and_then(|c| c.get("end_time"))
            .and_then(Value::as_str)
            .and_then(parse_iso_ms);
        Self {
            level: NotifyLevel::from_wire(v.get("message_notifications").and_then(Value::as_i64)),
            muted: v.get("muted").and_then(Value::as_bool).unwrap_or(false),
            mute_end_ms: end,
        }
    }
}

pub fn parse_iso_ms(s: &str) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .and_then(|d| u64::try_from(d.timestamp_millis()).ok())
}

pub fn iso_from_ms(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .unwrap_or_default()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

/// Mute durations offered by the bell popover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuteDuration {
    Minutes15,
    Hour1,
    Hours3,
    Hours8,
    Hours24,
    Forever,
}

impl MuteDuration {
    pub const ALL: [MuteDuration; 6] = [
        Self::Minutes15,
        Self::Hour1,
        Self::Hours3,
        Self::Hours8,
        Self::Hours24,
        Self::Forever,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Minutes15 => "For 15 Minutes",
            Self::Hour1 => "For 1 Hour",
            Self::Hours3 => "For 3 Hours",
            Self::Hours8 => "For 8 Hours",
            Self::Hours24 => "For 24 Hours",
            Self::Forever => "Until I turn it back on",
        }
    }

    pub fn seconds(self) -> Option<u64> {
        match self {
            Self::Minutes15 => Some(15 * 60),
            Self::Hour1 => Some(3600),
            Self::Hours3 => Some(3 * 3600),
            Self::Hours8 => Some(8 * 3600),
            Self::Hours24 => Some(24 * 3600),
            Self::Forever => None,
        }
    }

    /// The state this mute produces locally.
    pub fn apply(self, level: NotifyLevel, now_ms: u64) -> ChannelNotify {
        ChannelNotify {
            level,
            muted: true,
            mute_end_ms: self.seconds().map(|s| now_ms + s * 1000),
        }
    }
}

/// Per-channel part of the settings PATCH for a mute.
pub fn mute_override(d: MuteDuration, now_ms: u64) -> Value {
    match d.seconds() {
        Some(secs) => json!({
            "muted": true,
            "mute_config": {
                "selected_time_window": secs,
                "end_time": iso_from_ms(now_ms + secs * 1000),
            },
        }),
        None => json!({
            "muted": true,
            "mute_config": { "selected_time_window": -1, "end_time": null },
        }),
    }
}

pub fn unmute_override() -> Value {
    json!({ "muted": false, "mute_config": null })
}

pub fn level_override(level: NotifyLevel) -> Value {
    json!({ "message_notifications": level.wire() })
}

/// Body of `PATCH /users/@me/guilds/{guild|@me}/settings` for one channel.
pub fn channel_override_body(channel: ChannelId, over: Value) -> Value {
    json!({ "channel_overrides": { channel.to_string(): over } })
}

pub fn guild_settings_path(guild: Option<GuildId>) -> String {
    match guild {
        Some(g) => format!("/users/@me/guilds/{g}/settings"),
        None => "/users/@me/guilds/@me/settings".into(),
    }
}

// ---------------------------------------------------------------------------
// Invites and server creation
// ---------------------------------------------------------------------------

/// Extracts the invite code from a link or a bare code.
///
/// Accepts `discord.gg/<code>`, `discord.com/invite/<code>`,
/// `discordapp.com/invite/<code>` (with or without scheme / `www.`) and
/// `<code>`. Query strings and trailing slashes are ignored.
pub fn parse_invite_code(input: &str) -> Option<String> {
    let t = input.trim();
    let t = t
        .strip_prefix("https://")
        .or_else(|| t.strip_prefix("http://"))
        .unwrap_or(t);
    let t = t.strip_prefix("www.").unwrap_or(t);
    let code = if let Some(rest) = t
        .strip_prefix("discord.gg/")
        .or_else(|| t.strip_prefix("discord.com/invite/"))
        .or_else(|| t.strip_prefix("discordapp.com/invite/"))
    {
        rest
    } else if t.contains('/') || t.contains('.') {
        return None;
    } else {
        t
    };
    let code = code.split(['?', '#', '/']).next().unwrap_or("");
    let ok = !code.is_empty()
        && code.len() <= 64
        && code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    ok.then(|| code.to_owned())
}

/// What the invite preview card shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvitePreview {
    pub code: String,
    pub guild_id: GuildId,
    pub name: String,
    pub icon: Option<String>,
    pub description: Option<String>,
    pub members: Option<u32>,
    pub online: Option<u32>,
    pub inviter: Option<String>,
    pub channel: Option<String>,
    pub verified: bool,
    pub partnered: bool,
}

impl InvitePreview {
    pub fn icon_url(&self, cdn: &str) -> Option<String> {
        self.icon.as_ref().map(|h| {
            format!(
                "{}/icons/{}/{h}.png?size=96",
                cdn.trim_end_matches('/'),
                self.guild_id
            )
        })
    }
}

pub fn parse_invite(code: &str, v: &Value) -> Option<InvitePreview> {
    let g = v.get("guild")?;
    let features: Vec<&str> = g
        .get("features")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let inviter = v
        .get("inviter")
        .and_then(|u| s(u, "global_name").or_else(|| s(u, "username")));
    Some(InvitePreview {
        code: code.to_owned(),
        guild_id: sf(g, "id").or_else(|| sf(v, "guild_id"))?,
        name: s(g, "name")?,
        icon: s(g, "icon"),
        description: s(g, "description"),
        members: n(v, "approximate_member_count"),
        online: n(v, "approximate_presence_count"),
        inviter,
        channel: v.get("channel").and_then(|c| s(c, "name")),
        verified: features.contains(&"VERIFIED"),
        partnered: features.contains(&"PARTNERED"),
    })
}

/// Guild name and id out of `POST /guilds` or `POST /invites/{code}`.
pub fn parse_joined_guild(v: &Value) -> Option<(GuildId, String)> {
    let g = v.get("guild").unwrap_or(v);
    Some((sf(g, "id")?, s(g, "name").unwrap_or_default()))
}

/// Template the official client's "Create My Own" sends: Discord's
/// "Blank Server". Without it a user account's `POST /guilds` gets 403.
pub const BLANK_GUILD_TEMPLATE: &str = "2TffvPucqHkN";

/// Body of `POST /guilds`, shaped like the official client's.
pub fn create_guild_body(name: &str, icon_data_uri: Option<&str>) -> Value {
    json!({
        "name": name.trim(),
        "icon": icon_data_uri,
        "channels": [],
        "system_channel_id": null,
        "guild_template_code": BLANK_GUILD_TEMPLATE,
    })
}

/// Longest icon file accepted for a new server.
pub const MAX_ICON_BYTES: usize = 4 * 1024 * 1024;

/// `data:` URI for a server icon, sniffing the format from the magic
/// bytes. Anything that is not PNG, JPEG, GIF or WebP is rejected.
pub fn icon_data_uri(bytes: &[u8]) -> Result<String, &'static str> {
    use base64::Engine;
    if bytes.len() > MAX_ICON_BYTES {
        return Err("That image is too large (4 MB at most).");
    }
    let mime = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF8") {
        "image/gif"
    } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else {
        return Err("Pick a PNG, JPEG, GIF or WebP image.");
    };
    Ok(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoverGuild {
    pub id: GuildId,
    pub name: String,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub splash: Option<String>,
    pub members: Option<u32>,
    pub online: Option<u32>,
    pub verified: bool,
    pub partnered: bool,
}

impl DiscoverGuild {
    pub fn icon_url(&self, cdn: &str) -> Option<String> {
        self.icon.as_ref().map(|h| {
            format!(
                "{}/icons/{}/{h}.png?size=96",
                cdn.trim_end_matches('/'),
                self.id
            )
        })
    }

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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiscoverPage {
    pub guilds: Vec<DiscoverGuild>,
    pub total: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoverCategory {
    pub id: u32,
    pub name: String,
}

/// Page size of the discovery grid.
pub const DISCOVER_PAGE: u32 = 24;

pub fn parse_discover_guild(v: &Value) -> Option<DiscoverGuild> {
    let features: Vec<&str> = v
        .get("features")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    Some(DiscoverGuild {
        id: sf(v, "id")?,
        name: s(v, "name")?,
        description: s(v, "description"),
        icon: s(v, "icon"),
        splash: s(v, "splash").or_else(|| s(v, "discovery_splash")),
        members: n(v, "approximate_member_count"),
        online: n(v, "approximate_presence_count"),
        verified: features.contains(&"VERIFIED"),
        partnered: features.contains(&"PARTNERED"),
    })
}

/// Accepts `{ guilds, total }`, `{ hits, total }` or a bare array.
pub fn parse_discover(v: &Value) -> DiscoverPage {
    let list = match v {
        Value::Array(a) => Some(a),
        Value::Object(o) => o
            .get("guilds")
            .or_else(|| o.get("hits"))
            .and_then(Value::as_array),
        _ => None,
    };
    DiscoverPage {
        guilds: list
            .map(|a| a.iter().filter_map(parse_discover_guild).collect())
            .unwrap_or_default(),
        total: v.get("total").and_then(Value::as_u64).map(|t| t as u32),
    }
}

pub fn parse_categories(v: &Value) -> Vec<DiscoverCategory> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    Some(DiscoverCategory {
                        id: n(c, "id")?,
                        name: s(c, "name")?,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Path of the discovery listing: the default feed, a category or a text
/// search.
pub fn discover_path(query: &str, category: Option<u32>, offset: u32) -> String {
    let q = query.trim();
    if q.is_empty() {
        let mut p = format!("/discoverable-guilds?offset={offset}&limit={DISCOVER_PAGE}");
        if let Some(c) = category {
            p.push_str(&format!("&category_id={c}"));
        }
        p
    } else {
        let mut p = format!(
            "/discovery/search?query={}&offset={offset}&limit={DISCOVER_PAGE}",
            percent_encode(q)
        );
        if let Some(c) = category {
            p.push_str(&format!("&category_id={c}"));
        }
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_msg(id: u64) -> Value {
        json!({"id": id.to_string(), "channel_id": "10", "content": format!("m{id}"),
               "author": {"id": "2", "username": "alice"}, "attachments": [], "embeds": [],
               "mentions": [], "type": 0})
    }

    #[test]
    fn search_filters_are_split_from_the_text() {
        let users = |n: &str| (n.eq_ignore_ascii_case("alice")).then_some(Snowflake(2));
        let chans = |n: &str| (n == "general").then_some(Snowflake(10));
        let q = parse_search_query(
            "hello from:@Alice has:image in:#general world has:Link",
            users,
            chans,
        )
        .unwrap();
        assert_eq!(q.content, "hello world");
        assert_eq!(q.authors, vec![Snowflake(2)]);
        assert_eq!(q.has, vec!["image", "link"]);
        assert_eq!(q.channels, vec![Snowflake(10)]);
        assert!(!q.is_empty());

        let only = parse_search_query("has:file", users, chans).unwrap();
        assert!(only.content.is_empty() && !only.is_empty());
        assert!(parse_search_query("  ", users, chans).unwrap().is_empty());
        assert!(
            parse_search_query("from:bob", users, chans)
                .unwrap_err()
                .contains("bob")
        );
        assert!(parse_search_query("in:nowhere", users, chans).is_err());
        assert!(parse_search_query("has:gif", users, chans).is_err());
        // A bare prefix is just text.
        assert_eq!(
            parse_search_query("from: x", users, chans).unwrap().content,
            "from: x"
        );
        // Duplicates collapse.
        let q = parse_search_query("from:alice from:ALICE", users, chans).unwrap();
        assert_eq!(q.authors.len(), 1);
    }

    #[test]
    fn pins_accept_both_layouts_and_skip_garbage() {
        let legacy = parse_pins(json!([raw_msg(3), raw_msg(2), {"nonsense": true}]));
        assert_eq!(
            legacy.iter().map(|m| m.id.0).collect::<Vec<_>>(),
            vec![3, 2]
        );
        let paged = parse_pins(json!({
            "items": [{"pinned_at": "2026-01-01T00:00:00Z", "message": raw_msg(9)}],
            "has_more": false
        }));
        assert_eq!(paged.len(), 1);
        assert_eq!(paged[0].id.0, 9);
        assert!(parse_pins(json!({"message": "x"})).is_empty());
    }

    #[test]
    fn mentions_paths() {
        assert_eq!(
            mentions_path(None, None),
            "/users/@me/mentions?limit=25&roles=true&everyone=true"
        );
        assert_eq!(
            mentions_path(Some(Snowflake(5)), Some(Snowflake(7))),
            "/users/@me/mentions?limit=25&roles=true&everyone=true&before=5&guild_id=7"
        );
    }

    #[test]
    fn notify_levels_round_trip() {
        for l in [
            NotifyLevel::All,
            NotifyLevel::Mentions,
            NotifyLevel::Nothing,
            NotifyLevel::Default,
        ] {
            assert_eq!(NotifyLevel::from_wire(Some(i64::from(l.wire()))), l);
        }
        assert_eq!(NotifyLevel::from_wire(None), NotifyLevel::Default);
        assert_eq!(NotifyLevel::from_wire(Some(9)), NotifyLevel::Default);
    }

    #[test]
    fn mute_expiry() {
        let now = 1_000_000;
        let timed = MuteDuration::Hour1.apply(NotifyLevel::Default, now);
        assert!(timed.is_muted(now + 3_599_000));
        assert!(!timed.is_muted(now + 3_600_001));
        let forever = MuteDuration::Forever.apply(NotifyLevel::Mentions, now);
        assert!(forever.is_muted(u64::MAX - 1));
        assert!(!ChannelNotify::default().is_muted(now));
        assert!(ChannelNotify::default().is_default());
        assert!(!forever.is_default());
    }

    #[test]
    fn override_payloads() {
        let now = 1_700_000_000_000;
        let v = mute_override(MuteDuration::Minutes15, now);
        assert_eq!(v["muted"], true);
        assert_eq!(v["mute_config"]["selected_time_window"], 900);
        assert_eq!(
            v["mute_config"]["end_time"],
            Value::String(iso_from_ms(now + 900_000))
        );
        assert_eq!(
            parse_iso_ms(v["mute_config"]["end_time"].as_str().unwrap()),
            Some(now + 900_000)
        );
        let f = mute_override(MuteDuration::Forever, now);
        assert_eq!(f["mute_config"]["selected_time_window"], -1);
        assert!(f["mute_config"]["end_time"].is_null());
        assert_eq!(
            unmute_override(),
            json!({"muted": false, "mute_config": null})
        );
        assert_eq!(
            level_override(NotifyLevel::Nothing),
            json!({"message_notifications": 2})
        );
        assert_eq!(
            level_override(NotifyLevel::Default),
            json!({"message_notifications": 3})
        );
        assert_eq!(
            channel_override_body(Snowflake(42), unmute_override()),
            json!({"channel_overrides": {"42": {"muted": false, "mute_config": null}}})
        );
        assert_eq!(guild_settings_path(None), "/users/@me/guilds/@me/settings");
        assert_eq!(
            guild_settings_path(Some(Snowflake(3))),
            "/users/@me/guilds/3/settings"
        );
    }

    #[test]
    fn channel_notify_from_wire() {
        let c = ChannelNotify::from_wire(&json!({
            "channel_id": "5", "message_notifications": 1, "muted": true,
            "mute_config": {"selected_time_window": 3600, "end_time": "2026-01-01T10:00:00.000Z"}
        }));
        assert_eq!(c.level, NotifyLevel::Mentions);
        assert!(c.muted);
        assert_eq!(c.mute_end_ms, parse_iso_ms("2026-01-01T10:00:00Z"));
        let f = ChannelNotify::from_wire(&json!({"muted": true, "mute_config": null}));
        assert!(f.muted && f.mute_end_ms.is_none());
    }

    #[test]
    fn invite_codes() {
        for (input, want) in [
            ("abc123", Some("abc123")),
            ("  https://discord.gg/abc123  ", Some("abc123")),
            ("discord.gg/abc-123/", Some("abc-123")),
            ("https://discord.com/invite/Rust?event=1", Some("Rust")),
            ("http://www.discordapp.com/invite/x_y", Some("x_y")),
            ("", None),
            ("https://example.com/abc", None),
            ("not a code", None),
            ("discord.gg/", None),
        ] {
            assert_eq!(parse_invite_code(input).as_deref(), want, "{input}");
        }
        assert!(parse_invite_code(&"a".repeat(65)).is_none());
    }

    #[test]
    fn invite_preview_parsing() {
        let v = json!({
            "code": "abc",
            "guild": {"id": "55", "name": "Rustaceans", "icon": "hash", "description": "Crabs",
                      "features": ["VERIFIED", "COMMUNITY"]},
            "channel": {"id": "1", "name": "welcome", "type": 0},
            "inviter": {"id": "9", "username": "ferris", "global_name": "Ferris"},
            "approximate_member_count": 1200, "approximate_presence_count": 300
        });
        let p = parse_invite("abc", &v).unwrap();
        assert_eq!(p.guild_id, Snowflake(55));
        assert_eq!(p.name, "Rustaceans");
        assert_eq!(p.members, Some(1200));
        assert_eq!(p.online, Some(300));
        assert_eq!(p.inviter.as_deref(), Some("Ferris"));
        assert_eq!(p.channel.as_deref(), Some("welcome"));
        assert!(p.verified && !p.partnered);
        assert_eq!(
            p.icon_url("https://cdn.example/").as_deref(),
            Some("https://cdn.example/icons/55/hash.png?size=96")
        );
        assert!(parse_invite("abc", &json!({"code": "abc"})).is_none());
    }

    #[test]
    fn joined_guild_from_either_shape() {
        assert_eq!(
            parse_joined_guild(&json!({"id": "7", "name": "New"})),
            Some((Snowflake(7), "New".into()))
        );
        assert_eq!(
            parse_joined_guild(&json!({"code": "x", "guild": {"id": "8", "name": "Via invite"}})),
            Some((Snowflake(8), "Via invite".into()))
        );
        assert_eq!(parse_joined_guild(&json!({"code": "x"})), None);
    }

    #[test]
    fn create_guild_payload_and_icons() {
        let b = create_guild_body("  My server ", None);
        assert_eq!(b["name"], "My server");
        assert!(b["icon"].is_null());
        assert_eq!(b["guild_template_code"], BLANK_GUILD_TEMPLATE);
        let b = create_guild_body("x", Some("data:image/png;base64,AA"));
        assert_eq!(b["icon"], "data:image/png;base64,AA");

        let png = [0x89, b'P', b'N', b'G', 1, 2, 3];
        assert!(
            icon_data_uri(&png)
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        assert!(
            icon_data_uri(&[0xFF, 0xD8, 0xFF, 0])
                .unwrap()
                .contains("image/jpeg")
        );
        assert!(icon_data_uri(b"GIF89a..").unwrap().contains("image/gif"));
        assert!(
            icon_data_uri(b"RIFF\0\0\0\0WEBPVP8 ")
                .unwrap()
                .contains("image/webp")
        );
        assert!(icon_data_uri(b"MZ not an image").is_err());
        assert!(icon_data_uri(&vec![0x89; MAX_ICON_BYTES + 1]).is_err());
    }

    #[test]
    fn discovery_parsing_and_paths() {
        let v = json!({"guilds": [
            {"id": "1", "name": "A", "description": "d", "icon": "i",
             "approximate_member_count": 10, "approximate_presence_count": 4,
             "features": ["PARTNERED"]},
            {"id": "2"}
        ], "total": 99});
        let p = parse_discover(&v);
        assert_eq!(p.total, Some(99));
        assert_eq!(p.guilds.len(), 1);
        assert!(p.guilds[0].partnered);
        assert_eq!(p.guilds[0].acronym(), "A");
        assert_eq!(
            parse_discover(&json!([{"id": "3", "name": "Bare Array"}]))
                .guilds
                .len(),
            1
        );
        assert_eq!(
            parse_discover(&json!({"hits": [{"id": "3", "name": "H"}]}))
                .guilds
                .len(),
            1
        );
        assert!(parse_discover(&json!("x")).guilds.is_empty());

        let c = parse_categories(&json!([{"id": 1, "name": "Gaming"}, {"id": 2}, {"name": "x"}]));
        assert_eq!(
            c,
            vec![DiscoverCategory {
                id: 1,
                name: "Gaming".into()
            }]
        );

        assert_eq!(
            discover_path("", None, 0),
            "/discoverable-guilds?offset=0&limit=24"
        );
        assert_eq!(
            discover_path(" ", Some(3), 24),
            "/discoverable-guilds?offset=24&limit=24&category_id=3"
        );
        assert_eq!(
            discover_path("rust lang", None, 0),
            "/discovery/search?query=rust%20lang&offset=0&limit=24"
        );
        assert_eq!(
            discover_path("a", Some(1), 0),
            "/discovery/search?query=a&offset=0&limit=24&category_id=1"
        );
    }
}
