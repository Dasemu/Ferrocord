//! REST calls behind User Settings: the preloaded user settings proto and
//! the profile fields. All of them go through `RestClient::request`, so
//! they share the per-route rate limiter and 429 handling.

use serde_json::{Value, json};

use super::{Method, RestClient};
use crate::error::{Error, Result};
use crate::net::user_settings::{self, Edit};

const PRELOADED: &str = "/users/@me/settings-proto/1";

/// Profile fields the settings page edits (`None` = unchanged).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileEdit {
    pub global_name: Option<String>,
    pub pronouns: Option<String>,
    pub bio: Option<String>,
}

impl ProfileEdit {
    pub fn is_empty(&self) -> bool {
        self.global_name.is_none() && self.pronouns.is_none() && self.bio.is_none()
    }

    /// `PATCH /users/@me` body: an empty display name is `null` (falls back
    /// to the username, like clearing it in the official client).
    pub fn user_body(&self) -> Option<Value> {
        let n = self.global_name.as_deref()?;
        let n = n.trim();
        Some(if n.is_empty() {
            json!({ "global_name": null })
        } else {
            json!({ "global_name": n })
        })
    }

    /// `PATCH /users/@me/profile` body.
    pub fn profile_body(&self) -> Option<Value> {
        let mut m = serde_json::Map::new();
        if let Some(p) = &self.pronouns {
            m.insert("pronouns".into(), json!(p.trim()));
        }
        if let Some(b) = &self.bio {
            m.insert("bio".into(), json!(b.trim_end()));
        }
        (!m.is_empty()).then_some(Value::Object(m))
    }
}

impl RestClient {
    /// `GET /users/@me/settings-proto/1`: the preloaded user settings as
    /// raw protobuf bytes.
    pub async fn user_settings(&self) -> Result<Vec<u8>> {
        let v = self.request(Method::GET, PRELOADED, None).await?;
        decode(&v)
    }

    /// `PATCH /users/@me/settings-proto/1 {settings: base64}`. `partial`
    /// holds only the top-level fields to replace.
    pub async fn patch_user_settings(&self, partial: &[u8]) -> Result<()> {
        use base64::Engine;
        let body = json!({
            "settings": base64::engine::general_purpose::STANDARD.encode(partial),
        });
        let v = self.request(Method::PATCH, PRELOADED, Some(body)).await?;
        if v["out_of_date"].as_bool() == Some(true) {
            return Err(Error::Protocol("settings changed elsewhere".into()));
        }
        Ok(())
    }

    /// Read-modify-write of the settings: reads the current message,
    /// applies `edits` and sends back every top-level category they touch,
    /// each complete with the fields this client does not know. Returns
    /// the whole message as it now stands.
    pub async fn edit_user_settings(&self, edits: &[Edit]) -> Result<Vec<u8>> {
        let bad = |_| Error::Protocol("unreadable settings".into());
        let current = self.user_settings().await?;
        let next = user_settings::apply(&current, edits).map_err(bad)?;
        let partial =
            user_settings::top_level(&next, &user_settings::touched(edits)).map_err(bad)?;
        self.patch_user_settings(&partial).await?;
        Ok(next)
    }

    /// Display name (`PATCH /users/@me`), pronouns and bio
    /// (`PATCH /users/@me/profile`), as the official profile page saves
    /// them.
    pub async fn edit_profile(&self, edit: &ProfileEdit) -> Result<()> {
        if let Some(body) = edit.user_body() {
            self.request(Method::PATCH, "/users/@me", Some(body))
                .await?;
        }
        if let Some(body) = edit.profile_body() {
            self.request(Method::PATCH, "/users/@me/profile", Some(body))
                .await?;
        }
        Ok(())
    }
}

/// `{"settings": "<base64>"}` to bytes; missing or empty = empty message.
fn decode(v: &Value) -> Result<Vec<u8>> {
    use base64::Engine;
    let s = v["settings"].as_str().unwrap_or("");
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|_| Error::Protocol("unreadable settings".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_bodies() {
        let e = ProfileEdit {
            global_name: Some("  ".into()),
            pronouns: None,
            bio: Some("hi\n".into()),
        };
        assert_eq!(e.user_body(), Some(json!({ "global_name": null })));
        assert_eq!(e.profile_body(), Some(json!({ "bio": "hi" })));
        let e = ProfileEdit {
            global_name: Some(" Ferris ".into()),
            ..Default::default()
        };
        assert_eq!(e.user_body(), Some(json!({ "global_name": "Ferris" })));
        assert_eq!(e.profile_body(), None);
        assert!(ProfileEdit::default().is_empty());
    }
}
