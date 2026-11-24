//! The plugin ABI, version 1: hook and host-call ids, error codes and the
//! MessagePack payloads exchanged over linear memory.
//!
//! Every payload is MessagePack with structs encoded as **maps keyed by
//! field name** (`rmp_serde::to_vec_named`), so fields can be added in a
//! later minor revision without breaking older plugins: unknown keys are
//! ignored and missing optional keys default. Enums with data are maps with
//! a `"type"` key. See `docs/plugins.md` for the byte-level contract.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// ABI revision implemented by this crate. A plugin's manifest names the
/// version it was built for (`api_version`) and its `fc_abi_version`
/// export must return the same number.
pub const ABI_VERSION: u32 = 1;

/// Name of the import module every host function lives in.
pub const IMPORT_MODULE: &str = "ferrocord";

/// Hook ids: the first argument of the guest's `fc_call` export.
pub mod hook {
    /// Input [`super::LoadInfo`]; no output.
    pub const LOAD: i32 = 1;
    /// No input, no output. The instance is dropped afterwards.
    pub const UNLOAD: i32 = 2;
    /// Input [`super::Event`]; no output. Read-only and asynchronous.
    pub const EVENT: i32 = 3;
    /// Input [`super::TransformInput`]; output [`super::TransformOutput`].
    pub const TRANSFORM: i32 = 4;
    /// Input [`super::CommandInput`]; no output. A user action.
    pub const COMMAND: i32 = 5;
    /// Input [`super::ActionInput`]; no output. A user action.
    pub const ACTION: i32 = 6;
    /// Input [`super::SettingChanged`]; no output.
    pub const SETTING_CHANGED: i32 = 7;
    /// Input [`super::HttpResponse`]; no output.
    pub const HTTP_RESPONSE: i32 = 8;
}

/// Host-call operation ids: the first argument of the `host_call` import.
pub mod op {
    /// [`super::LogReq`] → nothing.
    pub const LOG: i32 = 1;
    /// [`super::ToastReq`] → nothing.
    pub const TOAST: i32 = 2;
    /// [`super::KeyReq`] → `Option<`[`super::SettingValue`]`>`.
    pub const GET_SETTING: i32 = 3;
    /// [`super::SetSettingReq`] → nothing.
    pub const SET_SETTING: i32 = 4;
    /// [`super::KeyReq`] → `Option<bin>`. Permission `storage`.
    pub const KV_GET: i32 = 5;
    /// [`super::KvSetReq`] → nothing. Permission `storage`.
    pub const KV_SET: i32 = 6;
    /// [`super::KeyReq`] → nothing. Permission `storage`.
    pub const KV_DELETE: i32 = 7;
    /// [`super::HttpRequest`] → request id (`u64`). Permission `network`.
    pub const HTTP_FETCH: i32 = 8;
    /// [`super::TextReq`] → nothing. Permission `send_messages`, user action.
    pub const SEND_MESSAGE: i32 = 9;
    /// [`super::DecorateReq`] → nothing. Permission `decorations`.
    pub const DECORATE: i32 = 10;
    /// [`super::TextReq`] → nothing. Permission `clipboard`, user action.
    pub const CLIPBOARD_WRITE: i32 = 11;
    /// [`super::TextReq`] → nothing. Permission `composer`, user action.
    pub const COMPOSER_INSERT: i32 = 12;
    /// [`super::TextReq`] (one Unicode emoji) → nothing. Permission
    /// `reactions`, user action (reacts to the action's message).
    pub const ADD_REACTION: i32 = 13;
    /// No input → `u64` Unix time in milliseconds.
    pub const NOW_MS: i32 = 14;
}

/// Negative results of `host_call`.
pub mod err {
    /// The plugin lacks the permission this call needs.
    pub const DENIED: i32 = -1;
    /// The input did not decode or failed validation.
    pub const BAD_INPUT: i32 = -2;
    /// Only allowed while handling a user action (command / menu action).
    pub const NOT_USER_ACTION: i32 = -3;
    /// A size, rate or quota limit was hit.
    pub const LIMIT: i32 = -4;
    /// Unknown operation id.
    pub const UNKNOWN_OP: i32 = -5;
    /// `http_fetch` to a host the manifest does not list.
    pub const HOST_NOT_ALLOWED: i32 = -6;
    /// The host could not do it right now (storage failure, ...).
    pub const UNAVAILABLE: i32 = -7;
}

/// A plugin setting value. Encoded untagged: a MessagePack bool, integer
/// or string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SettingValue {
    Bool(bool),
    Int(i64),
    Str(String),
}

impl SettingValue {
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// [`hook::LOAD`] input.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LoadInfo {
    pub api_version: u32,
    pub plugin_id: String,
    /// Current values of every setting declared in the manifest.
    pub settings: BTreeMap<String, SettingValue>,
}

/// A chat message as plugins see it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MessageInfo {
    pub id: u64,
    pub channel_id: u64,
    pub guild_id: Option<u64>,
    pub author_id: u64,
    /// Display name of the author.
    pub author_name: String,
    pub content: String,
    pub timestamp_ms: u64,
    /// Sent by the logged-in user.
    pub mine: bool,
}

/// [`hook::EVENT`] input. Internally tagged: `{"type": "message_create", ...}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Needs `read_messages`.
    MessageCreate { message: MessageInfo },
    /// Needs `read_messages`. `content` is absent when only embeds changed.
    MessageUpdate {
        id: u64,
        channel_id: u64,
        content: Option<String>,
    },
    /// Needs `read_messages`.
    MessageDelete { id: u64, channel_id: u64 },
    /// The newest loaded messages of a channel (on open and history pages,
    /// at most 50). Needs `read_messages`.
    History {
        channel_id: u64,
        messages: Vec<MessageInfo>,
    },
    /// The user opened another channel (or none).
    ChannelSwitch {
        channel_id: Option<u64>,
        guild_id: Option<u64>,
    },
    /// Needs `read_presence`. `status` is online / idle / dnd / offline.
    Presence { user_id: u64, status: String },
}

impl Event {
    /// The manifest `events` name that subscribes to this event.
    pub fn name(&self) -> &'static str {
        match self {
            Self::MessageCreate { .. } => "message_create",
            Self::MessageUpdate { .. } => "message_update",
            Self::MessageDelete { .. } => "message_delete",
            Self::History { .. } => "history",
            Self::ChannelSwitch { .. } => "channel_switch",
            Self::Presence { .. } => "presence",
        }
    }
}

/// Every event name a manifest may subscribe to.
pub const EVENT_NAMES: [&str; 6] = [
    "message_create",
    "message_update",
    "message_delete",
    "history",
    "channel_switch",
    "presence",
];

/// [`hook::TRANSFORM`] input: the composer text about to be sent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TransformInput {
    pub channel_id: u64,
    pub text: String,
}

/// [`hook::TRANSFORM`] output. `None` keeps the text unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TransformOutput {
    pub text: Option<String>,
}

/// [`hook::COMMAND`] input: the user typed `/<name> <args>` and pressed Enter.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CommandInput {
    pub name: String,
    pub args: String,
    pub channel_id: u64,
    pub guild_id: Option<u64>,
}

/// [`hook::ACTION`] input: the user picked a message context-menu action.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ActionInput {
    /// The `id` of the manifest `[[actions]]` entry.
    pub action: String,
    pub message: MessageInfo,
}

/// [`hook::SETTING_CHANGED`] input (the user changed it on the Plugins page).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingChanged {
    pub key: String,
    pub value: SettingValue,
}

/// [`op::HTTP_FETCH`] input.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpRequest {
    /// `GET` or `POST`.
    pub method: String,
    /// `https://` URL whose host is in the manifest's `network_hosts`.
    pub url: String,
    pub headers: Vec<(String, String)>,
    #[serde(with = "serde_bytes")]
    pub body: Vec<u8>,
}

/// [`hook::HTTP_RESPONSE`] input.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpResponse {
    pub request_id: u64,
    /// HTTP status, or 0 when the request failed (see `error`).
    pub status: u16,
    #[serde(with = "serde_bytes")]
    pub body: Vec<u8>,
    pub error: Option<String>,
}

/// [`op::LOG`] input. Levels: 0 error, 1 warn, 2 info, 3 debug.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LogReq {
    pub level: u8,
    pub message: String,
}

/// Input of the calls that take one text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextReq {
    pub text: String,
}

/// [`op::TOAST`] input.
pub type ToastReq = TextReq;

/// Input of the calls that take one key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyReq {
    pub key: String,
}

/// [`op::SET_SETTING`] input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetSettingReq {
    pub key: String,
    pub value: SettingValue,
}

/// [`op::KV_SET`] input.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KvSetReq {
    pub key: String,
    #[serde(with = "serde_bytes")]
    pub value: Vec<u8>,
}

/// How the host shows a decoration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecorationKind {
    /// A small pill under the message.
    #[default]
    Badge,
    /// A muted text line under the message (a translation, a note).
    Line,
    /// Tints the message row; `text` is shown as a badge if not empty.
    Highlight,
}

/// One decoration. The host renders it with the theme; plugins never draw.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Decoration {
    pub kind: DecorationKind,
    pub text: String,
    /// Shown on hover (badges only).
    pub tooltip: Option<String>,
}

/// [`op::DECORATE`] input: replaces this plugin's decorations of one
/// message (an empty list removes them).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DecorateReq {
    pub message_id: u64,
    pub decorations: Vec<Decoration>,
}

/// Encodes `v` the way the ABI requires (structs as maps).
pub fn encode<T: Serialize + ?Sized>(v: &T) -> Result<Vec<u8>, rmp_serde::encode::Error> {
    rmp_serde::to_vec_named(v)
}

/// Decodes an ABI payload.
pub fn decode<'a, T: Deserialize<'a>>(b: &'a [u8]) -> Result<T, rmp_serde::decode::Error> {
    rmp_serde::from_slice(b)
}

/// Packs a guest pointer and length into `fc_call`'s `i64` result.
pub fn pack(ptr: u32, len: u32) -> i64 {
    ((u64::from(ptr) << 32) | u64::from(len)) as i64
}

/// Inverse of [`pack`].
pub fn unpack(v: i64) -> (u32, u32) {
    let v = v as u64;
    ((v >> 32) as u32, v as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_tagged_maps() {
        let ev = Event::MessageDelete {
            id: 7,
            channel_id: 9,
        };
        let b = encode(&ev).unwrap();
        // fixmap(3) "type" "message_delete" ...
        assert_eq!(b[0], 0x83);
        let back: Event = decode(&b).unwrap();
        assert_eq!(back, ev);
        assert_eq!(back.name(), "message_delete");
    }

    #[test]
    fn unknown_fields_are_ignored_and_missing_ones_default() {
        #[derive(Serialize)]
        struct Newer<'a> {
            text: &'a str,
            extra: u32,
        }
        let b = encode(&Newer {
            text: "hi",
            extra: 3,
        })
        .unwrap();
        let t: TextReq = decode(&b).unwrap();
        assert_eq!(t.text, "hi");
        let empty = encode(&BTreeMap::<String, u8>::new()).unwrap();
        let m: MessageInfo = decode(&empty).unwrap();
        assert_eq!(m, MessageInfo::default());
    }

    #[test]
    fn setting_values_are_untagged() {
        for v in [
            SettingValue::Bool(true),
            SettingValue::Int(-4),
            SettingValue::Str("x".into()),
        ] {
            let b = encode(&v).unwrap();
            assert_eq!(decode::<SettingValue>(&b).unwrap(), v);
        }
        assert_eq!(encode(&SettingValue::Bool(true)).unwrap(), [0xc3]);
    }

    #[test]
    fn bytes_are_bin() {
        let r = KvSetReq {
            key: "k".into(),
            value: vec![1, 2, 3],
        };
        let b = encode(&r).unwrap();
        // ... "value" bin8(3) 01 02 03
        assert!(b.ends_with(&[0xc4, 3, 1, 2, 3]));
    }

    #[test]
    fn pack_roundtrip() {
        assert_eq!(unpack(pack(0x1234, 0xffff_ffff)), (0x1234, 0xffff_ffff));
        assert_eq!(unpack(pack(u32::MAX, 1)), (u32::MAX, 1));
    }
}
