//! The preloaded user settings protobuf (`GET`/`PATCH
//! /users/@me/settings-proto/1`), where the official client keeps the
//! account-wide settings: theme, message display, embeds, privacy filters,
//! status, locale and more.
//!
//! Field numbers follow
//! `discord_protos.discord_users.v1.PreloadedUserSettings` (the public
//! discord-protos definitions, checked against `PreloadedUserSettings.proto`
//! of the PyPI package discord-protos 1.2.429). The ones used here:
//!
//! ```text
//! PreloadedUserSettings {
//!   TextAndImagesSettings text_and_images = 6;  NotificationSettings ... = 7;
//!   PrivacySettings privacy = 8;  StatusSettings status = 11;
//!   LocalizationSettings localization = 12;  AppearanceSettings appearance = 13; }
//! TextAndImagesSettings { StringValue render_spoilers = 4;
//!   BoolValue inline_attachment_media = 9; BoolValue inline_embed_media = 10;
//!   BoolValue gif_auto_play = 11; BoolValue render_embeds = 12;
//!   BoolValue render_reactions = 13; BoolValue animate_emoji = 14;
//!   UInt32Value animate_stickers = 15; BoolValue message_display_compact = 17;
//!   BoolValue convert_emoticons = 21; DmSpamFilterV2 dm_spam_filter_v2 = 27;
//!   ExplicitContentSettings explicit_content_settings = 29; }
//! ExplicitContentSettings { ExplicitContentRedaction explicit_content_guilds = 1;
//!   explicit_content_friend_dm = 2; explicit_content_non_friend_dm = 3; }
//! PrivacySettings { bool default_guilds_restricted = 4;
//!   UInt32Value friend_source_flags = 11; }
//! StatusSettings { StringValue status = 1; CustomStatus custom_status = 2; }
//! CustomStatus { string text = 1; fixed64 emoji_id = 2; string emoji_name = 3;
//!   fixed64 expires_at_ms = 4; fixed64 created_at_ms = 5; }
//! LocalizationSettings { StringValue locale = 1; }
//! AppearanceSettings { Theme theme = 1; bool developer_mode = 2;
//!   TimestampHourCycle timestamp_hour_cycle = 9; }
//! ```
//!
//! `*Value` are the `google.protobuf` wrappers (a message whose field 1 is
//! the value): absent means "not set" (the client default applies), an
//! empty wrapper means the zero value (`false`, `0`, `""`).
//!
//! Writes follow Discord's PATCH rule: every top-level field that is sent
//! *replaces* the stored one, omitted top-level fields stay unchanged. So
//! [`apply`] edits the message read with `GET` (every other field and every
//! unknown field copied byte for byte) and [`top_level`] cuts out only the
//! top-level fields an edit touched, each complete.

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
#[error("malformed settings protobuf")]
pub struct ProtoError;

const WT_VARINT: u8 = 0;
const WT_I64: u8 = 1;
const WT_LEN: u8 = 2;
const WT_I32: u8 = 5;

/// Deepest path [`get`] / [`set`] follow (the settings are 3 levels deep).
const MAX_DEPTH: usize = 4;

/// One field as it was on the wire.
#[derive(Debug, Clone)]
struct RawField<'a> {
    number: u32,
    wire: u8,
    /// The varint/fixed bytes, or the inside of a length-delimited field.
    payload: &'a [u8],
    /// The whole field, tag included.
    raw: &'a [u8],
}

fn read_varint(buf: &[u8], pos: &mut usize) -> Result<u64, ProtoError> {
    let mut v: u64 = 0;
    for shift in (0..64).step_by(7) {
        let b = *buf.get(*pos).ok_or(ProtoError)?;
        *pos += 1;
        v |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err(ProtoError)
}

fn fields(buf: &[u8]) -> Result<Vec<RawField<'_>>, ProtoError> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < buf.len() {
        let start = pos;
        let key = read_varint(buf, &mut pos)?;
        let number = u32::try_from(key >> 3).map_err(|_| ProtoError)?;
        if number == 0 {
            return Err(ProtoError);
        }
        let wire = (key & 7) as u8;
        let payload_start;
        match wire {
            WT_VARINT => {
                payload_start = pos;
                read_varint(buf, &mut pos)?;
            }
            WT_I64 => {
                payload_start = pos;
                pos = pos.checked_add(8).ok_or(ProtoError)?;
            }
            WT_LEN => {
                let len = usize::try_from(read_varint(buf, &mut pos)?).map_err(|_| ProtoError)?;
                payload_start = pos;
                pos = pos.checked_add(len).ok_or(ProtoError)?;
            }
            WT_I32 => {
                payload_start = pos;
                pos = pos.checked_add(4).ok_or(ProtoError)?;
            }
            _ => return Err(ProtoError),
        }
        if pos > buf.len() {
            return Err(ProtoError);
        }
        out.push(RawField {
            number,
            wire,
            payload: &buf[payload_start..pos],
            raw: &buf[start..pos],
        });
    }
    Ok(out)
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_tag(out: &mut Vec<u8>, number: u32, wire: u8) {
    put_varint(out, (u64::from(number) << 3) | u64::from(wire));
}

fn put_bytes(out: &mut Vec<u8>, number: u32, bytes: &[u8]) {
    put_tag(out, number, WT_LEN);
    put_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn put_varint_field(out: &mut Vec<u8>, number: u32, v: u64) {
    if v != 0 {
        put_tag(out, number, WT_VARINT);
        put_varint(out, v);
    }
}

fn put_fixed64(out: &mut Vec<u8>, number: u32, v: u64) {
    if v != 0 {
        put_tag(out, number, WT_I64);
        out.extend_from_slice(&v.to_le_bytes());
    }
}

fn fixed64_of(f: &RawField<'_>) -> Option<u64> {
    (f.wire == WT_I64)
        .then(|| <[u8; 8]>::try_from(f.payload).ok().map(u64::from_le_bytes))
        .flatten()
}

/// How a setting is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `google.protobuf.BoolValue`.
    WrapBool,
    /// `google.protobuf.UInt32Value`.
    WrapU32,
    /// `google.protobuf.StringValue`.
    WrapString,
    /// A plain proto3 `bool` or enum (varint; the zero value is not written).
    Varint,
}

/// A setting's value.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Value {
    Bool(bool),
    U32(u32),
    Str(String),
}

impl Value {
    pub fn as_bool(&self) -> bool {
        match self {
            Self::Bool(b) => *b,
            Self::U32(n) => *n != 0,
            Self::Str(s) => !s.is_empty(),
        }
    }

    pub fn as_u32(&self) -> u32 {
        match self {
            Self::Bool(b) => u32::from(*b),
            Self::U32(n) => *n,
            Self::Str(_) => 0,
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Str(s) => s,
            _ => "",
        }
    }
}

/// The settings Ferrocord reads and writes, with their place in the proto.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Synced {
    // text_and_images (6)
    RenderSpoilers,
    InlineAttachmentMedia,
    InlineEmbedMedia,
    GifAutoPlay,
    RenderEmbeds,
    RenderReactions,
    AnimateEmoji,
    AnimateStickers,
    MessageDisplayCompact,
    ConvertEmoticons,
    DmSpamFilter,
    ExplicitGuilds,
    ExplicitFriendDm,
    ExplicitNonFriendDm,
    // privacy (8)
    DefaultGuildsRestricted,
    FriendSourceFlags,
    // status (11)
    Status,
    // localization (12)
    Locale,
    // appearance (13)
    Theme,
    DeveloperMode,
    HourCycle,
}

impl Synced {
    pub const ALL: [Synced; 21] = [
        Self::RenderSpoilers,
        Self::InlineAttachmentMedia,
        Self::InlineEmbedMedia,
        Self::GifAutoPlay,
        Self::RenderEmbeds,
        Self::RenderReactions,
        Self::AnimateEmoji,
        Self::AnimateStickers,
        Self::MessageDisplayCompact,
        Self::ConvertEmoticons,
        Self::DmSpamFilter,
        Self::ExplicitGuilds,
        Self::ExplicitFriendDm,
        Self::ExplicitNonFriendDm,
        Self::DefaultGuildsRestricted,
        Self::FriendSourceFlags,
        Self::Status,
        Self::Locale,
        Self::Theme,
        Self::DeveloperMode,
        Self::HourCycle,
    ];

    /// Field numbers from the top-level message down to the value.
    pub fn path(self) -> &'static [u32] {
        match self {
            Self::RenderSpoilers => &[6, 4],
            Self::InlineAttachmentMedia => &[6, 9],
            Self::InlineEmbedMedia => &[6, 10],
            Self::GifAutoPlay => &[6, 11],
            Self::RenderEmbeds => &[6, 12],
            Self::RenderReactions => &[6, 13],
            Self::AnimateEmoji => &[6, 14],
            Self::AnimateStickers => &[6, 15],
            Self::MessageDisplayCompact => &[6, 17],
            Self::ConvertEmoticons => &[6, 21],
            Self::DmSpamFilter => &[6, 27],
            Self::ExplicitGuilds => &[6, 29, 1],
            Self::ExplicitFriendDm => &[6, 29, 2],
            Self::ExplicitNonFriendDm => &[6, 29, 3],
            Self::DefaultGuildsRestricted => &[8, 4],
            Self::FriendSourceFlags => &[8, 11],
            Self::Status => &[11, 1],
            Self::Locale => &[12, 1],
            Self::Theme => &[13, 1],
            Self::DeveloperMode => &[13, 2],
            Self::HourCycle => &[13, 9],
        }
    }

    pub fn kind(self) -> Kind {
        match self {
            Self::RenderSpoilers | Self::Status | Self::Locale => Kind::WrapString,
            Self::AnimateStickers | Self::FriendSourceFlags => Kind::WrapU32,
            Self::DmSpamFilter
            | Self::ExplicitGuilds
            | Self::ExplicitFriendDm
            | Self::ExplicitNonFriendDm
            | Self::DefaultGuildsRestricted
            | Self::Theme
            | Self::DeveloperMode
            | Self::HourCycle => Kind::Varint,
            _ => Kind::WrapBool,
        }
    }

    /// The official client's value when the field is not set.
    pub fn default_value(self) -> Value {
        match self {
            Self::RenderSpoilers => Value::Str("ON_CLICK".into()),
            Self::Status => Value::Str("online".into()),
            Self::Locale => Value::Str("en-US".into()),
            // 0 always, 1 on interaction, 2 never; desktop default always.
            Self::AnimateStickers => Value::U32(0),
            // Mutual friends + mutual servers + everyone.
            Self::FriendSourceFlags => Value::U32(FRIENDS_ALL),
            Self::MessageDisplayCompact | Self::ConvertEmoticons => Value::Bool(false),
            Self::DmSpamFilter
            | Self::ExplicitGuilds
            | Self::ExplicitFriendDm
            | Self::ExplicitNonFriendDm
            | Self::Theme
            | Self::HourCycle => Value::U32(0),
            Self::DefaultGuildsRestricted | Self::DeveloperMode => Value::Bool(false),
            _ => Value::Bool(true),
        }
    }

    /// Top-level field this setting lives in.
    pub fn top(self) -> u32 {
        self.path()[0]
    }
}

/// `PrivacySettings.friend_source_flags` bits.
pub const FRIENDS_MUTUAL_FRIENDS: u32 = 1 << 1;
pub const FRIENDS_MUTUAL_GUILDS: u32 = 1 << 2;
/// Requires the two others.
pub const FRIENDS_NO_RELATION: u32 = 1 << 3;
pub const FRIENDS_ALL: u32 = FRIENDS_MUTUAL_FRIENDS | FRIENDS_MUTUAL_GUILDS | FRIENDS_NO_RELATION;

/// `StatusSettings.custom_status`. `Debug` hides the text (user content).
#[derive(Clone, PartialEq, Eq, Default)]
pub struct CustomStatus {
    pub text: String,
    pub emoji_id: u64,
    pub emoji_name: String,
    /// 0 = never expires.
    pub expires_at_ms: u64,
    pub created_at_ms: u64,
}

impl std::fmt::Debug for CustomStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CustomStatus")
            .field("text_len", &self.text.len())
            .field("expires_at_ms", &self.expires_at_ms)
            .finish_non_exhaustive()
    }
}

impl CustomStatus {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.emoji_name.is_empty() && self.emoji_id == 0
    }

    /// Whether it still shows at `now_ms`.
    pub fn active(&self, now_ms: u64) -> bool {
        !self.is_empty() && (self.expires_at_ms == 0 || self.expires_at_ms > now_ms)
    }

    fn encode(&self) -> Vec<u8> {
        let mut v = Vec::new();
        if !self.text.is_empty() {
            put_bytes(&mut v, 1, self.text.as_bytes());
        }
        put_fixed64(&mut v, 2, self.emoji_id);
        if !self.emoji_name.is_empty() {
            put_bytes(&mut v, 3, self.emoji_name.as_bytes());
        }
        put_fixed64(&mut v, 4, self.expires_at_ms);
        put_fixed64(&mut v, 5, self.created_at_ms);
        v
    }

    fn decode(buf: &[u8]) -> Result<Self, ProtoError> {
        let mut c = Self::default();
        let s = |f: &RawField<'_>| String::from_utf8_lossy(f.payload).into_owned();
        for f in fields(buf)? {
            match (f.number, f.wire) {
                (1, WT_LEN) => c.text = s(&f),
                (2, WT_I64) => c.emoji_id = fixed64_of(&f).unwrap_or(0),
                (3, WT_LEN) => c.emoji_name = s(&f),
                (4, WT_I64) => c.expires_at_ms = fixed64_of(&f).unwrap_or(0),
                (5, WT_I64) => c.created_at_ms = fixed64_of(&f).unwrap_or(0),
                _ => {}
            }
        }
        Ok(c)
    }
}

const CUSTOM_STATUS_PATH: [u32; 2] = [11, 2];

/// The payloads of every occurrence of field `n` in `msg` (an embedded
/// message repeated on the wire is merged, so all of them count).
fn occurrences<'a>(msg: &'a [u8], n: u32) -> Result<Vec<RawField<'a>>, ProtoError> {
    Ok(fields(msg)?.into_iter().filter(|f| f.number == n).collect())
}

/// The raw last occurrence of the field at `path`, merging repeated
/// embedded messages along the way.
fn find<'a>(msg: &'a [u8], path: &[u32]) -> Result<Option<RawField<'a>>, ProtoError> {
    if path.is_empty() || path.len() > MAX_DEPTH {
        return Err(ProtoError);
    }
    let found = occurrences(msg, path[0])?;
    if path.len() == 1 {
        return Ok(found.into_iter().last());
    }
    // Later occurrences win, so search them last-first.
    for f in found.iter().rev() {
        if f.wire != WT_LEN {
            continue;
        }
        if let Some(inner) = find(f.payload, &path[1..])? {
            return Ok(Some(inner));
        }
    }
    Ok(None)
}

fn decode_value(f: &RawField<'_>, kind: Kind) -> Option<Value> {
    match kind {
        Kind::Varint => {
            let v = (f.wire == WT_VARINT).then(|| read_varint(f.payload, &mut 0).ok())??;
            Some(Value::U32(u32::try_from(v).unwrap_or(u32::MAX)))
        }
        Kind::WrapBool | Kind::WrapU32 | Kind::WrapString => {
            if f.wire != WT_LEN {
                return None;
            }
            // Wrapper message: field 1 is the value (absent = zero).
            let inner = fields(f.payload).ok()?;
            let v = inner.iter().filter(|i| i.number == 1).last();
            Some(match kind {
                Kind::WrapString => Value::Str(
                    v.filter(|i| i.wire == WT_LEN)
                        .map(|i| String::from_utf8_lossy(i.payload).into_owned())
                        .unwrap_or_default(),
                ),
                _ => {
                    let n = v
                        .filter(|i| i.wire == WT_VARINT)
                        .and_then(|i| read_varint(i.payload, &mut 0).ok())
                        .unwrap_or(0);
                    if kind == Kind::WrapBool {
                        Value::Bool(n != 0)
                    } else {
                        Value::U32(u32::try_from(n).unwrap_or(u32::MAX))
                    }
                }
            })
        }
    }
}

/// The stored value of `s`, `None` when it is not set.
pub fn get(msg: &[u8], s: Synced) -> Result<Option<Value>, ProtoError> {
    let f = find(msg, s.path())?;
    let v = f.and_then(|f| decode_value(&f, s.kind()));
    // Plain proto3 bools are reported as bools.
    Ok(match (v, s.default_value()) {
        (Some(Value::U32(n)), Value::Bool(_)) => Some(Value::Bool(n != 0)),
        (v, _) => v,
    })
}

/// The stored value of `s`, or the client default.
pub fn get_or_default(msg: &[u8], s: Synced) -> Value {
    get(msg, s)
        .ok()
        .flatten()
        .unwrap_or_else(|| s.default_value())
}

/// The custom status, if one is set.
pub fn custom_status(msg: &[u8]) -> Result<Option<CustomStatus>, ProtoError> {
    match find(msg, &CUSTOM_STATUS_PATH)? {
        Some(f) if f.wire == WT_LEN => Ok(Some(CustomStatus::decode(f.payload)?)),
        _ => Ok(None),
    }
}

/// One change to the settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    Set(Synced, Value),
    /// `None` clears the custom status.
    CustomStatus(Option<CustomStatus>),
}

impl Edit {
    pub fn top(&self) -> u32 {
        match self {
            Self::Set(s, _) => s.top(),
            Self::CustomStatus(_) => CUSTOM_STATUS_PATH[0],
        }
    }
}

/// Encodes the field at the end of a path (`None` = remove it).
fn encode_leaf(number: u32, kind: Kind, v: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    match kind {
        Kind::Varint => put_varint_field(&mut out, number, u64::from(v.as_u32())),
        Kind::WrapBool | Kind::WrapU32 => {
            let mut w = Vec::new();
            put_varint_field(&mut w, 1, u64::from(v.as_u32()));
            put_bytes(&mut out, number, &w);
        }
        Kind::WrapString => {
            let mut w = Vec::new();
            if !v.as_str().is_empty() {
                put_bytes(&mut w, 1, v.as_str().as_bytes());
            }
            put_bytes(&mut out, number, &w);
        }
    }
    out
}

/// Replaces the field at `path` in `msg` with `leaf` (the whole encoded
/// field, or nothing to remove it). Repeated occurrences of an embedded
/// message on the way are merged into one, written where the first was;
/// every other field keeps its bytes and its place.
fn replace(msg: &[u8], path: &[u32], leaf: &[u8]) -> Result<Vec<u8>, ProtoError> {
    if path.is_empty() || path.len() > MAX_DEPTH {
        return Err(ProtoError);
    }
    let n = path[0];
    let all = fields(msg)?;
    let new_field = if path.len() == 1 {
        leaf.to_vec()
    } else {
        // Merge every occurrence (protobuf concatenation = merge).
        let mut inner = Vec::new();
        for f in all.iter().filter(|f| f.number == n) {
            if f.wire != WT_LEN {
                return Err(ProtoError);
            }
            inner.extend_from_slice(f.payload);
        }
        let inner = replace(&inner, &path[1..], leaf)?;
        let mut out = Vec::new();
        put_bytes(&mut out, n, &inner);
        out
    };
    let mut out = Vec::with_capacity(msg.len() + new_field.len());
    let mut written = false;
    for f in &all {
        if f.number == n {
            if !written {
                out.extend_from_slice(&new_field);
                written = true;
            }
        } else {
            out.extend_from_slice(f.raw);
        }
    }
    if !written {
        out.extend_from_slice(&new_field);
    }
    Ok(out)
}

/// Applies `edits` to `msg` (raw `PreloadedUserSettings`), keeping every
/// other byte.
pub fn apply(msg: &[u8], edits: &[Edit]) -> Result<Vec<u8>, ProtoError> {
    let mut cur = msg.to_vec();
    for e in edits {
        cur = match e {
            Edit::Set(s, v) => {
                let path = s.path();
                let (last, _) = path.split_last().ok_or(ProtoError)?;
                replace(&cur, path, &encode_leaf(*last, s.kind(), v))?
            }
            Edit::CustomStatus(c) => {
                let leaf = match c {
                    Some(c) if !c.is_empty() => {
                        let mut out = Vec::new();
                        put_bytes(&mut out, CUSTOM_STATUS_PATH[1], &c.encode());
                        out
                    }
                    _ => Vec::new(),
                };
                replace(&cur, &CUSTOM_STATUS_PATH, &leaf)?
            }
        };
    }
    Ok(cur)
}

/// The top-level fields numbered `tops` (each complete, merged into one
/// occurrence): the body of a `PATCH` that changes only those categories.
/// A category that ends up empty is still sent (as an empty message), so
/// the server resets it rather than keeping the old one.
pub fn top_level(msg: &[u8], tops: &[u32]) -> Result<Vec<u8>, ProtoError> {
    let all = fields(msg)?;
    let mut out = Vec::new();
    for &n in tops {
        let mut inner = Vec::new();
        for f in all.iter().filter(|f| f.number == n) {
            if f.wire != WT_LEN {
                return Err(ProtoError);
            }
            inner.extend_from_slice(f.payload);
        }
        put_bytes(&mut out, n, &inner);
    }
    Ok(out)
}

/// The top-level fields `edits` touch, in order, without repeats.
pub fn touched(edits: &[Edit]) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    for e in edits {
        if !out.contains(&e.top()) {
            out.push(e.top());
        }
    }
    out
}

/// `AppearanceSettings.Theme`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Theme {
    Dark,
    Light,
    /// "Ash" in the official picker.
    Darker,
    /// "Onyx".
    Midnight,
}

impl Theme {
    pub const ALL: [Theme; 4] = [Self::Light, Self::Dark, Self::Darker, Self::Midnight];

    pub fn from_wire(v: u32) -> Self {
        match v {
            2 => Self::Light,
            3 => Self::Darker,
            4 => Self::Midnight,
            // 0 (unset) means dark.
            _ => Self::Dark,
        }
    }

    pub fn wire(self) -> u32 {
        match self {
            Self::Dark => 1,
            Self::Light => 2,
            Self::Darker => 3,
            Self::Midnight => 4,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::Light => "Light",
            Self::Darker => "Ash",
            Self::Midnight => "Onyx",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Versions, text_and_images with an unknown field and a repeated
    /// string, a privacy category we never touch, and an unknown top-level
    /// field 99 (fixed32).
    fn sample() -> Vec<u8> {
        let mut versions = Vec::new();
        put_varint_field(&mut versions, 1, 7);
        put_varint_field(&mut versions, 3, 42);
        let mut text = Vec::new();
        put_bytes(&mut text, 5, b"people"); // emoji_picker_collapsed_sections
        put_bytes(&mut text, 12, &[]); // render_embeds = false
        put_bytes(&mut text, 77, b"future"); // unknown
        let mut s = Vec::new();
        put_bytes(&mut s, 1, &versions);
        put_bytes(&mut s, 6, &text);
        put_bytes(&mut s, 8, &[0x20, 0x01]); // default_guilds_restricted = true
        put_tag(&mut s, 99, WT_I32);
        s.extend_from_slice(&[1, 2, 3, 4]);
        s
    }

    #[test]
    fn reads_wrapped_and_plain_values() {
        let s = sample();
        assert_eq!(get(&s, Synced::RenderEmbeds), Ok(Some(Value::Bool(false))));
        assert_eq!(get(&s, Synced::RenderReactions), Ok(None));
        assert_eq!(
            get_or_default(&s, Synced::RenderReactions),
            Value::Bool(true)
        );
        assert_eq!(
            get(&s, Synced::DefaultGuildsRestricted),
            Ok(Some(Value::Bool(true)))
        );
        assert_eq!(get_or_default(&s, Synced::Theme), Value::U32(0));
        assert_eq!(get_or_default(&[], Synced::Status).as_str(), "online");
    }

    #[test]
    fn round_trip_keeps_unknown_fields_byte_for_byte() {
        let s = sample();
        // A no-op rewrite of a known field leaves the message intact.
        let same = apply(&s, &[Edit::Set(Synced::RenderEmbeds, Value::Bool(false))]).unwrap();
        assert_eq!(same, s);
        let next = apply(
            &s,
            &[
                Edit::Set(Synced::RenderEmbeds, Value::Bool(true)),
                Edit::Set(Synced::Theme, Value::U32(Theme::Midnight.wire())),
                Edit::Set(Synced::ExplicitFriendDm, Value::U32(2)),
            ],
        )
        .unwrap();
        assert_eq!(
            get(&next, Synced::RenderEmbeds),
            Ok(Some(Value::Bool(true)))
        );
        assert_eq!(get(&next, Synced::Theme), Ok(Some(Value::U32(4))));
        assert_eq!(
            get(&next, Synced::ExplicitFriendDm),
            Ok(Some(Value::U32(2)))
        );
        // Versions first, the unknown top-level field kept, privacy untouched.
        assert_eq!(&next[..6], &s[..6]);
        assert!(next.windows(6).any(|w| w == [0x9d, 0x06, 1, 2, 3, 4]));
        let tops: Vec<u32> = fields(&next).unwrap().iter().map(|f| f.number).collect();
        assert_eq!(tops, [1, 6, 8, 99, 13]);
        let text = find(&next, &[6]).unwrap().unwrap();
        let inner: Vec<u32> = fields(text.payload)
            .unwrap()
            .iter()
            .map(|f| f.number)
            .collect();
        // The unknown and repeated fields survive, in place.
        assert_eq!(inner, [5, 12, 77, 29]);
        assert!(text.payload.windows(6).any(|w| w == b"future"));
        assert_eq!(
            get(&next, Synced::DefaultGuildsRestricted),
            Ok(Some(Value::Bool(true)))
        );
    }

    #[test]
    fn zero_values_and_clears() {
        let s = apply(&[], &[Edit::Set(Synced::DeveloperMode, Value::Bool(true))]).unwrap();
        assert_eq!(s, [0x6a, 0x02, 0x10, 0x01]);
        // proto3: false is not written, the category stays (empty).
        let s = apply(&s, &[Edit::Set(Synced::DeveloperMode, Value::Bool(false))]).unwrap();
        assert_eq!(s, [0x6a, 0x00]);
        // An empty wrapper is an explicit false, not "unset".
        let s = apply(&[], &[Edit::Set(Synced::GifAutoPlay, Value::Bool(false))]).unwrap();
        assert_eq!(get(&s, Synced::GifAutoPlay), Ok(Some(Value::Bool(false))));
        let s = apply(&[], &[Edit::Set(Synced::Status, Value::Str("dnd".into()))]).unwrap();
        assert_eq!(get_or_default(&s, Synced::Status).as_str(), "dnd");
    }

    #[test]
    fn repeated_embedded_messages_are_merged() {
        // Two text_and_images occurrences: the second's value wins and the
        // rewrite merges them into one.
        let mut s = Vec::new();
        put_bytes(&mut s, 6, &[0x62, 0x00]); // render_embeds = false
        put_bytes(&mut s, 6, &[0x6a, 0x00]); // render_reactions = false
        assert_eq!(get(&s, Synced::RenderEmbeds), Ok(Some(Value::Bool(false))));
        let next = apply(&s, &[Edit::Set(Synced::AnimateEmoji, Value::Bool(false))]).unwrap();
        assert_eq!(occurrences(&next, 6).unwrap().len(), 1);
        assert_eq!(
            get(&next, Synced::RenderEmbeds),
            Ok(Some(Value::Bool(false)))
        );
        assert_eq!(
            get(&next, Synced::RenderReactions),
            Ok(Some(Value::Bool(false)))
        );
        assert_eq!(
            get(&next, Synced::AnimateEmoji),
            Ok(Some(Value::Bool(false)))
        );
    }

    #[test]
    fn custom_status_round_trip() {
        let c = CustomStatus {
            text: "shipping".into(),
            emoji_id: 0,
            emoji_name: "🚀".into(),
            expires_at_ms: 1_700_000_000_000,
            created_at_ms: 1_690_000_000_000,
        };
        let s = apply(
            &sample(),
            &[
                Edit::Set(Synced::Status, Value::Str("idle".into())),
                Edit::CustomStatus(Some(c.clone())),
            ],
        )
        .unwrap();
        assert_eq!(custom_status(&s), Ok(Some(c.clone())));
        assert_eq!(get_or_default(&s, Synced::Status).as_str(), "idle");
        assert!(c.active(1_699_999_999_999) && !c.active(1_700_000_000_000));
        let s = apply(&s, &[Edit::CustomStatus(None)]).unwrap();
        assert_eq!(custom_status(&s), Ok(None));
        assert_eq!(get_or_default(&s, Synced::Status).as_str(), "idle");
    }

    #[test]
    fn patch_body_holds_only_touched_categories() {
        let edits = [
            Edit::Set(Synced::Theme, Value::U32(2)),
            Edit::Set(Synced::RenderEmbeds, Value::Bool(true)),
            Edit::Set(Synced::HourCycle, Value::U32(2)),
        ];
        assert_eq!(touched(&edits), [13, 6]);
        let next = apply(&sample(), &edits).unwrap();
        let body = top_level(&next, &touched(&edits)).unwrap();
        let tops: Vec<u32> = fields(&body).unwrap().iter().map(|f| f.number).collect();
        assert_eq!(tops, [13, 6]);
        // The whole text_and_images category goes, unknown field included.
        assert!(body.windows(6).any(|w| w == b"future"));
        assert_eq!(get(&body, Synced::Theme), Ok(Some(Value::U32(2))));
    }

    #[test]
    fn malformed_input_is_an_error_not_a_panic() {
        assert!(get(&[0x0a, 0x05, 1], Synced::Theme).is_err());
        assert!(apply(&[0xff], &[]).is_ok()); // nothing to parse with no edits
        assert!(apply(&[0xff], &[Edit::Set(Synced::Theme, Value::U32(1))]).is_err());
        // A scalar where a category should be.
        assert!(
            apply(
                &[0x30, 0x01],
                &[Edit::Set(Synced::RenderEmbeds, Value::Bool(true))]
            )
            .is_err()
        );
        assert_eq!(Theme::from_wire(0), Theme::Dark);
        assert_eq!(Theme::from_wire(Theme::Darker.wire()), Theme::Darker);
    }
}
