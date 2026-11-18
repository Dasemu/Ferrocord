//! REST API client (`reqwest`) with rate limiting.

use std::sync::Arc;
use std::time::Duration;

use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::time::Instant;

use super::model::*;
use super::ratelimit::{RateLimitHeaders, RateLimiter, route_key};
use super::types::{CreateMessage, RateLimitBody, WireMessage, WireUser};
use crate::auth::secrets::Secret;
use crate::error::{Error, Result};

const MAX_RETRIES: u32 = 3;

/// Cheaply clonable REST handle.
#[derive(Clone)]
pub struct RestClient {
    http: reqwest::Client,
    base: Arc<str>,
    cdn: Arc<str>,
    token: Option<Secret>,
    limiter: Arc<Mutex<RateLimiter>>,
}

impl std::fmt::Debug for RestClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RestClient")
            .field("base", &self.base)
            .field("authenticated", &self.token.is_some())
            .finish()
    }
}

pub fn user_agent() -> String {
    format!(
        "Ferrocord/{} (+https://github.com/Dasemu/Ferrocord)",
        env!("CARGO_PKG_VERSION")
    )
}

impl RestClient {
    pub fn new(base: &str, cdn: &str, token: Option<Secret>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(user_agent())
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self {
            http,
            base: base.trim_end_matches('/').into(),
            cdn: cdn.trim_end_matches('/').into(),
            token,
            limiter: Arc::new(Mutex::new(RateLimiter::new())),
        })
    }

    pub fn with_token(&self, token: Secret) -> Self {
        Self {
            token: Some(token),
            ..self.clone()
        }
    }

    pub fn cdn_base(&self) -> &str {
        &self.cdn
    }

    /// Sends a request, waiting for rate limits and retrying 429s.
    async fn request(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let route = route_key(method.as_str(), path);
        let url = format!("{}{}", self.base, path);
        let mut attempt = 0;
        loop {
            // Wait for the bucket.
            loop {
                let wait = self.limiter.lock().await.acquire(&route, Instant::now());
                match wait {
                    Some(d) => {
                        tracing::debug!(route = %route, wait_ms = d.as_millis() as u64, "rate limited locally");
                        tokio::time::sleep(d).await;
                    }
                    None => break,
                }
            }
            let mut req = self.http.request(method.clone(), &url);
            if let Some(t) = &self.token {
                req = req.header(reqwest::header::AUTHORIZATION, t.expose());
            }
            if let Some(b) = &body {
                req = req.json(b);
            }
            let resp = req.send().await?;
            let status = resp.status();
            let headers = RateLimitHeaders::from_headers(resp.headers());
            let retry_after_header = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<f64>().ok());
            self.limiter
                .lock()
                .await
                .update(&route, &headers, Instant::now());

            if status == StatusCode::TOO_MANY_REQUESTS {
                let body: Option<RateLimitBody> = resp.json().await.ok();
                let secs = body
                    .as_ref()
                    .map(|b| b.retry_after)
                    .filter(|s| *s > 0.0)
                    .or(retry_after_header)
                    .unwrap_or(1.0);
                let global = headers.global || body.is_some_and(|b| b.global);
                let wait = Duration::from_secs_f64(secs.min(300.0));
                self.limiter
                    .lock()
                    .await
                    .on_429(&route, wait, global, Instant::now());
                attempt += 1;
                tracing::warn!(route = %route, retry_after_ms = wait.as_millis() as u64, global, "HTTP 429");
                if attempt > MAX_RETRIES {
                    return Err(Error::Http {
                        status: 429,
                        message: "rate limited".into(),
                    });
                }
                continue;
            }
            if status == StatusCode::NO_CONTENT {
                return Ok(Value::Null);
            }
            let text = resp.text().await?;
            if !status.is_success() {
                // The error body has a `message` field; it is a server
                // description, not user content.
                let message = serde_json::from_str::<Value>(&text)
                    .ok()
                    .and_then(|v| v["message"].as_str().map(String::from))
                    .unwrap_or_else(|| status.canonical_reason().unwrap_or("error").into());
                return Err(Error::Http {
                    status: status.as_u16(),
                    message,
                });
            }
            if text.is_empty() {
                return Ok(Value::Null);
            }
            return Ok(serde_json::from_str(&text)?);
        }
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        Ok(serde_json::from_value(
            self.request(Method::GET, path, None).await?,
        )?)
    }

    /// `GET /users/@me` — validates the token.
    pub async fn me(&self) -> Result<User> {
        let u: WireUser = self.get("/users/@me").await.map_err(|e| match e {
            Error::Http { status: 401, .. } => Error::Auth("invalid token".into()),
            other => other,
        })?;
        Ok(u.into())
    }

    /// `GET /channels/{id}/messages` — newest first on the wire; returned
    /// oldest first.
    pub async fn messages(
        &self,
        channel: ChannelId,
        before: Option<MessageId>,
        limit: u8,
    ) -> Result<Vec<Message>> {
        let mut path = format!("/channels/{channel}/messages?limit={}", limit.clamp(1, 100));
        if let Some(b) = before {
            path.push_str(&format!("&before={b}"));
        }
        let wire: Vec<WireMessage> = self.get(&path).await?;
        let mut msgs: Vec<Message> = wire.into_iter().map(WireMessage::into_domain).collect();
        msgs.sort_by_key(|m| m.id);
        Ok(msgs)
    }

    /// `POST /channels/{id}/messages`.
    pub async fn send_message(
        &self,
        channel: ChannelId,
        content: &str,
        nonce: &str,
    ) -> Result<Message> {
        let body = serde_json::to_value(CreateMessage {
            content,
            nonce,
            tts: false,
        })?;
        let v = self
            .request(
                Method::POST,
                &format!("/channels/{channel}/messages"),
                Some(body),
            )
            .await?;
        let wire: WireMessage = serde_json::from_value(v)?;
        Ok(wire.into_domain())
    }

    /// `POST /channels/{id}/typing`.
    pub async fn trigger_typing(&self, channel: ChannelId) -> Result<()> {
        self.request(Method::POST, &format!("/channels/{channel}/typing"), None)
            .await
            .map(|_| ())
    }

    /// `POST /channels/{c}/messages/{m}/ack` — marks the channel read.
    pub async fn ack(&self, channel: ChannelId, message: MessageId) -> Result<()> {
        self.request(
            Method::POST,
            &format!("/channels/{channel}/messages/{message}/ack"),
            Some(serde_json::json!({ "token": null })),
        )
        .await
        .map(|_| ())
    }

    /// `POST /users/@me/remote-auth/login` — exchanges a QR-login ticket for
    /// the RSA-encrypted token.
    pub async fn remote_auth_login(&self, ticket: &str) -> Result<String> {
        let v = self
            .request(
                Method::POST,
                "/users/@me/remote-auth/login",
                Some(serde_json::json!({ "ticket": ticket })),
            )
            .await?;
        v["encrypted_token"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| Error::Protocol("remote-auth login without encrypted_token".into()))
    }

    /// Downloads a CDN asset (avatar, icon, attachment preview).
    pub async fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let resp = self.http.get(url).send().await?;
        if !resp.status().is_success() {
            return Err(Error::Http {
                status: resp.status().as_u16(),
                message: "asset download failed".into(),
            });
        }
        Ok(resp.bytes().await?.to_vec())
    }

    pub fn avatar_url(&self, user: &User, size: u32) -> Option<String> {
        user.avatar
            .as_ref()
            .map(|h| format!("{}/avatars/{}/{h}.png?size={size}", self.cdn, user.id))
    }

    pub fn guild_icon_url(&self, guild: &Guild, size: u32) -> Option<String> {
        guild
            .icon
            .as_ref()
            .map(|h| format!("{}/icons/{}/{h}.png?size={size}", self.cdn, guild.id))
    }
}
