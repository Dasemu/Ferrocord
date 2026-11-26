//! Server administration calls: invites, leaving, editing and deleting a
//! server. Bodies and answers are built and parsed by the pure helpers at
//! the bottom (unit-tested).

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
