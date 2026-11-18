//! REST client against a local mock HTTP server: auth header, history
//! parsing, sending, ACKs, 401 handling, bucket limits and 429 retry.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{Response, http};
use ferrocord::auth::secrets::Secret;
use ferrocord::error::Error;
use ferrocord::net::model::Snowflake;
use ferrocord::net::rest::RestClient;
use serde_json::json;

const TOKEN: &str = "rest-test-token";

fn msg(id: u64) -> serde_json::Value {
    json!({"id": id.to_string(), "channel_id": "10", "content": format!("m{id}"),
           "author": {"id": "2", "username": "alice"}, "attachments": [], "embeds": [], "mentions": [], "type": 0})
}

#[tokio::test]
async fn sends_auth_header_and_parses_history_oldest_first() {
    let server = http(Arc::new(|req, _| {
        if req.path.starts_with("/channels/10/messages") {
            // Discord returns newest first.
            Response::json(200, json!([msg(30), msg(20), msg(10)]))
        } else {
            Response::json(404, json!({"message": "Unknown"}))
        }
    }))
    .await;
    let rest = RestClient::new(&server.base(), "http://cdn", Some(Secret::new(TOKEN))).unwrap();
    let msgs = rest
        .messages(Snowflake(10), Some(Snowflake(99)), 50)
        .await
        .unwrap();
    assert_eq!(
        msgs.iter().map(|m| m.id.0).collect::<Vec<_>>(),
        vec![10, 20, 30]
    );
    let r = &server.recorded()[0];
    assert_eq!(r.path, "/channels/10/messages?limit=50&before=99");
    // User tokens are sent raw (no "Bot " prefix).
    assert_eq!(r.header("authorization"), Some(TOKEN));
    assert!(r.header("user-agent").unwrap().starts_with("Ferrocord/"));
}

#[tokio::test]
async fn send_message_typing_and_ack() {
    let server = http(Arc::new(|req, _| {
        match (req.method.as_str(), req.path.as_str()) {
            ("POST", "/channels/10/messages") => {
                let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
                let mut m = msg(77);
                m["content"] = body["content"].clone();
                m["nonce"] = body["nonce"].clone();
                Response::json(200, m)
            }
            ("POST", "/channels/10/typing") => Response::json(204, json!(null)),
            ("POST", p) if p.ends_with("/ack") => Response::json(200, json!({"token": null})),
            _ => Response::json(404, json!({})),
        }
    }))
    .await;
    let rest = RestClient::new(&server.base(), "http://cdn", Some(Secret::new(TOKEN))).unwrap();
    let m = rest
        .send_message(Snowflake(10), "hello **world**", "123")
        .await
        .unwrap();
    assert_eq!(m.content, "hello **world**");
    assert_eq!(m.nonce.as_deref(), Some("123"));
    rest.trigger_typing(Snowflake(10)).await.unwrap();
    rest.ack(Snowflake(10), Snowflake(77)).await.unwrap();
    let rec = server.recorded();
    let sent: serde_json::Value = serde_json::from_str(&rec[0].body).unwrap();
    assert_eq!(sent["nonce"], "123");
    assert_eq!(sent["tts"], false);
    assert_eq!(rec[2].path, "/channels/10/messages/77/ack");
}

#[tokio::test]
async fn unauthorized_maps_to_revoked_session() {
    let server = http(Arc::new(|_, _| {
        Response::json(401, json!({"message": "401: Unauthorized", "code": 0}))
    }))
    .await;
    let rest = RestClient::new(&server.base(), "http://cdn", Some(Secret::new(TOKEN))).unwrap();
    let e = rest.me().await.unwrap_err();
    assert!(matches!(e, Error::Auth(_)));
    assert!(e.is_session_revoked());
    let e = rest.trigger_typing(Snowflake(1)).await.unwrap_err();
    assert!(matches!(e, Error::Http { status: 401, .. }));
}

#[tokio::test]
async fn retries_after_429_with_retry_after() {
    let server = http(Arc::new(|_, n| {
        if n == 0 {
            Response::json(429, json!({"message": "You are being rate limited.", "retry_after": 0.3, "global": false}))
                .with_header("retry-after", "1")
        } else {
            Response::json(200, json!({"id": "1", "username": "me"}))
        }
    }))
    .await;
    let rest = RestClient::new(&server.base(), "http://cdn", Some(Secret::new(TOKEN))).unwrap();
    let start = Instant::now();
    let me = rest.me().await.unwrap();
    assert_eq!(me.username, "me");
    let rec = server.recorded();
    assert_eq!(rec.len(), 2);
    // The JSON body's retry_after (0.3 s) wins over the header (1 s).
    let gap = rec[1].at - rec[0].at;
    assert!(gap >= Duration::from_millis(290), "{gap:?}");
    assert!(start.elapsed() < Duration::from_millis(990));
}

#[tokio::test]
async fn gives_up_after_repeated_429() {
    let server = http(Arc::new(|_, _| {
        Response::json(429, json!({"retry_after": 0.05, "global": true}))
    }))
    .await;
    let rest = RestClient::new(&server.base(), "http://cdn", Some(Secret::new(TOKEN))).unwrap();
    let e = rest.trigger_typing(Snowflake(1)).await.unwrap_err();
    assert!(matches!(e, Error::Http { status: 429, .. }));
    assert_eq!(server.recorded().len(), 4);
}

#[tokio::test]
async fn exhausted_bucket_delays_next_request() {
    let server = http(Arc::new(|_, _| {
        Response::json(204, json!(null))
            .with_header("x-ratelimit-bucket", "typing")
            .with_header("x-ratelimit-limit", "1")
            .with_header("x-ratelimit-remaining", "0")
            .with_header("x-ratelimit-reset-after", "0.4")
    }))
    .await;
    let rest = RestClient::new(&server.base(), "http://cdn", Some(Secret::new(TOKEN))).unwrap();
    rest.trigger_typing(Snowflake(5)).await.unwrap();
    rest.trigger_typing(Snowflake(5)).await.unwrap();
    // A different channel is a different bucket instance: no wait.
    rest.trigger_typing(Snowflake(6)).await.unwrap();
    let rec = server.recorded();
    assert_eq!(rec.len(), 3);
    let gap = rec[1].at - rec[0].at;
    assert!(gap >= Duration::from_millis(380), "waited {gap:?}");
    let gap2 = rec[2].at - rec[1].at;
    assert!(gap2 < Duration::from_millis(300), "{gap2:?}");
}
