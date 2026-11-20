//! In-chat video playback: the state of the one active video and the
//! actions around it (download, helper process, controls).
//!
//! At most one video is active. Starting another, leaving the channel or
//! closing the player drops the previous one: the helper process ends (its
//! decoder, buffers and sound stream go with it) and the downloaded file is
//! deleted. When a video plays to its end the helper is released at once
//! (the file stays, so replaying is instant).
//!
//! Frames become [`image::Handle`]s here, in `update`, never in `view`.

use std::sync::Arc;

use futures::StreamExt;
use futures::channel::mpsc as fmpsc;
use iced::Task;
use iced::widget::image;
use tokio::sync::mpsc::UnboundedReceiver;

use super::message::{Done, Message, Ui};
use super::{App, Screen};
use crate::media::clip::Clip;
use crate::media::scratch::{self, Scratch};
use crate::media::video::{self, Event, Failure, Sink};
use crate::media::video_proto::{Cmd, PlayState};
use crate::net::model::ChannelId;

/// Largest video downloaded (bytes).
pub const MAX_DOWNLOAD: u64 = 200 * 1024 * 1024;

/// Where the active video is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// Fetching the file.
    Downloading {
        /// Bytes so far.
        received: u64,
        /// Size when the server said.
        total: Option<u64>,
    },
    /// The helper started; waiting for the first picture.
    Starting,
    /// Frames are flowing (or the video is paused / ended).
    Playing,
    /// Could not play.
    Failed {
        /// Text for the player.
        text: String,
    },
}

/// A helper event or download result of one playback session.
#[derive(Debug, Clone)]
pub enum Progress {
    /// Download progress.
    Download {
        /// Bytes so far.
        received: u64,
        /// Total when known.
        total: Option<u64>,
    },
    /// The download finished (the file is deleted when the last clone of
    /// the handle goes) or failed.
    Downloaded(Result<Arc<Scratch>, String>),
    /// From the helper.
    Helper(Event),
}

/// What the app must do after a state change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// Start the helper again on the downloaded file (replay, or a seek
    /// after the end).
    Respawn,
}

/// The active video.
pub struct Playback {
    /// What plays.
    pub clip: Clip,
    /// Channel of the message (leaving it stops the video).
    pub channel: ChannelId,
    /// Generation: events of older sessions are ignored.
    pub session: u32,
    /// Where it is.
    pub phase: Phase,
    /// Latest frame, created when it arrived (never inside `view`).
    pub frame: Option<image::Handle>,
    /// Frame size in pixels.
    pub frame_size: (u32, u32),
    /// Position in ms.
    pub position_ms: u64,
    /// Duration in ms (0 = unknown).
    pub duration_ms: u64,
    /// Player state.
    pub state: PlayState,
    /// Volume 0..=100.
    pub volume: u8,
    /// Muted.
    pub muted: bool,
    /// Position the user is dragging the slider to.
    pub seeking: Option<u64>,
    /// Shown full screen.
    pub fullscreen: bool,
    pending_seek: Option<u64>,
    host: Option<Box<dyn Sink>>,
    file: Option<Arc<Scratch>>,
    tasks: Vec<iced::task::Handle>,
}

impl std::fmt::Debug for Playback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Playback({}, {:?})", self.clip.id, self.phase)
    }
}

impl Playback {
    /// A video that is about to download.
    pub fn new(clip: Clip, channel: ChannelId, session: u32) -> Self {
        Self {
            clip,
            channel,
            session,
            phase: Phase::Downloading {
                received: 0,
                total: None,
            },
            frame: None,
            frame_size: (0, 0),
            position_ms: 0,
            duration_ms: 0,
            state: PlayState::Paused,
            volume: 100,
            muted: false,
            seeking: None,
            fullscreen: false,
            pending_seek: None,
            host: None,
            file: None,
            tasks: Vec::new(),
        }
    }

    /// Slider position in ms.
    pub fn progress_ms(&self) -> u64 {
        self.seeking.unwrap_or(self.position_ms)
    }

    /// Running (not paused, ended or failed)?
    pub fn is_playing(&self) -> bool {
        self.phase == Phase::Playing
            && matches!(self.state, PlayState::Playing | PlayState::Buffering)
    }

    /// Ended?
    pub fn is_ended(&self) -> bool {
        self.phase == Phase::Playing && self.state == PlayState::Ended
    }

    /// Controls (play, seek, volume) work.
    pub fn is_active(&self) -> bool {
        self.phase == Phase::Playing
    }

    /// Has a helper (or mock player) attached.
    pub fn has_host(&self) -> bool {
        self.host.is_some()
    }

    /// Attaches the player that receives commands.
    pub fn attach(&mut self, host: Box<dyn Sink>, file: Option<Arc<Scratch>>) {
        self.host = Some(host);
        if file.is_some() {
            self.file = file;
        }
        self.phase = Phase::Starting;
    }

    /// The downloaded file, for starting the helper again.
    pub fn file(&self) -> Option<&Arc<Scratch>> {
        self.file.as_ref()
    }

    /// Keeps `handle` so the task is cancelled with the playback.
    pub fn keep(&mut self, handle: iced::task::Handle) {
        self.tasks.push(handle);
    }

    fn send(&self, cmd: Cmd) {
        if let Some(h) = &self.host {
            h.send(cmd);
        }
    }

    /// Releases the helper (its process ends) but keeps the file.
    fn release_host(&mut self) {
        self.host = None;
    }

    /// Applies download / helper progress.
    pub fn apply(&mut self, p: Progress) {
        match p {
            Progress::Download { received, total } => {
                if matches!(self.phase, Phase::Downloading { .. }) {
                    self.phase = Phase::Downloading { received, total };
                }
            }
            Progress::Downloaded(Ok(file)) => self.file = Some(file),
            Progress::Downloaded(Err(text)) => {
                self.phase = Phase::Failed { text };
            }
            Progress::Helper(ev) => self.apply_event(ev),
        }
    }

    fn apply_event(&mut self, ev: Event) {
        match ev {
            Event::Ready {
                width,
                height,
                duration_ms,
            } => {
                self.phase = Phase::Playing;
                self.frame_size = (width, height);
                self.duration_ms = duration_ms;
                self.state = PlayState::Playing;
                if let Some(ms) = self.pending_seek.take() {
                    self.position_ms = ms;
                    self.send(Cmd::Seek(ms));
                }
                self.send(Cmd::Volume(self.volume));
                self.send(Cmd::Mute(self.muted));
            }
            Event::Frame {
                width,
                height,
                rgba,
            } => {
                self.frame_size = (width, height);
                self.frame = Some(image::Handle::from_rgba(width, height, rgba));
                if self.phase == Phase::Starting {
                    self.phase = Phase::Playing;
                }
            }
            Event::Time {
                position_ms,
                duration_ms,
                state,
            } => {
                if self.seeking.is_none() {
                    self.position_ms = position_ms;
                }
                if duration_ms > 0 {
                    self.duration_ms = duration_ms;
                }
                self.state = state;
                if state == PlayState::Ended {
                    // Free the decoder as soon as the video is over.
                    self.release_host();
                }
            }
            Event::Failed(f) => {
                self.phase = Phase::Failed {
                    text: f.user_message(),
                };
                self.release_host();
            }
        }
    }

    /// Play / pause (replay after the end).
    pub fn toggle(&mut self) -> Option<Intent> {
        if !self.is_active() {
            return None;
        }
        if self.state == PlayState::Ended {
            self.state = PlayState::Playing;
            self.position_ms = 0;
            return if self.host.is_some() {
                self.send(Cmd::Play);
                None
            } else {
                Some(Intent::Respawn)
            };
        }
        if self.is_playing() {
            self.send(Cmd::Pause);
            self.state = PlayState::Paused;
        } else {
            self.send(Cmd::Play);
            self.state = PlayState::Playing;
        }
        None
    }

    /// The slider is being dragged.
    pub fn seek_drag(&mut self, ms: u64) {
        if self.is_active() && self.duration_ms > 0 {
            self.seeking = Some(ms.min(self.duration_ms));
        }
    }

    /// The slider was released.
    pub fn seek_release(&mut self) -> Option<Intent> {
        let ms = self.seeking.take()?;
        self.position_ms = ms;
        if self.state == PlayState::Ended {
            self.state = PlayState::Paused;
        }
        if self.host.is_some() {
            self.send(Cmd::Seek(ms));
            None
        } else {
            self.pending_seek = Some(ms);
            Some(Intent::Respawn)
        }
    }

    /// Sets the volume (and unmutes).
    pub fn set_volume(&mut self, v: u8) {
        self.volume = v.min(100);
        self.muted = false;
        self.send(Cmd::Volume(self.volume));
        self.send(Cmd::Mute(false));
    }

    /// Mute / unmute.
    pub fn toggle_mute(&mut self) {
        self.muted = !self.muted;
        self.send(Cmd::Mute(self.muted));
    }
}

/// Events of one helper, newest frame wins: when the UI falls behind,
/// stale frames are skipped instead of queueing up in memory.
fn helper_stream(rx: UnboundedReceiver<Event>) -> impl futures::Stream<Item = Event> {
    futures::stream::unfold((rx, None::<Event>), |(mut rx, held)| async move {
        let mut ev = match held {
            Some(e) => e,
            None => rx.recv().await?,
        };
        let mut next_held = None;
        if matches!(ev, Event::Frame { .. }) {
            while let Ok(next) = rx.try_recv() {
                if matches!(next, Event::Frame { .. }) {
                    ev = next;
                } else {
                    next_held = Some(next);
                    break;
                }
            }
        }
        Some((ev, (rx, next_held)))
    })
}

impl App {
    /// The directory downloaded videos are kept in.
    fn video_dir(&self) -> std::path::PathBuf {
        self.cfg
            .data_dir
            .clone()
            .unwrap_or_else(std::env::temp_dir)
            .join("videos")
    }

    /// Removes videos an earlier run left behind (off the UI thread).
    pub(super) fn sweep_videos(&self) -> Task<Message> {
        if self.cfg.mock {
            return Task::none();
        }
        let dir = self.video_dir();
        Task::perform(
            async move {
                let _ =
                    tokio::task::spawn_blocking(move || scratch::sweep(&dir, scratch::STALE_AFTER))
                        .await;
            },
            |()| Message::Done(Done::Ignored),
        )
    }

    /// The video of the active playback, if `id` is it.
    pub fn playback_of(&self, id: &str) -> Option<&Playback> {
        self.video.as_ref().filter(|v| v.clip.id == id)
    }

    /// Starts playing `clip`: downloads it, then starts the helper.
    pub(super) fn play_clip(&mut self, clip: Clip) -> Task<Message> {
        let Some(channel) = self.open_channel() else {
            return Task::none();
        };
        // A second click on the active video just toggles it.
        if let Some(v) = self.video.as_mut()
            && v.clip.id == clip.id
            && !matches!(v.phase, Phase::Failed { .. })
        {
            let intent = v.toggle();
            return self.run_intent(intent);
        }
        let leave = self.close_video();
        self.video_session = self.video_session.wrapping_add(1);
        let session = self.video_session;
        let mut pb = Playback::new(clip.clone(), channel, session);
        if self.cfg.mock {
            let (player, rx) = video::spawn_mock(42_000);
            pb.attach(Box::new(player), None);
            self.video = Some(pb);
            return Task::batch([leave, self.helper_task(rx)]);
        }
        if !self.rest.is_allowed_media_url(&clip.url) {
            pb.phase = Phase::Failed {
                text: "This video is not hosted by Discord, so it is not played in the app. \
                       Choose \u{201c}Open externally\u{201d} to watch it in the browser."
                    .into(),
            };
            self.video = Some(pb);
            return leave;
        }
        let dir = self.video_dir();
        let ext = clip
            .name
            .rsplit('.')
            .next()
            .filter(|e| e.len() <= 5)
            .unwrap_or("bin")
            .to_owned();
        let rest = self.rest.clone();
        let url = clip.url.clone();
        let stream = iced::stream::channel::<Progress>(
            32,
            move |mut out: fmpsc::Sender<Progress>| async move {
                let prepared =
                    tokio::task::spawn_blocking(move || Scratch::create(&dir, &ext)).await;
                let (scratch, file) = match prepared {
                    Ok(Ok(x)) => x,
                    _ => {
                        let _ = out.try_send(Progress::Downloaded(Err(
                            "The video could not be saved for playback.".into(),
                        )));
                        return;
                    }
                };
                let mut tx = out.clone();
                let result = rest
                    .download_file(&url, file, MAX_DOWNLOAD, move |received, total| {
                        let _ = tx.try_send(Progress::Download { received, total });
                    })
                    .await;
                let msg = match result {
                    Ok(_) => Progress::Downloaded(Ok(Arc::new(scratch))),
                    Err(e) => {
                        tracing::warn!(error = %e, "video download failed");
                        drop(scratch);
                        Progress::Downloaded(Err(download_error_text(&e)))
                    }
                };
                let _ = out.try_send(msg);
            },
        );
        let (task, handle) = Task::stream(stream.map(move |p| {
            Message::Done(Done::Video {
                session,
                progress: p,
            })
        }))
        .abortable();
        pb.keep(handle);
        self.video = Some(pb);
        Task::batch([leave, task])
    }

    /// A stream task for one helper's events.
    fn helper_task(&mut self, rx: UnboundedReceiver<Event>) -> Task<Message> {
        let session = self.video_session;
        let (task, handle) = Task::stream(helper_stream(rx).map(move |ev| {
            Message::Done(Done::Video {
                session,
                progress: Progress::Helper(ev),
            })
        }))
        .abortable();
        if let Some(v) = self.video.as_mut() {
            v.keep(handle);
        }
        task
    }

    /// Download progress or a helper event arrived.
    pub(super) fn on_video(&mut self, session: u32, progress: Progress) -> Task<Message> {
        let Some(v) = self.video.as_mut().filter(|v| v.session == session) else {
            return Task::none();
        };
        if let Progress::Helper(Event::Failed(f)) = &progress {
            let step = match f {
                Failure::Unavailable(_) => "helper start",
                Failure::MissingCodec(_) => "unsupported codec",
                Failure::Broken(_) => "decode",
            };
            tracing::warn!(step, detail = %f.detail(), "video playback failed");
        }
        let downloaded = match &progress {
            Progress::Downloaded(Ok(file)) => Some(file.clone()),
            _ => None,
        };
        v.apply(progress);
        match downloaded {
            Some(file) => self.start_helper(file),
            None => Task::none(),
        }
    }

    /// Starts the helper on `file` for the active playback.
    fn start_helper(&mut self, file: Arc<Scratch>) -> Task<Message> {
        match video::spawn(file.path()) {
            Ok((player, rx)) => {
                if let Some(v) = self.video.as_mut() {
                    v.attach(Box::new(player), Some(file));
                }
                self.helper_task(rx)
            }
            Err(f) => {
                tracing::warn!(detail = %f.detail(), "video helper unavailable");
                if let Some(v) = self.video.as_mut() {
                    v.phase = Phase::Failed {
                        text: f.user_message(),
                    };
                }
                Task::none()
            }
        }
    }

    /// Carries out what a state change asked for.
    fn run_intent(&mut self, intent: Option<Intent>) -> Task<Message> {
        match intent {
            Some(Intent::Respawn) => self.respawn_video(),
            None => Task::none(),
        }
    }

    /// Starts a new helper session on the file already on disk.
    fn respawn_video(&mut self) -> Task<Message> {
        self.video_session = self.video_session.wrapping_add(1);
        let session = self.video_session;
        let mock = self.cfg.mock;
        let Some(v) = self.video.as_mut() else {
            return Task::none();
        };
        v.session = session;
        v.phase = Phase::Starting;
        if mock {
            let (player, rx) = video::spawn_mock(42_000);
            v.attach(Box::new(player), None);
            return self.helper_task(rx);
        }
        match v.file().cloned() {
            Some(f) => self.start_helper(f),
            None => Task::none(),
        }
    }

    /// Stops the active video: the helper ends and the file is deleted.
    pub(super) fn close_video(&mut self) -> Task<Message> {
        let Some(v) = self.video.take() else {
            return Task::none();
        };
        let leave = if v.fullscreen {
            self.set_fullscreen_window(false)
        } else {
            Task::none()
        };
        // Dropping the playback aborts its tasks, ends the helper and
        // removes the file.
        drop(v);
        leave
    }

    /// Leaving the channel of the video stops it.
    pub(super) fn stop_video_if_left(&mut self) -> Task<Message> {
        match (&self.video, self.open_channel()) {
            (Some(v), open) if open != Some(v.channel) || self.screen() != Screen::Main => {
                self.close_video()
            }
            _ => Task::none(),
        }
    }

    fn set_fullscreen_window(&self, on: bool) -> Task<Message> {
        if self.cfg.mock {
            return Task::none();
        }
        let mode = if on {
            iced::window::Mode::Fullscreen
        } else {
            iced::window::Mode::Windowed
        };
        match self.window_id {
            Some(id) => iced::window::set_mode(id, mode),
            None => Task::none(),
        }
    }

    /// Handles the player's controls; `Err(ui)` for everything else.
    pub(super) fn on_ui_video(&mut self, ui: Ui) -> Result<Task<Message>, Ui> {
        Ok(match ui {
            Ui::PlayVideo(clip) => self.play_clip(*clip),
            Ui::CloseVideo => self.close_video(),
            Ui::VideoToggle => {
                let intent = self.video.as_mut().and_then(Playback::toggle);
                self.run_intent(intent)
            }
            Ui::VideoSeekDrag(ms) => {
                if let Some(v) = self.video.as_mut() {
                    v.seek_drag(ms);
                }
                Task::none()
            }
            Ui::VideoSeekRelease => {
                let intent = self.video.as_mut().and_then(Playback::seek_release);
                self.run_intent(intent)
            }
            Ui::VideoVolume(vol) => {
                if let Some(v) = self.video.as_mut() {
                    v.set_volume(vol);
                }
                Task::none()
            }
            Ui::VideoMute => {
                if let Some(v) = self.video.as_mut() {
                    v.toggle_mute();
                }
                Task::none()
            }
            Ui::VideoFullscreen => {
                let on = match self.video.as_mut() {
                    Some(v) if v.is_active() || v.frame.is_some() => {
                        v.fullscreen = !v.fullscreen;
                        v.fullscreen
                    }
                    _ => return Ok(Task::none()),
                };
                self.set_fullscreen_window(on)
            }
            other => return Err(other),
        })
    }

    /// Puts the mock UI in the state the `video` screenshots show: the
    /// showcase video playing (paused at 0:12 so the picture is still).
    pub(super) fn mock_video_screen(&mut self, fullscreen: bool) {
        struct Quiet;
        impl Sink for Quiet {
            fn send(&self, _: Cmd) {}
        }
        let Some(channel) = self.open_channel() else {
            return;
        };
        let clip = self.chat.messages.get(&channel).and_then(|w| {
            w.messages
                .iter()
                .rev()
                .find_map(|m| m.attachments.iter().find_map(Clip::from_attachment))
        });
        let Some(clip) = clip else { return };
        let (w, h) = (320u32, 180u32);
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let band = ((x / 40 + y / 45) % 2) as u8;
                rgba.extend_from_slice(&[
                    (50 + x * 150 / w) as u8,
                    (70 + y * 100 / h) as u8 + band * 25,
                    150,
                    255,
                ]);
            }
        }
        let mut pb = Playback::new(clip, channel, 1);
        pb.attach(Box::new(Quiet), None);
        for ev in [
            Event::Ready {
                width: w,
                height: h,
                duration_ms: 42_000,
            },
            Event::Frame {
                width: w,
                height: h,
                rgba,
            },
            Event::Time {
                position_ms: 12_000,
                duration_ms: 42_000,
                state: PlayState::Paused,
            },
        ] {
            pb.apply(Progress::Helper(ev));
        }
        pb.fullscreen = fullscreen;
        self.video = Some(pb);
    }

    /// Esc leaves the full-screen view. True when it did.
    pub(super) fn escape_video(&mut self) -> Option<Task<Message>> {
        let v = self.video.as_mut().filter(|v| v.fullscreen)?;
        v.fullscreen = false;
        Some(self.set_fullscreen_window(false))
    }
}

/// What the player says when the download fails.
fn download_error_text(e: &crate::error::Error) -> String {
    use crate::error::Error;
    match e {
        Error::Protocol(m) if m.contains("too large") => format!(
            "This video is larger than {} MB, which the in-app player does not download. \
             Choose \u{201c}Open externally\u{201d} to watch it in the browser.",
            MAX_DOWNLOAD / (1024 * 1024)
        ),
        Error::Protocol(m) if m.contains("not allowed") => {
            "This video is not hosted by Discord, so it is not played in the app.".into()
        }
        Error::Http {
            status: 404 | 403 | 410,
            ..
        } => "This video is no longer available (its link expired or it was deleted).".into(),
        _ => "This video could not be downloaded. Check your connection and try again.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Clone, Default)]
    struct Rec(Rc<RefCell<Vec<Cmd>>>);
    impl Sink for Rec {
        fn send(&self, cmd: Cmd) {
            self.0.borrow_mut().push(cmd);
        }
    }

    fn clip() -> Clip {
        Clip {
            id: "att:1".into(),
            url: "https://cdn.discordapp.com/a/b/c.mp4".into(),
            name: "c.mp4".into(),
            size: 10,
            width: Some(640),
            height: Some(360),
            poster: None,
        }
    }

    fn playing() -> (Playback, Rec) {
        let rec = Rec::default();
        let mut p = Playback::new(clip(), crate::net::model::Snowflake(1), 1);
        p.attach(Box::new(rec.clone()), None);
        p.apply(Progress::Helper(Event::Ready {
            width: 320,
            height: 180,
            duration_ms: 10_000,
        }));
        rec.0.borrow_mut().clear();
        (p, rec)
    }

    fn time(p: &mut Playback, ms: u64, state: PlayState) {
        p.apply(Progress::Helper(Event::Time {
            position_ms: ms,
            duration_ms: 10_000,
            state,
        }));
    }

    #[test]
    fn a_video_goes_from_download_to_playing() {
        let mut p = Playback::new(clip(), crate::net::model::Snowflake(1), 1);
        assert!(matches!(p.phase, Phase::Downloading { received: 0, .. }));
        p.apply(Progress::Download {
            received: 500,
            total: Some(1000),
        });
        assert_eq!(
            p.phase,
            Phase::Downloading {
                received: 500,
                total: Some(1000)
            }
        );
        p.attach(Box::new(Rec::default()), None);
        assert_eq!(p.phase, Phase::Starting);
        assert!(!p.is_active(), "controls are off until the first picture");
        p.apply(Progress::Helper(Event::Frame {
            width: 2,
            height: 1,
            rgba: vec![0; 8],
        }));
        assert_eq!(p.phase, Phase::Playing);
        assert!(p.frame.is_some());
        assert_eq!(p.frame_size, (2, 1));
    }

    #[test]
    fn ready_applies_the_volume_and_a_pending_seek() {
        let rec = Rec::default();
        let mut p = Playback::new(clip(), crate::net::model::Snowflake(1), 1);
        p.volume = 40;
        p.attach(Box::new(rec.clone()), None);
        p.apply(Progress::Helper(Event::Ready {
            width: 1,
            height: 1,
            duration_ms: 5000,
        }));
        assert_eq!(
            *rec.0.borrow(),
            vec![Cmd::Volume(40), Cmd::Mute(false)],
            "a new helper learns the volume the user chose"
        );
    }

    #[test]
    fn play_and_pause_send_commands_and_flip_the_state() {
        let (mut p, rec) = playing();
        assert!(p.is_playing());
        assert_eq!(p.toggle(), None);
        assert_eq!(p.state, PlayState::Paused);
        assert!(!p.is_playing());
        assert_eq!(p.toggle(), None);
        assert!(p.is_playing());
        assert_eq!(*rec.0.borrow(), vec![Cmd::Pause, Cmd::Play]);
    }

    #[test]
    fn dragging_the_slider_holds_the_position_until_release() {
        let (mut p, rec) = playing();
        time(&mut p, 1000, PlayState::Playing);
        p.seek_drag(7000);
        time(&mut p, 1250, PlayState::Playing);
        assert_eq!(p.progress_ms(), 7000, "helper times do not move the thumb");
        assert_eq!(p.seek_release(), None);
        assert_eq!(p.position_ms, 7000);
        assert_eq!(*rec.0.borrow(), vec![Cmd::Seek(7000)]);
        assert_eq!(p.seeking, None);
        // Beyond the end is clamped; an unknown duration cannot be seeked.
        p.seek_drag(99_999);
        assert_eq!(p.seeking, Some(10_000));
        let (mut q, _) = playing();
        q.duration_ms = 0;
        q.seek_drag(5);
        assert_eq!(q.seeking, None);
    }

    #[test]
    fn the_end_releases_the_helper_and_replay_asks_for_a_new_one() {
        let (mut p, rec) = playing();
        time(&mut p, 10_000, PlayState::Ended);
        assert!(p.is_ended());
        assert!(!p.has_host(), "RAM is returned when playback ends");
        assert_eq!(p.toggle(), Some(Intent::Respawn));
        assert!(p.is_playing());
        assert_eq!(p.position_ms, 0);
        assert!(rec.0.borrow().is_empty(), "nobody is left to command");
        // A seek after the end restarts too and lands where asked.
        let (mut q, _) = playing();
        time(&mut q, 10_000, PlayState::Ended);
        q.seek_drag(3000);
        assert_eq!(q.seek_release(), Some(Intent::Respawn));
        let rec2 = Rec::default();
        q.attach(Box::new(rec2.clone()), None);
        q.apply(Progress::Helper(Event::Ready {
            width: 1,
            height: 1,
            duration_ms: 10_000,
        }));
        assert!(rec2.0.borrow().contains(&Cmd::Seek(3000)));
    }

    #[test]
    fn replay_before_the_helper_is_gone_just_plays() {
        let (mut p, rec) = playing();
        // `Ended` arrives with the helper still attached only in a race
        // where the state was set locally.
        p.state = PlayState::Ended;
        assert_eq!(p.toggle(), None);
        assert_eq!(*rec.0.borrow(), vec![Cmd::Play]);
    }

    #[test]
    fn a_failure_shows_the_message_and_releases_the_helper() {
        let (mut p, _) = playing();
        p.apply(Progress::Helper(Event::Failed(Failure::MissingCodec(
            "H.265 (HEVC)".into(),
        ))));
        match &p.phase {
            Phase::Failed { text } => {
                assert!(
                    text.contains("H.265") && text.contains("Open externally"),
                    "{text}"
                );
            }
            other => panic!("{other:?}"),
        }
        assert!(!p.has_host());
        assert!(!p.is_active());
        assert_eq!(p.toggle(), None, "controls are inert after a failure");
        // A failed download is a failure too.
        let mut q = Playback::new(clip(), crate::net::model::Snowflake(1), 1);
        q.apply(Progress::Downloaded(Err("nope".into())));
        assert_eq!(
            q.phase,
            Phase::Failed {
                text: "nope".into()
            }
        );
    }

    #[test]
    fn volume_and_mute_reach_the_helper() {
        let (mut p, rec) = playing();
        p.toggle_mute();
        assert!(p.muted);
        p.set_volume(30);
        assert!(!p.muted, "moving the volume unmutes");
        p.set_volume(250);
        assert_eq!(p.volume, 100);
        assert_eq!(
            *rec.0.borrow(),
            vec![
                Cmd::Mute(true),
                Cmd::Volume(30),
                Cmd::Mute(false),
                Cmd::Volume(100),
                Cmd::Mute(false)
            ]
        );
    }

    #[test]
    fn dropping_the_playback_stops_the_helper_and_deletes_the_file() {
        struct Quit(Rc<RefCell<bool>>);
        impl Sink for Quit {
            fn send(&self, _: Cmd) {}
        }
        impl Drop for Quit {
            fn drop(&mut self) {
                *self.0.borrow_mut() = true;
            }
        }
        let dir = std::env::temp_dir().join(format!("ferrocord-pb-{}", std::process::id()));
        let (file, _f) = Scratch::create(&dir, "mp4").unwrap();
        let path = file.path().to_path_buf();
        let gone = Rc::new(RefCell::new(false));
        let mut p = Playback::new(clip(), crate::net::model::Snowflake(1), 1);
        p.attach(Box::new(Quit(gone.clone())), Some(Arc::new(file)));
        assert!(path.exists());
        drop(p);
        assert!(*gone.borrow(), "the helper handle was dropped");
        assert!(!path.exists(), "the downloaded file was removed");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn download_errors_have_plain_texts() {
        use crate::error::Error;
        assert!(download_error_text(&Error::Protocol("media too large".into())).contains("200 MB"));
        assert!(
            download_error_text(&Error::Http {
                status: 404,
                message: String::new()
            })
            .contains("no longer available")
        );
        assert!(
            download_error_text(&Error::Protocol("media host not allowed".into()))
                .contains("not hosted")
        );
    }

    #[tokio::test]
    async fn the_event_stream_keeps_only_the_newest_frame() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let frame = |b: u8| Event::Frame {
            width: 1,
            height: 1,
            rgba: vec![b; 4],
        };
        let time = Event::Time {
            position_ms: 1,
            duration_ms: 2,
            state: PlayState::Playing,
        };
        for ev in [frame(1), frame(2), frame(3), time.clone(), frame(4)] {
            tx.send(ev).unwrap();
        }
        drop(tx);
        let got: Vec<Event> = helper_stream(rx).collect().await;
        assert_eq!(got, vec![frame(3), time, frame(4)]);
    }
}
