//! DAVE (Discord Audio & Video End-to-End Encryption) on top of the
//! `davey` crate, which implements the MLS group and frame cryptors.
//!
//! This wrapper follows the opcode handling of the reference clients
//! (libdave, discord.js `DAVESession`): it turns voice gateway opcodes
//! 21-31 into calls on `davey::DaveSession` and returns the frames to send
//! back. MLS messages come from the network, so every `davey` call that
//! parses them is wrapped in `catch_unwind`: a library panic on hostile
//! input resets the group instead of taking the app down.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU16;
use std::panic::{AssertUnwindSafe, catch_unwind};

use davey::{DaveSession, MediaType, OPUS_SILENCE_PACKET, ProposalsOperationType};

/// Highest DAVE protocol version we implement (sent in Identify).
pub const MAX_PROTOCOL_VERSION: u16 = davey::DAVE_PROTOCOL_VERSION;
/// Consecutive decrypt failures tolerated before asking to be re-added.
const FAILURE_TOLERANCE: u32 = 36;
/// Seconds old keys stay usable after an upgrade from passthrough.
const TRANSITION_EXPIRY: u32 = 10;
/// Upper bound for a binary DAVE message (MLS commits/welcomes for a full
/// call are a few KiB; anything huge is hostile).
pub const MAX_BINARY: usize = 256 * 1024;

/// A frame for the voice gateway produced by DAVE handling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaveOut {
    /// Binary message: opcode then payload (client frames carry no seq).
    Binary(u8, Vec<u8>),
    /// JSON op 23 (transition ready).
    TransitionReady(u16),
    /// JSON op 31 (invalid commit / welcome).
    InvalidCommitWelcome(u16),
}

/// DAVE state for one voice connection.
pub struct Dave {
    user_id: u64,
    channel_id: u64,
    protocol_version: u16,
    session: Option<DaveSession>,
    downgraded: bool,
    reinitializing: bool,
    pending_transitions: HashMap<u16, u16>,
    last_transition: Option<u16>,
    consecutive_failures: u32,
    /// Users known to be in the call (ops 11 / 13), used to validate MLS
    /// proposals and welcomes.
    pub connected: HashSet<u64>,
}

impl std::fmt::Debug for Dave {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dave")
            .field("protocol_version", &self.protocol_version)
            .field("ready", &self.is_e2ee())
            .finish_non_exhaustive()
    }
}

fn guarded<T>(f: impl FnOnce() -> T) -> Option<T> {
    catch_unwind(AssertUnwindSafe(f)).ok()
}

impl Dave {
    /// Creates the state from Session Description's `dave_protocol_version`.
    /// Returns the key package to send when E2EE is active.
    pub fn new(user_id: u64, channel_id: u64, protocol_version: u16) -> (Self, Vec<DaveOut>) {
        let mut d = Self {
            user_id,
            channel_id,
            protocol_version,
            session: None,
            downgraded: protocol_version == 0,
            reinitializing: false,
            pending_transitions: HashMap::new(),
            last_transition: None,
            consecutive_failures: 0,
            connected: HashSet::new(),
        };
        let out = d.reinit();
        (d, out)
    }

    pub fn protocol_version(&self) -> u16 {
        self.protocol_version
    }

    /// True when media is end-to-end encrypted right now.
    pub fn is_e2ee(&self) -> bool {
        self.available() && self.session.as_ref().is_some_and(|s| s.is_ready())
    }

    /// Voice privacy code (shown like the official "end-to-end encrypted" badge).
    pub fn privacy_code(&self) -> Option<String> {
        if !self.available() {
            return None;
        }
        self.session
            .as_ref()
            .and_then(|s| s.voice_privacy_code())
            .filter(|c| !c.is_empty())
            .map(str::to_owned)
    }

    fn available(&self) -> bool {
        self.protocol_version != 0 && !self.downgraded
    }

    fn reinit(&mut self) -> Vec<DaveOut> {
        self.pending_transitions.clear();
        let Some(version) = NonZeroU16::new(self.protocol_version) else {
            if let Some(s) = self.session.as_mut() {
                let _ = guarded(|| s.reset());
                s.set_passthrough_mode(true, None);
            }
            return Vec::new();
        };
        let (user, channel) = (self.user_id, self.channel_id);
        match self.session.as_mut() {
            Some(s) => {
                if guarded(|| s.reinit(version, user, channel, None))
                    .and_then(Result::ok)
                    .is_none()
                {
                    tracing::warn!("DAVE reinit failed");
                }
            }
            None => match DaveSession::new(version, user, channel, None) {
                Ok(s) => self.session = Some(s),
                Err(e) => {
                    tracing::warn!(error = %e, "DAVE session init failed");
                    return Vec::new();
                }
            },
        }
        let Some(s) = self.session.as_mut() else {
            return Vec::new();
        };
        if self.downgraded {
            s.set_passthrough_mode(true, None);
        }
        match guarded(|| s.create_key_package()) {
            Some(Ok(kp)) => vec![DaveOut::Binary(26, kp)],
            _ => {
                tracing::warn!("DAVE key package failed");
                Vec::new()
            }
        }
    }

    /// Op 25: the voice gateway's external sender.
    pub fn on_external_sender(&mut self, payload: &[u8]) {
        if let Some(s) = self.session.as_mut() {
            match guarded(|| s.set_external_sender(payload)) {
                Some(Ok(())) => {}
                _ => tracing::warn!("DAVE external sender rejected"),
            }
        }
    }

    /// Op 21: prepare a transition (downgrade, or re-init when id is 0).
    pub fn on_prepare_transition(
        &mut self,
        transition_id: u16,
        protocol_version: u16,
    ) -> Vec<DaveOut> {
        self.pending_transitions
            .insert(transition_id, protocol_version);
        if let Some(s) = self.session.as_mut()
            && guarded(|| s.prepare_transition(transition_id, protocol_version))
                .and_then(Result::ok)
                .is_none()
        {
            tracing::debug!(transition_id, "DAVE prepare transition failed");
            return Vec::new();
        }
        if protocol_version == 0
            && let Some(s) = self.session.as_mut()
        {
            s.set_passthrough_mode(true, None);
        }
        if transition_id == 0 {
            self.on_execute_transition(0);
            Vec::new()
        } else {
            vec![DaveOut::TransitionReady(transition_id)]
        }
    }

    /// Op 22: execute a prepared transition.
    pub fn on_execute_transition(&mut self, transition_id: u16) -> bool {
        let Some(&version) = self.pending_transitions.get(&transition_id) else {
            return false;
        };
        if transition_id != 0 {
            let ok = self
                .session
                .as_mut()
                .and_then(|s| guarded(|| s.execute_transition(transition_id)))
                .and_then(Result::ok)
                .unwrap_or(false);
            if !ok {
                self.pending_transitions.remove(&transition_id);
                return false;
            }
        }
        let old = self.protocol_version;
        self.protocol_version = version;
        if old != version && version == 0 {
            self.downgraded = true;
        } else if transition_id > 0 && self.downgraded {
            self.downgraded = false;
            if let Some(s) = self.session.as_mut() {
                s.set_passthrough_mode(false, Some(TRANSITION_EXPIRY));
            }
        }
        self.reinitializing = false;
        self.last_transition = Some(transition_id);
        self.pending_transitions.remove(&transition_id);
        true
    }

    /// Op 24: prepare an epoch; epoch 1 means a new group.
    pub fn on_prepare_epoch(&mut self, epoch: u64, protocol_version: u16) -> Vec<DaveOut> {
        if epoch == 1 {
            self.protocol_version = protocol_version;
            return self.reinit();
        }
        Vec::new()
    }

    fn expected_users(&self) -> Vec<u64> {
        let mut v: Vec<u64> = self.connected.iter().copied().collect();
        v.push(self.user_id);
        v
    }

    /// Op 27: proposals to append or revoke; may produce op 28.
    pub fn on_proposals(&mut self, payload: &[u8]) -> Vec<DaveOut> {
        let Some((&op, rest)) = payload.split_first() else {
            return Vec::new();
        };
        let op = match op {
            0 => ProposalsOperationType::APPEND,
            1 => ProposalsOperationType::REVOKE,
            _ => return Vec::new(),
        };
        let expected = self.expected_users();
        let Some(s) = self.session.as_mut() else {
            return Vec::new();
        };
        match guarded(|| s.process_proposals(op, rest, Some(&expected))) {
            Some(Ok(Some(cw))) => {
                let mut out = cw.commit;
                if let Some(w) = cw.welcome {
                    out.extend_from_slice(&w);
                }
                vec![DaveOut::Binary(28, out)]
            }
            Some(Ok(None)) => Vec::new(),
            _ => {
                tracing::debug!("DAVE proposals rejected");
                Vec::new()
            }
        }
    }

    fn split_transition(payload: &[u8]) -> Option<(u16, &[u8])> {
        let (id, rest) = payload.split_at_checked(2)?;
        Some((u16::from_be_bytes([id[0], id[1]]), rest))
    }

    /// Op 29: the winning commit for the next epoch.
    pub fn on_commit(&mut self, payload: &[u8]) -> Vec<DaveOut> {
        let Some((tid, commit)) = Self::split_transition(payload) else {
            return Vec::new();
        };
        let Some(s) = self.session.as_mut() else {
            return Vec::new();
        };
        let version = s.protocol_version().get();
        let result = guarded(|| {
            s.process_commit(commit)
                .map_err(|e| e.to_string())
                .and_then(|applied| {
                    if applied {
                        s.prepare_transition(tid, version)
                            .map(|()| true)
                            .map_err(|e| e.to_string())
                    } else {
                        Ok(false)
                    }
                })
        });
        match result {
            Some(Ok(true)) => self.after_group_change(tid),
            Some(Ok(false)) => Vec::new(),
            _ => self.recover(tid),
        }
    }

    /// Op 30: we were welcomed into the group.
    pub fn on_welcome(&mut self, payload: &[u8]) -> Vec<DaveOut> {
        let Some((tid, welcome)) = Self::split_transition(payload) else {
            return Vec::new();
        };
        let expected = self.expected_users();
        let Some(s) = self.session.as_mut() else {
            return Vec::new();
        };
        let version = s.protocol_version().get();
        let result = guarded(|| {
            s.process_welcome(welcome, Some(&expected))
                .map_err(|e| e.to_string())
                .and_then(|()| {
                    s.prepare_transition(tid, version)
                        .map_err(|e| e.to_string())
                })
        });
        match result {
            Some(Ok(())) => self.after_group_change(tid),
            _ => self.recover(tid),
        }
    }

    fn after_group_change(&mut self, tid: u16) -> Vec<DaveOut> {
        if tid == 0 {
            self.reinitializing = false;
            self.last_transition = Some(0);
            Vec::new()
        } else {
            self.pending_transitions.insert(tid, self.protocol_version);
            vec![DaveOut::TransitionReady(tid)]
        }
    }

    /// Reports an unprocessable commit/welcome (op 31) and rejoins with a
    /// fresh key package (op 26).
    fn recover(&mut self, tid: u16) -> Vec<DaveOut> {
        tracing::debug!(
            transition_id = tid,
            "DAVE invalid commit/welcome; rejoining"
        );
        self.reinitializing = true;
        self.consecutive_failures = 0;
        let mut out = vec![DaveOut::InvalidCommitWelcome(tid)];
        out.extend(self.reinit());
        out
    }

    /// End-to-end encrypts an outgoing Opus frame. `None` = drop the frame
    /// (E2EE is required but keys are not ready yet).
    pub fn encrypt_opus(&mut self, frame: &[u8]) -> Option<Vec<u8>> {
        let ready = self.session.as_ref().is_some_and(|s| s.is_ready());
        if !self.available() || !ready || frame == OPUS_SILENCE_PACKET {
            return Some(frame.to_vec());
        }
        let s = self.session.as_mut()?;
        if !s.can_encrypt() {
            return None;
        }
        guarded(|| s.encrypt_opus(frame).ok().map(|c| c.into_owned())).flatten()
    }

    /// Decrypts a received frame from `user`. `Ok(None)` = drop silently.
    pub fn decrypt(&mut self, user: u64, frame: &[u8]) -> (Option<Vec<u8>>, Vec<DaveOut>) {
        if frame == OPUS_SILENCE_PACKET {
            return (Some(frame.to_vec()), Vec::new());
        }
        let available = self.available();
        let Some(s) = self.session.as_mut() else {
            return (Some(frame.to_vec()), Vec::new());
        };
        let can = s.is_ready() && (available || s.can_passthrough(user));
        if !can {
            let pass = !available && !is_dave_frame(frame);
            return (pass.then(|| frame.to_vec()), Vec::new());
        }
        match guarded(|| s.decrypt(user, MediaType::AUDIO, frame)) {
            Some(Ok(plain)) => {
                self.consecutive_failures = 0;
                (Some(plain), Vec::new())
            }
            _ => {
                let mut out = Vec::new();
                if available && !self.reinitializing && self.pending_transitions.is_empty() {
                    self.consecutive_failures += 1;
                    if self.consecutive_failures > FAILURE_TOLERANCE
                        && let Some(t) = self.last_transition
                    {
                        out = self.recover(t);
                    }
                }
                (None, out)
            }
        }
    }
}

/// Protocol frame check: DAVE frames end with the 0xFAFA magic marker.
pub fn is_dave_frame(frame: &[u8]) -> bool {
    frame.len() > 2 && frame.ends_with(&[0xfa, 0xfa])
}

#[cfg(test)]
mod tests {
    //! Two members and a fake voice gateway (the MLS external sender),
    //! driven through the same opcode handlers the real connection uses.

    use super::*;
    use openmls::prelude::tls_codec::{Deserialize as _, Serialize as _};
    use openmls::prelude::*;
    use openmls_basic_credential::SignatureKeyPair;
    use openmls_rust_crypto::OpenMlsRustCrypto;

    const CHANNEL: u64 = 1_234_567_890;
    const A: u64 = 1_111_111_111_111;
    const B: u64 = 2_222_222_222_222;

    struct FakeGateway {
        provider: OpenMlsRustCrypto,
        signer: SignatureKeyPair,
        sender: ExternalSender,
    }

    impl FakeGateway {
        fn new() -> Self {
            let cs = Ciphersuite::MLS_128_DHKEMP256_AES128GCM_SHA256_P256;
            let signer = SignatureKeyPair::new(cs.signature_algorithm()).unwrap();
            let sender = ExternalSender::new(
                signer.public().into(),
                BasicCredential::new(vec![0, 1, 1, 0]).into(),
            );
            Self {
                provider: OpenMlsRustCrypto::default(),
                signer,
                sender,
            }
        }

        fn sender_package(&self) -> Vec<u8> {
            self.sender.tls_serialize_detached().unwrap()
        }

        /// Op 27 payload (operation byte + proposals vector) adding `kp`.
        fn propose_add(&self, epoch: u64, kp: &[u8]) -> Vec<u8> {
            let kp = KeyPackageIn::tls_deserialize_exact(kp)
                .unwrap()
                .validate(self.provider.crypto(), ProtocolVersion::Mls10)
                .unwrap();
            let msg = ExternalProposal::new_add::<OpenMlsRustCrypto>(
                kp,
                GroupId::from_slice(&CHANNEL.to_be_bytes()),
                GroupEpoch::from(epoch),
                &self.signer,
                SenderExtensionIndex::new(0),
            )
            .unwrap();
            let mut out = vec![0u8];
            out.extend(
                openmls::prelude::tls_codec::VLBytes::new(msg.tls_serialize_detached().unwrap())
                    .tls_serialize_detached()
                    .unwrap(),
            );
            out
        }
    }

    fn key_package(out: &[DaveOut]) -> Vec<u8> {
        out.iter()
            .find_map(|o| match o {
                DaveOut::Binary(26, kp) => Some(kp.clone()),
                _ => None,
            })
            .expect("key package")
    }

    /// Splits an op 28 payload into (commit, welcome) by parsing the commit.
    fn split_commit_welcome(p: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut rest = p;
        let _ = MlsMessageIn::tls_deserialize(&mut rest).unwrap();
        let commit_len = p.len() - rest.len();
        (p[..commit_len].to_vec(), rest.to_vec())
    }

    fn with_tid(tid: u16, body: &[u8]) -> Vec<u8> {
        let mut v = tid.to_be_bytes().to_vec();
        v.extend_from_slice(body);
        v
    }

    /// Full handshake like the voice gateway runs it, then encrypted
    /// Opus frames both ways.
    #[test]
    fn two_member_loopback_session() {
        let gw = FakeGateway::new();
        // Session Description (dave_protocol_version 1): both create a
        // session and send a key package (op 26).
        let (mut a, out_a) = Dave::new(A, CHANNEL, 1);
        let (mut b, out_b) = Dave::new(B, CHANNEL, 1);
        let _ = key_package(&out_a);
        let kp_b = key_package(&out_b);
        a.connected.insert(B);
        b.connected.insert(A);
        // Op 25 to both: each creates its pending local group.
        a.on_external_sender(&gw.sender_package());
        b.on_external_sender(&gw.sender_package());

        // Gateway proposes adding B; A commits and welcomes.
        let out = a.on_proposals(&gw.propose_add(0, &kp_b));
        let commit_welcome = match out.as_slice() {
            [DaveOut::Binary(28, p)] => p.clone(),
            other => panic!("expected commit, got {other:?}"),
        };
        let (commit, welcome) = split_commit_welcome(&commit_welcome);
        assert!(!welcome.is_empty());

        // Gateway announces the commit to A and the welcome to B (tid 5).
        assert_eq!(
            a.on_commit(&with_tid(5, &commit)),
            vec![DaveOut::TransitionReady(5)]
        );
        assert_eq!(
            b.on_welcome(&with_tid(5, &welcome)),
            vec![DaveOut::TransitionReady(5)]
        );
        assert!(a.on_execute_transition(5));
        assert!(b.on_execute_transition(5));
        assert!(a.is_e2ee() && b.is_e2ee());
        assert_eq!(a.privacy_code(), b.privacy_code());
        assert!(a.privacy_code().is_some());

        let opus = b"\x78not-really-opus-but-bytes".to_vec();
        let sealed = a.encrypt_opus(&opus).unwrap();
        assert_ne!(sealed, opus);
        assert!(is_dave_frame(&sealed));
        let (plain, _) = b.decrypt(A, &sealed);
        assert_eq!(plain.unwrap(), opus);
        let back = b.encrypt_opus(b"reply").unwrap();
        assert_eq!(a.decrypt(B, &back).0.unwrap(), b"reply");
        // Silence frames pass through untouched.
        assert_eq!(
            a.encrypt_opus(&OPUS_SILENCE_PACKET).unwrap(),
            OPUS_SILENCE_PACKET
        );
        // A tampered frame is dropped, not passed through.
        let mut bad = sealed.clone();
        bad[1] ^= 0xff;
        assert_eq!(b.decrypt(A, &bad).0, None);
    }

    #[test]
    fn version_zero_is_passthrough() {
        let (mut d, out) = Dave::new(A, CHANNEL, 0);
        assert!(out.is_empty());
        assert!(!d.is_e2ee());
        assert_eq!(d.encrypt_opus(b"abc").unwrap(), b"abc");
        assert_eq!(d.decrypt(B, b"abc").0.unwrap(), b"abc");
    }

    #[test]
    fn hostile_binary_messages_are_survived() {
        let gw = FakeGateway::new();
        let (mut d, _) = Dave::new(A, CHANNEL, 1);
        d.on_external_sender(&gw.sender_package());
        let mut seed = 0x9e37_79b9u32;
        for len in 0..300usize {
            let p: Vec<u8> = (0..len)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    seed as u8
                })
                .collect();
            d.on_external_sender(&p);
            let _ = d.on_proposals(&p);
            let _ = d.on_commit(&p);
            let _ = d.on_welcome(&p);
            let _ = d.decrypt(B, &p);
            let _ = d.on_prepare_transition((len % 7) as u16, (len % 2) as u16);
            let _ = d.on_execute_transition((len % 7) as u16);
        }
        // An invalid welcome asks to be re-added with a fresh key package.
        let (mut d, _) = Dave::new(A, CHANNEL, 1);
        d.on_external_sender(&gw.sender_package());
        let out = d.on_welcome(&with_tid(9, b"garbage"));
        assert_eq!(out[0], DaveOut::InvalidCommitWelcome(9));
        assert!(matches!(out.get(1), Some(DaveOut::Binary(26, _))));
    }
}
