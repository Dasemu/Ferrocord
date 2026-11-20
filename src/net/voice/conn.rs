//! Async runner for one voice connection: voice gateway WebSocket, UDP
//! socket with IP discovery, the 20 ms media clock and resume handling.
//! The returned stream *is* the connection: dropping it (leaving the
//! channel) closes the socket, stops the media task and frees the audio
//! devices.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::{SinkExt, Stream, StreamExt};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

use super::audio::{AudioFactory, AudioIo};
use super::media::Engine;
use super::rtp::{self, Cipher, Mode};
use super::session::{Action, CloseKind, Identity, Session, SharedDave, close_kind};
use super::{VoiceControl, VoiceEvent};

/// Everything needed to connect to a voice server.
#[derive(Clone)]
pub struct VoiceConfig {
    /// `endpoint` from VOICE_SERVER_UPDATE (host[:port], no scheme), or a
    /// `ws://127.0.0.1:port` URL for local mock servers.
    pub endpoint: String,
    pub identity: Identity,
    pub control: VoiceControl,
    pub audio: AudioFactory,
}

impl std::fmt::Debug for VoiceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VoiceConfig { .. }")
    }
}

/// Resume attempts before giving up.
const MAX_RESUMES: u32 = 5;

pub fn spawn(cfg: VoiceConfig) -> impl Stream<Item = VoiceEvent> + Send + 'static {
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(run(cfg, tx));
    tokio_stream_from(rx)
}

fn tokio_stream_from(mut rx: mpsc::Receiver<VoiceEvent>) -> impl Stream<Item = VoiceEvent> {
    futures::stream::poll_fn(move |cx| rx.poll_recv(cx))
}

/// Validates the endpoint: any `host[:port]` (wss), or `ws://` to loopback.
pub fn url_for(endpoint: &str) -> Option<String> {
    let url = Session::url(endpoint);
    let u = url::Url::parse(&url).ok()?;
    if u.scheme() == "ws" {
        let loopback = matches!(
            u.host(),
            Some(url::Host::Domain("localhost"))
                | Some(url::Host::Ipv4(std::net::Ipv4Addr::LOCALHOST))
        );
        if !loopback {
            return None;
        }
    }
    u.host()?;
    Some(url)
}

/// Messages from the media task to the WebSocket loop.
enum ToWs {
    Text(String),
    Binary(Vec<u8>),
}

struct Media {
    task: tokio::task::JoinHandle<()>,
    /// SSRC -> user updates for the engine.
    ssrc_tx: mpsc::UnboundedSender<MediaCmd>,
}

enum MediaCmd {
    Map(u32, u64),
    Forget(u64),
}

async fn run(cfg: VoiceConfig, tx: mpsc::Sender<VoiceEvent>) {
    let Some(url) = url_for(&cfg.endpoint) else {
        let _ = tx
            .send(VoiceEvent::Ended {
                reason: "Invalid voice server".into(),
                rejoin: false,
            })
            .await;
        return;
    };
    let dave: SharedDave = Arc::new(Mutex::new(None));
    let mut session = Session::new(cfg.identity.clone(), dave.clone());
    let mut udp: Option<Arc<UdpSocket>> = None;
    let mut media: Option<Media> = None;
    let mut resumes = 0u32;
    let mut resuming = false;
    let (ws_tx, mut ws_rx) = mpsc::unbounded_channel::<ToWs>();
    let end = loop {
        let connect = tokio::time::timeout(
            Duration::from_secs(15),
            tokio_tungstenite::connect_async(url.as_str()),
        )
        .await;
        let mut ws = match connect {
            Ok(Ok((ws, _))) => ws,
            _ => {
                if resuming && resumes < MAX_RESUMES {
                    resumes += 1;
                    tokio::time::sleep(Duration::from_secs(1 << resumes.min(4))).await;
                    continue;
                }
                break ("Could not reach the voice server".to_owned(), false);
            }
        };
        let first = if resuming {
            session.resume()
        } else {
            session.identify()
        };
        if ws.send(WsMessage::text(first)).await.is_err() {
            break ("Voice connection lost".to_owned(), false);
        }
        let mut hb_interval = Duration::from_secs(13);
        let far = tokio::time::Instant::now() + Duration::from_secs(3600);
        let mut next_hb = far;
        let outcome: Result<(), CloseKind> = loop {
            tokio::select! {
                _ = tx.closed() => {
                    let _ = ws.close(Some(CloseFrame { code: CloseCode::Normal, reason: "".into() })).await;
                    if let Some(m) = media.take() { m.task.abort(); }
                    return;
                }
                _ = tokio::time::sleep_until(next_hb) => {
                    next_hb = tokio::time::Instant::now() + hb_interval;
                    match session.heartbeat(Instant::now()) {
                        Action::Send(t) => {
                            if ws.send(WsMessage::text(t)).await.is_err() { break Err(CloseKind::Resume); }
                        }
                        _ => break Err(CloseKind::Resume),
                    }
                }
                Some(m) = ws_rx.recv() => {
                    let msg = match m {
                        ToWs::Text(t) => WsMessage::text(t),
                        ToWs::Binary(b) => WsMessage::binary(b),
                    };
                    if ws.send(msg).await.is_err() { break Err(CloseKind::Resume); }
                }
                frame = ws.next() => {
                    let actions = match frame {
                        None => break Err(CloseKind::Resume),
                        Some(Err(_)) => break Err(CloseKind::Resume),
                        Some(Ok(WsMessage::Text(t))) => session.on_text(t.as_str(), Instant::now()),
                        Some(Ok(WsMessage::Binary(b))) => session.on_binary(&b),
                        Some(Ok(WsMessage::Close(f))) => {
                            let code = f.as_ref().map(|f| u16::from(f.code));
                            tracing::info!(code = ?code, "voice gateway closed");
                            break Err(close_kind(code));
                        }
                        Some(Ok(_)) => continue,
                    };
                    let mut stop = None;
                    for a in actions {
                        match a {
                            Action::Send(t) => {
                                if ws.send(WsMessage::text(t)).await.is_err() { stop = Some(CloseKind::Resume); break; }
                            }
                            Action::SendBinary(b) => {
                                if ws.send(WsMessage::binary(b)).await.is_err() { stop = Some(CloseKind::Resume); break; }
                            }
                            Action::StartHeartbeat(iv) => {
                                hb_interval = iv;
                                next_hb = tokio::time::Instant::now() + iv.mul_f64(fastrand::f64());
                            }
                            Action::OpenUdp { ip, port, ssrc, .. } => {
                                match open_udp(&ip, port, ssrc).await {
                                    Ok((sock, addr, p)) => {
                                        udp = Some(Arc::new(sock));
                                        if let Some(sel) = session.select_protocol(&addr, p)
                                            && ws.send(WsMessage::text(sel)).await.is_err()
                                        {
                                            stop = Some(CloseKind::Resume);
                                            break;
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!(error = %e, "voice UDP setup failed");
                                        stop = Some(CloseKind::Fatal("Could not open a voice connection (UDP)"));
                                        break;
                                    }
                                }
                            }
                            Action::SessionReady { mode, key } => {
                                if let (Some(sock), Some(ssrc)) = (udp.clone(), session.ssrc()) {
                                    if let Some(m) = media.take() { m.task.abort(); }
                                    match start_media(&cfg, sock, ssrc, mode, &key, dave.clone(), tx.clone(), ws_tx.clone()) {
                                        Ok(m) => media = Some(m),
                                        Err(e) => {
                                            tracing::warn!(error = %e, "voice media failed to start");
                                            stop = Some(CloseKind::Fatal("Could not open audio devices"));
                                            break;
                                        }
                                    }
                                    let e2ee = dave.lock().ok().and_then(|g| g.as_ref().map(|d| d.protocol_version() > 0)).unwrap_or(false);
                                    let _ = tx.send(VoiceEvent::Connected { e2ee }).await;
                                }
                            }
                            Action::UserSsrc { user, ssrc } => {
                                if let Some(m) = &media { let _ = m.ssrc_tx.send(MediaCmd::Map(ssrc, user)); }
                            }
                            Action::UserDisconnected(u) => {
                                if let Some(m) = &media { let _ = m.ssrc_tx.send(MediaCmd::Forget(u)); }
                            }
                            Action::UserConnected(_) => {}
                            Action::Ping(ms) => {
                                resumes = 0;
                                let _ = tx.try_send(VoiceEvent::Ping(ms));
                            }
                            Action::Resumed => {
                                resumes = 0;
                                let _ = tx.send(VoiceEvent::Connected { e2ee: dave.lock().ok().and_then(|g| g.as_ref().map(|d| d.is_e2ee())).unwrap_or(false) }).await;
                            }
                            Action::Reconnect => { stop = Some(CloseKind::Resume); break; }
                            Action::Fatal(r) => {
                                tracing::warn!(reason = %r, "voice connection failed");
                                stop = Some(CloseKind::Fatal("Voice connection failed"));
                                break;
                            }
                        }
                    }
                    if let Some(s) = stop { break Err(s); }
                }
            }
        };
        match outcome {
            Ok(()) => break ("Disconnected".to_owned(), false),
            Err(CloseKind::Fatal(r)) => {
                let _ = ws.close(None).await;
                break (r.to_owned(), false);
            }
            Err(CloseKind::Rejoin) => break ("Voice session expired".to_owned(), true),
            Err(CloseKind::Resume) => {
                let _ = ws
                    .close(Some(CloseFrame {
                        code: CloseCode::Library(4000),
                        reason: "".into(),
                    }))
                    .await;
                // Only a session that got as far as UDP can be resumed.
                if udp.is_none() || resumes >= MAX_RESUMES {
                    break ("Voice connection lost".to_owned(), true);
                }
                resumes += 1;
                resuming = true;
                session.on_reconnect();
                let _ = tx.try_send(VoiceEvent::Reconnecting);
                tokio::time::sleep(Duration::from_millis(500 * u64::from(resumes))).await;
            }
        }
    };
    if let Some(m) = media.take() {
        m.task.abort();
    }
    let _ = tx
        .send(VoiceEvent::Ended {
            reason: end.0,
            rejoin: end.1,
        })
        .await;
}

/// Binds a UDP socket, connects it to the voice server and runs IP
/// discovery (3 tries, 2 s each). Returns the socket and our external
/// address.
async fn open_udp(ip: &str, port: u16, ssrc: u32) -> std::io::Result<(UdpSocket, String, u16)> {
    let target: std::net::IpAddr = ip
        .parse()
        .map_err(|_| std::io::Error::other("bad voice server address"))?;
    let bind = if target.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let sock = UdpSocket::bind(bind).await?;
    sock.connect((target, port)).await?;
    let req = rtp::discovery_request(ssrc);
    let mut buf = [0u8; rtp::MAX_PACKET];
    for _ in 0..3 {
        sock.send(&req).await?;
        if let Ok(Ok(n)) = tokio::time::timeout(Duration::from_secs(2), sock.recv(&mut buf)).await
            && let Some((addr, p)) = rtp::parse_discovery(&buf[..n])
        {
            return Ok((sock, addr, p));
        }
    }
    Err(std::io::Error::other("IP discovery timed out"))
}

#[allow(clippy::too_many_arguments)]
fn start_media(
    cfg: &VoiceConfig,
    sock: Arc<UdpSocket>,
    ssrc: u32,
    mode: Mode,
    key: &[u8],
    dave: SharedDave,
    events: mpsc::Sender<VoiceEvent>,
    ws: mpsc::UnboundedSender<ToWs>,
) -> Result<Media, String> {
    let tx_c = Cipher::new(mode, key).ok_or("bad key")?;
    let rx_c = Cipher::new(mode, key).ok_or("bad key")?;
    let mut engine = Engine::new(
        cfg.identity.user_id,
        ssrc,
        tx_c,
        rx_c,
        dave,
        cfg.control.clone(),
    )?;
    let control = cfg.control.clone();
    let factory = cfg.audio.clone();
    let mut generation = control.device_generation();
    // No usable device still lets the user stay in the call (listen-only
    // or silent), like the official client.
    let mut io: Box<dyn AudioIo> = factory(&control.devices()).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "audio devices unavailable");
        Box::new(super::audio::NullIo)
    });
    let (ssrc_tx, mut ssrc_rx) = mpsc::unbounded_channel::<MediaCmd>();
    let task = tokio::spawn(async move {
        let mut clock = tokio::time::interval(Duration::from_millis(20));
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut buf = vec![0u8; rtp::MAX_PACKET];
        let mut keepalive = tokio::time::interval(Duration::from_secs(5));
        let mut ka_counter: u32 = 0;
        loop {
            let out = tokio::select! {
                _ = clock.tick() => {
                    if control.device_generation() != generation {
                        generation = control.device_generation();
                        match factory(&control.devices()) {
                            Ok(new_io) => io = new_io,
                            Err(e) => tracing::warn!(error = %e, "audio device switch failed"),
                        }
                    }
                    engine.tick(io.as_mut())
                }
                r = sock.recv(&mut buf) => match r {
                    Ok(n) => engine.on_packet(&buf[..n]),
                    Err(_) => continue,
                },
                Some(cmd) = ssrc_rx.recv() => {
                    match cmd {
                        MediaCmd::Map(s, u) => engine.map_ssrc(s, u),
                        MediaCmd::Forget(u) => {
                            if let Some(e) = engine.forget_user(u) { let _ = events.try_send(e); }
                        }
                    }
                    continue;
                }
                _ = keepalive.tick() => {
                    // UDP keepalive keeps the NAT mapping open while silent.
                    ka_counter = ka_counter.wrapping_add(1);
                    let mut p = [0u8; 8];
                    p[..4].copy_from_slice(&ka_counter.to_be_bytes());
                    let _ = sock.send(&p).await;
                    continue;
                }
            };
            for p in out.packets {
                let _ = sock.send(&p).await;
            }
            if let Some(flags) = out.speaking {
                let msg = serde_json::json!({"op": 5, "d": {"speaking": flags, "delay": 0, "ssrc": ssrc}});
                let _ = ws.send(ToWs::Text(msg.to_string()));
            }
            for d in out.dave {
                let _ = ws.send(match d {
                    super::dave::DaveOut::Binary(op, mut b) => {
                        b.insert(0, op);
                        ToWs::Binary(b)
                    }
                    super::dave::DaveOut::TransitionReady(t) => ToWs::Text(
                        serde_json::json!({"op": 23, "d": {"transition_id": t}}).to_string(),
                    ),
                    super::dave::DaveOut::InvalidCommitWelcome(t) => ToWs::Text(
                        serde_json::json!({"op": 31, "d": {"transition_id": t}}).to_string(),
                    ),
                });
            }
            for e in out.events {
                // Never block the media clock on the UI.
                let _ = events.try_send(e);
            }
        }
    });
    Ok(Media { task, ssrc_tx })
}
