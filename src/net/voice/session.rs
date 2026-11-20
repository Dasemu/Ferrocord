//! Voice gateway (v8) protocol as a pure state machine: it consumes text
//! and binary frames and returns actions for the async runner. DAVE
//! opcodes are handed to [`Dave`].

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use super::dave::{Dave, DaveOut, MAX_BINARY, MAX_PROTOCOL_VERSION};
use super::rtp::Mode;
use crate::auth::secrets::Secret;

/// Voice gateway version (buffered resume, `seq_ack`).
pub const VERSION: u8 = 8;
/// Voice gateway ops are small JSON; anything bigger is refused.
pub const MAX_TEXT: usize = 64 * 1024;

/// Who we are on the voice server.
#[derive(Clone)]
pub struct Identity {
    /// Guild id, or the DM/group channel id for calls.
    pub server_id: u64,
    pub channel_id: u64,
    pub user_id: u64,
    pub session_id: String,
    pub token: Secret,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Identity { .. }")
    }
}

/// Shared DAVE state (the media path encrypts and decrypts with it).
pub type SharedDave = Arc<Mutex<Option<Dave>>>;

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Send(String),
    SendBinary(Vec<u8>),
    StartHeartbeat(Duration),
    /// Op 2 Ready: open UDP to `ip:port` and run IP discovery.
    OpenUdp {
        ip: String,
        port: u16,
        ssrc: u32,
        mode: Mode,
    },
    /// Op 4 Session Description: media may flow.
    SessionReady {
        mode: Mode,
        key: Vec<u8>,
    },
    /// Op 5 from the server: `user` sends with `ssrc`.
    UserSsrc {
        user: u64,
        ssrc: u32,
    },
    UserConnected(Vec<u64>),
    UserDisconnected(u64),
    Ping(u32),
    Resumed,
    /// Close and resume the connection.
    Reconnect,
    /// Unrecoverable: stop with this reason.
    Fatal(String),
}

#[derive(Debug, Deserialize)]
struct Frame {
    op: u8,
    #[serde(default)]
    d: Value,
    #[serde(default)]
    seq: Option<i64>,
}

/// How a close code is handled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseKind {
    Resume,
    /// New session needed (rejoin through the main gateway).
    Rejoin,
    Fatal(&'static str),
}

pub fn close_kind(code: Option<u16>) -> CloseKind {
    match code {
        Some(4014) => CloseKind::Fatal("Disconnected"),
        Some(4022) => CloseKind::Fatal("The call ended"),
        Some(4021) => CloseKind::Fatal("Rate limited by the voice server"),
        Some(4004) => CloseKind::Fatal("Voice authentication failed"),
        Some(4017) => CloseKind::Fatal("This call requires end-to-end encryption"),
        Some(4016) => CloseKind::Fatal("Unsupported encryption mode"),
        Some(4011) => CloseKind::Fatal("Voice server not found"),
        Some(4006) | Some(4009) => CloseKind::Rejoin,
        Some(1000) => CloseKind::Fatal("Disconnected"),
        _ => CloseKind::Resume,
    }
}

pub struct Session {
    id: Identity,
    dave: SharedDave,
    seq_ack: i64,
    ssrc: Option<u32>,
    mode: Option<Mode>,
    /// Outstanding heartbeat nonce and when it was sent.
    pending: Option<(u64, Instant)>,
    nonce: u64,
    selected: bool,
    pub ping_ms: Option<u32>,
    /// Op 25 received before Session Description created the DAVE state.
    early_external_sender: Option<Vec<u8>>,
    /// Users in the call (ops 11 / 13), bounded.
    connected: std::collections::HashSet<u64>,
}

impl Session {
    pub fn new(id: Identity, dave: SharedDave) -> Self {
        Self {
            id,
            dave,
            seq_ack: -1,
            ssrc: None,
            mode: None,
            pending: None,
            nonce: 0,
            selected: false,
            ping_ms: None,
            early_external_sender: None,
            connected: std::collections::HashSet::new(),
        }
    }

    pub fn ssrc(&self) -> Option<u32> {
        self.ssrc
    }

    pub fn url(endpoint: &str) -> String {
        let host = endpoint
            .trim_start_matches("wss://")
            .trim_start_matches("ws://")
            .trim_end_matches('/');
        if endpoint.starts_with("ws://") {
            format!("ws://{host}/?v={VERSION}")
        } else {
            format!("wss://{host}/?v={VERSION}")
        }
    }

    pub fn identify(&self) -> String {
        json!({"op": 0, "d": {
            "server_id": self.id.server_id.to_string(),
            "user_id": self.id.user_id.to_string(),
            "session_id": self.id.session_id,
            "token": self.id.token.expose(),
            "max_dave_protocol_version": MAX_PROTOCOL_VERSION,
        }})
        .to_string()
    }

    pub fn resume(&self) -> String {
        json!({"op": 7, "d": {
            "server_id": self.id.server_id.to_string(),
            "session_id": self.id.session_id,
            "token": self.id.token.expose(),
            "seq_ack": self.seq_ack,
        }})
        .to_string()
    }

    /// Op 1 with the discovered external address (sent once per session).
    pub fn select_protocol(&mut self, address: &str, port: u16) -> Option<String> {
        if self.selected {
            return None;
        }
        let mode = self.mode?;
        self.selected = true;
        Some(
            json!({"op": 1, "d": {
                "protocol": "udp",
                "data": {"address": address, "port": port, "mode": mode.wire()},
            }})
            .to_string(),
        )
    }

    /// Op 5: our speaking flags (1 = microphone, 0 = stopped).
    pub fn speaking(&self, flags: u8) -> Option<String> {
        let ssrc = self.ssrc?;
        Some(json!({"op": 5, "d": {"speaking": flags, "delay": 0, "ssrc": ssrc}}).to_string())
    }

    /// Heartbeat timer fired.
    pub fn heartbeat(&mut self, now: Instant) -> Action {
        if self
            .pending
            .is_some_and(|(_, at)| now.duration_since(at) > Duration::from_secs(60))
        {
            self.pending = None;
            return Action::Reconnect;
        }
        self.nonce = self.nonce.wrapping_add(1);
        let t = crate::mock::now_ms() ^ self.nonce;
        self.pending = Some((t, now));
        Action::Send(json!({"op": 3, "d": {"t": t, "seq_ack": self.seq_ack}}).to_string())
    }

    fn dave_actions(&self, out: Vec<DaveOut>) -> Vec<Action> {
        out.into_iter()
            .map(|o| match o {
                DaveOut::Binary(op, mut payload) => {
                    payload.insert(0, op);
                    Action::SendBinary(payload)
                }
                DaveOut::TransitionReady(t) => {
                    Action::Send(json!({"op": 23, "d": {"transition_id": t}}).to_string())
                }
                DaveOut::InvalidCommitWelcome(t) => {
                    Action::Send(json!({"op": 31, "d": {"transition_id": t}}).to_string())
                }
            })
            .collect()
    }

    fn with_dave<R: Default>(&self, f: impl FnOnce(&mut Dave) -> R) -> R {
        match self.dave.lock() {
            Ok(mut g) => g.as_mut().map(f).unwrap_or_default(),
            Err(_) => R::default(),
        }
    }

    pub fn on_text(&mut self, text: &str, now: Instant) -> Vec<Action> {
        if text.len() > MAX_TEXT {
            return Vec::new();
        }
        let Ok(f) = serde_json::from_str::<Frame>(text) else {
            return Vec::new();
        };
        if let Some(s) = f.seq {
            self.seq_ack = s;
        }
        let d = &f.d;
        let u64_of = |v: &Value| -> Option<u64> {
            v.as_str()
                .and_then(|s| s.parse().ok())
                .or_else(|| v.as_u64())
        };
        let u16_of = |k: &str| {
            d.get(k)
                .and_then(Value::as_u64)
                .map(|v| v.min(u16::MAX.into()) as u16)
        };
        match f.op {
            8 => {
                let iv = d
                    .get("heartbeat_interval")
                    .and_then(Value::as_f64)
                    .unwrap_or(13_750.0)
                    .clamp(1_000.0, 120_000.0);
                vec![Action::StartHeartbeat(Duration::from_millis(iv as u64))]
            }
            2 => {
                let ssrc = d.get("ssrc").and_then(Value::as_u64).map(|v| v as u32);
                let ip = d.get("ip").and_then(Value::as_str).map(str::to_owned);
                let port = d
                    .get("port")
                    .and_then(Value::as_u64)
                    .and_then(|p| u16::try_from(p).ok());
                let modes: Vec<String> = d
                    .get("modes")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|m| m.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                let (Some(ssrc), Some(ip), Some(port)) = (ssrc, ip, port) else {
                    return vec![Action::Fatal("Malformed voice READY".into())];
                };
                if ip.parse::<std::net::IpAddr>().is_err() {
                    return vec![Action::Fatal("Malformed voice READY".into())];
                }
                let Some(mode) = Mode::choose(&modes) else {
                    return vec![Action::Fatal("No supported encryption mode".into())];
                };
                self.ssrc = Some(ssrc);
                self.mode = Some(mode);
                vec![Action::OpenUdp {
                    ip,
                    port,
                    ssrc,
                    mode,
                }]
            }
            4 => {
                let mode = d
                    .get("mode")
                    .and_then(Value::as_str)
                    .and_then(Mode::from_wire);
                let key: Option<Vec<u8>> = d.get("secret_key").and_then(Value::as_array).map(|a| {
                    a.iter()
                        .filter_map(|b| b.as_u64().and_then(|b| u8::try_from(b).ok()))
                        .collect()
                });
                let (Some(mode), Some(key)) = (mode, key) else {
                    return vec![Action::Fatal("Malformed session description".into())];
                };
                if key.len() != 32 {
                    return vec![Action::Fatal("Malformed session description".into())];
                }
                let version = u16_of("dave_protocol_version").unwrap_or(0);
                let (dave, out) = Dave::new(self.id.user_id, self.id.channel_id, version);
                if let Ok(mut g) = self.dave.lock() {
                    let mut dave = dave;
                    dave.connected = self.connected.clone();
                    if let Some(es) = self.early_external_sender.take() {
                        dave.on_external_sender(&es);
                    }
                    *g = Some(dave);
                }
                let mut acts = vec![Action::SessionReady { mode, key }];
                acts.extend(self.dave_actions(out));
                acts
            }
            6 => {
                let t = d.get("t").and_then(Value::as_u64).or_else(|| d.as_u64());
                match self.pending {
                    Some((n, at)) if Some(n) == t => {
                        self.pending = None;
                        let ms = now.duration_since(at).as_millis().min(60_000) as u32;
                        self.ping_ms = Some(ms);
                        vec![Action::Ping(ms)]
                    }
                    _ => Vec::new(),
                }
            }
            5 => {
                let user = d.get("user_id").and_then(u64_of);
                let ssrc = d.get("ssrc").and_then(Value::as_u64).map(|v| v as u32);
                match (user, ssrc) {
                    (Some(user), Some(ssrc)) => vec![Action::UserSsrc { user, ssrc }],
                    _ => Vec::new(),
                }
            }
            9 => vec![Action::Resumed],
            11 => {
                let ids: Vec<u64> = d
                    .get("user_ids")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(u64_of).take(1000).collect())
                    .unwrap_or_default();
                if self.connected.len() < 10_000 {
                    self.connected.extend(ids.iter().copied());
                }
                self.with_dave(|dv| dv.connected.extend(ids.iter().copied()));
                vec![Action::UserConnected(ids)]
            }
            13 => match d.get("user_id").and_then(u64_of) {
                Some(u) => {
                    self.connected.remove(&u);
                    self.with_dave(|dv| {
                        dv.connected.remove(&u);
                    });
                    vec![Action::UserDisconnected(u)]
                }
                None => Vec::new(),
            },
            21 => {
                let (Some(t), Some(v)) = (u16_of("transition_id"), u16_of("protocol_version"))
                else {
                    return Vec::new();
                };
                let out = self.with_dave(|dv| dv.on_prepare_transition(t, v));
                self.dave_actions(out)
            }
            22 => {
                if let Some(t) = u16_of("transition_id") {
                    self.with_dave(|dv| dv.on_execute_transition(t));
                }
                Vec::new()
            }
            24 => {
                let epoch = d.get("epoch").and_then(Value::as_u64);
                let (Some(e), Some(v)) = (epoch, u16_of("protocol_version")) else {
                    return Vec::new();
                };
                let out = self.with_dave(|dv| dv.on_prepare_epoch(e, v));
                self.dave_actions(out)
            }
            _ => Vec::new(),
        }
    }

    /// Binary server frame: `seq (u16) | op (u8) | payload`.
    pub fn on_binary(&mut self, frame: &[u8]) -> Vec<Action> {
        if frame.len() < 3 || frame.len() > MAX_BINARY {
            return Vec::new();
        }
        self.seq_ack = i64::from(u16::from_be_bytes([frame[0], frame[1]]));
        let payload = &frame[3..];
        let out = match frame[2] {
            25 => {
                let applied = self.with_dave(|dv| {
                    dv.on_external_sender(payload);
                    true
                });
                if !applied {
                    self.early_external_sender = Some(payload.to_vec());
                }
                Vec::new()
            }
            27 => self.with_dave(|dv| dv.on_proposals(payload)),
            29 => self.with_dave(|dv| dv.on_commit(payload)),
            30 => self.with_dave(|dv| dv.on_welcome(payload)),
            _ => Vec::new(),
        };
        self.dave_actions(out)
    }

    /// After a reconnect: send Resume (keeps the selected protocol).
    pub fn on_reconnect(&mut self) {
        self.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session::new(
            Identity {
                server_id: 41,
                channel_id: 127,
                user_id: 104,
                session_id: "sess".into(),
                token: Secret::new("vtok"),
            },
            Arc::new(Mutex::new(None)),
        )
    }

    #[test]
    fn identify_carries_dave_version() {
        let s = session();
        let v: Value = serde_json::from_str(&s.identify()).unwrap();
        assert_eq!(v["op"], 0);
        assert_eq!(v["d"]["server_id"], "41");
        assert_eq!(v["d"]["user_id"], "104");
        assert_eq!(v["d"]["token"], "vtok");
        assert_eq!(v["d"]["max_dave_protocol_version"], 1);
        assert_eq!(
            Session::url("x.discord.media:443"),
            "wss://x.discord.media:443/?v=8"
        );
        assert_eq!(Session::url("ws://127.0.0.1:9/"), "ws://127.0.0.1:9/?v=8");
    }

    #[test]
    fn handshake_flow() {
        let mut s = session();
        let now = Instant::now();
        assert_eq!(
            s.on_text(r#"{"op":8,"d":{"heartbeat_interval":41250,"v":8}}"#, now),
            vec![Action::StartHeartbeat(Duration::from_millis(41250))]
        );
        let ready = r#"{"op":2,"d":{"ssrc":7,"ip":"127.0.0.1","port":1234,
            "modes":["aead_aes256_gcm_rtpsize","aead_xchacha20_poly1305_rtpsize"]}}"#;
        assert_eq!(
            s.on_text(ready, now),
            vec![Action::OpenUdp {
                ip: "127.0.0.1".into(),
                port: 1234,
                ssrc: 7,
                mode: Mode::Aes256GcmRtpSize
            }]
        );
        let sel: Value = serde_json::from_str(&s.select_protocol("1.2.3.4", 5).unwrap()).unwrap();
        assert_eq!(sel["d"]["data"]["mode"], "aead_aes256_gcm_rtpsize");
        assert_eq!(sel["d"]["data"]["address"], "1.2.3.4");
        // Select Protocol is sent only once per session.
        assert!(s.select_protocol("1.2.3.4", 5).is_none());
        let key: Vec<u8> = (0..32).collect();
        let desc = json!({"op":4,"d":{"mode":"aead_aes256_gcm_rtpsize","secret_key":key,
            "dave_protocol_version":1}, "seq": 3});
        let acts = s.on_text(&desc.to_string(), now);
        assert_eq!(
            acts[0],
            Action::SessionReady {
                mode: Mode::Aes256GcmRtpSize,
                key
            }
        );
        // DAVE v1: a key package (binary op 26) goes out right away.
        assert!(matches!(&acts[1], Action::SendBinary(b) if b[0] == 26));
        assert!(s.dave.lock().unwrap().is_some());
        // seq is tracked for heartbeats.
        let hb = match s.heartbeat(now) {
            Action::Send(t) => serde_json::from_str::<Value>(&t).unwrap(),
            a => panic!("{a:?}"),
        };
        assert_eq!(hb["d"]["seq_ack"], 3);
        let ack = json!({"op":6,"d":{"t":hb["d"]["t"]}}).to_string();
        assert_eq!(
            s.on_text(&ack, now + Duration::from_millis(42)),
            vec![Action::Ping(42)]
        );
        let sp: Value = serde_json::from_str(&s.speaking(1).unwrap()).unwrap();
        assert_eq!(sp["d"], json!({"speaking":1,"delay":0,"ssrc":7}));
        assert_eq!(
            s.on_text(
                r#"{"op":5,"d":{"user_id":"55","ssrc":9,"speaking":1}}"#,
                now
            ),
            vec![Action::UserSsrc { user: 55, ssrc: 9 }]
        );
        assert_eq!(
            s.on_text(r#"{"op":11,"d":{"user_ids":["55","56"]}}"#, now),
            vec![Action::UserConnected(vec![55, 56])]
        );
        assert!(
            s.dave
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .connected
                .contains(&56)
        );
        assert_eq!(
            s.on_text(r#"{"op":13,"d":{"user_id":"56"}}"#, now),
            vec![Action::UserDisconnected(56)]
        );
        // Resume carries seq_ack.
        let r: Value = serde_json::from_str(&s.resume()).unwrap();
        assert_eq!(r["d"]["seq_ack"], 3);
    }

    #[test]
    fn transition_ready_is_answered() {
        let mut s = session();
        let desc = json!({"op":4,"d":{"mode":"aead_xchacha20_poly1305_rtpsize",
            "secret_key":vec![0u8;32],"dave_protocol_version":1}});
        let _ = s.on_text(&desc.to_string(), Instant::now());
        let acts = s.on_text(
            r#"{"op":21,"d":{"transition_id":4,"protocol_version":0}}"#,
            Instant::now(),
        );
        assert_eq!(
            acts,
            vec![Action::Send(r#"{"d":{"transition_id":4},"op":23}"#.into())]
        );
        let _ = s.on_text(r#"{"op":22,"d":{"transition_id":4}}"#, Instant::now());
        assert_eq!(
            s.dave.lock().unwrap().as_ref().unwrap().protocol_version(),
            0
        );
    }

    #[test]
    fn missed_acks_reconnect() {
        let mut s = session();
        let t0 = Instant::now();
        assert!(matches!(s.heartbeat(t0), Action::Send(_)));
        assert_eq!(s.heartbeat(t0 + Duration::from_secs(61)), Action::Reconnect);
    }

    #[test]
    fn close_codes() {
        assert_eq!(close_kind(Some(4015)), CloseKind::Resume);
        assert_eq!(close_kind(None), CloseKind::Resume);
        assert_eq!(close_kind(Some(4006)), CloseKind::Rejoin);
        assert!(matches!(close_kind(Some(4014)), CloseKind::Fatal(_)));
        assert!(matches!(close_kind(Some(4017)), CloseKind::Fatal(_)));
    }

    #[test]
    fn hostile_frames_never_panic() {
        let mut s = session();
        let now = Instant::now();
        for t in [
            "",
            "{",
            "null",
            r#"{"op":2,"d":{"ssrc":-1,"ip":5}}"#,
            r#"{"op":2,"d":{"ssrc":1,"ip":"evil.example","port":1,"modes":["aead_aes256_gcm_rtpsize"]}}"#,
            r#"{"op":4,"d":{"mode":"aead_aes256_gcm_rtpsize","secret_key":[1,2,3]}}"#,
            r#"{"op":4,"d":{"mode":"xsalsa20_poly1305","secret_key":[]}}"#,
            r#"{"op":21,"d":{"transition_id":99999999,"protocol_version":"x"}}"#,
            r#"{"op":24,"d":{"epoch":1,"protocol_version":65535}}"#,
            r#"{"op":11,"d":{"user_ids":[1,"a",null]}}"#,
            r#"{"op":6,"d":"x"}"#,
            r#"{"op":255}"#,
        ] {
            let _ = s.on_text(t, now);
        }
        assert!(s.on_text(&"x".repeat(MAX_TEXT + 1), now).is_empty());
        for len in 0..64usize {
            let mut b = vec![0u8; len];
            for (i, v) in b.iter_mut().enumerate() {
                *v = (i * 37 + len) as u8;
            }
            if len > 2 {
                b[2] = [25, 27, 29, 30][len % 4];
            }
            let _ = s.on_binary(&b);
        }
        assert!(s.on_binary(&vec![0; MAX_BINARY + 1]).is_empty());
    }
}
