//! QR-code login ("remote auth") over `wss://remote-auth-gateway.discord.gg/?v=2`.
//!
//! Protocol (client = this desktop app, phone = the official mobile app):
//!
//! 1. server → `hello { heartbeat_interval, timeout_ms }`
//! 2. client → `init { encoded_public_key }` (RSA-2048 SPKI, base64)
//! 3. server → `nonce_proof { encrypted_nonce }` (RSA-OAEP/SHA-256)
//! 4. client → `nonce_proof { proof }` = base64url(sha256(nonce)), no padding
//! 5. server → `pending_remote_init { fingerprint }`; the QR code encodes
//!    `https://discord.com/ra/<fingerprint>`
//! 6. server → `pending_ticket { encrypted_user_payload }` once the phone
//!    scanned it; payload is `id:discriminator:avatar:username`
//! 7. server → `pending_login { ticket }` once the phone confirmed; the
//!    client exchanges the ticket via `POST /users/@me/remote-auth/login`
//!    for `encrypted_token`, decrypted with the same private key.
//!    (Protocol v1 sent `finish { encrypted_token }` directly; also handled.)
//! 8. server → `cancel` if the user declined on the phone.
//!
//! The client sends `{ "op": "heartbeat" }` every `heartbeat_interval` ms.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use futures::{SinkExt, Stream, StreamExt};
use rsa::pkcs8::EncodePublicKey;
use rsa::{Oaep, RsaPrivateKey, RsaPublicKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use super::rest::RestClient;
use crate::auth::secrets::Secret;
use crate::error::{Error, Result};

pub const QR_URL_PREFIX: &str = "https://discord.com/ra/";

/// Events for the login screen.
#[derive(Debug, Clone)]
pub enum RaEvent {
    /// Show this URL as a QR code.
    QrReady(String),
    /// The phone scanned the code; waiting for confirmation.
    Scanned {
        username: String,
    },
    /// Login confirmed; store this token.
    Token(Secret),
    Cancelled,
    /// Discord wants a captcha for the ticket exchange. The ticket is bound
    /// to this socket, so the app moves on to Discord's own login page,
    /// where Discord shows the challenge itself.
    CaptchaRequired(Box<crate::net::captcha::CaptchaChallenge>),
    /// Session expired (the server closes after `timeout_ms`).
    Expired,
    Error(String),
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum ServerMsg {
    Hello { heartbeat_interval: u64 },
    NonceProof { encrypted_nonce: String },
    PendingRemoteInit { fingerprint: String },
    PendingTicket { encrypted_user_payload: String },
    PendingLogin { ticket: String },
    Finish { encrypted_token: String },
    HeartbeatAck,
    Cancel,
}

/// What the runner should do next.
#[derive(Debug)]
pub enum RaAction {
    Send(String),
    StartHeartbeat(Duration),
    Emit(RaEvent),
    /// Exchange this ticket over REST.
    ExchangeTicket(String),
    Close,
}

/// Pure protocol state (owns the RSA key pair).
pub struct RemoteAuth {
    key: RsaPrivateKey,
    fingerprint: Option<String>,
    awaiting_ack: bool,
}

impl RemoteAuth {
    pub fn new() -> Result<Self> {
        let mut rng = rand::rngs::OsRng;
        let key = RsaPrivateKey::new(&mut rng, 2048)
            .map_err(|e| Error::Auth(format!("could not create key pair: {e}")))?;
        Ok(Self {
            key,
            fingerprint: None,
            awaiting_ack: false,
        })
    }

    pub fn from_key(key: RsaPrivateKey) -> Self {
        Self {
            key,
            fingerprint: None,
            awaiting_ack: false,
        }
    }

    pub fn public_key(&self) -> RsaPublicKey {
        RsaPublicKey::from(&self.key)
    }

    pub fn encoded_public_key(&self) -> Result<String> {
        let der = self
            .public_key()
            .to_public_key_der()
            .map_err(|e| Error::Auth(format!("could not encode public key: {e}")))?;
        Ok(STANDARD.encode(der.as_bytes()))
    }

    /// The fingerprint the server must announce for our key:
    /// base64url(SHA-256(SPKI DER)), unpadded.
    pub fn expected_fingerprint(&self) -> Result<String> {
        let der = self
            .public_key()
            .to_public_key_der()
            .map_err(|e| Error::Auth(format!("could not encode public key: {e}")))?;
        Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(der.as_bytes())))
    }

    /// Next client heartbeat, or `None` if the previous one was never
    /// acknowledged (the connection is dead).
    pub fn heartbeat(&mut self) -> Option<String> {
        if self.awaiting_ack {
            return None;
        }
        self.awaiting_ack = true;
        Some(r#"{"op":"heartbeat"}"#.into())
    }

    pub fn decrypt(&self, b64: &str) -> Result<Vec<u8>> {
        let ct = STANDARD
            .decode(b64.trim())
            .map_err(|_| Error::Protocol("remote auth: bad base64".into()))?;
        self.key
            .decrypt(Oaep::new::<Sha256>(), &ct)
            .map_err(|_| Error::Protocol("remote auth: decryption failed".into()))
    }

    pub fn on_message(&mut self, text: &str) -> Result<Vec<RaAction>> {
        let msg: ServerMsg = match serde_json::from_str(text) {
            Ok(m) => m,
            // Unknown ops are ignored.
            Err(_) => return Ok(vec![]),
        };
        Ok(match msg {
            ServerMsg::Hello {
                heartbeat_interval, ..
            } => vec![
                RaAction::StartHeartbeat(Duration::from_millis(heartbeat_interval.max(1000))),
                RaAction::Send(
                    serde_json::json!({ "op": "init", "encoded_public_key": self.encoded_public_key()? })
                        .to_string(),
                ),
            ],
            ServerMsg::NonceProof { encrypted_nonce } => {
                let nonce = self.decrypt(&encrypted_nonce)?;
                let proof = URL_SAFE_NO_PAD.encode(Sha256::digest(&nonce));
                vec![RaAction::Send(
                    serde_json::json!({ "op": "nonce_proof", "proof": proof }).to_string(),
                )]
            }
            ServerMsg::PendingRemoteInit { fingerprint } => {
                // The fingerprint binds the QR code to our key; a mismatch
                // means the server (or something in between) is not
                // talking about our session.
                if fingerprint != self.expected_fingerprint()? {
                    return Err(Error::Protocol(
                        "remote auth: fingerprint does not match our key".into(),
                    ));
                }
                let url = format!("{QR_URL_PREFIX}{fingerprint}");
                self.fingerprint = Some(fingerprint);
                vec![RaAction::Emit(RaEvent::QrReady(url))]
            }
            ServerMsg::PendingTicket {
                encrypted_user_payload,
            } => {
                // Only cosmetic: a bad payload must not abort the login.
                let username = match self.decrypt(&encrypted_user_payload) {
                    Ok(raw) => {
                        let s = String::from_utf8_lossy(&raw);
                        // id:discriminator:avatar:username (username may contain ':')
                        s.splitn(4, ':').nth(3).unwrap_or("").to_owned()
                    }
                    Err(_) => {
                        tracing::warn!("remote auth: undecodable user payload");
                        String::new()
                    }
                };
                vec![RaAction::Emit(RaEvent::Scanned { username })]
            }
            ServerMsg::PendingLogin { ticket } => vec![RaAction::ExchangeTicket(ticket)],
            ServerMsg::Finish { encrypted_token } => {
                let token = self.decrypt_token(&encrypted_token)?;
                vec![RaAction::Emit(RaEvent::Token(token)), RaAction::Close]
            }
            ServerMsg::Cancel => vec![RaAction::Emit(RaEvent::Cancelled), RaAction::Close],
            ServerMsg::HeartbeatAck => {
                self.awaiting_ack = false;
                vec![]
            }
        })
    }

    pub fn decrypt_token(&self, encrypted: &str) -> Result<Secret> {
        let raw = self.decrypt(encrypted)?;
        String::from_utf8(raw)
            .map(Secret::new)
            .map_err(|_| Error::Protocol("remote auth: token is not UTF-8".into()))
    }
}

/// Runs one QR-login attempt. The returned stream ends after a terminal
/// event (token, cancel, expiry, error); dropping it aborts the attempt.
pub fn spawn(url: String, rest: RestClient) -> impl Stream<Item = RaEvent> + Send + 'static {
    let (tx, rx) = mpsc::channel(16);
    tokio::spawn(async move {
        if let Err(e) = run(&url, rest, &tx).await {
            let _ = tx.send(RaEvent::Error(e.user_message())).await;
        }
    });
    futures::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|e| (e, rx)) })
}

async fn run(url: &str, rest: RestClient, tx: &mpsc::Sender<RaEvent>) -> Result<()> {
    // RSA key generation is CPU-bound: keep it off the async workers. Like
    // the official client, fetch the logged-out fingerprint (and Discord's
    // cookies) meanwhile; the ticket exchange sends it as `X-Fingerprint`.
    let (state, fingerprint) = tokio::join!(
        tokio::task::spawn_blocking(RemoteAuth::new),
        rest.fingerprint()
    );
    let mut state = state.map_err(|_| Error::Cancelled)??;
    let mut req = url.into_client_request()?;
    req.headers_mut().insert(
        "Origin",
        reqwest::header::HeaderValue::from_static("https://discord.com"),
    );
    let (mut ws, _) = tokio::time::timeout(
        Duration::from_secs(20),
        tokio_tungstenite::connect_async(req),
    )
    .await
    .map_err(|_| Error::Net("remote auth connect timed out".into()))??;

    let far = Duration::from_secs(3600);
    let mut interval = far;
    let mut next = tokio::time::Instant::now() + far;
    loop {
        tokio::select! {
            _ = tx.closed() => {
                let _ = ws.close(None).await;
                return Ok(());
            }
            _ = tokio::time::sleep_until(next) => {
                next = tokio::time::Instant::now() + interval;
                match state.heartbeat() {
                    Some(hb) => ws.send(WsMessage::text(hb)).await?,
                    None => {
                        tracing::warn!("remote auth heartbeat not acknowledged");
                        let _ = ws.close(None).await;
                        let _ = tx.send(RaEvent::Expired).await;
                        return Ok(());
                    }
                }
            }
            frame = ws.next() => {
                let text = match frame {
                    None | Some(Ok(WsMessage::Close(_))) => {
                        let _ = tx.send(RaEvent::Expired).await;
                        return Ok(());
                    }
                    Some(Err(e)) => return Err(e.into()),
                    Some(Ok(WsMessage::Text(t))) => t.as_str().to_owned(),
                    Some(Ok(_)) => continue,
                };
                for action in state.on_message(&text)? {
                    match action {
                        RaAction::Send(s) => ws.send(WsMessage::text(s)).await?,
                        RaAction::StartHeartbeat(d) => {
                            interval = d;
                            next = tokio::time::Instant::now() + d;
                        }
                        RaAction::Emit(ev) => {
                            let _ = tx.send(ev).await;
                        }
                        RaAction::ExchangeTicket(ticket) => {
                            let enc = match rest
                                .remote_auth_login_with(&ticket, fingerprint.as_deref(), None)
                                .await
                            {
                                Ok(enc) => enc,
                                Err(Error::CaptchaRequired(c)) => {
                                    tracing::info!(challenge = ?c, "remote auth: captcha required");
                                    let _ = tx.send(RaEvent::CaptchaRequired(c)).await;
                                    let _ = ws.close(None).await;
                                    return Ok(());
                                }
                                Err(e) => return Err(e),
                            };
                            let token = state.decrypt_token(&enc)?;
                            let _ = tx.send(RaEvent::Token(token)).await;
                            let _ = ws.close(None).await;
                            return Ok(());
                        }
                        RaAction::Close => {
                            let _ = ws.close(None).await;
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
}

/// Renders `data` as a QR code matrix (true = dark module), with the
/// standard 4-module quiet zone.
pub fn qr_matrix(data: &str) -> Result<(usize, Vec<bool>)> {
    let code = qrcode::QrCode::with_error_correction_level(data, qrcode::EcLevel::M)
        .map_err(|e| Error::Protocol(format!("qr: {e}")))?;
    let w = code.width();
    let quiet = 4;
    let size = w + 2 * quiet;
    let mut out = vec![false; size * size];
    for (i, c) in code.to_colors().into_iter().enumerate() {
        let (x, y) = (i % w, i / w);
        out[(y + quiet) * size + x + quiet] = c == qrcode::Color::Dark;
    }
    Ok((size, out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pkcs8::DecodePublicKey;

    fn test_state() -> RemoteAuth {
        // Small key keeps the test fast; OAEP-SHA256 with 1024 bits can
        // still carry 62 bytes, enough for the nonce/user payload.
        let key = RsaPrivateKey::new(&mut rand::rngs::OsRng, 1024).unwrap();
        RemoteAuth::from_key(key)
    }

    fn encrypt(pk: &RsaPublicKey, data: &[u8]) -> String {
        STANDARD.encode(
            pk.encrypt(&mut rand::rngs::OsRng, Oaep::new::<Sha256>(), data)
                .unwrap(),
        )
    }

    #[test]
    fn hello_sends_init_with_spki_key() {
        let mut s = test_state();
        let a = s
            .on_message(r#"{"op":"hello","heartbeat_interval":41250,"timeout_ms":120000}"#)
            .unwrap();
        assert!(matches!(a[0], RaAction::StartHeartbeat(d) if d == Duration::from_millis(41250)));
        let RaAction::Send(init) = &a[1] else {
            panic!()
        };
        let v: serde_json::Value = serde_json::from_str(init).unwrap();
        assert_eq!(v["op"], "init");
        let der = STANDARD
            .decode(v["encoded_public_key"].as_str().unwrap())
            .unwrap();
        let pk = RsaPublicKey::from_public_key_der(&der).unwrap();
        assert_eq!(pk, s.public_key());
    }

    #[test]
    fn nonce_proof_is_sha256_base64url() {
        let mut s = test_state();
        let nonce = b"0123456789abcdef0123456789abcdef";
        let enc = encrypt(&s.public_key(), nonce);
        let a = s
            .on_message(&format!(
                r#"{{"op":"nonce_proof","encrypted_nonce":"{enc}"}}"#
            ))
            .unwrap();
        let RaAction::Send(txt) = &a[0] else { panic!() };
        let v: serde_json::Value = serde_json::from_str(txt).unwrap();
        assert_eq!(
            v["proof"].as_str().unwrap(),
            URL_SAFE_NO_PAD.encode(Sha256::digest(nonce))
        );
    }

    #[test]
    fn fingerprint_ticket_and_cancel() {
        let mut s = test_state();
        assert!(
            s.on_message(r#"{"op":"pending_remote_init","fingerprint":"abc123"}"#)
                .is_err(),
            "foreign fingerprint must be rejected"
        );
        let fp = s.expected_fingerprint().unwrap();
        let a = s
            .on_message(&format!(
                r#"{{"op":"pending_remote_init","fingerprint":"{fp}"}}"#
            ))
            .unwrap();
        assert!(
            matches!(&a[0], RaAction::Emit(RaEvent::QrReady(u)) if *u == format!("https://discord.com/ra/{fp}"))
        );
        let enc = encrypt(&s.public_key(), b"42:0:avhash:alice:x");
        let a = s
            .on_message(&format!(
                r#"{{"op":"pending_ticket","encrypted_user_payload":"{enc}"}}"#
            ))
            .unwrap();
        assert!(
            matches!(&a[0], RaAction::Emit(RaEvent::Scanned { username }) if username == "alice:x")
        );
        let a = s
            .on_message(r#"{"op":"pending_login","ticket":"T"}"#)
            .unwrap();
        assert!(matches!(&a[0], RaAction::ExchangeTicket(t) if t == "T"));
        let a = s.on_message(r#"{"op":"cancel"}"#).unwrap();
        assert!(matches!(a[0], RaAction::Emit(RaEvent::Cancelled)));
    }

    #[test]
    fn heartbeat_zombie_and_recoverable_payload() {
        let mut s = test_state();
        assert!(s.heartbeat().is_some());
        assert!(s.heartbeat().is_none(), "no ACK -> dead");
        s.awaiting_ack = false;
        assert!(s.heartbeat().is_some());
        s.on_message(r#"{"op":"heartbeat_ack"}"#).unwrap();
        assert!(s.heartbeat().is_some());
        // Garbage user payload: still reports the scan.
        let a = s
            .on_message(r#"{"op":"pending_ticket","encrypted_user_payload":"AAAA"}"#)
            .unwrap();
        assert!(
            matches!(&a[0], RaAction::Emit(RaEvent::Scanned { username }) if username.is_empty())
        );
    }

    #[test]
    fn bad_ciphertext_is_error() {
        let mut s = test_state();
        assert!(
            s.on_message(r#"{"op":"nonce_proof","encrypted_nonce":"AAAA"}"#)
                .is_err()
        );
    }

    #[test]
    fn qr_has_quiet_zone() {
        let (size, m) = qr_matrix("https://discord.com/ra/abc").unwrap();
        assert_eq!(m.len(), size * size);
        assert!(!m[0]);
        assert!(m.iter().any(|d| *d));
    }
}
