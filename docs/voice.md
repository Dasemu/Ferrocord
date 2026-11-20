# Voice, calls and video

Ferrocord joins voice channels and DM / group calls the way the official
client does. This page covers the design, the protocol details, and the
feasibility pass for video. The short protocol tables live in
`docs/protocol.md` §10.

> Third-party clients break Discord's Terms of Service. See the README warning.

## 1. What is implemented

| Feature | State |
|---|---|
| Voice channels in servers: join / move / leave, users under the channel, speaking rings | done |
| "Voice Connected" panel: channel / server, ping, quality colour, E2EE shield, disconnect | done |
| Mic mute and deafen in the user panel (remembered when not in voice) | done |
| Input / output device, input sensitivity (voice activity threshold) with live meter | done |
| Per-user volume (0-200 %) and local mute (right-click a user in voice) | done |
| DM / group calls: phone button, ring, incoming call overlay, OS notification, tray attention, decline | done |
| Call service messages ("X started a call that lasted …", missed calls) | done |
| DAVE end-to-end encryption (mandatory since March 2026) | done (`davey`) |
| Receiving camera video / Go Live | not done: see §6 |
| Sending camera video / screen share | not done: see §6 |

There is no video button. A button that cannot work is worse than none.

## 2. Flow

```
click voice channel / phone button
  └─ op 4 VOICE_STATE_UPDATE {guild_id|null, channel_id, self_mute, self_deaf, self_video:false}
       ├─ VOICE_STATE_UPDATE (ours)   → session_id
       └─ VOICE_SERVER_UPDATE         → endpoint + token (guild_id, or channel_id for calls)
            └─ voice gateway  wss://{endpoint}/?v=8
                 → op 0 IDENTIFY {server_id, user_id, session_id, token, max_dave_protocol_version: 1}
                 ← op 8 HELLO, op 2 READY {ssrc, ip, port, modes}
                 UDP: IP discovery (74-byte request/response)
                 → op 1 SELECT PROTOCOL {udp, address, port, mode}
                 ← op 4 SESSION DESCRIPTION {mode, secret_key, dave_protocol_version}
                 DAVE ops 21-31 (MLS group)  ·  op 3/6 heartbeat with seq_ack  ·  op 5 speaking
```

For DM and group calls `server_id` is the channel id. Starting a call joins
the call channel with op 4 and then rings everyone with
`POST /channels/{id}/call/ring {"recipients": null}` (only when no call is
already running there). Declining sends
`POST /channels/{id}/call/stop-ringing {"recipients": [me]}`.
`CALL_CREATE` / `CALL_UPDATE` carry `ringing` (who is being rung) and the
call's voice states; `CALL_DELETE` ends it.

## 3. Code map

| Module | Role |
|---|---|
| `net/voice/session.rs` | Voice gateway v8 as a pure state machine (identify, resume with `seq_ack`, heartbeats, op parsing, close codes). |
| `net/voice/rtp.rs` | IP discovery, RTP headers, `aead_aes256_gcm_rtpsize` and `aead_xchacha20_poly1305_rtpsize`. |
| `net/voice/dave.rs` | DAVE opcodes on top of `davey::DaveSession` (same handling as libdave / discord.js). |
| `net/voice/media.rs` | 20 ms engine: mic → VAD → Opus → DAVE → RTP/AEAD → UDP and back; one decoder and jitter buffer per speaker, mixer with per-user gain. Socket-free, so two engines can be wired together in tests. |
| `net/voice/jitter.rs`, `mixer.rs` | Bounded reordering buffer (60 ms pre-roll, 1 s cap, loss concealment), mixing, levels, VAD, resampling. |
| `net/voice/audio.rs` | `cpal` devices on their own thread (streams are not `Send`), bounded rings; fake I/O for tests. |
| `net/voice/conn.rs` | Async runner: WebSocket, UDP, media task, resume (5 tries), the event stream the app subscribes to. |
| `app/voice.rs` | Pure reducer: voice states, own connection phases, mute/deafen, the call ring/accept/decline machine, settings; app glue. |
| `ui/voice.rs` | Voice users, connected panel, DM call area, incoming overlay, user volume menu, call messages, settings section. |

**Stay light.** The voice connection is an iced subscription that exists
only while we are in a channel and the voice server is known
(`App::voice_subscription`). Audio devices open when media starts and
close when the stream is dropped (leaving, logout, app exit). Nothing
voice-related runs or allocates otherwise; the only idle cost is the voice
state map (bounded to 20 000 entries) and the compiled code.

**Hostile input.** Every packet parser is bounds-checked and fuzzed with
random bytes in unit tests (RTP, discovery, voice gateway JSON and binary,
DAVE/MLS messages, jitter buffer). Text frames are capped at 64 KiB,
binary DAVE frames at 256 KiB, UDP datagrams at 1500 bytes, Opus frames at
1275 bytes. At most 64 speakers are decoded at once. `davey` calls that
parse MLS messages run under `catch_unwind`, so a library panic resets the
group instead of crashing.

## 4. DAVE

Discord's docs state that only end-to-end encrypted calls are supported
from **March 1, 2026** for DMs, group DMs, voice channels and Go Live
(Stage channels excepted). A client that does not announce DAVE gets close
code **4017** ("E2EE/DAVE protocol required").

Ferrocord uses [`davey`](https://crates.io/crates/davey) 0.2 (MIT), the
Rust DAVE implementation behind discord.js (`@snazzah/davey`). It builds on
OpenMLS (`MLS_128_DHKEMP256_AES128GCM_SHA256_P256`, ciphersuite of DAVE
v1). It is pure Rust and adds no system dependency. The opcode handling in
`net/voice/dave.rs` follows libdave and discord.js:

* op 4 `dave_protocol_version > 0` → session + key package (binary op 26);
* op 25 external sender (buffered if it arrives before op 4);
* op 27 proposals (append / revoke, validated against the users from ops
  11 / 13) → op 28 commit (+ welcome);
* op 29 commit / op 30 welcome → op 23 transition ready; failure → op 31
  and a fresh key package;
* op 21 / 22 / 24 transitions, including downgrade to passthrough and the
  sole-member reset (`epoch = 1`);
* media: Opus frames are encrypted per sender (AES-128-GCM, truncated tag,
  ULEB128 nonce, `0xFAFA` marker); silence frames pass through.

Identity keys are ephemeral (new per call), which the protocol allows.
Persistent keys and the verification-code UI are not implemented.

Tests: a two-member loopback with a fake voice gateway as external sender
(proposal → commit/welcome → transition → encrypted Opus both ways, tamper
detection) and hostile MLS input. The voice gateway integration test checks
that a DAVE session sends its key package.

## 5. Audio

Opus 48 kHz stereo, 20 ms frames, 64 kb/s, in-band FEC; libopus is built
from the source bundled in `audiopus_sys` and linked statically
(`.cargo/config.toml`, needs `cmake`). Devices are opened at 48 kHz when
they support it, otherwise at their default rate with linear resampling.
Voice activity uses an RMS threshold (the "Input Sensitivity" slider,
-100…0 dB, default -60 dB) with a 300 ms hangover. After talking stops,
five Opus silence frames are sent, then op 5 `speaking: 0`. A small UDP
keepalive (8 bytes every 5 s) keeps the NAT mapping open while silent,
like other voice libraries do. Speaking rings for others come from voice
activity in the received audio (250 ms hold), as the server sends op 5
only once per SSRC.

Not done: noise suppression, echo cancellation, automatic gain control,
push-to-talk, and automatic sensitivity. The official client uses
WebRTC's APM (plus Krisp) for these. A pure-Rust or statically linked APM
would be the next audio step.

## 6. Video: feasibility pass

What receiving video needs, in order:

1. **Voice gateway video negotiation.** IDENTIFY with `video: true` and a
   `streams` list, op 12 (VIDEO) with our audio/video/RTX SSRCs, the
   `codecs` list in Select Protocol (H264 / VP8 / VP9 / AV1 with payload
   types and RTX), handling the server's op 12 / op 18 stream
   announcements and op 15 media sink wants (quality per SSRC).
2. **RTP video depacketizing.** H.264 (RFC 6184: single NAL, STAP-A,
   FU-A) and VP8 (RFC 7741), with the marker bit closing a frame, RTX
   (RFC 4588) recovery, out-of-order handling, and a frame-level jitter
   buffer. RTCP: receiver reports, NACK and PLI so senders resend a
   keyframe after loss. Without PLI a viewer joining mid-stream waits up
   to a full keyframe interval and stalls after every loss.
3. **DAVE for video.** `davey` handles the codec-specific unencrypted
   ranges (H.264 NAL headers, VP8 payload descriptor), so
   `decrypt(user, MediaType::VIDEO, frame)` works on whole frames.
4. **Decoding.** `openh264` 0.9 (BSD-2) can build Cisco's decoder from
   source and link it statically (nasm optional), so H.264 needs no system
   library. VP8/VP9 would need `libvpx` (C, cmake/yasm) or a pure-Rust
   decoder, and AV1 `dav1d`. The official client prefers H.264 when every
   viewer supports it, but Go Live from some clients arrives as VP8/AV1.
   The `feat/video` branch (attachment video player) already decodes
   H.264/VP8/VP9/AV1 in a helper process (`ferrocord-player`). Reusing
   that helper for call video would keep codec code out of the main
   binary.
5. **Rendering.** Decoded I420 → RGBA at up to 30 fps per tile, uploaded
   as `image::Handle`s built in `update` (never in `view`, see LESSONS on
   flicker). With the tiny-skia renderer each 720p frame costs ~3.7 MB and
   a CPU blit; several tiles at 30 fps would need the wgpu renderer.
6. **Go Live** uses a separate connection: op 18 `STREAM_WATCH` on the
   main gateway, `STREAM_CREATE` / `STREAM_SERVER_UPDATE`, then a second
   voice-gateway session for the stream key `guild:{g}:{c}:{user}`.

Sending camera video adds capture (PipeWire camera portal or V4L2), an
encoder (OpenH264 encoder, statically built) with rate control driven by
REMB / transport-cc feedback, simulcast/RTX on the send side, and
`self_video: true` in op 4.

**Decision for this PR:** none of steps 1-6 can be checked without a real
account and a real sender. Their failure modes (missing PLI, wrong
payload types, codec mismatch) give a black tile, not an error. Video is
therefore not shipped. Voice and DM calls ship complete and are the
foundation (the gateway, UDP, DAVE and jitter code all carry over).

**Concrete next steps for video receive:**

1. Add op 12 / `video: true` / `codecs` negotiation and op 15 sink wants
   (`net/voice/session.rs`), tested against fixtures recorded from the
   official client's web build.
2. `net/voice/video/h264.rs`: FU-A/STAP-A depacketizer with a frame
   assembler bounded at 4 MB per frame, plus fuzz tests.
3. RTCP RR + NACK + PLI in `net/voice/rtcp.rs` (sent when a frame is
   incomplete for > 100 ms).
4. Decode in the `ferrocord-player` helper (or `openh264` in-process
   behind a feature), deliver RGBA frames at most 15 fps per tile on
   tiny-skia, and add a 2x2 video grid to the DM call area and a "Watch
   Stream" tile in voice channels.
5. Only then add a "Video" button to the call controls.

## 7. Verified vs. needs a real account

Verified here (no network): the voice gateway handshake, discovery and
Opus media both ways against a local mock server; transport encryption
round trips and a known-answer vector; the DAVE MLS handshake between two
in-process members; jitter buffer, mixer, VAD and resampler; the
app-level join / leave / mute / deafen and ring / accept / decline flows;
mock screenshots.

Needs a real account: that Discord accepts our IDENTIFY and op 4 as sent;
real voice servers' DAVE behaviour (external sender timing, transitions
with several members, the official clients' commits); DM call ringing and
`stop-ringing` semantics; NAT traversal on real networks; audio quality
and latency on real devices (PipeWire through ALSA); behaviour when the
same account is in a call on another device.
