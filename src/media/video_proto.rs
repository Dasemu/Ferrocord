//! Wire format between the app and the `ferrocord-player` helper process.
//!
//! The helper decodes video (OpenH264, pure-Rust VP8/VP9, AAC and Opus;
//! linked into the helper only), plays the sound itself and writes messages
//! to its stdout; the app sends line-based commands to its stdin. Keeping the
//! decoders in a separate process keeps the main binary light, isolates a
//! decoder crash from the app, and closing a video frees every byte the
//! decoder held by ending the process.
//!
//! Helper → app: a tag byte, then little-endian fields.
//!
//! | tag | fields |
//! |---|---|
//! | `R` ready | `u32` width, `u32` height, `u64` duration ms (0 = unknown) |
//! | `F` frame | `u32` width, `u32` height, `w*h*4` RGBA bytes |
//! | `P` timed frame | `u64` time ms, then the fields of `F` (frames mode only) |
//! | `T` time | `u64` position ms, `u64` duration ms, `u8` state |
//! | `E` failed | `u8` kind (0 error, 1 missing decoder), `u32` length, UTF-8 text |

use std::io::{self, Read, Write};

/// Largest frame edge accepted (pixels).
pub const MAX_DIM: u32 = 4096;
/// Largest frame accepted (bytes).
pub const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;
/// Longest error text accepted (bytes).
pub const MAX_TEXT: usize = 2048;

/// What the player is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlayState {
    /// Paused (also before playback starts).
    #[default]
    Paused = 0,
    /// Playing.
    Playing = 1,
    /// Reached the end.
    Ended = 2,
    /// Waiting for data.
    Buffering = 3,
}

impl PlayState {
    fn from_u8(b: u8) -> Self {
        match b {
            1 => Self::Playing,
            2 => Self::Ended,
            3 => Self::Buffering,
            _ => Self::Paused,
        }
    }
}

/// A message from the helper.
#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    /// The first frame is ready: size and duration are known.
    Ready {
        /// Frame width.
        width: u32,
        /// Frame height.
        height: u32,
        /// Duration in ms, 0 when unknown (live or broken containers).
        duration_ms: u64,
    },
    /// One decoded frame.
    Frame {
        /// Width.
        width: u32,
        /// Height.
        height: u32,
        /// RGBA pixels, `width * height * 4` bytes.
        rgba: Vec<u8>,
    },
    /// A frame with its presentation time (frames mode, see
    /// [`crate::media::gifv`]).
    Picture {
        /// Presentation time in ms.
        time_ms: u64,
        /// Width.
        width: u32,
        /// Height.
        height: u32,
        /// RGBA pixels, `width * height * 4` bytes.
        rgba: Vec<u8>,
    },
    /// Playback position.
    Time {
        /// Position in ms.
        position_ms: u64,
        /// Duration in ms (0 = unknown).
        duration_ms: u64,
        /// State.
        state: PlayState,
    },
    /// Playback failed; the helper exits afterwards.
    Failed {
        /// A decoder / demuxer plug-in is missing.
        missing: bool,
        /// Human-readable detail (never contains file names or paths).
        text: String,
    },
}

fn u32le(r: &mut impl Read) -> io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn u64le(r: &mut impl Read) -> io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// Size and pixels of a frame, checked before anything is allocated.
fn read_pixels<R: Read>(r: &mut R) -> io::Result<(u32, u32, Vec<u8>)> {
    let (width, height) = (u32le(r)?, u32le(r)?);
    if width == 0 || height == 0 || width > MAX_DIM || height > MAX_DIM {
        return Err(bad("frame size out of range"));
    }
    let len = width as usize * height as usize * 4;
    if len > MAX_FRAME_BYTES {
        return Err(bad("frame too large"));
    }
    let mut rgba = vec![0u8; len];
    r.read_exact(&mut rgba)?;
    Ok((width, height, rgba))
}

/// Reads one message; `Ok(None)` at a clean end of stream.
pub fn read_msg<R: Read>(r: &mut R) -> io::Result<Option<Msg>> {
    let mut tag = [0u8; 1];
    if r.read(&mut tag)? == 0 {
        return Ok(None);
    }
    Ok(Some(match tag[0] {
        b'R' => Msg::Ready {
            width: u32le(r)?,
            height: u32le(r)?,
            duration_ms: u64le(r)?,
        },
        b'F' => {
            let (width, height, rgba) = read_pixels(r)?;
            Msg::Frame {
                width,
                height,
                rgba,
            }
        }
        b'P' => {
            let time_ms = u64le(r)?;
            let (width, height, rgba) = read_pixels(r)?;
            Msg::Picture {
                time_ms,
                width,
                height,
                rgba,
            }
        }
        b'T' => {
            let position_ms = u64le(r)?;
            let duration_ms = u64le(r)?;
            let mut s = [0u8; 1];
            r.read_exact(&mut s)?;
            Msg::Time {
                position_ms,
                duration_ms,
                state: PlayState::from_u8(s[0]),
            }
        }
        b'E' => {
            let mut k = [0u8; 1];
            r.read_exact(&mut k)?;
            let len = u32le(r)? as usize;
            if len > MAX_TEXT {
                return Err(bad("error text too long"));
            }
            let mut text = vec![0u8; len];
            r.read_exact(&mut text)?;
            Msg::Failed {
                missing: k[0] == 1,
                text: String::from_utf8_lossy(&text).into_owned(),
            }
        }
        _ => return Err(bad("unknown message tag")),
    }))
}

/// Writes a frame without copying the pixels into a [`Msg`].
pub fn write_frame<W: Write>(w: &mut W, width: u32, height: u32, rgba: &[u8]) -> io::Result<()> {
    w.write_all(b"F")?;
    w.write_all(&width.to_le_bytes())?;
    w.write_all(&height.to_le_bytes())?;
    w.write_all(rgba)?;
    w.flush()
}

/// Writes a timed frame (frames mode) without copying the pixels.
pub fn write_picture<W: Write>(
    w: &mut W,
    time_ms: u64,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> io::Result<()> {
    w.write_all(b"P")?;
    w.write_all(&time_ms.to_le_bytes())?;
    w.write_all(&width.to_le_bytes())?;
    w.write_all(&height.to_le_bytes())?;
    w.write_all(rgba)?;
    w.flush()
}

/// Writes any message.
pub fn write_msg<W: Write>(w: &mut W, m: &Msg) -> io::Result<()> {
    match m {
        Msg::Frame {
            width,
            height,
            rgba,
        } => return write_frame(w, *width, *height, rgba),
        Msg::Picture {
            time_ms,
            width,
            height,
            rgba,
        } => return write_picture(w, *time_ms, *width, *height, rgba),
        Msg::Ready {
            width,
            height,
            duration_ms,
        } => {
            w.write_all(b"R")?;
            w.write_all(&width.to_le_bytes())?;
            w.write_all(&height.to_le_bytes())?;
            w.write_all(&duration_ms.to_le_bytes())?;
        }
        Msg::Time {
            position_ms,
            duration_ms,
            state,
        } => {
            w.write_all(b"T")?;
            w.write_all(&position_ms.to_le_bytes())?;
            w.write_all(&duration_ms.to_le_bytes())?;
            w.write_all(&[*state as u8])?;
        }
        Msg::Failed { missing, text } => {
            let t = &text.as_bytes()[..text.len().min(MAX_TEXT)];
            w.write_all(b"E")?;
            w.write_all(&[u8::from(*missing)])?;
            w.write_all(&(t.len() as u32).to_le_bytes())?;
            w.write_all(t)?;
        }
    }
    w.flush()
}

/// A command from the app to the helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    /// Resume (from the start when the end was reached).
    Play,
    /// Pause.
    Pause,
    /// Seek to a position in ms.
    Seek(u64),
    /// Volume 0..=100.
    Volume(u8),
    /// Mute / unmute.
    Mute(bool),
    /// Stop and exit.
    Quit,
}

impl Cmd {
    /// The command as one line (with the newline).
    pub fn to_line(self) -> String {
        match self {
            Cmd::Play => "play\n".into(),
            Cmd::Pause => "pause\n".into(),
            Cmd::Seek(ms) => format!("seek {ms}\n"),
            Cmd::Volume(v) => format!("volume {}\n", v.min(100)),
            Cmd::Mute(m) => format!("mute {}\n", u8::from(m)),
            Cmd::Quit => "quit\n".into(),
        }
    }

    /// Parses one line; `None` for anything unknown.
    pub fn parse(line: &str) -> Option<Cmd> {
        let mut it = line.split_whitespace();
        let cmd = it.next()?;
        let arg = it.next();
        Some(match (cmd, arg) {
            ("play", None) => Cmd::Play,
            ("pause", None) => Cmd::Pause,
            ("quit", None) => Cmd::Quit,
            ("seek", Some(a)) => Cmd::Seek(a.parse().ok()?),
            ("volume", Some(a)) => Cmd::Volume(a.parse::<u8>().ok()?.min(100)),
            ("mute", Some("0")) => Cmd::Mute(false),
            ("mute", Some("1")) => Cmd::Mute(true),
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn round_trip(m: Msg) -> Msg {
        let mut buf = Vec::new();
        write_msg(&mut buf, &m).unwrap();
        read_msg(&mut Cursor::new(buf)).unwrap().unwrap()
    }

    #[test]
    fn messages_round_trip() {
        let msgs = [
            Msg::Ready {
                width: 320,
                height: 180,
                duration_ms: 3000,
            },
            Msg::Frame {
                width: 2,
                height: 1,
                rgba: vec![1, 2, 3, 4, 5, 6, 7, 8],
            },
            Msg::Picture {
                time_ms: 40,
                width: 1,
                height: 1,
                rgba: vec![9, 8, 7, 255],
            },
            Msg::Time {
                position_ms: 1234,
                duration_ms: 5000,
                state: PlayState::Playing,
            },
            Msg::Failed {
                missing: true,
                text: "H.264 decoder".into(),
            },
        ];
        for m in msgs {
            assert_eq!(round_trip(m.clone()), m);
        }
    }

    #[test]
    fn stream_of_messages_ends_cleanly() {
        let mut buf = Vec::new();
        write_msg(
            &mut buf,
            &Msg::Time {
                position_ms: 1,
                duration_ms: 2,
                state: PlayState::Ended,
            },
        )
        .unwrap();
        let mut c = Cursor::new(buf);
        assert!(read_msg(&mut c).unwrap().is_some());
        assert!(read_msg(&mut c).unwrap().is_none());
    }

    #[test]
    fn hostile_input_is_rejected_not_trusted() {
        // Unknown tag.
        assert!(read_msg(&mut Cursor::new(vec![b'Z'])).is_err());
        // A frame claiming to be huge must not allocate.
        let mut huge = vec![b'F'];
        huge.extend(MAX_DIM.to_le_bytes());
        huge.extend(MAX_DIM.to_le_bytes());
        assert!(read_msg(&mut Cursor::new(huge)).is_err());
        let mut zero = vec![b'F'];
        zero.extend(0u32.to_le_bytes());
        zero.extend(10u32.to_le_bytes());
        assert!(read_msg(&mut Cursor::new(zero)).is_err());
        let mut wide = vec![b'F'];
        wide.extend((MAX_DIM + 1).to_le_bytes());
        wide.extend(1u32.to_le_bytes());
        assert!(read_msg(&mut Cursor::new(wide)).is_err());
        let mut timed = vec![b'P'];
        timed.extend(0u64.to_le_bytes());
        timed.extend(MAX_DIM.to_le_bytes());
        timed.extend(MAX_DIM.to_le_bytes());
        assert!(read_msg(&mut Cursor::new(timed)).is_err());
        // Truncated frame body.
        let mut cut = vec![b'F'];
        cut.extend(2u32.to_le_bytes());
        cut.extend(2u32.to_le_bytes());
        cut.extend([0u8; 5]);
        assert!(read_msg(&mut Cursor::new(cut)).is_err());
        // Oversized error text.
        let mut long = vec![b'E', 0];
        long.extend(((MAX_TEXT + 1) as u32).to_le_bytes());
        assert!(read_msg(&mut Cursor::new(long)).is_err());
    }

    #[test]
    fn commands_round_trip_and_reject_garbage() {
        for c in [
            Cmd::Play,
            Cmd::Pause,
            Cmd::Seek(12_345),
            Cmd::Volume(55),
            Cmd::Mute(true),
            Cmd::Mute(false),
            Cmd::Quit,
        ] {
            assert_eq!(Cmd::parse(&c.to_line()), Some(c));
        }
        // Out of range values do not parse as u8 or are clamped.
        assert_eq!(Cmd::parse("volume 250"), Some(Cmd::Volume(100)));
        assert_eq!(Cmd::parse("volume 999"), None);
        assert_eq!(Cmd::Volume(250).to_line(), "volume 100\n");
        assert_eq!(Cmd::parse("seek -5"), None);
        assert_eq!(Cmd::parse("rm -rf"), None);
        assert_eq!(Cmd::parse(""), None);
        assert_eq!(Cmd::parse("play now"), None);
    }
}
