//! UDP media packets: IP discovery, RTP headers and the transport
//! encryption modes Discord requires (`aead_aes256_gcm_rtpsize`,
//! `aead_xchacha20_poly1305_rtpsize`).
//!
//! Everything here is pure and bounds-checked: packets come from the
//! network and may be hostile, so parsing returns `None` instead of
//! panicking.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce as GcmNonce};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};

/// RTP payload type Discord uses for Opus.
pub const OPUS_PAYLOAD_TYPE: u8 = 0x78;
/// Fixed RTP header size (no CSRCs, no extension).
pub const RTP_HEADER_LEN: usize = 12;
/// Size of the IP discovery request and response.
pub const DISCOVERY_LEN: usize = 74;
/// Size of the appended truncated nonce.
const NONCE_SUFFIX: usize = 4;
/// AEAD tag size for both modes.
const TAG_LEN: usize = 16;
/// Largest UDP datagram we accept or build.
pub const MAX_PACKET: usize = 1500;

/// Transport encryption mode negotiated with Select Protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Aes256GcmRtpSize,
    XChaCha20Poly1305RtpSize,
}

impl Mode {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Aes256GcmRtpSize => "aead_aes256_gcm_rtpsize",
            Self::XChaCha20Poly1305RtpSize => "aead_xchacha20_poly1305_rtpsize",
        }
    }

    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "aead_aes256_gcm_rtpsize" => Some(Self::Aes256GcmRtpSize),
            "aead_xchacha20_poly1305_rtpsize" => Some(Self::XChaCha20Poly1305RtpSize),
            _ => None,
        }
    }

    /// Picks the mode from the server's list: AES-GCM when offered
    /// (preferred), XChaCha20 otherwise (always offered).
    pub fn choose<S: AsRef<str>>(offered: &[S]) -> Option<Self> {
        let has = |m: Mode| offered.iter().any(|o| o.as_ref() == m.wire());
        if has(Self::Aes256GcmRtpSize) {
            Some(Self::Aes256GcmRtpSize)
        } else if has(Self::XChaCha20Poly1305RtpSize) {
            Some(Self::XChaCha20Poly1305RtpSize)
        } else {
            None
        }
    }
}

/// IP discovery request: type 1, length 70, SSRC, 64 zero bytes, port 0.
pub fn discovery_request(ssrc: u32) -> [u8; DISCOVERY_LEN] {
    let mut p = [0u8; DISCOVERY_LEN];
    p[0..2].copy_from_slice(&1u16.to_be_bytes());
    p[2..4].copy_from_slice(&70u16.to_be_bytes());
    p[4..8].copy_from_slice(&ssrc.to_be_bytes());
    p
}

/// Parses an IP discovery response into `(address, port)`.
pub fn parse_discovery(p: &[u8]) -> Option<(String, u16)> {
    if p.len() < DISCOVERY_LEN || p[0..2] != 2u16.to_be_bytes() {
        return None;
    }
    let addr = &p[8..72];
    let end = addr.iter().position(|b| *b == 0).unwrap_or(addr.len());
    let addr = std::str::from_utf8(&addr[..end]).ok()?.trim();
    // Only accept something that is actually an IP address.
    let ip: std::net::IpAddr = addr.parse().ok()?;
    let port = u16::from_be_bytes([p[72], p[73]]);
    Some((ip.to_string(), port))
}

/// Builds a discovery response (used by tests and the in-process loopback).
pub fn discovery_response(ssrc: u32, ip: &str, port: u16) -> [u8; DISCOVERY_LEN] {
    let mut p = [0u8; DISCOVERY_LEN];
    p[0..2].copy_from_slice(&2u16.to_be_bytes());
    p[2..4].copy_from_slice(&70u16.to_be_bytes());
    p[4..8].copy_from_slice(&ssrc.to_be_bytes());
    let b = ip.as_bytes();
    let n = b.len().min(63);
    p[8..8 + n].copy_from_slice(&b[..n]);
    p[72..74].copy_from_slice(&port.to_be_bytes());
    p
}

/// The parts of an RTP header we use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtpHeader {
    pub payload_type: u8,
    pub sequence: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    /// Bytes of the unencrypted header in rtpsize modes: fixed header,
    /// CSRCs and (when present) the 4-byte extension preamble.
    pub header_len: usize,
    /// Length in bytes of the extension body, which is encrypted together
    /// with the payload and must be skipped after decryption.
    pub extension_len: usize,
}

impl RtpHeader {
    pub fn parse(p: &[u8]) -> Option<Self> {
        if p.len() < RTP_HEADER_LEN || p[0] >> 6 != 2 {
            return None;
        }
        let csrc = usize::from(p[0] & 0x0f);
        let has_ext = p[0] & 0x10 != 0;
        let mut header_len = RTP_HEADER_LEN + csrc * 4;
        let mut extension_len = 0;
        if has_ext {
            let pre = p.get(header_len..header_len + 4)?;
            extension_len = usize::from(u16::from_be_bytes([pre[2], pre[3]])) * 4;
            header_len += 4;
        }
        if p.len() < header_len {
            return None;
        }
        Some(Self {
            payload_type: p[1] & 0x7f,
            sequence: u16::from_be_bytes([p[2], p[3]]),
            timestamp: u32::from_be_bytes([p[4], p[5], p[6], p[7]]),
            ssrc: u32::from_be_bytes([p[8], p[9], p[10], p[11]]),
            header_len,
            extension_len,
        })
    }

    /// RTCP shares the socket: payload types 72-76 (with the marker bit
    /// folded in, 200-204) are RTCP and are ignored.
    pub fn is_rtcp(p: &[u8]) -> bool {
        p.len() >= 2 && (200..=204).contains(&p[1])
    }
}

/// Fixed 12-byte RTP header for an outgoing Opus packet.
pub fn opus_header(sequence: u16, timestamp: u32, ssrc: u32) -> [u8; RTP_HEADER_LEN] {
    let mut h = [0u8; RTP_HEADER_LEN];
    h[0] = 0x80;
    h[1] = OPUS_PAYLOAD_TYPE;
    h[2..4].copy_from_slice(&sequence.to_be_bytes());
    h[4..8].copy_from_slice(&timestamp.to_be_bytes());
    h[8..12].copy_from_slice(&ssrc.to_be_bytes());
    h
}

/// Transport cipher with the session key from Session Description.
pub struct Cipher {
    mode: Mode,
    aes: Option<Aes256Gcm>,
    xchacha: Option<XChaCha20Poly1305>,
    /// Outgoing 32-bit nonce counter.
    nonce: u32,
}

impl std::fmt::Debug for Cipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Cipher({:?})", self.mode)
    }
}

impl Cipher {
    /// `key` must be the 32-byte `secret_key`.
    pub fn new(mode: Mode, key: &[u8]) -> Option<Self> {
        if key.len() != 32 {
            return None;
        }
        let (aes, xchacha) = match mode {
            Mode::Aes256GcmRtpSize => (Some(Aes256Gcm::new_from_slice(key).ok()?), None),
            Mode::XChaCha20Poly1305RtpSize => {
                (None, Some(XChaCha20Poly1305::new_from_slice(key).ok()?))
            }
        };
        Some(Self {
            mode,
            aes,
            xchacha,
            nonce: 0,
        })
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    fn seal(&self, nonce: u32, aad: &[u8], msg: &[u8]) -> Option<Vec<u8>> {
        let payload = Payload { msg, aad };
        match (&self.aes, &self.xchacha) {
            (Some(c), _) => {
                let mut n = [0u8; 12];
                n[..4].copy_from_slice(&nonce.to_be_bytes());
                c.encrypt(GcmNonce::from_slice(&n), payload).ok()
            }
            (_, Some(c)) => {
                let mut n = [0u8; 24];
                n[..4].copy_from_slice(&nonce.to_be_bytes());
                c.encrypt(XNonce::from_slice(&n), payload).ok()
            }
            _ => None,
        }
    }

    fn open(&self, nonce: [u8; 4], aad: &[u8], msg: &[u8]) -> Option<Vec<u8>> {
        let payload = Payload { msg, aad };
        match (&self.aes, &self.xchacha) {
            (Some(c), _) => {
                let mut n = [0u8; 12];
                n[..4].copy_from_slice(&nonce);
                c.decrypt(GcmNonce::from_slice(&n), payload).ok()
            }
            (_, Some(c)) => {
                let mut n = [0u8; 24];
                n[..4].copy_from_slice(&nonce);
                c.decrypt(XNonce::from_slice(&n), payload).ok()
            }
            _ => None,
        }
    }

    /// Builds a complete UDP packet: header (AAD) | ciphertext+tag | nonce.
    pub fn encrypt_packet(&mut self, header: &[u8], payload: &[u8]) -> Option<Vec<u8>> {
        if header.len() + payload.len() + TAG_LEN + NONCE_SUFFIX > MAX_PACKET {
            return None;
        }
        self.nonce = self.nonce.wrapping_add(1);
        let nonce = self.nonce;
        let sealed = self.seal(nonce, header, payload)?;
        let mut out = Vec::with_capacity(header.len() + sealed.len() + NONCE_SUFFIX);
        out.extend_from_slice(header);
        out.extend_from_slice(&sealed);
        out.extend_from_slice(&nonce.to_be_bytes());
        Some(out)
    }

    /// Decrypts a received RTP packet; returns the header and the media
    /// payload (with the encrypted extension body already skipped).
    pub fn decrypt_packet(&self, packet: &[u8]) -> Option<(RtpHeader, Vec<u8>)> {
        if packet.len() > MAX_PACKET {
            return None;
        }
        let h = RtpHeader::parse(packet)?;
        let min = h.header_len + TAG_LEN + NONCE_SUFFIX;
        if packet.len() < min {
            return None;
        }
        let (body, nonce) = packet.split_at(packet.len() - NONCE_SUFFIX);
        let nonce: [u8; 4] = nonce.try_into().ok()?;
        let (aad, sealed) = body.split_at(h.header_len);
        let plain = self.open(nonce, aad, sealed)?;
        let media = plain.get(h.extension_len..)?.to_vec();
        Some((h, media))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_round_trip() {
        let req = discovery_request(0xdead_beef);
        assert_eq!(&req[0..4], &[0, 1, 0, 70]);
        assert_eq!(&req[4..8], &0xdead_beefu32.to_be_bytes());
        let resp = discovery_response(0xdead_beef, "203.0.113.7", 50_123);
        assert_eq!(
            parse_discovery(&resp),
            Some(("203.0.113.7".to_owned(), 50_123))
        );
        // A request is not a response; garbage is rejected.
        assert_eq!(parse_discovery(&req), None);
        assert_eq!(parse_discovery(&resp[..40]), None);
        let mut bad = resp;
        bad[8..20].copy_from_slice(b"not-an-ip!!!");
        assert_eq!(parse_discovery(&bad), None);
        // An address without a NUL terminator still parses safely.
        let mut full = discovery_response(1, "", 1);
        full[8..72].fill(b'9');
        assert_eq!(parse_discovery(&full), None);
    }

    #[test]
    fn mode_choice_prefers_aes() {
        assert_eq!(
            Mode::choose(&["aead_xchacha20_poly1305_rtpsize", "aead_aes256_gcm_rtpsize"]),
            Some(Mode::Aes256GcmRtpSize)
        );
        assert_eq!(
            Mode::choose(&["xsalsa20_poly1305", "aead_xchacha20_poly1305_rtpsize"]),
            Some(Mode::XChaCha20Poly1305RtpSize)
        );
        assert_eq!(Mode::choose(&["xsalsa20_poly1305"]), None);
    }

    #[test]
    fn transport_round_trip_both_modes() {
        let key = [7u8; 32];
        for mode in [Mode::Aes256GcmRtpSize, Mode::XChaCha20Poly1305RtpSize] {
            let mut tx = Cipher::new(mode, &key).unwrap();
            let rx = Cipher::new(mode, &key).unwrap();
            let header = opus_header(10, 960, 42);
            let pkt = tx.encrypt_packet(&header, b"opus-frame").unwrap();
            // header | ciphertext (10) + tag (16) | nonce (4)
            assert_eq!(pkt.len(), 12 + 10 + 16 + 4);
            assert_eq!(&pkt[pkt.len() - 4..], &1u32.to_be_bytes());
            let (h, media) = rx.decrypt_packet(&pkt).unwrap();
            assert_eq!((h.sequence, h.timestamp, h.ssrc), (10, 960, 42));
            assert_eq!(media, b"opus-frame");
            // The header is authenticated: flipping a header bit fails.
            let mut tampered = pkt.clone();
            tampered[3] ^= 1;
            assert!(rx.decrypt_packet(&tampered).is_none());
            // Wrong key fails.
            let other = Cipher::new(mode, &[8u8; 32]).unwrap();
            assert!(other.decrypt_packet(&pkt).is_none());
        }
    }

    /// Known-answer vector: AES-256-GCM with an all-zero key, nonce
    /// counter 1 and the RTP header as AAD. Checked against an independent
    /// implementation (Python `cryptography`).
    #[test]
    fn aes_gcm_known_vector() {
        let mut tx = Cipher::new(Mode::Aes256GcmRtpSize, &[0u8; 32]).unwrap();
        let header = opus_header(1, 2, 3);
        let pkt = tx.encrypt_packet(&header, &[0xf8, 0xff, 0xfe]).unwrap();
        let hex: String = pkt[12..].iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, AES_VECTOR);
    }
    const AES_VECTOR: &str = include_str!("testdata/aes_gcm_vector.txt");

    #[test]
    fn rtpsize_header_with_csrc_and_extension() {
        let key = [1u8; 32];
        let mut tx = Cipher::new(Mode::XChaCha20Poly1305RtpSize, &key).unwrap();
        let rx = Cipher::new(Mode::XChaCha20Poly1305RtpSize, &key).unwrap();
        // V=2, X=1, CC=1; one CSRC; extension preamble 0xBEDE, length 1.
        let mut header = vec![0x91, 0x78, 0, 5, 0, 0, 0, 9, 0, 0, 0, 7];
        header.extend_from_slice(&[0, 0, 0, 99]);
        header.extend_from_slice(&[0xbe, 0xde, 0, 1]);
        // The extension body (4 bytes) is encrypted with the payload.
        let mut body = vec![0x10, 0xaa, 0, 0];
        body.extend_from_slice(b"payload");
        let pkt = tx.encrypt_packet(&header, &body).unwrap();
        let (h, media) = rx.decrypt_packet(&pkt).unwrap();
        assert_eq!(h.header_len, 20);
        assert_eq!(h.extension_len, 4);
        assert_eq!(h.ssrc, 7);
        assert_eq!(media, b"payload");
    }

    #[test]
    fn hostile_packets_never_panic() {
        let rx = Cipher::new(Mode::Aes256GcmRtpSize, &[0u8; 32]).unwrap();
        let mut seed = 0x1234_5678u32;
        for len in 0..200usize {
            let p: Vec<u8> = (0..len)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    seed as u8
                })
                .collect();
            let _ = RtpHeader::parse(&p);
            let _ = rx.decrypt_packet(&p);
            let _ = parse_discovery(&p);
            // Force the RTP version bits and huge CSRC/extension lengths.
            let mut q = p.clone();
            if !q.is_empty() {
                q[0] = 0x9f;
            }
            if q.len() > 75 {
                q[74] = 0xff;
                q[75] = 0xff;
            }
            let _ = RtpHeader::parse(&q);
            let _ = rx.decrypt_packet(&q);
        }
        assert!(rx.decrypt_packet(&vec![0x80; MAX_PACKET + 1]).is_none());
        assert!(Cipher::new(Mode::Aes256GcmRtpSize, &[0u8; 31]).is_none());
    }

    #[test]
    fn rtcp_is_detected() {
        assert!(RtpHeader::is_rtcp(&[0x81, 200, 0, 6]));
        assert!(!RtpHeader::is_rtcp(&opus_header(1, 1, 1)));
    }
}
