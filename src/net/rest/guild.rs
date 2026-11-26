//! Server administration calls: invites, leaving, editing and deleting a
//! server, and creating, editing, deleting and reordering its channels.
//! Bodies and answers are built and parsed by the pure helpers at the
//! bottom (unit-tested).

use serde_json::{Value, json};

use super::{Method, RestClient};
use crate::error::{Error, Result};
use crate::net::model::*;

/// Invites made from the "Invite People" dialog last 7 days with no use
/// limit, the official client's default.
pub const INVITE_MAX_AGE_S: u32 = 7 * 24 * 60 * 60;

impl RestClient {
    /// `POST /channels/{c}/invites` — returns the invite code.
    pub async fn create_invite(&self, channel: ChannelId) -> Result<String> {
        let v = self
            .request(
                Method::POST,
                &format!("/channels/{channel}/invites"),
                Some(invite_body()),
            )
            .await?;
        parse_invite_code(&v).ok_or_else(|| Error::Protocol("invite answer without a code".into()))
    }

    /// `DELETE /users/@me/guilds/{g}` — leaves a server.
    pub async fn leave_guild(&self, guild: GuildId) -> Result<()> {
        self.request(
            Method::DELETE,
            &format!("/users/@me/guilds/{guild}"),
            Some(json!({ "lurking": false })),
        )
        .await
        .map(|_| ())
    }

    /// `PATCH /guilds/{g}` with the changed overview fields.
    pub async fn modify_guild(&self, guild: GuildId, edit: &GuildEdit) -> Result<()> {
        self.request(
            Method::PATCH,
            &format!("/guilds/{guild}"),
            Some(edit.body()),
        )
        .await
        .map(|_| ())
    }

    /// `POST /guilds/{g}/channels` — creates a channel or category.
    pub async fn create_channel(&self, guild: GuildId, new: &NewChannel) -> Result<Channel> {
        let v = self
            .request(
                Method::POST,
                &format!("/guilds/{guild}/channels"),
                Some(new.body()),
            )
            .await?;
        let w: crate::net::types::WireChannel = serde_json::from_value(v)
            .map_err(|_| Error::Protocol("channel answer without a channel".into()))?;
        Ok(w.into_domain(Some(guild)).0)
    }

    /// `PATCH /channels/{c}` with the changed fields.
    pub async fn modify_channel(&self, channel: ChannelId, edit: &ChannelEdit) -> Result<()> {
        self.request(
            Method::PATCH,
            &format!("/channels/{channel}"),
            Some(edit.body()),
        )
        .await
        .map(|_| ())
    }

    /// `DELETE /channels/{c}` — deletes a server channel or category.
    pub async fn delete_channel(&self, channel: ChannelId) -> Result<()> {
        self.request(Method::DELETE, &format!("/channels/{channel}"), None)
            .await
            .map(|_| ())
    }

    /// `PATCH /guilds/{g}/channels` — new positions of several channels.
    pub async fn reorder_channels(&self, guild: GuildId, moves: &[(ChannelId, i32)]) -> Result<()> {
        self.request(
            Method::PATCH,
            &format!("/guilds/{guild}/channels"),
            Some(reorder_body(moves)),
        )
        .await
        .map(|_| ())
    }

    /// `DELETE /guilds/{g}` — deletes a server (owner only).
    pub async fn delete_guild(&self, guild: GuildId) -> Result<()> {
        self.request(Method::DELETE, &format!("/guilds/{guild}"), None)
            .await
            .map(|_| ())
    }
}

/// Body of `POST /channels/{c}/invites`.
pub fn invite_body() -> Value {
    json!({
        "max_age": INVITE_MAX_AGE_S,
        "max_uses": 0,
        "temporary": false,
    })
}

pub fn parse_invite_code(v: &Value) -> Option<String> {
    v.get("code")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .map(str::to_owned)
}

/// Changes to a server's overview. `None` = unchanged.
#[derive(Clone, Default, PartialEq)]
pub struct GuildEdit {
    pub name: Option<String>,
    /// `Some(None)` removes the icon, `Some(Some(uri))` sets a `data:` URI.
    pub icon: Option<Option<String>>,
}

impl std::fmt::Debug for GuildEdit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuildEdit")
            .field("name", &self.name.as_ref().map(|_| "<set>"))
            .field(
                "icon",
                &self
                    .icon
                    .as_ref()
                    .map(|i| if i.is_some() { "<set>" } else { "<removed>" }),
            )
            .finish()
    }
}

impl GuildEdit {
    pub fn is_empty(&self) -> bool {
        self.name.is_none() && self.icon.is_none()
    }

    pub fn body(&self) -> Value {
        let mut m = serde_json::Map::new();
        if let Some(n) = &self.name {
            m.insert("name".into(), json!(n.trim()));
        }
        if let Some(i) = &self.icon {
            m.insert("icon".into(), json!(i));
        }
        Value::Object(m)
    }
}

/// Kinds a channel can be created as from Ferrocord.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewKind {
    Text,
    Voice,
    Category,
}

impl NewKind {
    pub fn wire(self) -> u8 {
        match self {
            Self::Text => 0,
            Self::Voice => 2,
            Self::Category => 4,
        }
    }
}

/// Body of `POST /guilds/{g}/channels`.
#[derive(Clone, PartialEq)]
pub struct NewChannel {
    pub name: String,
    pub kind: NewKind,
    pub parent: Option<ChannelId>,
}

impl std::fmt::Debug for NewChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewChannel")
            .field("kind", &self.kind)
            .field("parent", &self.parent)
            .finish_non_exhaustive()
    }
}

impl NewChannel {
    pub fn body(&self) -> Value {
        let mut b = json!({
            "name": self.name.trim(),
            "type": self.kind.wire(),
            "permission_overwrites": [],
        });
        if self.kind != NewKind::Category
            && let Some(p) = self.parent
        {
            b["parent_id"] = json!(p.to_string());
        }
        b
    }
}

/// Changes to a channel. `None` = unchanged.
#[derive(Clone, Default, PartialEq)]
pub struct ChannelEdit {
    pub name: Option<String>,
    /// `Some("")` clears the topic.
    pub topic: Option<String>,
    pub nsfw: Option<bool>,
    /// `Some(None)` moves the channel out of any category.
    pub parent: Option<Option<ChannelId>>,
}

impl std::fmt::Debug for ChannelEdit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelEdit")
            .field("name", &self.name.as_ref().map(|_| "<set>"))
            .field("topic", &self.topic.as_ref().map(|_| "<set>"))
            .field("nsfw", &self.nsfw)
            .field("parent", &self.parent)
            .finish()
    }
}

impl ChannelEdit {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub fn body(&self) -> Value {
        let mut m = serde_json::Map::new();
        if let Some(n) = &self.name {
            m.insert("name".into(), json!(n.trim()));
        }
        if let Some(t) = &self.topic {
            let t = t.trim();
            m.insert(
                "topic".into(),
                if t.is_empty() { Value::Null } else { json!(t) },
            );
        }
        if let Some(n) = self.nsfw {
            m.insert("nsfw".into(), json!(n));
        }
        if let Some(p) = self.parent {
            m.insert("parent_id".into(), json!(p.map(|p| p.to_string())));
        }
        Value::Object(m)
    }
}

/// Body of `PATCH /guilds/{g}/channels`.
pub fn reorder_body(moves: &[(ChannelId, i32)]) -> Value {
    Value::Array(
        moves
            .iter()
            .map(|(id, pos)| json!({ "id": id.to_string(), "position": pos }))
            .collect(),
    )
}

/// How Discord stores a text channel name: lower case, runs of spaces as
/// one dash. Voice channels and categories keep the name as typed.
pub fn channel_slug(name: &str) -> String {
    let mut out = String::new();
    for w in name.split_whitespace() {
        if !out.is_empty() {
            out.push('-');
        }
        out.extend(w.chars().flat_map(char::to_lowercase));
    }
    out
}

/// Public link of an invite code.
pub fn invite_url(code: &str) -> String {
    format!("https://discord.gg/{code}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invite_body_and_code() {
        let b = invite_body();
        assert_eq!(b["max_age"], 604_800);
        assert_eq!(b["max_uses"], 0);
        assert_eq!(b["temporary"], false);
        assert_eq!(
            parse_invite_code(&json!({"code": "abc", "guild": {}})).as_deref(),
            Some("abc")
        );
        assert_eq!(parse_invite_code(&json!({"code": ""})), None);
        assert_eq!(parse_invite_code(&json!({})), None);
        assert_eq!(invite_url("abc"), "https://discord.gg/abc");
    }

    #[test]
    fn channel_bodies() {
        let n = NewChannel {
            name: " general ".into(),
            kind: NewKind::Text,
            parent: Some(Snowflake(9)),
        };
        assert_eq!(
            n.body(),
            json!({"name": "general", "type": 0, "parent_id": "9", "permission_overwrites": []})
        );
        let n = NewChannel {
            name: "Stuff".into(),
            kind: NewKind::Category,
            parent: Some(Snowflake(9)),
        };
        assert_eq!(
            n.body(),
            json!({"name": "Stuff", "type": 4, "permission_overwrites": []})
        );
        assert!(!format!("{n:?}").contains("Stuff"));
        assert_eq!(ChannelEdit::default().body(), json!({}));
        let e = ChannelEdit {
            name: Some("a".into()),
            topic: Some("  ".into()),
            nsfw: Some(true),
            parent: Some(None),
        };
        assert_eq!(
            e.body(),
            json!({"name": "a", "topic": null, "nsfw": true, "parent_id": null})
        );
        let e = ChannelEdit {
            parent: Some(Some(Snowflake(4))),
            ..Default::default()
        };
        assert_eq!(e.body(), json!({"parent_id": "4"}));
        assert_eq!(
            reorder_body(&[(Snowflake(1), 0), (Snowflake(2), 1)]),
            json!([{"id": "1", "position": 0}, {"id": "2", "position": 1}])
        );
        assert_eq!(channel_slug("  Off  Topic Chat "), "off-topic-chat");
        assert_eq!(channel_slug("ÁRBOL"), "árbol");
    }

    #[test]
    fn guild_edit_sends_only_changes() {
        assert!(GuildEdit::default().is_empty());
        assert_eq!(GuildEdit::default().body(), json!({}));
        let e = GuildEdit {
            name: Some("  New  ".into()),
            icon: None,
        };
        assert_eq!(e.body(), json!({"name": "New"}));
        let e = GuildEdit {
            name: None,
            icon: Some(None),
        };
        assert_eq!(e.body(), json!({"icon": null}));
        let e = GuildEdit {
            name: None,
            icon: Some(Some("data:image/png;base64,AA".into())),
        };
        assert_eq!(e.body(), json!({"icon": "data:image/png;base64,AA"}));
        assert!(!format!("{e:?}").contains("base64"));
    }
}
