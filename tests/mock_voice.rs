//! Voice connection against a local mock voice server: WebSocket handshake
//! (HELLO, IDENTIFY with DAVE support, READY, Select Protocol, Session
//! Description, heartbeats with seq_ack), UDP IP discovery, and real Opus
//! media both ways with transport encryption, fed by a fake microphone.

#![allow(clippy::result_large_err)] // tungstenite handshake callback signature

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ferrocord::auth::secrets::Secret;
use ferrocord::net::voice::audio::{AudioFactory, AudioIo, FakeIo};
use ferrocord::net::voice::conn::{self, VoiceConfig};
use ferrocord::net::voice::mixer::{FRAME_LEN, level_db};
use ferrocord::net::voice::rtp::{self, Cipher, Mode};
use ferrocord::net::voice::session::Identity;
use ferrocord::net::voice::{VoiceControl, VoiceEvent};
use futures::{SinkExt, Stream, StreamExt};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message as Ws;

const STEP: Duration = Duration::from_secs(10);
const KEY: [u8; 32] = [9; 32];
const MY_SSRC: u32 = 4242;
const PEER_SSRC: u32 = 777;
const PEER: u64 = 55;

type Server = WebSocketStream<TcpStream>;

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

async fn recv(ws: &mut Server) -> Ws {
    tokio::time::timeout(STEP, ws.next())
        .await
        .expect("timed out waiting for client frame")
        .expect("client closed")
        .unwrap()
}

/// Next text frame with opcode `op` (others are skipped), within STEP.
async fn recv_op(ws: &mut Server, op: u64) -> Value {
    let deadline = tokio::time::Instant::now() + STEP;
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no op {op} from client"
        );
        if let Ws::Text(t) = recv(ws).await {
            let v: Value = serde_json::from_str(t.as_str()).unwrap();
            if v["op"] == op {
                return v;
            }
        }
    }
}

/// Collects the first frame of each opcode in `ops` (any order).
async fn recv_ops(ws: &mut Server, ops: &[u64]) -> Vec<Value> {
    let deadline = tokio::time::Instant::now() + STEP;
    let mut found: Vec<Option<Value>> = vec![None; ops.len()];
    while found.iter().any(Option::is_none) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "missing ops {ops:?}"
        );
        if let Ws::Text(t) = recv(ws).await {
            let v: Value = serde_json::from_str(t.as_str()).unwrap();
            if let Some(i) = ops.iter().position(|o| v["op"] == *o)
                && found[i].is_none()
            {
                found[i] = Some(v);
            }
        }
    }
    found.into_iter().flatten().collect()
}

async fn next_event(s: &mut (impl Stream<Item = VoiceEvent> + Unpin)) -> VoiceEvent {
    tokio::time::timeout(STEP, s.next())
        .await
        .expect("timed out waiting for voice event")
        .expect("voice stream ended")
}

fn config(endpoint: String, played: Arc<Mutex<Vec<f32>>>) -> VoiceConfig {
    let factory: AudioFactory = Arc::new(move |_| {
        let (mut io, _) = FakeIo::new(440.0, 0.4);
        io.played = played.clone();
        Ok(Box::new(io) as Box<dyn AudioIo>)
    });
    VoiceConfig {
        endpoint,
        identity: Identity {
            server_id: 100,
            channel_id: 140,
            user_id: 1,
            session_id: "voice-session".into(),
            token: Secret::new("voice-token-not-real"),
        },
        control: VoiceControl::new(),
        audio: factory,
    }
}

/// Starts the mock UDP media server; returns its port.
async fn udp_server() -> (Arc<UdpSocket>, u16) {
    let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = s.local_addr().unwrap().port();
    (Arc::new(s), port)
}

#[tokio::test]
async fn handshake_discovery_and_media() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let (udp, udp_port) = udp_server().await;
    let played: Arc<Mutex<Vec<f32>>> = Arc::default();
    let mut events = Box::pin(conn::spawn(config(endpoint, played.clone())));

    let (sock, _) = tokio::time::timeout(STEP, listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut uri = String::new();
    let mut ws = tokio_tungstenite::accept_hdr_async(
        sock,
        |req: &tokio_tungstenite::tungstenite::handshake::server::Request,
         resp: tokio_tungstenite::tungstenite::handshake::server::Response| {
            uri = req.uri().to_string();
            Ok(resp)
        },
    )
    .await
    .unwrap();
    assert_eq!(uri, "/?v=8");

    // IDENTIFY first, announcing DAVE support.
    let id = recv_op(&mut ws, 0).await;
    assert_eq!(id["d"]["server_id"], "100");
    assert_eq!(id["d"]["user_id"], "1");
    assert_eq!(id["d"]["session_id"], "voice-session");
    assert_eq!(id["d"]["token"], "voice-token-not-real");
    assert_eq!(id["d"]["max_dave_protocol_version"], 1);

    ws.send(Ws::text(fixture("voice_hello.json").to_string()))
        .await
        .unwrap();
    let mut ready = fixture("voice_ready.json");
    ready["d"]["port"] = json!(udp_port);
    ready["seq"] = json!(1);
    ws.send(Ws::text(ready.to_string())).await.unwrap();

    // IP discovery over UDP.
    let mut buf = [0u8; 1500];
    let (n, client_addr) = tokio::time::timeout(STEP, udp.recv_from(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(n, rtp::DISCOVERY_LEN);
    assert_eq!(&buf[4..8], &MY_SSRC.to_be_bytes());
    let resp = rtp::discovery_response(MY_SSRC, "203.0.113.9", 61_000);
    udp.send_to(&resp, client_addr).await.unwrap();

    // Select Protocol with the discovered address and the preferred mode.
    let sel = recv_op(&mut ws, 1).await;
    assert_eq!(sel["d"]["protocol"], "udp");
    assert_eq!(sel["d"]["data"]["address"], "203.0.113.9");
    assert_eq!(sel["d"]["data"]["port"], 61_000);
    assert_eq!(sel["d"]["data"]["mode"], "aead_aes256_gcm_rtpsize");

    // Transport-only session (DAVE version 0) so the media can be checked here;
    // DAVE itself is covered by the two-member loopback unit test.
    let desc = json!({"op": 4, "seq": 2, "d": {"mode": "aead_aes256_gcm_rtpsize",
        "secret_key": KEY.to_vec(), "dave_protocol_version": 0}});
    ws.send(Ws::text(desc.to_string())).await.unwrap();
    assert_eq!(
        next_event(&mut events).await,
        VoiceEvent::Connected { e2ee: false }
    );

    // The peer's SSRC mapping, then a heartbeat with seq_ack.
    let speaking = json!({"op": 5, "seq": 3, "d": {"user_id": PEER.to_string(), "ssrc": PEER_SSRC, "speaking": 1}});
    ws.send(Ws::text(speaking.to_string())).await.unwrap();
    // The fake microphone talks right away (op 5 speaking); a heartbeat
    // carries seq_ack.
    let got = recv_ops(&mut ws, &[3, 5]).await;
    let (hb, sp) = (&got[0], &got[1]);
    assert!(hb["d"]["seq_ack"].as_i64().unwrap() >= 2);
    ws.send(Ws::text(
        json!({"op": 6, "d": {"t": hb["d"]["t"]}}).to_string(),
    ))
    .await
    .unwrap();
    assert_eq!(sp["d"], json!({"speaking": 1, "delay": 0, "ssrc": MY_SSRC}));
    let server_cipher = Cipher::new(Mode::Aes256GcmRtpSize, &KEY).unwrap();
    let mut decoder = opus::Decoder::new(48_000, opus::Channels::Stereo).unwrap();
    let mut pcm = vec![0.0f32; FRAME_LEN];
    let mut got = Vec::new();
    while got.len() < 25 * FRAME_LEN {
        let (n, _) = tokio::time::timeout(STEP, udp.recv_from(&mut buf))
            .await
            .unwrap()
            .unwrap();
        let Some((h, opus_frame)) = server_cipher.decrypt_packet(&buf[..n]) else {
            continue; // keepalives
        };
        assert_eq!(h.ssrc, MY_SSRC);
        assert_eq!(h.payload_type, rtp::OPUS_PAYLOAD_TYPE);
        let samples = decoder.decode_float(&opus_frame, &mut pcm, false).unwrap();
        got.extend_from_slice(&pcm[..samples * 2]);
    }
    let lvl = level_db(&got[got.len() - 10 * FRAME_LEN..]);
    assert!(lvl > -15.0, "server received level {lvl}");

    // The peer talks back: a sine wave arrives at the fake speakers.
    let mut tx = Cipher::new(Mode::Aes256GcmRtpSize, &KEY).unwrap();
    let mut enc =
        opus::Encoder::new(48_000, opus::Channels::Stereo, opus::Application::Voip).unwrap();
    let mut out = [0u8; 1275];
    let mut speaking_seen = false;
    for i in 0..60u32 {
        let frame: Vec<f32> = (0..FRAME_LEN / 2)
            .flat_map(|k| {
                let t = (i as usize * FRAME_LEN / 2 + k) as f32 / 48_000.0;
                let v = 0.4 * (2.0 * std::f32::consts::PI * 330.0 * t).sin();
                [v, v]
            })
            .collect();
        let n = enc.encode_float(&frame, &mut out).unwrap();
        let header = rtp::opus_header(i as u16, i * 960, PEER_SSRC);
        let p = tx.encrypt_packet(&header, &out[..n]).unwrap();
        udp.send_to(&p, client_addr).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Drain events: Ping and the peer's speaking indicator must show up.
    let mut ping = false;
    let deadline = tokio::time::Instant::now() + STEP;
    while (!ping || !speaking_seen) && tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), events.next()).await {
            Ok(Some(VoiceEvent::Ping(_))) => ping = true,
            Ok(Some(VoiceEvent::Speaking {
                user: PEER,
                speaking: true,
            })) => speaking_seen = true,
            _ => {}
        }
    }
    assert!(ping, "no Ping event");
    assert!(speaking_seen, "no speaking indicator for the peer");
    let played = played.lock().unwrap().clone();
    assert!(played.len() >= 20 * FRAME_LEN, "played {}", played.len());
    let lvl = level_db(&played[played.len() - 10 * FRAME_LEN..]);
    assert!(lvl > -15.0, "client played level {lvl}");

    // Close 4014 (kicked): the connection ends without rejoining.
    ws.close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame {
        code: 4014.into(),
        reason: "".into(),
    }))
    .await
    .unwrap();
    loop {
        match next_event(&mut events).await {
            VoiceEvent::Ended { rejoin, .. } => {
                assert!(!rejoin);
                break;
            }
            _ => continue,
        }
    }
}

#[tokio::test]
async fn dave_session_sends_key_package_and_4017_is_fatal() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let (udp, udp_port) = udp_server().await;
    let mut events = Box::pin(conn::spawn(config(endpoint, Arc::default())));
    let (sock, _) = tokio::time::timeout(STEP, listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
    let _ = recv_op(&mut ws, 0).await;
    ws.send(Ws::text(fixture("voice_hello.json").to_string()))
        .await
        .unwrap();
    let mut ready = fixture("voice_ready.json");
    ready["d"]["port"] = json!(udp_port);
    ready["d"]["modes"] = json!(["aead_xchacha20_poly1305_rtpsize"]);
    ws.send(Ws::text(ready.to_string())).await.unwrap();
    let mut buf = [0u8; 1500];
    let (_, addr) = tokio::time::timeout(STEP, udp.recv_from(&mut buf))
        .await
        .unwrap()
        .unwrap();
    udp.send_to(
        &rtp::discovery_response(MY_SSRC, "198.51.100.2", 5000),
        addr,
    )
    .await
    .unwrap();
    let sel = recv_op(&mut ws, 1).await;
    assert_eq!(sel["d"]["data"]["mode"], "aead_xchacha20_poly1305_rtpsize");
    let desc = json!({"op": 4, "d": {"mode": "aead_xchacha20_poly1305_rtpsize",
        "secret_key": KEY.to_vec(), "dave_protocol_version": 1}});
    ws.send(Ws::text(desc.to_string())).await.unwrap();
    // DAVE v1: the client sends its MLS key package as binary op 26.
    let deadline = tokio::time::Instant::now() + STEP;
    loop {
        assert!(tokio::time::Instant::now() < deadline, "no key package");
        if let Ws::Binary(b) = recv(&mut ws).await {
            assert_eq!(b[0], 26);
            assert!(b.len() > 64);
            break;
        }
    }
    ws.close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame {
        code: 4017.into(),
        reason: "".into(),
    }))
    .await
    .unwrap();
    loop {
        if let VoiceEvent::Ended { reason, rejoin } = next_event(&mut events).await {
            assert!(!rejoin);
            assert!(reason.contains("end-to-end"));
            break;
        }
    }
}

#[test]
fn endpoints_are_validated() {
    assert_eq!(
        conn::url_for("c-fra08.discord.media:443").as_deref(),
        Some("wss://c-fra08.discord.media:443/?v=8")
    );
    assert!(conn::url_for("ws://127.0.0.1:9").is_some());
    // Plain ws only to loopback.
    assert!(conn::url_for("ws://evil.example:80").is_none());
    assert!(conn::url_for("").is_none());
}
