//! Video decoder helper for Ferrocord, with no system codec dependency.
//!
//! `ferrocord-player <file>` plays one MP4 / QuickTime (H.264 or VP9 + AAC)
//! or WebM / Matroska (VP8 / VP9 / H.264 and Opus) file: H.264 through OpenH264 (Cisco's BSD-licensed decoder, built
//! from source and linked statically), VP8 and VP9 through pure-Rust
//! decoders, AAC through symphonia (pure Rust), Opus through a statically
//! linked libopus, sound through cpal. It writes RGBA frames and playback
//! times to stdout and reads commands from stdin; the protocol is in
//! `ferrocord::media::video_proto`. The app spawns one helper per playing
//! video and ends it when the video closes, so a decoder never outlives its
//! video and the main process links no codec.
//!
//! `ferrocord-player --frames <w>x<h> <frames> <bytes> <file>` is the
//! *frames mode* used for `gifv` embeds (`ferrocord::media::gifv`): picture
//! only, decoded as fast as possible, scaled to fit `w` x `h`, evenly
//! spaced frames within the budget written as timed pictures, then exit.
//!
//! The codecs live in [`codec`] (with the H.264 SPS checks in [`h264`]) and
//! the demuxers in [`mp4`] / [`webm`], free of any playback logic, so a
//! second binary can reuse them.
//!
//! Failures name the step that failed (open, container, codec, decode) so
//! the viewer can say what happened.

mod audio;
mod codec;
mod h264;
mod mp4;
mod source;
mod webm;
mod yuv;

fn main() {
    player::run();
}

mod player {
    use std::collections::VecDeque;
    use std::io::{self, BufRead, BufWriter, Write};
    use std::path::Path;
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::time::{Duration, Instant};

    use ferrocord::media::gifv::{FRAMES_FLAG, FrameSpec, Sampler};
    use ferrocord::media::video_proto::{self as proto, Cmd, Msg, PlayState};

    use super::audio::{self, AHEAD_MS, AudioSource, Output};
    use super::codec::{self, Geometry, Pictures, VideoDecoder};
    use super::source::{self, Container, Packet, PacketSource, TrackError, VideoInfo};
    use super::yuv::{self, Matrix};
    use super::{mp4, webm};

    /// Widest / tallest frame sent to the app (aspect ratio is kept). The
    /// viewer scales it up; decoding and piping more would only cost RAM.
    const MAX_W: usize = 960;
    const MAX_H: usize = 540;
    /// Largest coded picture accepted (pixels per side / in total).
    const MAX_SIDE: usize = 8192;
    const MAX_PIXELS: usize = 8192 * 4608;
    /// How often the position is reported.
    const TIME_EVERY: Duration = Duration::from_millis(250);
    /// Frames are sent at most this often (about 33 per second).
    const FRAME_GAP: Duration = Duration::from_millis(30);
    /// A sound device that consumed nothing for this long is not driving
    /// the clock.
    const STALL_AFTER: Duration = Duration::from_millis(900);
    /// Consecutive undecodable packets before giving up.
    const MAX_BAD: usize = 90;

    type Out = BufWriter<std::fs::File>;

    pub fn run() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let (path, frames) = match args.as_slice() {
            [flag, b, n, bytes, path] if flag == FRAMES_FLAG => {
                match FrameSpec::parse(b, n, bytes) {
                    Some(spec) => (path.clone(), Some(spec)),
                    None => usage(),
                }
            }
            [path] if !path.starts_with("--") => (path.clone(), None),
            _ => usage(),
        };
        // stdout as a plain file: no line buffering for binary frames.
        let out_file = match stdout_file() {
            Ok(f) => f,
            Err(_) => std::process::exit(2),
        };
        let mut out: Out = BufWriter::with_capacity(1 << 20, out_file);
        // A hostile file must never take the app down, and a bug in a
        // demuxer must still end with a message instead of silence.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match frames {
            Some(spec) => run_frames(Path::new(&path), spec, &mut out),
            None => Player::open(Path::new(&path)).and_then(|p| p.run(&mut out)),
        }));
        let failure = match result {
            Ok(Ok(())) => return,
            Ok(Err(f)) => f,
            Err(_) => fail("the decoder stopped on malformed data"),
        };
        send(&mut out, &failure);
        std::process::exit(1);
    }

    /// A duplicate of the stdout handle as a plain file.
    #[cfg(unix)]
    fn stdout_file() -> io::Result<std::fs::File> {
        use std::os::fd::AsFd;
        io::stdout()
            .as_fd()
            .try_clone_to_owned()
            .map(std::fs::File::from)
    }

    /// A duplicate of the stdout handle as a plain file (a pipe from the
    /// app: binary, no console translation).
    #[cfg(windows)]
    fn stdout_file() -> io::Result<std::fs::File> {
        use std::os::windows::io::AsHandle;
        io::stdout()
            .as_handle()
            .try_clone_to_owned()
            .map(std::fs::File::from)
    }

    fn usage() -> ! {
        eprintln!("usage: ferrocord-player [--frames <w>x<h> <frames> <bytes>] <file>");
        std::process::exit(2);
    }

    /// Frames mode: the picture track only, decoded as fast as it goes,
    /// with the frames the [`Sampler`] keeps written as timed pictures.
    fn run_frames(path: &Path, spec: FrameSpec, out: &mut Out) -> Result<(), Msg> {
        if !path.is_file() {
            return Err(fail("the video file is missing"));
        }
        let container = source::detect(path).map_err(track_failure)?;
        let duration_ms = match container {
            Container::Mp4 => mp4::duration_ms(path).map_err(track_failure)?,
            Container::Webm => 0,
        };
        let Some(mut v) = open_picture(path, container, spec.max_w as usize, spec.max_h as usize)?
        else {
            return Err(fail("the file has no picture"));
        };
        let duration_ms = if duration_ms == 0 {
            v.info.duration_ms
        } else {
            duration_ms
        };
        let (w, h) = (v.out_w as u32, v.out_h as u32);
        let Some(mut sampler) = Sampler::new(spec, duration_ms, u64::from(w) * u64::from(h) * 4)
        else {
            return Err(fail("the clip is too large to loop"));
        };
        v.fill()?;
        if v.ready.is_none() {
            return Err(no_picture(&v));
        }
        send(
            out,
            &Msg::Ready {
                width: w,
                height: h,
                duration_ms,
            },
        );
        // Times count from the first picture (edit lists and B-frame delays
        // can start a stream above zero).
        let mut start = None;
        while let Some((t, px)) = v.ready.take() {
            let t = t.max(0) as u64;
            let t = t.saturating_sub(*start.get_or_insert(t));
            if sampler.keep(t)
                && proto::write_picture(out, t, w, h, &px)
                    .and_then(|()| out.flush())
                    .is_err()
            {
                // The app stopped reading: nothing left to do.
                std::process::exit(0);
            }
            if sampler.done() {
                break;
            }
            v.fill()?;
        }
        Ok(())
    }

    fn send(out: &mut Out, m: &Msg) {
        if proto::write_msg(out, m).and_then(|()| out.flush()).is_err() {
            // The app is gone (broken pipe): nothing left to play for.
            std::process::exit(0);
        }
    }

    fn fail(text: impl Into<String>) -> Msg {
        Msg::Failed {
            missing: false,
            text: text.into(),
        }
    }

    /// The decoder cannot handle this stream (`what` names it).
    fn unsupported(what: String) -> Msg {
        Msg::Failed {
            missing: true,
            text: what,
        }
    }

    /// The whole stream went through the decoder and no picture came out.
    fn no_picture(v: &Video) -> Msg {
        if let Some(why) = v.decoder.unsupported() {
            return unsupported(why);
        }
        fail(v.decoder.no_picture_reason().unwrap_or_else(|| {
            format!(
                "no picture could be decoded from the {} stream",
                v.info.codec.name()
            )
        }))
    }

    fn track_failure(e: TrackError) -> Msg {
        match e {
            TrackError::Broken(why) => fail(why),
            TrackError::Unsupported(codec) => Msg::Failed {
                missing: true,
                text: codec,
            },
        }
    }

    /// The playback clock: wall time while playing, frozen while paused.
    /// With a sound device (`master`) the audio the device consumed drives
    /// it, so picture and sound stay together.
    struct Clock {
        base_ms: i64,
        since: Option<Instant>,
        master: bool,
        /// Sound consumed when the clock last started (the counter keeps
        /// its value across a pause).
        origin_ms: i64,
    }

    impl Clock {
        fn now(&self, sound: Option<&Output>) -> i64 {
            match (self.since, self.master, sound) {
                (None, _, _) => self.base_ms,
                (Some(_), true, Some(s)) => self.base_ms + s.consumed_ms() - self.origin_ms,
                (Some(t), _, _) => self.base_ms + t.elapsed().as_millis() as i64,
            }
        }
        /// Jumps to `ms` (after a seek; the sound queue was cleared too).
        fn set(&mut self, ms: i64) {
            self.base_ms = ms;
            self.origin_ms = 0;
            if self.since.is_some() {
                self.since = Some(Instant::now());
            }
        }
        fn play(&mut self, sound: Option<&Output>) {
            if self.since.is_none() {
                self.since = Some(Instant::now());
                self.origin_ms = sound.map_or(0, Output::consumed_ms);
            }
        }
        fn pause(&mut self, sound: Option<&Output>) {
            self.base_ms = self.now(sound);
            self.since = None;
        }
        /// The sound ran out before the picture: carry on by wall time.
        fn detach_audio(&mut self, sound: Option<&Output>) {
            if self.master {
                self.base_ms = self.now(sound);
                if self.since.is_some() {
                    self.since = Some(Instant::now());
                }
                self.master = false;
            }
        }
        fn playing(&self) -> bool {
            self.since.is_some()
        }
    }

    struct Video {
        source: Box<dyn PacketSource>,
        decoder: Box<dyn VideoDecoder>,
        info: VideoInfo,
        packet: Packet,
        /// Decoded pictures not yet taken.
        queue: Pictures,
        /// Decoded picture waiting for its time.
        ready: Option<(i64, Vec<u8>)>,
        out_w: usize,
        out_h: usize,
        bad: usize,
        eos: bool,
        flushed: bool,
    }

    impl Video {
        /// Decodes until one picture is ready (or the stream ends).
        /// `Err` after too many consecutive undecodable packets.
        fn fill(&mut self) -> Result<(), Msg> {
            while self.ready.is_none() {
                if let Some(t) = self.queue.pop_front() {
                    self.ready = Some(t);
                    return Ok(());
                }
                if self.eos {
                    if self.flushed {
                        return Ok(());
                    }
                    self.flushed = true;
                    let mut tail = VecDeque::new();
                    self.decoder.flush(&mut tail);
                    self.queue = tail;
                    continue;
                }
                match self.source.next(&mut self.packet) {
                    Ok(true) => {}
                    Ok(false) => {
                        self.eos = true;
                        continue;
                    }
                    Err(e) => return Err(fail(e)),
                }
                let decoded = self.decoder.decode(&self.packet, &mut self.queue);
                if self.queue.is_empty()
                    && let Some(why) = self.decoder.unsupported()
                {
                    // Nothing in this stream will ever decode: say why now
                    // instead of reading the whole file first.
                    return Err(unsupported(why));
                }
                match decoded {
                    Ok(()) => self.bad = 0,
                    Err(e) => {
                        self.bad += 1;
                        if self.bad >= MAX_BAD {
                            return Err(fail(format!(
                                "decoding failed at {:.1} s: {e}",
                                self.packet.pts_ms as f64 / 1000.0
                            )));
                        }
                    }
                }
            }
            Ok(())
        }

        /// Restarts decoding from the keyframe before `ms`.
        fn seek(&mut self, ms: i64) -> Result<(), Msg> {
            self.source.seek(ms).map_err(fail)?;
            self.decoder.reset().map_err(fail)?;
            self.queue.clear();
            self.ready = None;
            self.eos = false;
            self.flushed = false;
            self.bad = 0;
            Ok(())
        }

        fn ended(&self) -> bool {
            self.ready.is_none() && self.queue.is_empty() && self.eos && self.flushed
        }
    }

    struct Player {
        video: Option<Video>,
        audio: Option<Box<dyn AudioSource>>,
        duration_ms: u64,
    }

    type Opened = Option<(Box<dyn PacketSource>, VideoInfo)>;

    /// The picture track with a decoder whose frames fit `max_w` x `max_h`;
    /// `None` for a file without one.
    fn open_picture(
        path: &Path,
        container: Container,
        max_w: usize,
        max_h: usize,
    ) -> Result<Option<Video>, Msg> {
        let Some((source, info)) = open_video(path, container).map_err(track_failure)? else {
            return Ok(None);
        };
        let (dw, dh) = info.rotation.display(info.width, info.height);
        if dw == 0 || dh == 0 {
            return Err(fail("the video has no picture size"));
        }
        if dw > MAX_SIDE || dh > MAX_SIDE || dw * dh > MAX_PIXELS {
            return Err(fail(format!(
                "the video is too large ({dw}x{dh}; the limit is {MAX_SIDE}x{MAX_SIDE})"
            )));
        }
        let (out_w, out_h) = yuv::fit(dw, dh, max_w, max_h);
        let geo = Geometry {
            rotation: info.rotation,
            out_w,
            out_h,
            matrix: Matrix::for_height(info.height),
        };
        let decoder = codec::new_decoder(&info, geo).map_err(fail)?;
        if let Some(why) = decoder.unsupported() {
            return Err(unsupported(why));
        }
        Ok(Some(Video {
            source,
            decoder,
            info,
            packet: Packet::default(),
            queue: VecDeque::new(),
            ready: None,
            out_w,
            out_h,
            bad: 0,
            eos: false,
            flushed: false,
        }))
    }

    fn open_video(path: &Path, container: Container) -> Result<Opened, TrackError> {
        Ok(match container {
            Container::Mp4 => mp4::Mp4Video::open(path)?.map(|v| {
                let info = v.info.clone();
                (Box::new(v) as Box<dyn PacketSource>, info)
            }),
            Container::Webm => webm::WebmVideo::open(path)?.map(|v| {
                let info = v.info.clone();
                (Box::new(v) as Box<dyn PacketSource>, info)
            }),
        })
    }

    impl Player {
        fn open(path: &Path) -> Result<Self, Msg> {
            if !path.is_file() {
                return Err(fail("the video file is missing"));
            }
            let container = source::detect(path).map_err(track_failure)?;
            let mut duration_ms = match container {
                Container::Mp4 => mp4::duration_ms(path).map_err(track_failure)?,
                Container::Webm => 0,
            };
            let video = open_picture(path, container, MAX_W, MAX_H)?;
            if duration_ms == 0
                && let Some(v) = &video
            {
                duration_ms = v.info.duration_ms;
            }
            let audio = match audio::open(path, container) {
                Ok(a) => a,
                Err(e) => {
                    // The picture still plays; say why there is no sound.
                    eprintln!("ferrocord-player: no sound: {e}");
                    None
                }
            };
            if video.is_none() && audio.is_none() {
                return Err(fail("the file has no playable video or audio"));
            }
            Ok(Self {
                video,
                audio,
                duration_ms,
            })
        }

        fn run(mut self, out: &mut Out) -> Result<(), Msg> {
            let (tx, rx) = mpsc::channel::<Cmd>();
            std::thread::spawn(move || {
                for line in io::stdin().lock().lines() {
                    match line {
                        Ok(l) => {
                            if let Some(c) = Cmd::parse(&l)
                                && tx.send(c).is_err()
                            {
                                return;
                            }
                        }
                        Err(_) => break,
                    }
                }
                // stdin closed: the app is gone.
                let _ = tx.send(Cmd::Quit);
            });

            // First picture: it gives the size for `Ready` and is shown at once.
            let (width, height) = match self.video.as_mut() {
                Some(v) => {
                    v.fill()?;
                    if v.ready.is_none() {
                        return Err(no_picture(v));
                    }
                    (v.out_w as u32, v.out_h as u32)
                }
                None => (0, 0),
            };
            send(
                out,
                &Msg::Ready {
                    width,
                    height,
                    duration_ms: self.duration_ms,
                },
            );
            let sound = self.audio.as_ref().and_then(|_| Output::open());
            let sound = sound.as_ref();
            let mut volume = 1.0f32;
            let mut muted = false;
            let mut clock = Clock {
                base_ms: 0,
                since: None,
                master: sound.is_some(),
                origin_ms: 0,
            };
            let mut want_play = true;
            let mut ended = false;
            // Show the picture at the clock even while paused (after a seek).
            let mut show_now = true;
            let mut last_time = Instant::now() - TIME_EVERY;
            let mut last_frame = Instant::now() - FRAME_GAP;

            loop {
                if want_play && !ended && !clock.playing() {
                    clock.play(sound);
                    if let Some(s) = sound {
                        s.set_playing(true);
                    }
                }
                let wait = match self.video.as_ref().and_then(|v| v.ready.as_ref()) {
                    Some((t, _)) if clock.playing() => (t - clock.now(sound)).clamp(1, 15) as u64,
                    _ => 15,
                };
                match rx.recv_timeout(Duration::from_millis(wait)) {
                    Ok(Cmd::Quit) | Err(RecvTimeoutError::Disconnected) => return Ok(()),
                    Ok(Cmd::Play) => {
                        want_play = true;
                        if ended {
                            ended = false;
                            self.seek(0, sound, &mut clock)?;
                            show_now = true;
                        }
                    }
                    Ok(Cmd::Pause) => {
                        want_play = false;
                        clock.pause(sound);
                        if let Some(s) = sound {
                            s.set_playing(false);
                        }
                    }
                    Ok(Cmd::Seek(ms)) => {
                        ended = false;
                        let ms = ms.min(self.duration_ms.max(1));
                        self.seek(ms as i64, sound, &mut clock)?;
                        show_now = true;
                    }
                    Ok(Cmd::Volume(v)) => {
                        volume = f32::from(v.min(100)) / 100.0;
                        if let Some(s) = sound {
                            s.set_volume(if muted { 0.0 } else { volume });
                        }
                    }
                    Ok(Cmd::Mute(m)) => {
                        muted = m;
                        if let Some(s) = sound {
                            s.set_volume(if muted { 0.0 } else { volume });
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                }

                // Audio first: it drives the clock.
                if let (Some(a), Some(s)) = (self.audio.as_mut(), sound)
                    && clock.playing()
                {
                    let (ch, rate) = a.format();
                    let now = clock.now(sound);
                    while !a.done() && a.decoded_until() < now + AHEAD_MS {
                        match a.next_chunk() {
                            Some((_, samples)) => s.push(ch, rate, &samples),
                            None => break,
                        }
                    }
                    if a.done() && s.queued_frames() < 2 {
                        clock.detach_audio(sound);
                    }
                }
                // A sound stream whose callbacks never run (a device that
                // opened but is not playing) must not freeze the picture:
                // fall back to the wall clock.
                if clock.master
                    && let (Some(t), Some(s)) = (clock.since, sound)
                    && t.elapsed() > STALL_AFTER
                    && s.consumed_ms() == clock.origin_ms
                {
                    clock.detach_audio(sound);
                }
                let now = clock.now(sound);
                if let Some(v) = self.video.as_mut() {
                    v.fill()?;
                    // Late pictures are skipped (decoded, not sent) so a slow
                    // machine stays in sync instead of drifting behind.
                    let mut frame = None;
                    while let Some((t, _)) = &v.ready {
                        if *t > now && !(show_now && frame.is_none()) {
                            break;
                        }
                        frame = v.ready.take();
                        v.fill()?;
                        if show_now {
                            break;
                        }
                    }
                    if let Some((_, px)) = frame
                        && (show_now || last_frame.elapsed() >= FRAME_GAP)
                    {
                        show_now = false;
                        last_frame = Instant::now();
                        if proto::write_frame(out, v.out_w as u32, v.out_h as u32, &px)
                            .and_then(|()| out.flush())
                            .is_err()
                        {
                            std::process::exit(0);
                        }
                    }
                }

                let video_done = self.video.as_ref().is_none_or(Video::ended);
                let audio_done = self.audio.as_ref().is_none_or(|a| a.done()) || sound.is_none();
                if clock.playing()
                    && video_done
                    && audio_done
                    && (now as u64 >= self.duration_ms || self.video.is_some())
                {
                    ended = true;
                    want_play = false;
                    clock.pause(sound);
                    if let Some(s) = sound {
                        s.set_playing(false);
                    }
                    last_time = Instant::now() - TIME_EVERY;
                }

                if last_time.elapsed() >= TIME_EVERY {
                    last_time = Instant::now();
                    let state = if ended {
                        PlayState::Ended
                    } else if clock.playing() {
                        PlayState::Playing
                    } else {
                        PlayState::Paused
                    };
                    let pos = if ended {
                        self.duration_ms
                    } else {
                        (clock.now(sound).max(0) as u64).min(self.duration_ms.max(1))
                    };
                    send(
                        out,
                        &Msg::Time {
                            position_ms: pos,
                            duration_ms: self.duration_ms,
                            state,
                        },
                    );
                }
            }
        }

        fn seek(&mut self, ms: i64, sound: Option<&Output>, clock: &mut Clock) -> Result<(), Msg> {
            if let Some(s) = sound {
                s.clear();
            }
            clock.set(ms);
            // After a seek the sound is the master again.
            clock.master = sound.is_some();
            if let Some(v) = self.video.as_mut() {
                v.seek(ms)?;
                // Decode up to the target: pictures before it are not shown.
                loop {
                    v.fill()?;
                    match &v.ready {
                        Some((t, _)) if *t + 20 < ms => v.ready = None,
                        _ => break,
                    }
                }
            }
            if let Some(a) = self.audio.as_mut() {
                a.seek(ms.max(0) as u64);
            }
            Ok(())
        }
    }
}
