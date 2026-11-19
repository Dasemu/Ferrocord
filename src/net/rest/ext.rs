//! REST calls behind the channel-header buttons, the server rail and
//! discovery. All of them go through `RestClient::request`, so they share
//! the per-route rate limiter and 429 handling of every other call.

use serde_json::{Value, json};

use super::{Method, RestClient};
use crate::error::{Error, Result};
use crate::net::extras::{self, DiscoverCategory, DiscoverPage, InvitePreview, MuteDuration};
use crate::net::model::*;

impl RestClient {
    /// `GET /channels/{id}/pins` — newest pin first.
    pub async fn pins(&self, channel: ChannelId) -> Result<Vec<Message>> {
        let v = self
            .request(Method::GET, &format!("/channels/{channel}/pins"), None)
            .await?;
        Ok(extras::parse_pins(v))
    }

    /// `DELETE /channels/{c}/pins/{m}` (needs Manage Messages in servers).
    pub async fn unpin(&self, channel: ChannelId, message: MessageId) -> Result<()> {
        self.request(
            Method::DELETE,
            &format!("/channels/{channel}/pins/{message}"),
            None,
        )
        .await
        .map(|_| ())
    }

    /// `GET /users/@me/mentions` — the inbox "Mentions" tab.
    pub async fn mentions(
        &self,
        before: Option<MessageId>,
        guild: Option<GuildId>,
    ) -> Result<Vec<Message>> {
        let v = self
            .request(Method::GET, &extras::mentions_path(before, guild), None)
            .await?;
        Ok(extras::parse_mentions(v))
    }

    /// `PATCH /users/@me/guilds/{guild|@me}/settings` with a body built by
    /// `extras::channel_override_body`.
    pub async fn patch_guild_settings(&self, guild: Option<GuildId>, body: Value) -> Result<()> {
        self.request(
            Method::PATCH,
            &extras::guild_settings_path(guild),
            Some(body),
        )
        .await
        .map(|_| ())
    }

    /// Mutes a channel for `d`.
    pub async fn mute_channel(
        &self,
        guild: Option<GuildId>,
        channel: ChannelId,
        d: MuteDuration,
        now_ms: u64,
    ) -> Result<()> {
        let body = extras::channel_override_body(channel, extras::mute_override(d, now_ms));
        self.patch_guild_settings(guild, body).await
    }

    /// `PATCH /users/@me/settings` with the folder list (the same legacy
    /// settings endpoint that already stores the status; see
    /// `docs/protocol.md` for why not `settings-proto`).
    pub async fn set_guild_folders(&self, body: Value) -> Result<()> {
        self.request(Method::PATCH, "/users/@me/settings", Some(body))
            .await
            .map(|_| ())
    }

    /// `GET /invites/{code}?with_counts=true&with_expiration=true`.
    pub async fn invite(&self, code: &str) -> Result<InvitePreview> {
        let path = format!(
            "/invites/{}?with_counts=true&with_expiration=true",
            percent_encode(code)
        );
        let v = self.request(Method::GET, &path, None).await?;
        extras::parse_invite(code, &v)
            .ok_or_else(|| Error::Protocol("invite without a server".into()))
    }

    /// `POST /invites/{code}` — joins the server. A captcha demand comes
    /// back as [`Error::Captcha`]; Ferrocord never tries to solve it.
    pub async fn join_invite(
        &self,
        code: &str,
        session_id: Option<&str>,
    ) -> Result<(GuildId, String)> {
        let body = match session_id.filter(|s| !s.is_empty()) {
            Some(s) => json!({ "session_id": s }),
            None => json!({}),
        };
        let v = self
            .request(
                Method::POST,
                &format!("/invites/{}", percent_encode(code)),
                Some(body),
            )
            .await?;
        extras::parse_joined_guild(&v)
            .ok_or_else(|| Error::Protocol("join answer without a server".into()))
    }

    /// `POST /guilds` — creates a server.
    pub async fn create_guild(
        &self,
        name: &str,
        icon_data_uri: Option<&str>,
    ) -> Result<(GuildId, String)> {
        let v = self
            .request(
                Method::POST,
                "/guilds",
                Some(extras::create_guild_body(name, icon_data_uri)),
            )
            .await?;
        extras::parse_joined_guild(&v)
            .ok_or_else(|| Error::Protocol("create answer without a server".into()))
    }

    /// `GET /discovery/categories`.
    pub async fn discovery_categories(&self) -> Result<Vec<DiscoverCategory>> {
        let v = self
            .request(Method::GET, "/discovery/categories", None)
            .await?;
        Ok(extras::parse_categories(&v))
    }

    /// `GET /discoverable-guilds` (feed / category) or
    /// `GET /discovery/search` (text query).
    pub async fn discover(
        &self,
        query: &str,
        category: Option<u32>,
        offset: u32,
    ) -> Result<DiscoverPage> {
        let v = self
            .request(
                Method::GET,
                &extras::discover_path(query, category, offset),
                None,
            )
            .await?;
        Ok(extras::parse_discover(&v))
    }

    /// `PUT /guilds/{id}/members/@me` — joins a discoverable server.
    pub async fn join_discoverable(&self, guild: GuildId, session_id: Option<&str>) -> Result<()> {
        let body = match session_id.filter(|s| !s.is_empty()) {
            Some(s) => json!({ "session_id": s }),
            None => json!({}),
        };
        self.request(
            Method::PUT,
            &format!("/guilds/{guild}/members/@me"),
            Some(body),
        )
        .await
        .map(|_| ())
    }
}
