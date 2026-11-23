//! The video helper against real clips (generated with ffmpeg and
//! committed: 128x72, 2 s, H.264 High with B-frames + AAC in MP4, and VP9 /
//! VP8 + Opus in WebM; plus the layouts real recorders write: fragmented
//! MP4, QuickTime `.mov`, `avc3` with in-band parameter sets, faststart
//! Baseline, H.264 with Opus or AAC in Matroska, H.265 `hvc1` + AAC, AV1;
//! and a few frames each of formats only platform decoders handle, which
//! play when the system has one and are named otherwise), hostile files,
//! and the app-side `Player` against
//! scripted fake helpers (shell scripts: Unix only). No network, no
//! window: frames are just bytes.

use std::collections::HashMap;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use ferrocord::media::gifv::{self, FrameSpec, Frames};
use ferrocord::media::video::{self, Event, Failure, spawn_with};
use ferrocord::media::video_proto::{Cmd, PlayState};
use tokio::sync::mpsc::UnboundedReceiver;

const HELPER: &str = env!("CARGO_BIN_EXE_ferrocord-player");
const WAIT: Duration = Duration::from_secs(30);

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ferrocord-vt-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn clip(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/video")
        .join(name)
}

async fn next(rx: &mut UnboundedReceiver<Event>) -> Option<Event> {
    tokio::time::timeout(WAIT, rx.recv()).await.ok().flatten()
}

/// Plays `file` to its end, checking what the helper reports on the way.
async fn play_through(file: &Path) {
    let (player, mut rx) = spawn_with(Path::new(HELPER), file).expect("spawn");
    let mut ready = None;
    let mut frames = 0;
    let mut last_pos = 0;
    let mut ended = false;
    let mut bright = 0usize;
    while let Some(ev) = next(&mut rx).await {
        match ev {
            Event::Ready {
                width,
                height,
                duration_ms,
            } => ready = Some((width, height, duration_ms)),
            Event::Frame {
                width,
                height,
                rgba,
            } => {
                assert_eq!((width, height), (128, 72));
                assert_eq!(rgba.len(), 128 * 72 * 4);
                assert!(rgba.chunks(4).all(|p| p[3] == 255));
                // testsrc2 is a colourful pattern: a picture of zeros or a
                // flat grey means the decoder produced garbage.
                bright += rgba.chunks(4).filter(|p| p[0] > 100 || p[1] > 100).count();
                frames += 1;
            }
            Event::Time {
                position_ms, state, ..
            } => {
                last_pos = last_pos.max(position_ms);
                if state == PlayState::Ended {
                    ended = true;
                    break;
                }
            }
            Event::Failed(f) => panic!("playback failed: {f:?}"),
        }
    }
    let (w, h, dur) = ready.expect("ready message");
    assert_eq!((w, h), (128, 72));
    assert!((1700..=2300).contains(&dur), "duration {dur}");
    assert!(frames >= 10, "only {frames} frames");
    assert!(bright > frames * 500, "the pictures look empty");
    assert!(ended, "never reached the end");
    assert!(last_pos >= 1700, "position stopped at {last_pos}");

    // Replaying from the end works, and a seek lands where asked.
    player.send(Cmd::Play);
    let mut replayed = false;
    while let Some(ev) = next(&mut rx).await {
        if let Event::Time {
            position_ms, state, ..
        } = ev
            && state == PlayState::Playing
            && position_ms < 1500
        {
            replayed = true;
            break;
        }
    }
    assert!(replayed, "Play after the end did not restart");
    player.send(Cmd::Pause);
    player.send(Cmd::Seek(1300));
    let mut sought = false;
    let mut frame_after_seek = false;
    while let Some(ev) = next(&mut rx).await {
        match ev {
            Event::Frame { .. } => frame_after_seek = true,
            Event::Time {
                position_ms, state, ..
            } if state == PlayState::Paused && (1000..=1700).contains(&position_ms) => {
                sought = frame_after_seek;
                if sought {
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(sought, "seek did not land near 1.3 s with a picture");

    // Closing frees the decoder: the helper exits and the channel closes.
    drop(player);
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        while rx.recv().await.is_some() {}
    })
    .await;
    assert!(closed.is_ok(), "the helper outlived its player");
}

#[tokio::test]
async fn h264_high_with_aac_in_mp4_plays() {
    play_through(&clip("high_aac.mp4")).await;
}

/// A fragmented MP4 (`moof` fragments, empty `moov` sample table), as
/// written by browsers, OBS and other recorders that must survive a crash.
/// It used to fail with "no picture could be decoded from the H.264
/// stream": the sample table the old demuxer read was empty.
#[tokio::test]
async fn fragmented_mp4_plays() {
    play_through(&clip("h264_frag_aac.mp4")).await;
}

/// QuickTime `.mov` (iPhone, macOS): its sound description used to make the
/// whole file "not a playable MP4 file".
#[tokio::test]
async fn quicktime_mov_plays() {
    play_through(&clip("h264_aac.mov")).await;
}

#[tokio::test]
async fn faststart_baseline_mp4_plays() {
    play_through(&clip("h264_baseline_faststart.mp4")).await;
}

/// `avc3`: parameter sets in band, repeated before each keyframe.
#[tokio::test]
async fn avc3_with_in_band_parameter_sets_plays() {
    play_through(&clip("h264_avc3.mp4")).await;
}

/// OBS's default container: H.264 in Matroska.
#[tokio::test]
async fn h264_in_matroska_plays() {
    play_through(&clip("h264_opus.mkv")).await;
}

// ---------------------------------------------------------------------
// Platform decoders (system FFmpeg on Linux, Media Foundation on Windows)
// ---------------------------------------------------------------------

/// What `ferrocord-player --backends` reports: codec key -> decoder, for
/// the codecs this machine has a platform decoder for.
fn backends() -> HashMap<String, String> {
    let out = Command::new(HELPER)
        .arg("--backends")
        .output()
        .expect("run --backends");
    assert!(out.status.success(), "--backends failed");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    eprintln!("platform decoders:\n{text}");
    assert!(text.starts_with("platform "), "{text}");
    text.lines()
        .skip(1)
        .filter_map(|l| l.split_once(' '))
        .filter(|(_, d)| *d != "-")
        .map(|(k, d)| (k.to_string(), d.to_string()))
        .collect()
}

/// CI sets this where the platform decoders must be found (Linux with
/// libavcodec installed, Windows): a missing one then fails instead of
/// being skipped.
fn platform_expected() -> bool {
    std::env::var("FERROCORD_EXPECT_PLATFORM").is_ok_and(|v| v == "1")
}

/// Frames mode with extra environment for the helper (and only for it).
fn frames_with_env(name: &str, env: &[(&str, &str)]) -> Result<Frames, Failure> {
    let mut cmd = Command::new(HELPER);
    cmd.args(GIFV.to_args())
        .arg(clip(name))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn");
    let mut out = BufReader::new(child.stdout.take().expect("stdout"));
    let r = gifv::read_frames(&mut out, GIFV);
    let _ = child.kill();
    let _ = child.wait();
    r
}

/// Every picture is the expected size and shows the test pattern.
fn assert_pictures(name: &str, f: &Frames, at_least: usize) {
    assert!(f.len() >= at_least, "{name}: only {} pictures", f.len());
    for (w, h, px, _) in f {
        assert_eq!((*w, *h), (128, 72), "{name}");
        assert_eq!(px.len(), 128 * 72 * 4, "{name}");
        let bright = px.chunks(4).filter(|p| p[0] > 100 || p[1] > 100).count();
        assert!(bright > 500, "{name}: the picture looks empty");
    }
}

/// The streams the built-in decoders refuse, and the platform decoder
/// (`--backends` key) that plays them.
const PLATFORM_ONLY: [(&str, &str, &str); 7] = [
    ("hevc_main10.mov", "hevc", "H.265 (HEVC)"),
    ("hevc_aac.mp4", "hevc", "H.265 (HEVC)"),
    ("av1_opus.webm", "av1", "AV1"),
    ("av1.mp4", "av1", "AV1"),
    ("h264_high10.mp4", "h264", "10-bit H.264"),
    ("h264_444.mp4", "h264", "H.264 with 4:4:4 colour"),
    ("h264_interlaced.mp4", "h264", "interlaced H.264"),
];

#[test]
fn platform_decoders_are_reported() {
    let b = backends();
    if platform_expected() {
        // Media Foundation's H.264 and AAC decoders ship with Windows;
        // the FFmpeg CI job installs a libavcodec with all four.
        let need: &[&str] = if cfg!(windows) {
            &["h264", "aac"]
        } else {
            &["h264", "hevc", "av1", "aac"]
        };
        for k in need {
            assert!(b.contains_key(*k), "no platform {k} decoder: {b:?}");
        }
    }
}

/// HEVC, AV1 and the H.264 variants OpenH264 refuses play through the
/// platform decoder when the system has one, as in the official client.
/// Without one they are named, with what to install.
#[test]
fn streams_the_builtin_decoders_refuse_play_with_a_platform_decoder() {
    let b = backends();
    for (name, key, what) in PLATFORM_ONLY {
        let r = frames_with_env(name, &[]);
        match (b.get(key), r) {
            (Some(_), Ok(f)) => {
                eprintln!(
                    "{name}: {} pictures through the platform {key} decoder",
                    f.len()
                );
                assert_pictures(name, &f, 3);
            }
            // Media Foundation's H.264 decoder has no High 10 / 4:4:4
            // support, and a Store extension (HEVC from Device
            // Manufacturer) may need a GPU: on Windows the key only says a
            // decoder is registered.
            (Some(_), Err(Failure::MissingCodec(m))) if cfg!(windows) => {
                eprintln!("{name}: Media Foundation cannot decode it: {m}");
                assert!(m.contains(what), "{name}: {m}");
            }
            (Some(d), Err(e)) => panic!("{name}: the platform decoder {d} failed: {e:?}"),
            (None, Ok(_)) => panic!("{name}: played without a platform {key} decoder"),
            (None, Err(Failure::MissingCodec(m))) => {
                assert!(!platform_expected() || cfg!(windows), "{name}: {m}");
                assert!(m.contains(what), "{name}: {m}");
                let shown = Failure::MissingCodec(m).user_message();
                assert!(shown.contains("Open externally"), "{name}: {shown}");
            }
            (None, Err(e)) => panic!("{name}: expected a named codec, got {e:?}"),
        }
    }
}

/// With no platform decoder (nothing installed, simulated here) the
/// helper still starts, plays what the built-in decoders handle, and names
/// both the codec and what to install for the rest.
#[test]
fn without_platform_decoders_the_message_names_what_to_install() {
    let off = [("FERROCORD_PLATFORM_DECODERS", "0")];
    assert_pictures(
        "high_aac.mp4",
        &frames_with_env("high_aac.mp4", &off).expect("plays"),
        10,
    );
    for (name, _, what) in PLATFORM_ONLY {
        match frames_with_env(name, &off) {
            Err(f @ Failure::MissingCodec(_)) => {
                let Failure::MissingCodec(text) = &f else {
                    unreachable!()
                };
                assert!(text.starts_with(what), "{name}: {text}");
                let m = f.user_message();
                assert!(m.contains(&format!("uses {what}")), "{name}: {m}");
                assert!(m.contains("Open externally"), "{name}: {m}");
                if cfg!(target_os = "linux") {
                    assert!(m.contains("Install the FFmpeg libraries"), "{name}: {m}");
                }
                if cfg!(windows) && what.contains("HEVC") {
                    // Or the Media Feature Pack, where Windows has no Media
                    // Foundation at all.
                    assert!(
                        m.contains("HEVC Video Extensions") || m.contains("Media Feature Pack"),
                        "{name}: {m}"
                    );
                }
            }
            other => panic!("{name}: expected a named codec, got {other:?}"),
        }
    }
    // The open-GOP clip: OpenH264 cannot start it, and the message says
    // what would.
    match frames_with_env("h264_no_idr.mp4", &off) {
        Err(Failure::Broken(why)) => {
            assert!(why.contains("IDR"), "{why}");
            if cfg!(target_os = "linux") {
                assert!(why.contains("FFmpeg"), "{why}");
            }
        }
        other => panic!("expected a broken stream, got {other:?}"),
    }
}

/// An open-GOP clip cut without re-encoding: no IDR picture, which
/// OpenH264 needs to start. The platform decoder takes over; without one
/// the message says so.
#[test]
fn a_stream_without_an_idr_keyframe_falls_back_or_says_so() {
    let b = backends();
    match (
        b.contains_key("h264"),
        frames_with_env("h264_no_idr.mp4", &[]),
    ) {
        (true, Ok(f)) => assert!(!f.is_empty()),
        (true, Err(e)) if cfg!(windows) => eprintln!("Media Foundation: {e:?}"),
        (false, Err(Failure::Broken(why))) => assert!(why.contains("IDR"), "{why}"),
        (_, other) => panic!("unexpected: {other:?}"),
    }
}

/// The platform H.264 decoder also handles what OpenH264 plays (forced
/// here): on Windows this is Media Foundation's own decoder.
#[test]
fn the_platform_h264_decoder_plays_the_common_profiles() {
    if !backends().contains_key("h264") {
        assert!(!platform_expected(), "no platform H.264 decoder");
        return;
    }
    let prefer = [("FERROCORD_VIDEO_DECODER", "platform")];
    for name in [
        "high_aac.mp4",
        "h264_baseline_faststart.mp4",
        "h264_opus.mkv",
        "h264_avc3.mp4",
    ] {
        let f = frames_with_env(name, &prefer).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_pictures(name, &f, 10);
        let total: Duration = f.iter().map(|x| x.3).sum();
        assert!(
            (1500..=2300).contains(&total.as_millis()),
            "{name}: loop of {total:?}"
        );
    }
}

/// An iPhone-style H.265 clip (`hvc1` + AAC) plays through to the end,
/// seeks and replays like any other.
#[tokio::test]
async fn hevc_with_aac_plays_through_with_a_platform_decoder() {
    if !backends().contains_key("hevc") {
        assert!(
            !platform_expected() || cfg!(windows),
            "no platform H.265 decoder"
        );
        return;
    }
    if cfg!(windows)
        && let Err(e) = frames_with_env("hevc_aac.mp4", &[])
    {
        eprintln!("the registered H.265 decoder does not run here: {e:?}");
        return;
    }
    play_through(&clip("hevc_aac.mp4")).await;
}

/// OBS's Matroska with AAC sound: the picture always plays; the sound
/// needs a platform AAC decoder (checked in the helper's unit tests).
#[tokio::test]
async fn h264_with_aac_in_matroska_plays() {
    play_through(&clip("h264_aac.mkv")).await;
}

#[tokio::test]
async fn vp9_with_opus_in_webm_plays() {
    play_through(&clip("vp9_opus.webm")).await;
}

#[tokio::test]
async fn vp8_with_opus_in_webm_plays() {
    play_through(&clip("vp8_opus.webm")).await;
}

/// Frames mode (gifv): the whole clip as timed frames, fitted to the box,
/// within the budget, and the helper exits by itself.
fn frames_of(name: &str, spec: FrameSpec) -> Frames {
    gifv::decode_with(Path::new(HELPER), &clip(name), spec).expect("frames")
}

const GIFV: FrameSpec = FrameSpec {
    max_w: 400,
    max_h: 300,
    max_frames: 48,
    max_bytes: 6 * 1024 * 1024,
};

#[test]
fn frames_mode_decodes_every_supported_codec() {
    for name in [
        "high_aac.mp4",
        "vp9_opus.webm",
        "vp8_opus.webm",
        "h264_frag_aac.mp4",
        "h264_aac.mov",
        "h264_baseline_faststart.mp4",
        "h264_avc3.mp4",
        "h264_opus.mkv",
        "h264_aac.mkv",
    ] {
        let f = frames_of(name, GIFV);
        assert!(f.len() >= 10, "{name}: only {} frames", f.len());
        assert!(
            f.iter()
                .all(|(w, h, px, _)| (*w, *h) == (128, 72) && px.len() == 128 * 72 * 4)
        );
        let total: Duration = f.iter().map(|x| x.3).sum();
        assert!(
            (1500..=2300).contains(&total.as_millis()),
            "{name}: loop of {total:?}"
        );
        let bright: usize = f
            .iter()
            .map(|x| x.2.chunks(4).filter(|p| p[0] > 100 || p[1] > 100).count())
            .sum();
        assert!(bright > f.len() * 500, "{name}: the pictures look empty");
    }
}

#[test]
fn frames_mode_fits_the_box_and_keeps_the_budget() {
    let small = FrameSpec {
        max_w: 64,
        max_h: 64,
        max_frames: 8,
        max_bytes: 1024 * 1024,
    };
    let f = frames_of("high_aac.mp4", small);
    assert_eq!(f.len(), 8);
    assert!(f.iter().all(|(w, h, _, _)| (*w, *h) == (64, 36)));
    // Spread over the clip, not its first 8 frames.
    let total: Duration = f.iter().map(|x| x.3).sum();
    assert!(total.as_millis() >= 1500, "loop of {total:?}");
}

#[test]
fn frames_mode_reports_broken_files() {
    let dir = tmp("gifv-garbage");
    let bad = dir.join("bad.mp4");
    std::fs::write(&bad, b"not a video at all").unwrap();
    assert!(gifv::decode_with(Path::new(HELPER), &bad, GIFV).is_err());
    assert!(gifv::decode_with(Path::new(HELPER), &dir.join("missing.mp4"), GIFV).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

async fn expect_clean_failure(file: &Path) {
    let (player, mut rx) = spawn_with(Path::new(HELPER), file).expect("spawn");
    // A hostile file may fail at open, part way, or (when the damage is
    // in data the decoder conceals) play to the end. A failure ends the
    // helper by itself; a finished video idles until the player is
    // dropped. Either way it never hangs and never takes the test down.
    let mut ended = false;
    while let Some(ev) = next(&mut rx).await {
        match ev {
            Event::Failed(f) => {
                assert!(!f.user_message().is_empty());
                break;
            }
            Event::Time {
                state: PlayState::Ended,
                ..
            } => {
                ended = true;
                break;
            }
            _ => {}
        }
    }
    if ended {
        drop(player);
    }
    let closed = tokio::time::timeout(Duration::from_secs(10), async {
        while rx.recv().await.is_some() {}
    })
    .await;
    assert!(
        closed.is_ok(),
        "the helper did not exit on {}",
        file.display()
    );
}

#[tokio::test]
async fn garbage_and_empty_and_missing_files_fail_cleanly() {
    let dir = tmp("hostile");
    let text = dir.join("broken.mp4");
    std::fs::write(
        &text,
        b"this is not a video at all, just text padding".repeat(8),
    )
    .unwrap();
    let (_p, mut rx) = spawn_with(Path::new(HELPER), &text).expect("spawn");
    match next(&mut rx).await {
        Some(Event::Failed(Failure::Broken(_) | Failure::MissingCodec(_))) => {}
        other => panic!("expected a clean failure, got {other:?}"),
    }
    let empty = dir.join("empty.webm");
    std::fs::write(&empty, b"").unwrap();
    let (_p, mut rx) = spawn_with(Path::new(HELPER), &empty).expect("spawn");
    assert!(matches!(next(&mut rx).await, Some(Event::Failed(_))));
    let (_p, mut rx) =
        spawn_with(Path::new(HELPER), Path::new("/nonexistent/clip.mp4")).expect("spawn");
    assert!(matches!(next(&mut rx).await, Some(Event::Failed(_))));
    // A directory is not a video either.
    let (_p, mut rx) = spawn_with(Path::new(HELPER), &dir).expect("spawn");
    assert!(matches!(next(&mut rx).await, Some(Event::Failed(_))));
    let _ = std::fs::remove_dir_all(dir);
}

/// Clips cut and corrupted below: every decoder path, the platform
/// decoders (H.265, AV1, AAC in Matroska) included when present.
const TORTURE: [&str; 6] = [
    "high_aac.mp4",
    "vp9_opus.webm",
    "vp8_opus.webm",
    "hevc_aac.mp4",
    "av1_opus.webm",
    "h264_aac.mkv",
];

#[tokio::test]
async fn truncated_clips_do_not_crash_the_helper() {
    let dir = tmp("trunc");
    for name in TORTURE {
        let bytes = std::fs::read(clip(name)).unwrap();
        for keep in [bytes.len() / 2, bytes.len() * 9 / 10, 64] {
            let f = dir.join(format!("{keep}-{name}"));
            std::fs::write(&f, &bytes[..keep]).unwrap();
            expect_clean_failure(&f).await;
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn corrupted_clips_do_not_crash_the_helper() {
    let dir = tmp("corrupt");
    for name in TORTURE {
        let bytes = std::fs::read(clip(name)).unwrap();
        // Flip bytes deterministically in the middle (the coded data) and
        // at the start (the headers).
        for (i, seed) in [(0usize, 7u8), (1, 31), (2, 101)] {
            let mut b = bytes.clone();
            let n = b.len();
            let (from, to) = match i {
                0 => (n / 3, n / 3 + 400),
                1 => (0, 200.min(n)),
                _ => (n / 2, n / 2 + 1500),
            };
            for (k, byte) in b[from..to.min(n)].iter_mut().enumerate() {
                *byte ^= seed.wrapping_mul(k as u8).wrapping_add(seed);
            }
            let f = dir.join(format!("{i}-{name}"));
            std::fs::write(&f, &b).unwrap();
            expect_clean_failure(&f).await;
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn an_absurd_picture_size_is_refused_without_allocating_it() {
    // An EBML header followed by a segment declaring a 4 GB element: the
    // demuxer must give up, not allocate.
    let dir = tmp("huge");
    let f = dir.join("huge.webm");
    let mut b = vec![0x1A, 0x45, 0xDF, 0xA3, 0x84, 0x42, 0x82, 0x81, 0x00];
    b.extend([
        0x18, 0x53, 0x80, 0x67, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    ]);
    b.extend(vec![0xAB; 64]);
    std::fs::write(&f, &b).unwrap();
    expect_clean_failure(&f).await;
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn the_container_is_taken_from_the_bytes_not_the_name() {
    // A WebM named .mp4 (senders choose names) still plays.
    let dir = tmp("misnamed");
    let f = dir.join("really-webm.mp4");
    std::fs::copy(clip("vp9_opus.webm"), &f).unwrap();
    let (_p, mut rx) = spawn_with(Path::new(HELPER), &f).expect("spawn");
    match next(&mut rx).await {
        Some(Event::Ready { width, height, .. }) => assert_eq!((width, height), (128, 72)),
        other => panic!("{other:?}"),
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// Writes an executable shell script acting as the helper.
#[cfg(unix)]
fn script(dir: &Path, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join("helper.sh");
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

/// Starts a helper script written moments ago. A child forked by another test
/// thread can still hold the script's write descriptor until it execs, so
/// exec fails with ETXTBSY ("Text file busy"); retry as Go's os/exec does.
#[cfg(unix)]
fn spawn_script(
    helper: &Path,
    file: &Path,
) -> Result<(video::Player, tokio::sync::mpsc::UnboundedReceiver<Event>), Failure> {
    for _ in 0..50 {
        match spawn_with(helper, file) {
            Err(Failure::Unavailable(m)) if m.contains("os error 26") => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            other => return other,
        }
    }
    spawn_with(helper, file)
}

#[cfg(unix)]
#[tokio::test]
async fn player_parses_helper_output_and_delivers_commands() {
    let dir = tmp("script");
    // Ready(2x1, 1000 ms), a 2x1 frame, a time message, then record stdin.
    let helper = script(
        &dir,
        r#"printf 'R\002\000\000\000\001\000\000\000\350\003\000\000\000\000\000\000'
printf 'F\002\000\000\000\001\000\000\000\001\002\003\004\005\006\007\010'
printf 'T\364\001\000\000\000\000\000\000\350\003\000\000\000\000\000\000\001'
cat > "$1.cmds""#,
    );
    let target = dir.join("clip.mp4");
    let (player, mut rx) = spawn_script(&helper, &target).expect("spawn");
    assert_eq!(
        next(&mut rx).await,
        Some(Event::Ready {
            width: 2,
            height: 1,
            duration_ms: 1000
        })
    );
    assert_eq!(
        next(&mut rx).await,
        Some(Event::Frame {
            width: 2,
            height: 1,
            rgba: vec![1, 2, 3, 4, 5, 6, 7, 8]
        })
    );
    assert_eq!(
        next(&mut rx).await,
        Some(Event::Time {
            position_ms: 500,
            duration_ms: 1000,
            state: PlayState::Playing
        })
    );
    player.send(Cmd::Pause);
    player.send(Cmd::Seek(750));
    player.send(Cmd::Volume(40));
    player.send(Cmd::Mute(true));
    drop(player);
    // The script exits once stdin closes, which ends the stream.
    assert!(next(&mut rx).await.is_none());
    let cmds = std::fs::read_to_string(dir.join("clip.mp4.cmds")).unwrap();
    assert_eq!(cmds, "pause\nseek 750\nvolume 40\nmute 1\nquit\n");
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[tokio::test]
async fn a_hostile_helper_cannot_make_the_app_allocate_or_hang() {
    let dir = tmp("hostile-helper");
    // A frame claiming 4096x4096 pixels but sending 3 bytes, then a flood
    // of garbage tags: the reader refuses and the player reports failure.
    let helper = script(
        &dir,
        r#"printf 'F\000\020\000\000\000\020\000\000abc'
sleep 1"#,
    );
    let (_player, mut rx) = spawn_script(&helper, &dir.join("x.mp4")).expect("spawn");
    match next(&mut rx).await {
        Some(Event::Failed(Failure::Unavailable(_))) => {}
        other => panic!("{other:?}"),
    }
    let helper = script(&dir, r#"printf 'Z\377\377\377\377'; sleep 1"#);
    let (_player, mut rx) = spawn_script(&helper, &dir.join("x.mp4")).expect("spawn");
    assert!(matches!(next(&mut rx).await, Some(Event::Failed(_))));
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[tokio::test]
async fn a_helper_the_loader_refuses_is_reported_with_the_loader_message() {
    let dir = tmp("noloader");
    let helper = script(
        &dir,
        "echo 'ferrocord-player: error while loading shared libraries: libfoo.so.1: cannot open shared object file' >&2; exit 127",
    );
    let (_player, mut rx) = spawn_script(&helper, &dir.join("x.mp4")).expect("spawn");
    match next(&mut rx).await {
        Some(Event::Failed(f @ Failure::Unavailable(_))) => {
            let msg = f.user_message();
            assert!(msg.contains("libfoo.so.1"), "{msg}");
            assert!(msg.contains("Open externally"));
        }
        other => panic!("{other:?}"),
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_helper_is_found_in_its_directory() {
    let dir = Path::new(HELPER).parent().unwrap();
    assert_eq!(video::helper_in(dir).as_deref(), Some(Path::new(HELPER)));
    assert!(video::helper_in(Path::new("/nonexistent")).is_none());
}
