//! The video helper against real clips (generated with ffmpeg and
//! committed: 128x72, 2 s, H.264 High with B-frames + AAC in MP4, and VP9 /
//! VP8 + Opus in WebM), hostile files, and the app-side `Player` against
//! scripted fake helpers. No network, no window: frames are just bytes.
#![cfg(unix)]

use std::path::{Path, PathBuf};
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
    for name in ["high_aac.mp4", "vp9_opus.webm", "vp8_opus.webm"] {
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

#[tokio::test]
async fn an_unsupported_codec_is_named_not_crashed_on() {
    let (_player, mut rx) = spawn_with(Path::new(HELPER), &clip("av1_opus.webm")).expect("spawn");
    match next(&mut rx).await {
        Some(Event::Failed(Failure::MissingCodec(what))) => assert!(what.contains("AV1"), "{what}"),
        other => panic!("expected a named codec, got {other:?}"),
    }
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

#[tokio::test]
async fn truncated_clips_do_not_crash_the_helper() {
    let dir = tmp("trunc");
    for name in ["high_aac.mp4", "vp9_opus.webm", "vp8_opus.webm"] {
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
    for name in ["high_aac.mp4", "vp9_opus.webm", "vp8_opus.webm"] {
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
