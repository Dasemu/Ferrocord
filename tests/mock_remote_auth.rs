//! QR login (remote auth v2) against a mock remote-auth gateway and a mock
#![allow(clippy::result_large_err)] // tungstenite handshake callback signature
//! REST endpoint for the ticket exchange.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use common::{Response, http};
use ferrocord::net::remote_auth::{self, RaEvent};
use ferrocord::net::rest::RestClient;
use futures::{SinkExt, StreamExt};
use rsa::pkcs8::DecodePublicKey;
use rsa::{Oaep, RsaPublicKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message as Ws;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response as WsResponse};

const STEP: Duration = Duration::from_secs(20);

fn encrypt(pk: &RsaPublicKey, data: &[u8]) -> String {
    STANDARD.encode(
        pk.encrypt(&mut rand::rngs::OsRng, Oaep::new::<Sha256>(), data)
            .unwrap(),
    )
}

#[tokio::test]
async fn full_qr_login_flow() {
    let public_key: Arc<Mutex<Option<RsaPublicKey>>> = Arc::default();

    // REST: ticket -> encrypted token.
    let pk = public_key.clone();
    let rest_server = http(Arc::new(move |req, _| {
        // The official client fetches its logged-out fingerprint first;
        // the response also sets Discord's device cookies.
        if req.path == "/experiments" {
            assert!(req.header("authorization").is_none());
            return Response::json(
                200,
                json!({"fingerprint": "1234567890.FiNgErPrInT", "assignments": []}),
            )
            .with_header("set-cookie", "__dcfduid=dcf123; Path=/; HttpOnly")
            .with_header("set-cookie", "__sdcfduid=sdcf456; Path=/; HttpOnly");
        }
        assert_eq!(req.path, "/users/@me/remote-auth/login");
        let body: Value = serde_json::from_str(&req.body).unwrap();
        assert_eq!(body["ticket"], "TICKET-123");
        let key = pk.lock().unwrap().clone().unwrap();
        Response::json(
            200,
            json!({"encrypted_token": encrypt(&key, b"the.resulting.token")}),
        )
    }))
    .await;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/?v=2", listener.local_addr().unwrap());
    let pk = public_key.clone();
    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let mut origin = String::new();
        let mut ws =
            tokio_tungstenite::accept_hdr_async(sock, |req: &Request, resp: WsResponse| {
                origin = req
                    .headers()
                    .get("origin")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_owned();
                Ok(resp)
            })
            .await
            .unwrap();
        assert_eq!(origin, "https://discord.com");

        ws.send(Ws::text(
            json!({"op": "hello", "heartbeat_interval": 41250, "timeout_ms": 120000}).to_string(),
        ))
        .await
        .unwrap();
        let init = next_non_heartbeat(&mut ws).await;
        assert_eq!(init["op"], "init");
        let der = STANDARD
            .decode(init["encoded_public_key"].as_str().unwrap())
            .unwrap();
        let key = RsaPublicKey::from_public_key_der(&der).unwrap();
        *pk.lock().unwrap() = Some(key.clone());

        let nonce = b"a-random-nonce-from-the-server-32";
        ws.send(Ws::text(
            json!({"op": "nonce_proof", "encrypted_nonce": encrypt(&key, nonce)}).to_string(),
        ))
        .await
        .unwrap();
        let proof = next_non_heartbeat(&mut ws).await;
        assert_eq!(proof["op"], "nonce_proof");
        assert_eq!(
            proof["proof"],
            URL_SAFE_NO_PAD.encode(Sha256::digest(nonce))
        );

        // The fingerprint is base64url(SHA-256(SPKI DER)) of the client key.
        let fp = URL_SAFE_NO_PAD.encode(Sha256::digest(&der));
        ws.send(Ws::text(
            json!({"op": "pending_remote_init", "fingerprint": fp}).to_string(),
        ))
        .await
        .unwrap();
        ws.send(Ws::text(
            json!({"op": "pending_ticket", "encrypted_user_payload": encrypt(&key, b"1:0:avatar:ferris_fan")}).to_string(),
        ))
        .await
        .unwrap();
        ws.send(Ws::text(
            json!({"op": "pending_login", "ticket": "TICKET-123"}).to_string(),
        ))
        .await
        .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(5), ws.next()).await;
    });

    let rest = RestClient::new(&rest_server.base(), "http://cdn", None).unwrap();
    let mut events = Box::pin(remote_auth::spawn(url, rest));
    let mut next = async || {
        tokio::time::timeout(STEP, events.next())
            .await
            .unwrap()
            .unwrap()
    };
    match next().await {
        RaEvent::QrReady(u) => assert!(
            u.starts_with("https://discord.com/ra/") && u.len() > 30,
            "{u}"
        ),
        other => panic!("{other:?}"),
    }
    match next().await {
        RaEvent::Scanned { username } => assert_eq!(username, "ferris_fan"),
        other => panic!("{other:?}"),
    }
    match next().await {
        RaEvent::Token(t) => assert_eq!(t.expose(), "the.resulting.token"),
        other => panic!("{other:?}"),
    }
    let recorded = rest_server.recorded();
    assert_eq!(recorded[0].path, "/experiments");
    let login = recorded
        .iter()
        .find(|r| r.path == "/users/@me/remote-auth/login")
        .unwrap();
    // The ticket exchange is unauthenticated and complete: fingerprint,
    // cookies, super properties, locale.
    assert!(login.header("authorization").is_none());
    assert_eq!(
        login.header("x-fingerprint"),
        Some("1234567890.FiNgErPrInT")
    );
    let cookie = login.header("cookie").unwrap_or("");
    assert!(cookie.contains("__dcfduid=dcf123"), "{cookie}");
    assert!(cookie.contains("__sdcfduid=sdcf456"), "{cookie}");
    assert!(login.header("x-super-properties").is_some());
    assert!(login.header("x-discord-locale").is_some());
    assert!(login.header("x-captcha-key").is_none());
    server.await.unwrap();
}

#[tokio::test]
async fn captcha_on_the_ticket_exchange_is_reported_with_the_challenge() {
    let rest_server = http(Arc::new(move |req, _| {
        if req.path == "/experiments" {
            return Response::json(500, json!({"message": "down"}));
        }
        Response::json(
            400,
            json!({
                "captcha_key": ["captcha-required"],
                "captcha_sitekey": "a9b5fb07-92ff-493f-86fe-352a2803b3df",
                "captcha_service": "hcaptcha",
                "captcha_rqdata": "rq-data",
                "captcha_rqtoken": "rq-token"
            }),
        )
    }))
    .await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/?v=2", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
        ws.send(Ws::text(r#"{"op":"hello","heartbeat_interval":41250}"#))
            .await
            .unwrap();
        next_non_heartbeat(&mut ws).await;
        ws.send(Ws::text(r#"{"op":"pending_login","ticket":"T"}"#))
            .await
            .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(5), ws.next()).await;
    });
    let rest = RestClient::new(&rest_server.base(), "http://cdn", None).unwrap();
    let mut events = Box::pin(remote_auth::spawn(url, rest));
    let ev = tokio::time::timeout(STEP, events.next())
        .await
        .unwrap()
        .unwrap();
    let RaEvent::CaptchaRequired(c) = ev else {
        panic!("{ev:?}")
    };
    assert!(c.is_solvable());
    assert_eq!(c.rqtoken.as_deref(), Some("rq-token"));
    // A failed fingerprint fetch is not fatal; the header is just absent.
    let login = rest_server
        .recorded()
        .into_iter()
        .find(|r| r.path == "/users/@me/remote-auth/login")
        .unwrap();
    assert!(login.header("x-fingerprint").is_none());
    // The stream ends after the terminal event.
    assert!(
        tokio::time::timeout(STEP, events.next())
            .await
            .unwrap()
            .is_none()
    );
    server.await.unwrap();
}

async fn next_non_heartbeat(
    ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
) -> Value {
    loop {
        let m = tokio::time::timeout(STEP, ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Ws::Text(t) = m {
            let v: Value = serde_json::from_str(t.as_str()).unwrap();
            if v["op"] != "heartbeat" {
                return v;
            }
        }
    }
}

#[tokio::test]
async fn cancel_from_phone_is_reported() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/?v=2", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
        ws.send(Ws::text(r#"{"op":"hello","heartbeat_interval":41250}"#))
            .await
            .unwrap();
        next_non_heartbeat(&mut ws).await;
        ws.send(Ws::text(r#"{"op":"cancel"}"#)).await.unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(5), ws.next()).await;
    });
    let rest = RestClient::new("http://127.0.0.1:9", "http://cdn", None).unwrap();
    let mut events = Box::pin(remote_auth::spawn(url, rest));
    let ev = tokio::time::timeout(STEP, events.next())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(ev, RaEvent::Cancelled), "{ev:?}");
    server.await.unwrap();
}
