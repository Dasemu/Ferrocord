//! In-app video playback through the `ferrocord-player` helper process.
//!
//! The helper (OpenH264, VP8/VP9, AAC, Opus: no system codecs) decodes one file and
//! streams RGBA frames; this
//! module spawns it, parses its output on a reader thread and forwards
//! [`Event`]s over a channel. One [`Player`] exists at a time (the app keeps
//! it in the open viewer); dropping it ends the process, which releases the
//! decoder, its buffers and the audio stream.

use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Mutex;

use tokio::sync::mpsc;

use super::video_proto::{self as proto, Cmd, Msg, PlayState};

/// Name of the helper executable.
pub const HELPER_NAME: &str = "ferrocord-player";

/// What went wrong, in terms the user can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The helper is missing or could not start.
    Unavailable(String),
    /// The video uses a codec the built-in decoder does not handle.
    MissingCodec(String),
    /// The file could not be played.
    Broken(String),
}

impl Failure {
    /// The text shown in the viewer: which step failed, never a crash, never
    /// silent. Path-like words are removed from the helper's detail.
    pub fn user_message(&self) -> String {
        match self {
            Failure::Unavailable(d) => format!(
                "The video player could not start ({}). Choose \u{201c}Open externally\u{201d} \
                 to use another player.",
                scrub(d)
            ),
            Failure::MissingCodec(what) => format!(
                "This video uses {}, which the built-in player cannot decode. Choose \
                 \u{201c}Open externally\u{201d} to play it with another player.",
                scrub(what)
            ),
            Failure::Broken(d) => format!(
                "This video could not be played: {}. Choose \u{201c}Open externally\u{201d} \
                 to try another player.",
                scrub(d)
            ),
        }
    }

    /// Short detail for logs (no paths).
    pub fn detail(&self) -> String {
        match self {
            Failure::Unavailable(d) | Failure::MissingCodec(d) | Failure::Broken(d) => scrub(d),
        }
    }
}

/// Drops words that look like file paths and caps the length.
fn scrub(s: &str) -> String {
    let words: Vec<&str> = s
        .split_whitespace()
        .filter(|w| !w.contains('/') && !w.contains('\\'))
        .collect();
    let mut out: String = words.join(" ").chars().take(160).collect();
    if out.is_empty() {
        out = "unknown error".into();
    }
    out
}

/// What the helper reports.
#[derive(Clone, PartialEq)]
pub enum Event {
    /// Playback started; size and duration are known.
    Ready {
        /// Frame width (0 for audio-only files).
        width: u32,
        /// Frame height.
        height: u32,
        /// Duration in ms, 0 when unknown.
        duration_ms: u64,
    },
    /// A decoded frame (RGBA).
    Frame {
        /// Width.
        width: u32,
        /// Height.
        height: u32,
        /// Pixels.
        rgba: Vec<u8>,
    },
    /// Playback position and state.
    Time {
        /// Position in ms.
        position_ms: u64,
        /// Duration in ms (0 = unknown).
        duration_ms: u64,
        /// State.
        state: PlayState,
    },
    /// Playback failed; no more events follow.
    Failed(Failure),
}

impl std::fmt::Debug for Event {
    /// Frames are megabytes of pixels: they are never printed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Event::Ready {
                width,
                height,
                duration_ms,
            } => write!(f, "Ready({width}x{height}, {duration_ms} ms)"),
            Event::Frame { width, height, .. } => write!(f, "Frame({width}x{height})"),
            Event::Time {
                position_ms, state, ..
            } => write!(f, "Time({position_ms} ms, {state:?})"),
            Event::Failed(x) => write!(f, "Failed({x:?})"),
        }
    }
}

/// Where the app sends playback commands: the helper process, or the
/// scripted player of mock mode. Tests record the commands instead.
pub trait Sink {
    /// Sends a command; errors (the helper already exited) are ignored.
    fn send(&self, cmd: Cmd);
}

impl Sink for Player {
    fn send(&self, cmd: Cmd) {
        Player::send(self, cmd);
    }
}

/// The helper inside `dir`, if it is there.
pub fn helper_in(dir: &Path) -> Option<PathBuf> {
    let p = dir.join(HELPER_NAME);
    p.is_file().then_some(p)
}

/// The helper executable: next to this binary, else on `PATH`.
pub fn helper_path() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        if let Some(p) = helper_in(dir) {
            return Some(p);
        }
        // `cargo test` runs from target/debug/deps.
        if let Some(p) = dir.parent().and_then(helper_in) {
            return Some(p);
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(HELPER_NAME))
        .find(|p| p.is_file())
}

/// A running helper. Dropping it stops playback and frees the decoder.
pub struct Player {
    child: Mutex<Option<Child>>,
    stdin: Mutex<Option<ChildStdin>>,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Player")
    }
}

/// Starts `helper` on `file`. Events arrive on the returned channel; the
/// channel closes after the last event.
pub fn spawn_with(
    helper: &Path,
    file: &Path,
) -> Result<(Player, mpsc::UnboundedReceiver<Event>), Failure> {
    let mut child = Command::new(helper)
        .arg(file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Failure::Unavailable(format!("cannot start the video helper: {e}")))?;
    let (Some(stdout), Some(stderr), stdin) =
        (child.stdout.take(), child.stderr.take(), child.stdin.take())
    else {
        let _ = child.kill();
        return Err(Failure::Unavailable("no pipes to the video helper".into()));
    };
    let (tx, rx) = mpsc::unbounded_channel();
    // stderr must be drained: a chatty helper with a full pipe would
    // stall the helper. Only the last line is kept (for load failures).
    let last_err = std::sync::Arc::new(Mutex::new(String::new()));
    {
        let last_err = last_err.clone();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in BufReader::new(stderr.take(1 << 20)).lines() {
                let Ok(line) = line else { break };
                if let Ok(mut g) = last_err.lock() {
                    *g = line.chars().take(300).collect();
                }
            }
        });
    }
    std::thread::spawn(move || {
        let mut reader = BufReader::with_capacity(1 << 16, stdout);
        let mut announced = false;
        #[allow(clippy::while_let_loop)]
        loop {
            match proto::read_msg(&mut reader) {
                Ok(Some(msg)) => {
                    let ev = match msg {
                        Msg::Ready {
                            width,
                            height,
                            duration_ms,
                        } => {
                            announced = true;
                            Event::Ready {
                                width,
                                height,
                                duration_ms,
                            }
                        }
                        Msg::Frame {
                            width,
                            height,
                            rgba,
                        } => Event::Frame {
                            width,
                            height,
                            rgba,
                        },
                        Msg::Time {
                            position_ms,
                            duration_ms,
                            state,
                        } => Event::Time {
                            position_ms,
                            duration_ms,
                            state,
                        },
                        Msg::Failed { missing, text } => {
                            announced = true;
                            Event::Failed(if missing {
                                Failure::MissingCodec(text)
                            } else {
                                Failure::Broken(text)
                            })
                        }
                    };
                    if tx.send(ev).is_err() {
                        return;
                    }
                }
                Ok(None) | Err(_) => break,
            }
        }
        if !announced {
            // The helper died before saying anything: the dynamic loader
            // refused it, or it crashed on start.
            // Give the stderr thread a moment to see the loader's message.
            std::thread::sleep(std::time::Duration::from_millis(100));
            let line = last_err
                .lock()
                .map(|g| g.clone())
                .ok()
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| "the helper exited".into());
            let _ = tx.send(Event::Failed(Failure::Unavailable(line)));
        }
    });
    Ok((
        Player {
            child: Mutex::new(Some(child)),
            stdin: Mutex::new(stdin),
        },
        rx,
    ))
}

/// Starts the helper found by [`helper_path`].
pub fn spawn(file: &Path) -> Result<(Player, mpsc::UnboundedReceiver<Event>), Failure> {
    let helper = helper_path().ok_or_else(|| {
        Failure::Unavailable(format!(
            "the {HELPER_NAME} helper was not found next to Ferrocord"
        ))
    })?;
    spawn_with(&helper, file)
}

impl Player {
    /// Sends a command; errors (the helper already exited) are ignored.
    pub fn send(&self, cmd: Cmd) {
        if let Ok(mut g) = self.stdin.lock()
            && let Some(s) = g.as_mut()
            && (s.write_all(cmd.to_line().as_bytes()).is_err() || s.flush().is_err())
        {
            *g = None;
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.send(Cmd::Quit);
        if let Ok(mut g) = self.stdin.lock() {
            *g = None;
        }
        let child = self.child.lock().ok().and_then(|mut c| c.take());
        if let Some(mut child) = child {
            // Give it a moment to exit cleanly, then make sure it is gone;
            // reaping happens off the UI thread.
            std::thread::spawn(move || {
                for _ in 0..20 {
                    if matches!(child.try_wait(), Ok(Some(_))) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                let _ = child.kill();
                let _ = child.wait();
            });
        }
    }
}

/// A player that needs no helper and no file: it draws a moving test
/// pattern and keeps a clock. Used by `--mock`, so the player UI can be run
/// and photographed without a network or a decoder.
pub fn spawn_mock(duration_ms: u64) -> (MockPlayer, mpsc::UnboundedReceiver<Event>) {
    use std::sync::mpsc::{RecvTimeoutError, channel};
    use std::time::{Duration, Instant};

    const W: u32 = 320;
    const H: u32 = 180;
    let (tx, rx) = mpsc::unbounded_channel();
    let (cmd_tx, cmd_rx) = channel::<Cmd>();
    std::thread::spawn(move || {
        let _ = tx.send(Event::Ready {
            width: W,
            height: H,
            duration_ms,
        });
        let mut base = 0u64;
        let mut since: Option<Instant> = Some(Instant::now());
        let mut ended = false;
        let mut last_time = Instant::now() - Duration::from_secs(1);
        loop {
            let pos = |base: u64, since: Option<Instant>| {
                (base + since.map_or(0, |s| s.elapsed().as_millis() as u64)).min(duration_ms)
            };
            match cmd_rx.recv_timeout(Duration::from_millis(40)) {
                Ok(Cmd::Quit) | Err(RecvTimeoutError::Disconnected) => return,
                Ok(Cmd::Play) => {
                    if ended {
                        ended = false;
                        base = 0;
                    }
                    since.get_or_insert_with(Instant::now);
                }
                Ok(Cmd::Pause) => {
                    base = pos(base, since);
                    since = None;
                }
                Ok(Cmd::Seek(ms)) => {
                    ended = false;
                    base = ms.min(duration_ms);
                    if since.is_some() {
                        since = Some(Instant::now());
                    }
                }
                Ok(Cmd::Volume(_) | Cmd::Mute(_)) | Err(RecvTimeoutError::Timeout) => {}
            }
            let now = pos(base, since);
            if since.is_some() && now >= duration_ms {
                ended = true;
                base = duration_ms;
                since = None;
                last_time = Instant::now() - Duration::from_secs(1);
            }
            let mut rgba = Vec::with_capacity((W * H * 4) as usize);
            let shift = (now / 20) as u32;
            for y in 0..H {
                for x in 0..W {
                    let v = ((x + shift) % 64) * 2;
                    rgba.extend_from_slice(&[(40 + v) as u8, (60 + y / 3) as u8, 140, 255]);
                }
            }
            if since.is_some() || last_time.elapsed() >= Duration::from_millis(250) {
                let _ = tx.send(Event::Frame {
                    width: W,
                    height: H,
                    rgba,
                });
            }
            if last_time.elapsed() >= Duration::from_millis(250) {
                last_time = Instant::now();
                let state = if ended {
                    PlayState::Ended
                } else if since.is_some() {
                    PlayState::Playing
                } else {
                    PlayState::Paused
                };
                if tx
                    .send(Event::Time {
                        position_ms: now,
                        duration_ms,
                        state,
                    })
                    .is_err()
                {
                    return;
                }
            }
        }
    });
    (MockPlayer { cmd: cmd_tx }, rx)
}

/// Handle of the scripted mock player; dropping it stops its thread.
pub struct MockPlayer {
    cmd: std::sync::mpsc::Sender<Cmd>,
}

impl Sink for MockPlayer {
    fn send(&self, cmd: Cmd) {
        let _ = self.cmd.send(cmd);
    }
}

impl Drop for MockPlayer {
    fn drop(&mut self) {
        let _ = self.cmd.send(Cmd::Quit);
    }
}

/// `65_000` ms -> `1:05`; `3_725_000` -> `1:02:05`.
pub fn format_clock(ms: u64) -> String {
    let s = ms / 1000;
    let (h, m, sec) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m}:{sec:02}")
    }
}

/// Seek target for a slider fraction in `0.0..=1.0`.
pub fn seek_target(fraction: f32, duration_ms: u64) -> u64 {
    (f64::from(fraction.clamp(0.0, 1.0)) * duration_ms as f64) as u64
}

/// The largest rectangle of aspect `w:h` inside `avail_w x avail_h`.
pub fn fit_inside(w: u32, h: u32, avail_w: f32, avail_h: f32) -> (f32, f32) {
    if w == 0 || h == 0 || avail_w <= 0.0 || avail_h <= 0.0 {
        return (avail_w.max(0.0), avail_h.max(0.0));
    }
    let scale = (avail_w / w as f32).min(avail_h / h as f32);
    (w as f32 * scale, h as f32 * scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_formats() {
        assert_eq!(format_clock(0), "0:00");
        assert_eq!(format_clock(65_000), "1:05");
        assert_eq!(format_clock(3_725_000), "1:02:05");
        assert_eq!(format_clock(999), "0:00");
    }

    #[test]
    fn seek_fraction_is_clamped() {
        assert_eq!(seek_target(0.5, 10_000), 5_000);
        assert_eq!(seek_target(-1.0, 10_000), 0);
        assert_eq!(seek_target(7.0, 10_000), 10_000);
        assert_eq!(seek_target(0.5, 0), 0);
    }

    #[test]
    fn fit_keeps_the_aspect_ratio() {
        assert_eq!(fit_inside(1920, 1080, 960.0, 960.0), (960.0, 540.0));
        assert_eq!(fit_inside(1080, 1920, 960.0, 540.0), (303.75, 540.0));
        assert_eq!(fit_inside(0, 0, 100.0, 50.0), (100.0, 50.0));
    }

    #[test]
    fn failures_say_which_step_failed() {
        let m = Failure::Unavailable("the helper exited".into()).user_message();
        assert!(m.contains("could not start (the helper exited)") && m.contains("Open externally"));
        let m = Failure::MissingCodec("H.265 (HEVC)".into()).user_message();
        assert!(m.contains("uses H.265 (HEVC)") && m.contains("cannot decode"));
        let m = Failure::Broken("H.264 decoding failed at 1.0 s: bitstream".into()).user_message();
        assert!(m.contains("H.264 decoding failed at 1.0 s"));
        // Paths (which may name files) are never shown.
        let m = Failure::Broken("cannot open /secret/path now".into()).user_message();
        assert!(!m.contains("secret") && m.contains("cannot open now"));
        assert!(
            Failure::Broken(String::new())
                .user_message()
                .contains("unknown error")
        );
    }

    #[test]
    fn a_missing_helper_is_a_clear_failure_not_a_crash() {
        let r = spawn_with(
            Path::new("/nonexistent/ferrocord-player"),
            Path::new("/tmp/x"),
        );
        assert!(matches!(r, Err(Failure::Unavailable(_))));
    }
}
