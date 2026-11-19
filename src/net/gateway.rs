//! User-account Gateway client (API v10, JSON encoding, optional
//! `zlib-stream` transport compression).
//!
//! The protocol logic lives in [`Session`], a pure state machine that turns
//! incoming frames into [`Action`]s; it is unit-tested without sockets. The
//! async runner [`spawn`] owns the WebSocket, timers and reconnect loop and
//! executes those actions.

use std::time::Duration;

use futures::{SinkExt, Stream, StreamExt};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

use super::types::{self, ClientProperties, GatewayPayload, Hello, OpCode};
use super::zlib::ZlibStream;
use super::{ConnectionState, Event};
use crate::auth::secrets::Secret;
use crate::error::Result;

pub const API_VERSION: u8 = 10;
/// A session must stay up this long before the reconnect attempt counter
/// is reset; flapping connections keep backing off.
pub const STABLE_AFTER: Duration = Duration::from_secs(60);
/// Minimum delay before any reconnect.
pub const MIN_RECONNECT: Duration = Duration::from_millis(1000);

/// Everything needed to (re)connect.
#[derive(Clone, Debug)]
pub struct GatewayConfig {
    /// Base URL, e.g. `wss://gateway.discord.gg`.
    pub url: String,
    pub token: Secret,
    pub zlib: bool,
    pub properties: ClientProperties,
}

/// What the runner must do after a frame or timer.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Send(String),
    Emit(EventKind),
    /// Start heartbeating with this interval.
    StartHeartbeat(Duration),
    /// Drop the socket and reconnect; `resume` keeps the session.
    Reconnect {
        resume: bool,
        delay: Duration,
    },
    /// Stop for good.
    Fatal(FatalReason),
}

/// Lightweight tag for emitted events, so `Action` stays `PartialEq` for
/// tests; the full event is carried separately.
#[derive(Debug, Clone, PartialEq)]
pub enum EventKind {
    Dispatch(String),
    Connected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FatalReason {
    AuthenticationFailed,
    Other(String),
}

/// What to do when the socket closes with `code`.
pub fn close_code_action(code: Option<u16>, resumable: bool) -> Action {
    match code {
        Some(4004) => Action::Fatal(FatalReason::AuthenticationFailed),
        Some(4010) => Action::Fatal(FatalReason::Other("invalid shard".into())),
        Some(4011) => Action::Fatal(FatalReason::Other("sharding required".into())),
        Some(4012) => Action::Fatal(FatalReason::Other("invalid API version".into())),
        Some(4013) | Some(4014) => Action::Fatal(FatalReason::Other("invalid intents".into())),
        // Invalid seq / session timed out / not authenticated / already
        // authenticated: the session is unusable, identify again. The
        // caller's backoff keeps this from becoming a tight loop.
        Some(4003) | Some(4005) | Some(4007) | Some(4009) => Action::Reconnect {
            resume: false,
            delay: Duration::ZERO,
        },
        // Rate limited (sent too fast): back off noticeably.
        Some(4008) => Action::Reconnect {
            resume: resumable,
            delay: Duration::from_secs(5),
        },
        _ => Action::Reconnect {
            resume: resumable,
            delay: Duration::ZERO,
        },
    }
}

/// Exponential backoff with jitter: 1 s, 2 s, 4 s ... capped at 60 s, each
/// multiplied by a random factor in [0.5, 1.0).
pub fn backoff(attempt: u32, jitter: f64) -> Duration {
    let base = 1000u64.saturating_mul(1u64 << attempt.saturating_sub(1).min(16));
    let capped = base.min(60_000) as f64;
    Duration::from_millis((capped * (0.5 + 0.5 * jitter.clamp(0.0, 1.0))) as u64)
}

/// Pure gateway protocol state for one logical session (survives resumes).
#[derive(Debug, Default)]
pub struct Session {
    pub seq: Option<u64>,
    pub session_id: Option<String>,
    pub resume_url: Option<String>,
    pub heartbeat_interval: Option<Duration>,
    /// A heartbeat was sent and no ACK arrived yet.
    pub awaiting_ack: bool,
    pub last_heartbeat_at: Option<Instant>,
    pub latency: Option<Duration>,
    /// Set when the current connection is trying to resume.
    pub resuming: bool,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn can_resume(&self) -> bool {
        self.session_id.is_some() && self.seq.is_some()
    }

    /// Forget the session (next connect identifies from scratch).
    pub fn reset(&mut self) {
        self.seq = None;
        self.session_id = None;
        self.resume_url = None;
        self.resuming = false;
    }

    /// URL to connect to: the resume URL when resuming, else the base URL.
    pub fn connect_url(&self, base: &str, zlib: bool) -> String {
        let host = if self.can_resume() {
            self.resume_url.as_deref().unwrap_or(base)
        } else {
            base
        };
        let host = host.trim_end_matches('/');
        let mut url = format!("{host}/?v={API_VERSION}&encoding=json");
        if zlib {
            url.push_str("&compress=zlib-stream");
        }
        url
    }

    /// Called when a new socket is opened.
    pub fn on_connect(&mut self) {
        self.awaiting_ack = false;
        self.heartbeat_interval = None;
        self.resuming = self.can_resume();
    }

    /// Builds the next heartbeat. If the previous one was never
    /// acknowledged the connection is a zombie and must be resumed.
    pub fn heartbeat(&mut self, now: Instant) -> Action {
        if self.awaiting_ack {
            return Action::Reconnect {
                resume: true,
                delay: Duration::ZERO,
            };
        }
        self.awaiting_ack = true;
        self.last_heartbeat_at = Some(now);
        Action::Send(to_json(&types::heartbeat_payload(self.seq)))
    }

    /// Handles one decoded frame.
    pub fn on_payload(
        &mut self,
        payload: &GatewayPayload,
        token: &str,
        props: &ClientProperties,
        now: Instant,
    ) -> Vec<Action> {
        if let Some(s) = payload.s {
            self.seq = Some(self.seq.map_or(s, |cur| cur.max(s)));
        }
        let Some(op) = OpCode::from_u8(payload.op) else {
            return vec![];
        };
        match op {
            OpCode::Hello => {
                let interval = serde_json::from_value::<Hello>(payload.d.clone())
                    .map(|h| h.heartbeat_interval)
                    .unwrap_or(41_250);
                let interval = Duration::from_millis(interval.max(1_000));
                self.heartbeat_interval = Some(interval);
                let first = match (&self.session_id, self.seq, self.resuming) {
                    (Some(sid), Some(seq), true) => types::resume_payload(token, sid, seq),
                    _ => {
                        self.resuming = false;
                        types::identify_payload(token, props)
                    }
                };
                vec![
                    Action::StartHeartbeat(interval),
                    Action::Send(to_json(&first)),
                ]
            }
            OpCode::HeartbeatAck => {
                self.awaiting_ack = false;
                if let Some(at) = self.last_heartbeat_at {
                    self.latency = Some(now.saturating_duration_since(at));
                }
                vec![]
            }
            OpCode::Heartbeat => {
                // Server asks for an immediate heartbeat; this does not
                // count as a missed ACK.
                self.awaiting_ack = false;
                vec![self.heartbeat(now)]
            }
            OpCode::Reconnect => vec![Action::Reconnect {
                resume: true,
                delay: Duration::ZERO,
            }],
            OpCode::InvalidSession => {
                let resumable = payload.d.as_bool().unwrap_or(false);
                if !resumable {
                    self.reset();
                }
                // Discord asks clients to wait 1-5 s before re-identifying.
                vec![Action::Reconnect {
                    resume: resumable,
                    delay: Duration::from_millis(1_000 + fastrand::u64(0..4_000)),
                }]
            }
            OpCode::Dispatch => {
                let t = payload.t.clone().unwrap_or_default();
                let mut out = Vec::new();
                if t == "READY" {
                    self.session_id = payload.d["session_id"].as_str().map(String::from);
                    // Never follow a resume URL that is not wss (or loopback ws).
                    self.resume_url = payload.d["resume_gateway_url"]
                        .as_str()
                        .filter(|u| validate_url(u).is_ok())
                        .map(String::from);
                    self.resuming = false;
                    out.push(Action::Emit(EventKind::Connected));
                } else if t == "RESUMED" {
                    self.resuming = false;
                    out.push(Action::Emit(EventKind::Connected));
                }
                out.push(Action::Emit(EventKind::Dispatch(t)));
                out
            }
            // Client-only opcodes.
            _ => vec![],
        }
    }
}

fn to_json(p: &GatewayPayload) -> String {
    // Serialising a `GatewayPayload` cannot fail (string keys, JSON values).
    serde_json::to_string(p).unwrap_or_else(|_| String::from("{}"))
}

enum Outcome {
    /// Receiver dropped (logout / app exit).
    Closed,
    Reconnect {
        resume: bool,
        delay: Duration,
        /// The connection was up for at least `STABLE_AFTER`.
        was_ready: bool,
    },
    Fatal(FatalReason),
}

/// Starts the gateway on the current tokio runtime. The connection lives
/// as long as the returned stream: dropping it closes the socket.
pub fn spawn(cfg: GatewayConfig) -> impl Stream<Item = Event> + Send + 'static {
    let (tx, rx) = mpsc::channel::<Event>(256);
    tokio::spawn(run(cfg, tx));
    futures::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|e| (e, rx)) })
}

async fn run(cfg: GatewayConfig, tx: mpsc::Sender<Event>) {
    let mut session = Session::new();
    let mut attempt: u32 = 0;
    loop {
        let state = if attempt == 0 {
            ConnectionState::Connecting
        } else {
            ConnectionState::Reconnecting {
                attempt,
                delay_ms: 0,
            }
        };
        if tx.send(Event::Connection(state)).await.is_err() {
            return;
        }
        let outcome = connect_once(&cfg, &mut session, &tx).await;
        let extra_delay = match outcome {
            Outcome::Closed => return,
            Outcome::Fatal(reason) => {
                let ev = match reason {
                    FatalReason::AuthenticationFailed => Event::AuthFailed,
                    FatalReason::Other(r) => Event::Fatal(r),
                };
                let _ = tx.send(ev).await;
                let _ = tx
                    .send(Event::Connection(ConnectionState::Disconnected))
                    .await;
                return;
            }
            Outcome::Reconnect {
                resume,
                delay,
                was_ready,
            } => {
                if !resume {
                    session.reset();
                }
                if was_ready {
                    attempt = 0;
                }
                delay
            }
        };
        attempt = attempt.saturating_add(1);
        let delay = if attempt == 1 && extra_delay.is_zero() {
            // First retry after a stable session: reconnect quickly, but
            // never instantly.
            MIN_RECONNECT + Duration::from_millis(fastrand::u64(0..1_000))
        } else {
            backoff(attempt, fastrand::f64())
                .max(extra_delay)
                .max(MIN_RECONNECT)
        };
        tracing::info!(
            attempt,
            delay_ms = delay.as_millis() as u64,
            "gateway reconnecting"
        );
        if tx
            .send(Event::Connection(ConnectionState::Reconnecting {
                attempt,
                delay_ms: delay.as_millis() as u64,
            }))
            .await
            .is_err()
        {
            return;
        }
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = tx.closed() => return,
        }
    }
}

async fn connect_once(
    cfg: &GatewayConfig,
    session: &mut Session,
    tx: &mpsc::Sender<Event>,
) -> Outcome {
    let url = session.connect_url(&cfg.url, cfg.zlib);
    tracing::info!(resume = session.can_resume(), "gateway connecting");
    let connect = tokio::time::timeout(
        Duration::from_secs(20),
        tokio_tungstenite::connect_async(url.as_str()),
    )
    .await;
    let mut ws = match connect {
        Ok(Ok((ws, _resp))) => ws,
        Ok(Err(e)) => {
            tracing::warn!(error = %crate::error::Error::from(e), "gateway connect failed");
            return Outcome::Reconnect {
                resume: session.can_resume(),
                delay: Duration::ZERO,
                was_ready: false,
            };
        }
        Err(_) => {
            tracing::warn!("gateway connect timed out");
            return Outcome::Reconnect {
                resume: session.can_resume(),
                delay: Duration::ZERO,
                was_ready: false,
            };
        }
    };
    session.on_connect();
    let mut inflater = cfg.zlib.then(ZlibStream::new);
    let mut was_ready = false;
    let mut ready_at: Option<Instant> = None;
    // No heartbeat until HELLO arrives.
    let far = Instant::now() + Duration::from_secs(3600);
    let mut next_heartbeat = far;
    let mut interval = Duration::from_secs(41);
    let hello_deadline = tokio::time::sleep(Duration::from_secs(30));
    tokio::pin!(hello_deadline);

    let outcome = loop {
        tokio::select! {
            _ = tx.closed() => {
                let _ = ws.close(Some(CloseFrame { code: CloseCode::Normal, reason: "".into() })).await;
                break Outcome::Closed;
            }
            _ = &mut hello_deadline, if session.heartbeat_interval.is_none() => {
                tracing::warn!("no HELLO from gateway");
                break Outcome::Reconnect { resume: session.can_resume(), delay: Duration::ZERO, was_ready };
            }
            _ = tokio::time::sleep_until(next_heartbeat) => {
                let action = session.heartbeat(Instant::now());
                next_heartbeat = Instant::now() + interval;
                match action {
                    Action::Send(text) => {
                        if ws.send(WsMessage::text(text)).await.is_err() {
                            break Outcome::Reconnect { resume: session.can_resume(), delay: Duration::ZERO, was_ready };
                        }
                    }
                    Action::Reconnect { resume, delay } => {
                        tracing::warn!("heartbeat not acknowledged; zombie connection");
                        break Outcome::Reconnect { resume: resume && session.can_resume(), delay, was_ready };
                    }
                    _ => {}
                }
            }
            frame = ws.next() => {
                let text = match frame {
                    None => break Outcome::Reconnect { resume: session.can_resume(), delay: Duration::ZERO, was_ready },
                    Some(Err(e)) => {
                        tracing::warn!(error = %crate::error::Error::from(e), "gateway read error");
                        break Outcome::Reconnect { resume: session.can_resume(), delay: Duration::ZERO, was_ready };
                    }
                    Some(Ok(WsMessage::Text(t))) => t.as_str().to_owned(),
                    Some(Ok(WsMessage::Binary(b))) => {
                        let decoded = match inflater.as_mut() {
                            Some(z) => z.push(&b),
                            None => Ok(String::from_utf8(b.to_vec()).ok()),
                        };
                        match decoded {
                            Ok(Some(t)) => t,
                            Ok(None) => continue,
                            Err(e) => {
                                tracing::warn!(error = %e, "gateway decompression failed");
                                break Outcome::Reconnect { resume: false, delay: Duration::ZERO, was_ready };
                            }
                        }
                    }
                    Some(Ok(WsMessage::Close(frame))) => {
                        let code = frame.as_ref().map(|f| u16::from(f.code));
                        tracing::info!(code = ?code, "gateway closed");
                        match close_code_action(code, session.can_resume()) {
                            Action::Fatal(r) => break Outcome::Fatal(r),
                            Action::Reconnect { resume, delay } => break Outcome::Reconnect { resume, delay, was_ready },
                            _ => break Outcome::Reconnect { resume: session.can_resume(), delay: Duration::ZERO, was_ready },
                        }
                    }
                    Some(Ok(_)) => continue,
                };
                let payload: GatewayPayload = match serde_json::from_str(&text) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(error = %e, "undecodable gateway frame");
                        continue;
                    }
                };
                let actions = session.on_payload(&payload, cfg.token.expose(), &cfg.properties, Instant::now());
                let mut stop = None;
                for action in actions {
                    match action {
                        Action::Send(text) => {
                            if ws.send(WsMessage::text(text)).await.is_err() {
                                stop = Some(Outcome::Reconnect { resume: session.can_resume(), delay: Duration::ZERO, was_ready });
                                break;
                            }
                        }
                        Action::StartHeartbeat(iv) => {
                            interval = iv;
                            // First heartbeat after `interval * jitter`.
                            let jitter = iv.mul_f64(fastrand::f64());
                            next_heartbeat = Instant::now() + jitter;
                        }
                        Action::Emit(EventKind::Connected) => {
                            was_ready = true;
                            ready_at.get_or_insert_with(Instant::now);
                            if tx.send(Event::Connection(ConnectionState::Connected)).await.is_err() {
                                stop = Some(Outcome::Closed);
                                break;
                            }
                        }
                        Action::Emit(EventKind::Dispatch(t)) => {
                            match types::parse_dispatch(&t, payload.d.clone()) {
                                Ok(Some(ev)) => {
                                    if tx.send(ev).await.is_err() {
                                        stop = Some(Outcome::Closed);
                                        break;
                                    }
                                }
                                Ok(None) => {}
                                Err(e) => tracing::warn!(event = %t, error = %e, "failed to parse dispatch"),
                            }
                        }
                        Action::Reconnect { resume, delay } => {
                            stop = Some(Outcome::Reconnect { resume, delay, was_ready });
                            break;
                        }
                        Action::Fatal(r) => {
                            stop = Some(Outcome::Fatal(r));
                            break;
                        }
                    }
                }
                if let Some(o) = stop {
                    break o;
                }
            }
        }
    };
    // Only a session that stayed up for a while resets the backoff.
    let _ = was_ready;
    let stable = ready_at.is_some_and(|t| t.elapsed() >= STABLE_AFTER);
    let outcome = match outcome {
        Outcome::Reconnect { resume, delay, .. } => Outcome::Reconnect {
            resume,
            delay,
            was_ready: stable,
        },
        other => other,
    };
    if let Outcome::Reconnect { resume: true, .. } = outcome {
        // Close with a non-1000 code so the session stays resumable.
        let _ = ws
            .close(Some(CloseFrame {
                code: CloseCode::Library(4000),
                reason: "".into(),
            }))
            .await;
    }
    outcome
}

/// Validates a gateway URL: `wss://` for any host, plain `ws://` only for
/// loopback hosts (local mock servers). Applied to the configured URLs and
/// to the `resume_gateway_url` the server hands us.
pub fn validate_url(url: &str) -> Result<()> {
    let u = url::Url::parse(url).map_err(|e| crate::error::Error::Protocol(e.to_string()))?;
    let loopback = matches!(
        u.host(),
        Some(url::Host::Domain("localhost"))
            | Some(url::Host::Ipv4(std::net::Ipv4Addr::LOCALHOST))
            | Some(url::Host::Ipv6(std::net::Ipv6Addr::LOCALHOST))
    );
    match u.scheme() {
        "wss" if u.host().is_some() => Ok(()),
        "ws" if loopback => Ok(()),
        s => Err(crate::error::Error::Protocol(format!(
            "refusing gateway URL with scheme {s} (wss required)"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn props() -> ClientProperties {
        ClientProperties::default()
    }

    fn frame(v: serde_json::Value) -> GatewayPayload {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn hello_triggers_identify_and_heartbeat() {
        let mut s = Session::new();
        s.on_connect();
        let actions = s.on_payload(
            &frame(json!({"op":10,"d":{"heartbeat_interval":41250}})),
            "tok",
            &props(),
            Instant::now(),
        );
        assert_eq!(
            actions[0],
            Action::StartHeartbeat(Duration::from_millis(41250))
        );
        let Action::Send(text) = &actions[1] else {
            panic!("expected send")
        };
        let v: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(v["op"], 2);
        assert_eq!(v["d"]["token"], "tok");
    }

    #[test]
    fn ready_stores_session_and_resume_uses_it() {
        let mut s = Session::new();
        s.on_connect();
        let a = s.on_payload(
            &frame(json!({"op":0,"t":"READY","s":1,"d":{"session_id":"abc","resume_gateway_url":"wss://resume.example","user":{"id":"1"}}})),
            "tok",
            &props(),
            Instant::now(),
        );
        assert!(a.contains(&Action::Emit(EventKind::Connected)));
        assert_eq!(s.session_id.as_deref(), Some("abc"));
        assert_eq!(s.seq, Some(1));
        s.on_payload(
            &frame(json!({"op":0,"t":"MESSAGE_CREATE","s":7,"d":{}})),
            "tok",
            &props(),
            Instant::now(),
        );
        assert_eq!(s.seq, Some(7));
        assert!(
            s.connect_url("wss://gateway.discord.gg", true)
                .starts_with("wss://resume.example/?v=10&encoding=json&compress=zlib-stream")
        );

        // New connection: HELLO now triggers RESUME with the last seq.
        s.on_connect();
        let a = s.on_payload(
            &frame(json!({"op":10,"d":{"heartbeat_interval":1000}})),
            "tok",
            &props(),
            Instant::now(),
        );
        let Action::Send(text) = &a[1] else { panic!() };
        let v: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(v["op"], 6);
        assert_eq!(v["d"]["session_id"], "abc");
        assert_eq!(v["d"]["seq"], 7);
    }

    #[test]
    fn insecure_resume_url_is_ignored() {
        let mut s = Session::new();
        s.on_payload(
            &frame(json!({"op":0,"t":"READY","s":1,"d":{"session_id":"abc","resume_gateway_url":"ws://evil.example.com","user":{"id":"1"}}})),
            "tok",
            &props(),
            Instant::now(),
        );
        assert_eq!(s.resume_url, None);
        assert!(
            s.connect_url("wss://gateway.discord.gg", false)
                .starts_with("wss://gateway.discord.gg/")
        );
    }

    #[test]
    fn seq_never_goes_backwards() {
        let mut s = Session::new();
        for n in [5, 3, 9, 8] {
            s.on_payload(
                &frame(json!({"op":0,"t":"X","s":n,"d":{}})),
                "t",
                &props(),
                Instant::now(),
            );
        }
        assert_eq!(s.seq, Some(9));
    }

    #[test]
    fn missed_ack_is_a_zombie() {
        let mut s = Session::new();
        let now = Instant::now();
        assert!(matches!(s.heartbeat(now), Action::Send(_)));
        assert!(matches!(
            s.heartbeat(now),
            Action::Reconnect { resume: true, .. }
        ));
    }

    #[test]
    fn ack_clears_pending_and_measures_latency() {
        let mut s = Session::new();
        let now = Instant::now();
        let _ = s.heartbeat(now);
        s.on_payload(
            &frame(json!({"op":11})),
            "t",
            &props(),
            now + Duration::from_millis(40),
        );
        assert!(!s.awaiting_ack);
        assert_eq!(s.latency, Some(Duration::from_millis(40)));
        assert!(matches!(s.heartbeat(now), Action::Send(_)));
    }

    #[test]
    fn server_heartbeat_request_is_answered() {
        let mut s = Session::new();
        s.seq = Some(3);
        let a = s.on_payload(
            &frame(json!({"op":1,"d":null})),
            "t",
            &props(),
            Instant::now(),
        );
        assert_eq!(a, vec![Action::Send(r#"{"op":1,"d":3}"#.into())]);
    }

    #[test]
    fn invalid_session_not_resumable_resets() {
        let mut s = Session::new();
        s.session_id = Some("x".into());
        s.seq = Some(4);
        let a = s.on_payload(
            &frame(json!({"op":9,"d":false})),
            "t",
            &props(),
            Instant::now(),
        );
        assert!(matches!(a[0], Action::Reconnect { resume: false, .. }));
        assert!(!s.can_resume());
    }

    #[test]
    fn reconnect_opcode_resumes() {
        let mut s = Session::new();
        let a = s.on_payload(
            &frame(json!({"op":7,"d":null})),
            "t",
            &props(),
            Instant::now(),
        );
        assert!(matches!(a[0], Action::Reconnect { resume: true, .. }));
    }

    #[test]
    fn close_codes() {
        assert_eq!(
            close_code_action(Some(4004), true),
            Action::Fatal(FatalReason::AuthenticationFailed)
        );
        assert!(matches!(
            close_code_action(Some(4009), true),
            Action::Reconnect { resume: false, .. }
        ));
        assert!(matches!(
            close_code_action(Some(1006), true),
            Action::Reconnect { resume: true, .. }
        ));
        assert!(matches!(
            close_code_action(Some(4014), true),
            Action::Fatal(_)
        ));
        for code in [4003, 4005, 4007] {
            assert!(matches!(
                close_code_action(Some(code), true),
                Action::Reconnect { resume: false, .. }
            ));
        }
        assert!(matches!(
            close_code_action(Some(4008), true),
            Action::Reconnect { delay, .. } if delay >= Duration::from_secs(5)
        ));
    }

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff(1, 1.0), Duration::from_millis(1000));
        assert_eq!(backoff(2, 1.0), Duration::from_millis(2000));
        assert_eq!(backoff(3, 0.0), Duration::from_millis(2000));
        assert_eq!(backoff(30, 1.0), Duration::from_millis(60_000));
        assert!(backoff(30, 0.0) >= Duration::from_millis(30_000));
    }

    #[test]
    fn url_validation() {
        assert!(validate_url("wss://gateway.discord.gg").is_ok());
        assert!(validate_url("https://x").is_err());
        assert!(validate_url("ws://evil.example.com").is_err());
        assert!(validate_url("ws://127.0.0.1:9000").is_ok());
        assert!(validate_url("ws://localhost:9000").is_ok());
        assert!(validate_url("not a url").is_err());
    }
}
