//! REST calls for DM and group calls (ringing). Like every other call they
//! go through `RestClient::request` (per-route rate limits, 429 retry).

use serde_json::json;

use super::{Method, RestClient};
use crate::error::Result;
use crate::net::model::*;

/// Body of `POST /channels/{id}/call/ring`: `None` rings every recipient
/// (what the official client sends when starting a call).
pub fn ring_body(recipients: Option<&[UserId]>) -> serde_json::Value {
    match recipients {
        None => json!({ "recipients": null }),
        Some(r) => json!({ "recipients": r.iter().map(|u| u.to_string()).collect::<Vec<_>>() }),
    }
}

impl RestClient {
    /// `POST /channels/{id}/call/ring`: rings the recipients of a DM/group.
    pub async fn ring(&self, channel: ChannelId, recipients: Option<Vec<UserId>>) -> Result<()> {
        self.request(
            Method::POST,
            &format!("/channels/{channel}/call/ring"),
            Some(ring_body(recipients.as_deref())),
        )
        .await
        .map(|_| ())
    }

    /// `POST /channels/{id}/call/stop-ringing`: declining an incoming call
    /// stops ringing for ourselves (`recipients: [me]`).
    pub async fn stop_ringing(&self, channel: ChannelId, recipients: Vec<UserId>) -> Result<()> {
        self.request(
            Method::POST,
            &format!("/channels/{channel}/call/stop-ringing"),
            Some(ring_body(Some(&recipients))),
        )
        .await
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_bodies() {
        assert_eq!(ring_body(None).to_string(), r#"{"recipients":null}"#);
        assert_eq!(
            ring_body(Some(&[Snowflake(5), Snowflake(6)])).to_string(),
            r#"{"recipients":["5","6"]}"#
        );
    }
}
