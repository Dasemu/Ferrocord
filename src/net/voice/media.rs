//! The media engine: microphone -> VAD -> Opus -> DAVE -> RTP/transport
//! encryption -> UDP, and UDP -> decrypt -> DAVE -> jitter buffer -> Opus
//! decoder (one per speaker) -> mixer -> speakers.
//!
//! [`Engine`] is synchronous and socket-free so two engines can be wired
//! together in-process (the loopback test); `conn` drives it from a 20 ms
//! timer and the UDP socket.

use std::collections::HashMap;

use davey::OPUS_SILENCE_PACKET;

use super::audio::AudioIo;
use super::dave::DaveOut;
use super::jitter::{JitterBuffer, Pop};
use super::mixer::{FRAME_LEN, FRAME_SAMPLES, Vad, apply_gain, finish, level_db, mix_into};
use super::rtp::{Cipher, OPUS_PAYLOAD_TYPE, RtpHeader, opus_header};
use super::session::SharedDave;
use super::{VoiceControl, VoiceEvent};

/// Speakers tracked at once (oldest evicted).
const MAX_STREAMS: usize = 64;
/// Ticks (20 ms) a user stays "speaking" after their last voice packet.
const SPEAKING_HOLD: u64 = 12;
/// Silence frames sent after talking stops (docs: five).
const TRAILING_SILENCE: u8 = 5;
/// Largest Opus packet we produce or accept.
const MAX_OPUS: usize = 1275;

/// What one engine step produced.
#[derive(Debug, Default)]
pub struct Out {
    /// UDP datagrams to send.
    pub packets: Vec<Vec<u8>>,
    /// Our speaking flags changed (op 5).
    pub speaking: Option<u8>,
    /// DAVE frames for the voice gateway.
    pub dave: Vec<DaveOut>,
    pub events: Vec<VoiceEvent>,
}

struct Stream {
    jitter: JitterBuffer,
    decoder: opus::Decoder,
    user: Option<u64>,
    last_voice: u64,
    last_seen: u64,
    speaking: bool,
}

pub struct Engine {
    user_id: u64,
    ssrc: u32,
    tx: Cipher,
    rx: Cipher,
    dave: SharedDave,
    control: VoiceControl,
    encoder: opus::Encoder,
    streams: HashMap<u32, Stream>,
    users: HashMap<u32, u64>,
    seq: u16,
    ts: u32,
    vad: Vad,
    talking: bool,
    silence_left: u8,
    tick: u64,
    self_speaking: bool,
    frame: Vec<f32>,
    e2ee: bool,
}

impl Engine {
    pub fn new(
        user_id: u64,
        ssrc: u32,
        tx: Cipher,
        rx: Cipher,
        dave: SharedDave,
        control: VoiceControl,
    ) -> Result<Self, String> {
        let mut encoder =
            opus::Encoder::new(48_000, opus::Channels::Stereo, opus::Application::Voip)
                .map_err(|e| e.to_string())?;
        // Discord's default voice bitrate (64 kb/s) with in-band FEC.
        let _ = encoder.set_bitrate(opus::Bitrate::Bits(64_000));
        let _ = encoder.set_inband_fec(true);
        let _ = encoder.set_packet_loss_perc(5);
        Ok(Self {
            user_id,
            ssrc,
            tx,
            rx,
            dave,
            control,
            encoder,
            streams: HashMap::new(),
            users: HashMap::new(),
            seq: fastrand::u16(..),
            ts: fastrand::u32(..),
            vad: Vad::new(),
            talking: false,
            silence_left: 0,
            tick: 0,
            self_speaking: false,
            frame: vec![0.0; FRAME_LEN],
            e2ee: false,
        })
    }

    /// Op 5 from the server: `ssrc` belongs to `user`.
    pub fn map_ssrc(&mut self, ssrc: u32, user: u64) {
        if ssrc == self.ssrc {
            return;
        }
        if self.users.len() >= MAX_STREAMS * 4 && !self.users.contains_key(&ssrc) {
            return;
        }
        self.users.insert(ssrc, user);
        if let Some(s) = self.streams.get_mut(&ssrc) {
            s.user = Some(user);
        }
    }

    /// A user left: drop their stream.
    pub fn forget_user(&mut self, user: u64) -> Option<VoiceEvent> {
        self.users.retain(|_, u| *u != user);
        let ssrcs: Vec<u32> = self
            .streams
            .iter()
            .filter(|(_, s)| s.user == Some(user))
            .map(|(k, _)| *k)
            .collect();
        let mut was = false;
        for k in ssrcs {
            if let Some(s) = self.streams.remove(&k) {
                was |= s.speaking;
            }
        }
        was.then_some(VoiceEvent::Speaking {
            user,
            speaking: false,
        })
    }

    fn send_opus(&mut self, opus: &[u8], out: &mut Out) {
        let sealed = match self.dave.lock() {
            Ok(mut g) => match g.as_mut() {
                Some(d) => d.encrypt_opus(opus),
                None => Some(opus.to_vec()),
            },
            Err(_) => None,
        };
        let Some(sealed) = sealed else {
            return; // E2EE required but not ready yet: drop like libdave
        };
        let header = opus_header(self.seq, self.ts, self.ssrc);
        if let Some(p) = self.tx.encrypt_packet(&header, &sealed) {
            out.packets.push(p);
        }
        self.seq = self.seq.wrapping_add(1);
        self.ts = self.ts.wrapping_add(FRAME_SAMPLES as u32);
    }

    /// One 20 ms step: capture/encode/send, then decode/mix/play.
    pub fn tick(&mut self, io: &mut dyn AudioIo) -> Out {
        let mut out = Out::default();
        self.tick += 1;
        let muted = self.control.self_mute() || self.control.self_deaf();

        // --- send
        let mut frame = std::mem::take(&mut self.frame);
        let captured = io.capture(&mut frame);
        if captured {
            apply_gain(&mut frame, self.control.input_gain());
        }
        let level = if captured {
            level_db(&frame)
        } else {
            super::mixer::FLOOR_DB
        };
        self.control.set_input_level(level);
        let voice = captured && !muted && self.vad.update(level, self.control.threshold_db());
        if muted {
            self.vad.reset();
        }
        if voice {
            if !self.talking {
                self.talking = true;
                out.speaking = Some(1);
            }
            let mut buf = [0u8; MAX_OPUS];
            match self.encoder.encode_float(&frame, &mut buf) {
                Ok(n) => self.send_opus(&buf[..n.min(MAX_OPUS)], &mut out),
                Err(e) => tracing::debug!(error = %e, "opus encode failed"),
            }
        } else if self.talking {
            self.talking = false;
            self.silence_left = TRAILING_SILENCE;
        }
        if !voice && self.silence_left > 0 {
            self.silence_left -= 1;
            self.send_opus(&OPUS_SILENCE_PACKET, &mut out);
            if self.silence_left == 0 {
                out.speaking = Some(0);
            }
        }
        self.frame = frame;
        if voice != self.self_speaking {
            self.self_speaking = voice;
            out.events.push(VoiceEvent::Speaking {
                user: self.user_id,
                speaking: voice,
            });
        }

        // --- receive
        let deaf = self.control.self_deaf();
        let mut mix = vec![0.0f32; FRAME_LEN];
        let mut any = false;
        let mut pcm = vec![0.0f32; FRAME_LEN];
        for s in self.streams.values_mut() {
            let gain = s.user.map_or(1.0, |u| self.control.gain(u));
            let got = match s.jitter.pop() {
                Pop::Frame(f) => s.decoder.decode_float(&f, &mut pcm, false).ok(),
                Pop::Lost => s.decoder.decode_float(&[], &mut pcm, false).ok(),
                Pop::Empty => None,
            };
            if let Some(n) = got
                && !deaf
                && gain > 0.0
            {
                let n = (n * 2).min(FRAME_LEN);
                mix_into(&mut mix[..n], &pcm[..n], gain);
                any = true;
            }
        }
        if any {
            apply_gain(&mut mix, self.control.output_gain());
            finish(&mut mix);
            io.play(&mix);
        }
        // Speaking indicators from voice activity.
        let tick = self.tick;
        for s in self.streams.values_mut() {
            let now = tick.saturating_sub(s.last_voice) <= SPEAKING_HOLD && s.last_voice > 0;
            if now != s.speaking {
                s.speaking = now;
                if let Some(user) = s.user {
                    out.events.push(VoiceEvent::Speaking {
                        user,
                        speaking: now,
                    });
                }
            }
        }
        // E2EE badge.
        let e2ee = self
            .dave
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|d| d.is_e2ee()))
            .unwrap_or(false);
        if e2ee != self.e2ee {
            self.e2ee = e2ee;
            out.events.push(VoiceEvent::Encryption { e2ee });
        }
        out
    }

    /// A UDP datagram arrived.
    pub fn on_packet(&mut self, p: &[u8]) -> Out {
        let mut out = Out::default();
        if RtpHeader::is_rtcp(p) {
            return out;
        }
        let Some((h, media)) = self.rx.decrypt_packet(p) else {
            return out;
        };
        if h.payload_type != OPUS_PAYLOAD_TYPE || h.ssrc == self.ssrc || media.len() > MAX_OPUS * 2
        {
            return out;
        }
        let user = self.users.get(&h.ssrc).copied();
        // Without the SSRC -> user mapping, DAVE frames cannot be
        // decrypted yet; drop until op 5 arrives.
        let frame = match (user, self.dave.lock()) {
            (Some(u), Ok(mut g)) => match g.as_mut() {
                Some(d) => {
                    let (f, dv) = d.decrypt(u, &media);
                    out.dave = dv;
                    f
                }
                None => Some(media),
            },
            (None, _) => None,
            (_, Err(_)) => None,
        };
        let Some(frame) = frame else {
            return out;
        };
        if !self.streams.contains_key(&h.ssrc) {
            if self.streams.len() >= MAX_STREAMS {
                // Evict the stream heard from least recently.
                if let Some((&old, _)) = self.streams.iter().min_by_key(|(_, s)| s.last_seen) {
                    self.streams.remove(&old);
                }
            }
            let Ok(decoder) = opus::Decoder::new(48_000, opus::Channels::Stereo) else {
                return out;
            };
            self.streams.insert(
                h.ssrc,
                Stream {
                    jitter: JitterBuffer::new(),
                    decoder,
                    user,
                    last_voice: 0,
                    last_seen: self.tick,
                    speaking: false,
                },
            );
        }
        if let Some(s) = self.streams.get_mut(&h.ssrc) {
            s.last_seen = self.tick;
            s.user = user;
            if frame != OPUS_SILENCE_PACKET {
                s.last_voice = self.tick.max(1);
            }
            s.jitter.push(h.sequence, frame);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::super::audio::FakeIo;
    use super::super::rtp::Mode;
    use super::*;
    use std::sync::{Arc, Mutex};

    fn engine(user: u64, ssrc: u32, key: &[u8; 32], control: VoiceControl) -> Engine {
        let dave: SharedDave = Arc::new(Mutex::new(None));
        Engine::new(
            user,
            ssrc,
            Cipher::new(Mode::Aes256GcmRtpSize, key).unwrap(),
            Cipher::new(Mode::Aes256GcmRtpSize, key).unwrap(),
            dave,
            control,
        )
        .unwrap()
    }

    /// Two engines wired back to back (no sockets): a sine wave goes in
    /// one side and comes out of the other with real energy; mute, deafen
    /// and local mute silence it.
    #[test]
    fn loopback_sine_in_energy_out() {
        let key = [3u8; 32];
        let ca = VoiceControl::new();
        let cb = VoiceControl::new();
        let mut a = engine(1, 100, &key, ca.clone());
        let mut b = engine(2, 200, &key, cb.clone());
        a.map_ssrc(200, 2);
        b.map_ssrc(100, 1);
        let (mut mic_a, _) = FakeIo::new(440.0, 0.4);
        let (mut io_b, played_b) = FakeIo::new(0.0, 0.0);
        let mut events = Vec::new();
        let mut speaking_flags = Vec::new();
        for _ in 0..50 {
            let out = a.tick(&mut mic_a);
            speaking_flags.extend(out.speaking);
            for p in &out.packets {
                b.on_packet(p);
            }
            let out_b = b.tick(&mut io_b);
            events.extend(out_b.events);
        }
        assert_eq!(speaking_flags, vec![1]);
        let played = played_b.lock().unwrap().clone();
        assert!(played.len() >= FRAME_LEN * 40, "{}", played.len());
        let tail = &played[played.len() - FRAME_LEN * 10..];
        let lvl = level_db(tail);
        assert!(lvl > -15.0, "received level {lvl}");
        assert!(events.contains(&VoiceEvent::Speaking {
            user: 1,
            speaking: true
        }));

        // Local mute of user 1 on B: B plays nothing new.
        cb.set_user(1, 1.0, true);
        played_b.lock().unwrap().clear();
        for _ in 0..10 {
            for p in a.tick(&mut mic_a).packets {
                b.on_packet(&p);
            }
            b.tick(&mut io_b);
        }
        assert!(played_b.lock().unwrap().is_empty());
        cb.set_user(1, 1.0, false);

        // Self mute on A: five silence frames then speaking 0, then nothing.
        ca.set_self_mute(true);
        let mut sent = 0;
        let mut flags = Vec::new();
        for _ in 0..20 {
            let out = a.tick(&mut mic_a);
            sent += out.packets.len();
            flags.extend(out.speaking);
        }
        assert_eq!(sent, TRAILING_SILENCE as usize);
        assert_eq!(flags, vec![0]);
    }

    #[test]
    fn below_threshold_is_not_sent() {
        let key = [3u8; 32];
        let c = VoiceControl::new();
        c.set_threshold_db(-10.0);
        let mut a = engine(1, 100, &key, c);
        let (mut quiet, _) = FakeIo::new(440.0, 0.01);
        for _ in 0..10 {
            assert!(a.tick(&mut quiet).packets.is_empty());
        }
    }

    #[test]
    fn input_volume_scales_the_microphone() {
        // A quiet microphone below the threshold is sent once "Input
        // Volume" boosts it, and the meter shows the boosted level.
        let key = [3u8; 32];
        let c = VoiceControl::new();
        c.set_threshold_db(-30.0);
        c.set_volumes(100.0, 100.0);
        let mut a = engine(1, 100, &key, c.clone());
        let (mut quiet, _) = FakeIo::new(440.0, 0.02);
        assert!(a.tick(&mut quiet).packets.is_empty());
        let unboosted = c.input_level();
        c.set_volumes(200.0, 100.0);
        assert!((c.input_gain() - 2.0).abs() < 1e-6);
        let _ = a.tick(&mut quiet);
        assert!((c.input_level() - unboosted - 6.02).abs() < 0.2);
        c.set_volumes(0.0, 0.0);
        let _ = a.tick(&mut quiet);
        assert_eq!(c.input_level(), super::super::mixer::FLOOR_DB);
        assert_eq!(c.output_gain(), 0.0);
    }

    #[test]
    fn hostile_packets_are_ignored() {
        let key = [3u8; 32];
        let mut b = engine(2, 200, &key, VoiceControl::new());
        b.map_ssrc(100, 1);
        let mut a = engine(1, 100, &key, VoiceControl::new());
        // Valid transport encryption but garbage Opus: decoding fails
        // quietly; huge counts of SSRCs stay bounded.
        for i in 0..500u32 {
            let header = opus_header(i as u16, i, 1000 + i);
            b.map_ssrc(1000 + i, u64::from(i));
            let p = a.tx.encrypt_packet(&header, &[0xff; 40]).unwrap();
            b.on_packet(&p);
            b.on_packet(&p[..p.len() / 2]);
        }
        assert!(b.streams.len() <= MAX_STREAMS);
        let (mut io, _) = FakeIo::new(0.0, 0.0);
        let _ = b.tick(&mut io);
    }
}
