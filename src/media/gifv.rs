//! `gifv` embeds (Tenor / Giphy links): a short, silent MP4 that the
//! official client plays muted and looped, like a GIF.
//!
//! They are not played by the video viewer (one video at a time, with
//! controls and sound). Instead the `ferrocord-player` helper runs once in
//! its *frames mode* (`--frames`): it decodes the picture track only, as
//! fast as it can, scaled to fit the chat's media box, keeps evenly spaced
//! frames within a frame and byte budget ([`Sampler`]), writes them with
//! their times and exits. The app turns them into an
//! [`crate::app::anim::Anim`] that the self-playing `ui::anim_image` widget
//! loops like any GIF, so a gifv follows the GIF rules (autoplay setting,
//! window focus, "Reduce motion", only redrawing while visible), several
//! can play at once, and no decoder process stays alive.

use std::io::{BufReader, Read};
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::Duration;

use super::video::{Failure, helper_command};
use super::video_proto::{self as proto, Msg};

/// Flag that selects the frames mode of the helper.
pub const FRAMES_FLAG: &str = "--frames";
/// Only the first this-many ms of a long clip are kept.
pub const MAX_SPAN_MS: u64 = 30_000;
/// Frames closer than this are never both kept (about 30 per second).
pub const MIN_GAP_MS: u64 = 33;
/// Shortest delay a kept frame is shown for.
pub const MIN_DELAY_MS: u64 = 20;
/// The helper is stopped if it has not finished after this long.
pub const DECODE_TIMEOUT: Duration = Duration::from_secs(20);

/// What the helper may produce for one clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameSpec {
    /// Frames are scaled down (never up) to fit `max_w` x `max_h`.
    pub max_w: u32,
    pub max_h: u32,
    /// Frames kept at most.
    pub max_frames: u32,
    /// RGBA bytes kept at most (all frames together).
    pub max_bytes: u64,
}

impl FrameSpec {
    /// Command-line arguments of the frames mode, before the file.
    pub fn to_args(self) -> Vec<String> {
        vec![
            FRAMES_FLAG.into(),
            format!("{}x{}", self.max_w, self.max_h),
            self.max_frames.to_string(),
            self.max_bytes.to_string(),
        ]
    }

    /// Parses what follows [`FRAMES_FLAG`]: `<w>x<h> <frames> <bytes>`.
    pub fn parse(box_: &str, frames: &str, bytes: &str) -> Option<FrameSpec> {
        let (w, h) = box_.split_once('x')?;
        let spec = FrameSpec {
            max_w: w.parse().ok()?,
            max_h: h.parse().ok()?,
            max_frames: frames.parse().ok()?,
            max_bytes: bytes.parse().ok()?,
        };
        let sane = (1..=proto::MAX_DIM).contains(&spec.max_w)
            && (1..=proto::MAX_DIM).contains(&spec.max_h)
            && (2..=1000).contains(&spec.max_frames)
            && spec.max_bytes > 0;
        sane.then_some(spec)
    }
}

/// Picks which decoded pictures to keep: evenly spaced over the clip (at
/// most [`MAX_SPAN_MS`] of it), so a long clip drops frames instead of
/// being cut short, within the frame and byte budget.
#[derive(Debug, Clone)]
pub struct Sampler {
    allowed: u32,
    gap: u64,
    end: u64,
    next_due: Option<u64>,
    kept: u32,
}

impl Sampler {
    /// `None` when not even two frames of `frame_bytes` fit the budget.
    pub fn new(spec: FrameSpec, duration_ms: u64, frame_bytes: u64) -> Option<Sampler> {
        let by_bytes = spec.max_bytes / frame_bytes.max(1);
        let allowed = u64::from(spec.max_frames).min(by_bytes);
        if allowed < 2 {
            return None;
        }
        let span = if duration_ms == 0 {
            MAX_SPAN_MS
        } else {
            duration_ms.min(MAX_SPAN_MS)
        };
        // Unknown duration: keep frames at the minimum gap until full.
        let gap = if duration_ms == 0 {
            MIN_GAP_MS
        } else {
            (span / allowed).max(MIN_GAP_MS)
        };
        Some(Sampler {
            allowed: allowed as u32,
            gap,
            end: span,
            next_due: None,
            kept: 0,
        })
    }

    /// Whether the picture shown at `time_ms` is kept.
    pub fn keep(&mut self, time_ms: u64) -> bool {
        if self.done() || time_ms >= self.end {
            return false;
        }
        // Half a gap of tolerance: a 25 fps clip with a 41 ms gap keeps
        // every frame instead of every second one.
        match self.next_due {
            Some(due) if time_ms + self.gap / 2 < due => false,
            due => {
                let mut due = due.unwrap_or(time_ms) + self.gap;
                while due <= time_ms {
                    due += self.gap;
                }
                self.next_due = Some(due);
                self.kept += 1;
                true
            }
        }
    }

    /// No further picture will be kept: decoding can stop.
    pub fn done(&self) -> bool {
        self.kept >= self.allowed || self.next_due.is_some_and(|d| d >= self.end)
    }
}

/// How long each kept frame shows, from their times: until the next one,
/// and the last one until the end of the clip (or for the usual gap when
/// the duration is unknown). The loop length is the clip's.
pub fn delays(times: &[u64], duration_ms: u64) -> Vec<Duration> {
    let mut out = Vec::with_capacity(times.len());
    let gaps: Vec<u64> = times
        .windows(2)
        .map(|w| w[1].saturating_sub(w[0]))
        .collect();
    let typical = if gaps.is_empty() {
        100
    } else {
        gaps.iter().sum::<u64>() / gaps.len() as u64
    };
    for (i, &t) in times.iter().enumerate() {
        let d = match times.get(i + 1) {
            Some(&next) => next.saturating_sub(t),
            None => {
                let end = duration_ms.min(MAX_SPAN_MS);
                if end > t { end - t } else { typical }
            }
        };
        out.push(Duration::from_millis(d.clamp(MIN_DELAY_MS, 5_000)));
    }
    out
}

/// Decoded frames of a clip: `(width, height, rgba, delay)`, ready for
/// `anim::from_rgba_frames`.
pub type Frames = Vec<(u32, u32, Vec<u8>, Duration)>;

/// Runs `helper` in frames mode on `file` and collects what it keeps.
/// Blocking: call it from `spawn_blocking`. The process is killed after
/// [`DECODE_TIMEOUT`].
pub fn decode_with(helper: &Path, file: &Path, spec: FrameSpec) -> Result<Frames, Failure> {
    let mut child = helper_command(helper)
        .args(spec.to_args())
        .arg(file)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| Failure::Unavailable(format!("cannot start the video helper: {e}")))?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(Failure::Unavailable("no pipe from the video helper".into()));
    };
    // Watchdog: the reader below ends when the helper exits or is killed.
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let watchdog = std::thread::spawn(move || {
        if let Err(mpsc::RecvTimeoutError::Timeout) = stop_rx.recv_timeout(DECODE_TIMEOUT) {
            let _ = child.kill();
        }
        let _ = child.wait();
    });
    let result = read_frames(&mut BufReader::with_capacity(1 << 16, stdout), spec);
    let _ = stop_tx.send(());
    let _ = watchdog.join();
    result
}

/// Reads a frames-mode stream to its end.
pub fn read_frames<R: Read>(r: &mut R, spec: FrameSpec) -> Result<Frames, Failure> {
    let mut duration = None;
    let mut pics: Vec<(u64, u32, u32, Vec<u8>)> = Vec::new();
    let mut bytes = 0u64;
    loop {
        match proto::read_msg(r) {
            Ok(Some(Msg::Ready { duration_ms, .. })) => duration = Some(duration_ms),
            Ok(Some(Msg::Picture {
                time_ms,
                width,
                height,
                rgba,
            })) => {
                // The helper is trusted to keep the budget, but never more
                // than asked is held here either.
                if width > spec.max_w || height > spec.max_h {
                    return Err(Failure::Broken("frame larger than asked".into()));
                }
                bytes += rgba.len() as u64;
                if pics.len() as u32 >= spec.max_frames || bytes > spec.max_bytes {
                    break;
                }
                pics.push((time_ms, width, height, rgba));
            }
            Ok(Some(Msg::Failed { missing, text })) => {
                return Err(if missing {
                    Failure::MissingCodec(text)
                } else {
                    Failure::Broken(text)
                });
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(e) => return Err(Failure::Broken(format!("bad helper output: {e}"))),
        }
    }
    let Some(duration) = duration else {
        return Err(Failure::Unavailable("the helper exited".into()));
    };
    if pics.len() < 2 {
        return Err(Failure::Broken("fewer than two frames".into()));
    }
    let times: Vec<u64> = pics.iter().map(|p| p.0).collect();
    Ok(pics
        .into_iter()
        .zip(delays(&times, duration))
        .map(|((_, w, h, px), d)| (w, h, px, d))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: FrameSpec = FrameSpec {
        max_w: 400,
        max_h: 300,
        max_frames: 48,
        max_bytes: 6 * 1024 * 1024,
    };

    #[test]
    fn args_round_trip_and_garbage_is_refused() {
        let a = SPEC.to_args();
        assert_eq!(a[0], FRAMES_FLAG);
        assert_eq!(FrameSpec::parse(&a[1], &a[2], &a[3]), Some(SPEC));
        assert_eq!(FrameSpec::parse("400", "48", "1"), None);
        assert_eq!(FrameSpec::parse("0x300", "48", "1"), None);
        assert_eq!(FrameSpec::parse("400x300", "1", "1"), None);
        assert_eq!(FrameSpec::parse("400x300", "48", "0"), None);
        assert_eq!(FrameSpec::parse("99999x1", "48", "1"), None);
    }

    /// Feeds pictures every `step` ms up to `dur` and returns the kept times.
    fn run(s: &mut Sampler, step: u64, dur: u64) -> Vec<u64> {
        let mut kept = Vec::new();
        let mut t = 0;
        while t < dur && !s.done() {
            if s.keep(t) {
                kept.push(t);
            }
            t += step;
        }
        kept
    }

    #[test]
    fn a_short_clip_keeps_every_frame_up_to_30_fps() {
        // 2 s at 25 fps, small frames: all 50 fit... but 48 is the cap.
        let mut s = Sampler::new(SPEC, 2_000, 200 * 100 * 4).unwrap();
        let kept = run(&mut s, 40, 2_000);
        assert_eq!(kept.len(), 48);
        assert!(kept.windows(2).all(|w| w[1] - w[0] >= MIN_GAP_MS));
        // 60 fps source: thinned to about 30.
        let mut s = Sampler::new(SPEC, 1_000, 200 * 100 * 4).unwrap();
        let kept = run(&mut s, 16, 1_000);
        assert!((25..=31).contains(&kept.len()), "{}", kept.len());
    }

    #[test]
    fn a_long_clip_is_spread_not_cut() {
        // 10 s, frames of 400x225: the byte budget allows 17.
        let frame = 400 * 225 * 4;
        let mut s = Sampler::new(SPEC, 10_000, frame).unwrap();
        let kept = run(&mut s, 40, 10_000);
        let allowed = (SPEC.max_bytes / frame) as usize;
        assert_eq!(kept.len(), allowed);
        assert!(*kept.last().unwrap() > 8_000, "covers the clip: {kept:?}");
    }

    #[test]
    fn huge_frames_are_refused() {
        assert!(Sampler::new(SPEC, 1_000, SPEC.max_bytes).is_none());
    }

    #[test]
    fn delays_follow_the_times_and_end_with_the_clip() {
        let d = delays(&[0, 100, 250], 400);
        assert_eq!(
            d,
            [100, 150, 150].map(Duration::from_millis).to_vec(),
            "last frame lasts to the end"
        );
        // Unknown duration: the last frame gets the average gap.
        assert_eq!(delays(&[0, 100, 200], 0)[2], Duration::from_millis(100));
        // Equal times never give a zero delay.
        assert_eq!(delays(&[5, 5], 10)[0], Duration::from_millis(MIN_DELAY_MS));
    }

    fn stream(msgs: &[Msg]) -> Vec<u8> {
        let mut buf = Vec::new();
        for m in msgs {
            proto::write_msg(&mut buf, m).unwrap();
        }
        buf
    }

    fn pic(t: u64) -> Msg {
        Msg::Picture {
            time_ms: t,
            width: 2,
            height: 1,
            rgba: vec![1; 8],
        }
    }

    #[test]
    fn a_frames_stream_becomes_timed_frames() {
        let buf = stream(&[
            Msg::Ready {
                width: 2,
                height: 1,
                duration_ms: 300,
            },
            pic(0),
            pic(100),
            pic(200),
        ]);
        let f = read_frames(&mut buf.as_slice(), SPEC).unwrap();
        assert_eq!(f.len(), 3);
        assert!(f.iter().all(|x| x.3 == Duration::from_millis(100)));
    }

    #[test]
    fn failures_and_short_streams_are_errors() {
        let failed = stream(&[Msg::Failed {
            missing: true,
            text: "VP9".into(),
        }]);
        assert_eq!(
            read_frames(&mut failed.as_slice(), SPEC),
            Err(Failure::MissingCodec("VP9".into()))
        );
        let one = stream(&[
            Msg::Ready {
                width: 2,
                height: 1,
                duration_ms: 300,
            },
            pic(0),
        ]);
        assert!(read_frames(&mut one.as_slice(), SPEC).is_err());
        assert!(matches!(
            read_frames(&mut [].as_slice(), SPEC),
            Err(Failure::Unavailable(_))
        ));
        let big = stream(&[
            Msg::Ready {
                width: 2,
                height: 1,
                duration_ms: 300,
            },
            Msg::Picture {
                time_ms: 0,
                width: 401,
                height: 1,
                rgba: vec![0; 401 * 4],
            },
        ]);
        assert!(read_frames(&mut big.as_slice(), SPEC).is_err());
    }
}
