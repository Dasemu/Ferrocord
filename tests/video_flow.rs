//! The in-chat video player driven through `App::update` in mock mode:
//! start, play, pause, seek, volume, full screen, end, replay, failure,
//! channel switch, stale events. The mock player replaces the helper, and
//! helper events are fed in by hand so the test is deterministic.

use ferrocord::app::video::{Phase, Progress};
use ferrocord::app::{App, Done, Message, Ui};
use ferrocord::config::Config;
use ferrocord::media::clip::Clip;
use ferrocord::media::video::{Event, Failure};
use ferrocord::media::video_proto::PlayState;
use ferrocord::mock;

fn app_in_showcase() -> App {
    let mut a = App::boot(Config::for_tests()).0;
    let _ = a.update(Message::Ui(Ui::SelectChannel(mock::CH_SHOWCASE)));
    a
}

fn showcase_clip(a: &App) -> Clip {
    a.chat.messages[&mock::CH_SHOWCASE]
        .messages
        .iter()
        .flat_map(|m| m.attachments.iter())
        .find_map(Clip::from_attachment)
        .expect("the mock showcase has a video attachment")
}

fn feed(a: &mut App, ev: Event) {
    let session = a.video.as_ref().expect("a video is active").session;
    let _ = a.update(Message::Done(Done::Video {
        session,
        progress: Progress::Helper(ev),
    }));
}

fn ready(a: &mut App) {
    feed(
        a,
        Event::Ready {
            width: 16,
            height: 9,
            duration_ms: 42_000,
        },
    );
    feed(
        a,
        Event::Frame {
            width: 16,
            height: 9,
            rgba: vec![200; 16 * 9 * 4],
        },
    );
}

fn time(a: &mut App, ms: u64, state: PlayState) {
    feed(
        a,
        Event::Time {
            position_ms: ms,
            duration_ms: 42_000,
            state,
        },
    );
}

fn ui(a: &mut App, u: Ui) {
    let _ = a.update(Message::Ui(u));
}

fn start(a: &mut App) {
    let clip = showcase_clip(a);
    ui(a, Ui::PlayVideo(Box::new(clip)));
}

#[test]
fn the_showcase_has_a_playable_video_with_a_poster() {
    let a = app_in_showcase();
    let c = showcase_clip(&a);
    assert_eq!(c.name, "renderer-demo.mp4");
    assert_eq!(c.inline_size(), (400, 225));
    assert_eq!(c.poster.as_deref(), Some("mock://video/poster"));
    assert!(
        a.image("mock://video/poster").is_some(),
        "the poster is cached"
    );
}

#[test]
fn play_pause_seek_volume_and_close() {
    let mut a = app_in_showcase();
    assert!(a.video.is_none());
    start(&mut a);
    let v = a.video.as_ref().unwrap();
    assert_eq!(v.phase, Phase::Starting);
    assert!(v.has_host());
    ready(&mut a);
    let v = a.video.as_ref().unwrap();
    assert_eq!(v.phase, Phase::Playing);
    assert!(v.is_playing() && v.frame.is_some());
    assert_eq!(v.duration_ms, 42_000);

    ui(&mut a, Ui::VideoToggle);
    assert!(!a.video.as_ref().unwrap().is_playing());
    ui(&mut a, Ui::VideoToggle);
    assert!(a.video.as_ref().unwrap().is_playing());

    time(&mut a, 3_000, PlayState::Playing);
    ui(&mut a, Ui::VideoSeekDrag(20_000));
    time(&mut a, 3_250, PlayState::Playing);
    assert_eq!(a.video.as_ref().unwrap().progress_ms(), 20_000);
    ui(&mut a, Ui::VideoSeekRelease);
    assert_eq!(a.video.as_ref().unwrap().position_ms, 20_000);
    assert_eq!(a.video.as_ref().unwrap().seeking, None);

    ui(&mut a, Ui::VideoMute);
    assert!(a.video.as_ref().unwrap().muted);
    ui(&mut a, Ui::VideoVolume(35));
    let v = a.video.as_ref().unwrap();
    assert!(!v.muted && v.volume == 35);

    ui(&mut a, Ui::CloseVideo);
    assert!(a.video.is_none());
}

#[test]
fn full_screen_opens_and_escape_closes_it_without_stopping_the_video() {
    let mut a = app_in_showcase();
    start(&mut a);
    ui(&mut a, Ui::VideoFullscreen);
    assert!(
        !a.video.as_ref().unwrap().fullscreen,
        "nothing to show before the first picture"
    );
    ready(&mut a);
    ui(&mut a, Ui::VideoFullscreen);
    assert!(a.video.as_ref().unwrap().fullscreen);
    ui(&mut a, Ui::Escape);
    let v = a.video.as_ref().unwrap();
    assert!(!v.fullscreen, "Esc leaves full screen");
    assert!(v.is_playing(), "the video keeps playing inline");
    // Esc with no full screen does not stop it either.
    ui(&mut a, Ui::Escape);
    assert!(a.video.is_some());
}

#[test]
fn the_end_frees_the_helper_and_replay_starts_a_new_one() {
    let mut a = app_in_showcase();
    start(&mut a);
    ready(&mut a);
    let first = a.video.as_ref().unwrap().session;
    time(&mut a, 42_000, PlayState::Ended);
    let v = a.video.as_ref().unwrap();
    assert!(v.is_ended());
    assert!(!v.has_host(), "the decoder is released at the end");
    assert!(v.frame.is_some(), "the last picture stays");

    ui(&mut a, Ui::VideoToggle);
    let v = a.video.as_ref().unwrap();
    assert!(v.has_host(), "replay starts a fresh player");
    assert_ne!(v.session, first);
    // The old session's late events are ignored.
    let _ = a.update(Message::Done(Done::Video {
        session: first,
        progress: Progress::Helper(Event::Time {
            position_ms: 999,
            duration_ms: 42_000,
            state: PlayState::Ended,
        }),
    }));
    assert!(a.video.as_ref().unwrap().has_host());
    ready(&mut a);
    assert!(a.video.as_ref().unwrap().is_playing());
}

#[test]
fn a_failure_is_shown_and_can_be_closed() {
    let mut a = app_in_showcase();
    start(&mut a);
    feed(
        &mut a,
        Event::Failed(Failure::MissingCodec("H.265 (HEVC)".into())),
    );
    match &a.video.as_ref().unwrap().phase {
        Phase::Failed { text } => {
            assert!(text.contains("H.265") && text.contains("Open externally"))
        }
        other => panic!("{other:?}"),
    }
    assert!(!a.video.as_ref().unwrap().has_host());
    // Controls do nothing on a failed video; clicking play again starts over.
    ui(&mut a, Ui::VideoToggle);
    assert!(matches!(
        a.video.as_ref().unwrap().phase,
        Phase::Failed { .. }
    ));
    start(&mut a);
    assert_eq!(a.video.as_ref().unwrap().phase, Phase::Starting);
    feed(&mut a, Event::Failed(Failure::Broken("decode".into())));
    ui(&mut a, Ui::CloseVideo);
    assert!(a.video.is_none());
}

#[test]
fn leaving_the_channel_stops_the_video() {
    let mut a = app_in_showcase();
    start(&mut a);
    ready(&mut a);
    assert!(a.video.is_some());
    ui(&mut a, Ui::SelectChannel(mock::CH_GENERAL));
    assert!(
        a.video.is_none(),
        "the helper ends when its channel is left"
    );
}

#[test]
fn starting_another_video_replaces_the_first() {
    let mut a = app_in_showcase();
    start(&mut a);
    let first = a.video.as_ref().unwrap().session;
    let mut other = showcase_clip(&a);
    other.id = "att:other".into();
    ui(&mut a, Ui::PlayVideo(Box::new(other)));
    let v = a.video.as_ref().unwrap();
    assert_eq!(v.clip.id, "att:other");
    assert_ne!(v.session, first);
    // Clicking the active video again just toggles it, it does not restart.
    ready(&mut a);
    let session = a.video.as_ref().unwrap().session;
    let again = a.video.as_ref().unwrap().clip.clone();
    ui(&mut a, Ui::PlayVideo(Box::new(again)));
    let v = a.video.as_ref().unwrap();
    assert_eq!(v.session, session);
    assert!(!v.is_playing());
}

#[test]
fn logging_out_drops_the_player() {
    let mut a = app_in_showcase();
    start(&mut a);
    ready(&mut a);
    ui(&mut a, Ui::Logout);
    assert!(
        a.video.is_none(),
        "nothing keeps playing on the login screen"
    );
}

#[test]
fn frames_become_handles_once_per_frame_not_per_view() {
    let mut a = app_in_showcase();
    start(&mut a);
    ready(&mut a);
    let h1 = a.video.as_ref().unwrap().frame.clone().unwrap();
    // Reading the frame for drawing does not create a new handle.
    let h1_again = a.video.as_ref().unwrap().frame.clone().unwrap();
    assert_eq!(h1.id(), h1_again.id());
    feed(
        &mut a,
        Event::Frame {
            width: 16,
            height: 9,
            rgba: vec![10; 16 * 9 * 4],
        },
    );
    let h2 = a.video.as_ref().unwrap().frame.clone().unwrap();
    assert_ne!(h1.id(), h2.id(), "a new picture is a new handle");
}
